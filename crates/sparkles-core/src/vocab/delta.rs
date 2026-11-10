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
//! An empty file, or one whose first chunk a crash cut short before its bytes reached
//! the disk, holds only zeros and no entries. The header is written with the first
//! chunk. Pre-release files held the entries alone without a header, as version 1 of
//! the format, and this build refuses one with the dump-and-reload message (spec X16
//! Phase 1).
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

/// The first bytes of a framed delta vocabulary file. A pre-release file of version 1
/// starts with the length of its first key, which would have to be over a gigabyte to
/// match.
pub const MAGIC: [u8; 8] = *b"\x89SDV\r\n\x1a\n";
/// The version of the framed format in its header, and the only version this build
/// reads.
pub const VERSION: u32 = 2;
/// The version of a pre-release file without a header, which this build refuses.
const UNFRAMED_VERSION: u32 = 1;
/// Bytes of the file header.
pub const HEADER: usize = 16;
/// Bytes of a chunk header.
pub const CHUNK_HEADER: usize = 24;
/// The most payload one chunk holds, unless a single entry is larger. A write of more
/// entries, such as a group commit fence's terms, spans several chunks, since a chunk's
/// header records its payload length in 32 bits.
const MAX_CHUNK: usize = 1 << 20;
/// The most a delta vocabulary grows ahead of its chunks at once.
pub const MAX_PREALLOC: u64 = 1 << 20;

/// The complete entries of a delta vocabulary file.
pub struct ParsedDelta<'a> {
    pub keys: Vec<&'a [u8]>,
    /// where the complete entries end: past it lie zeros or a torn tail
    pub end: usize,
    /// chunks of the file
    pub chunks: usize,
}

/// Read a delta vocabulary file's content (see the module documentation). A file of a
/// later version, and a pre-release file without a header, are [`Error::Unsupported`],
/// and a chunk whose checksum matches but whose entries do not fill it exactly is
/// [`Error::Corrupt`].
pub fn parse_delta(buf: &[u8]) -> Result<ParsedDelta<'_>> {
    parse_delta_with(buf, |_| {})
}

/// Where each chunk of the file ends, in order.
pub fn chunk_ends(buf: &[u8]) -> Result<Vec<usize>> {
    let mut ends = Vec::new();
    parse_delta_with(buf, |end| ends.push(end))?;
    Ok(ends)
}

fn parse_delta_with(buf: &[u8], on_chunk: impl FnMut(usize)) -> Result<ParsedDelta<'_>> {
    parse_delta_inner(buf, on_chunk, None)
}

pub(crate) fn parse_delta_controlled<'a>(
    buf: &'a [u8],
    budget: &mut crate::store::cold::ColdBudget<'_>,
) -> Result<ParsedDelta<'a>> {
    parse_delta_inner(buf, |_| {}, Some(budget))
}

fn parse_delta_inner<'a>(
    buf: &'a [u8],
    mut on_chunk: impl FnMut(usize),
    mut budget: Option<&mut crate::store::cold::ColdBudget<'_>>,
) -> Result<ParsedDelta<'a>> {
    let mut out = ParsedDelta {
        keys: Vec::new(),
        end: 0,
        chunks: 0,
    };
    if buf.len() < MAGIC.len() || buf[..MAGIC.len()] != MAGIC {
        if let Some(budget) = budget.as_deref_mut() {
            budget.check()?;
        }
        if !torn_first_chunk(buf) {
            return Err(crate::formats::DELTA_VOCAB.refuse("delta.vocab", UNFRAMED_VERSION, None));
        }
        return Ok(out);
    }
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
        if let Some(budget) = budget.as_deref_mut() {
            budget.check()?;
        }
        let word = |at: usize| u32::from_le_bytes(h[at..at + 4].try_into().unwrap());
        let (crc, n, plen) = (word(0), word(4) as usize, word(8) as usize);
        let first = u64::from_le_bytes(h[16..24].try_into().unwrap());
        if n == 0 || first != out.keys.len() as u64 {
            break;
        }
        let Some(payload) = buf.get(pos + CHUNK_HEADER..pos + CHUNK_HEADER + plen) else {
            break;
        };
        let actual = match budget.as_deref_mut() {
            Some(budget) => budget.crc32(&[&h[4..], payload])?,
            None => chunk_crc(&h[4..], payload),
        };
        if crc != actual {
            break;
        }
        let (mut p, before) = (0usize, out.keys.len());
        for _ in 0..n {
            if let Some(budget) = budget.as_deref_mut() {
                budget.check()?;
            }
            let len = match payload.get(p..p + 4) {
                Some(l) => u32::from_le_bytes(l.try_into().unwrap()) as usize,
                None => break,
            };
            if len == 0 || p + 4 + len > plen {
                break;
            }
            if let Some(budget) = budget.as_deref_mut() {
                let need = out.keys.len() + 1;
                budget.reserve(&mut out.keys, need)?;
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

fn chunk_crc(header: &[u8], payload: &[u8]) -> u32 {
    let mut c = flate2::Crc::new();
    c.update(header);
    c.update(payload);
    c.sum()
}

/// `keys` as chunks of at most [`MAX_CHUNK`] bytes of payload each (one entry when it is
/// larger), the first numbered `first`, after the file header when `header` is set.
/// Refuses an entry whose length a chunk cannot record.
fn encode_chunks(first: u64, keys: &[Arc<[u8]>], header: bool) -> Result<Vec<u8>> {
    if let Some(k) = keys
        .iter()
        .find(|k| k.len() > u32::MAX as usize - 4 - CHUNK_HEADER)
    {
        return Err(Error::Unsupported(format!(
            "a term of {} bytes is too large for the delta vocabulary",
            k.len()
        )));
    }
    let mut chunks = Vec::new();
    let mut start = 0usize;
    while start < keys.len() {
        let (mut end, mut size) = (start, 0usize);
        while end < keys.len() && (end == start || size + 4 + keys[end].len() <= MAX_CHUNK) {
            size += 4 + keys[end].len();
            end += 1;
        }
        chunks.push(start..end);
        start = end;
    }
    let total: usize = keys.iter().map(|k| 4 + k.len()).sum();
    let mut out = Vec::with_capacity(HEADER + total + CHUNK_HEADER * chunks.len());
    for range in chunks {
        let header = header && range.start == 0;
        encode_chunk(&mut out, first + range.start as u64, &keys[range], header);
    }
    Ok(out)
}

/// Append a chunk holding `keys`, whose first entry has id `first` within the file,
/// preceded by the file header when `header` is set.
fn encode_chunk(out: &mut Vec<u8>, first: u64, keys: &[Arc<[u8]>], header: bool) {
    let plen: usize = keys.iter().map(|k| 4 + k.len()).sum();
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
}

/// The end of a delta vocabulary at some moment (see [`DeltaVocab::mark`]).
#[derive(Clone, Copy, Debug)]
pub struct VocabMark {
    entries: u64,
    /// the file's entries written to it, and where they end
    written: u64,
    end: u64,
}

impl VocabMark {
    /// The vocabulary's entries when the mark was taken.
    pub(crate) fn entries(&self) -> u64 {
        self.entries
    }
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
    /// a chunk is being written through the write-through handle, without the mutex
    writing: bool,
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
    /// Entries of the vocabulary not written to the file yet, before entry `upto` of the
    /// vocabulary (`u64::MAX`: all of them).
    fn pending(&self, upto: u64) -> Vec<Arc<[u8]>> {
        let v = self.vocab.read();
        let from = (self.base + self.written) as usize;
        let to = (upto.min(v.len())).max(self.base + self.written) as usize;
        v.keys.get(from..to).map_or_else(Vec::new, <[_]>::to_vec)
    }

    /// Whether a written chunk is not durable yet, or an entry before `upto` is not
    /// written yet.
    fn needs_sync(&self, upto: u64) -> bool {
        self.durable < self.end || self.base + self.written < self.vocab.read().len().min(upto)
    }

    /// Write the pending entries before entry `upto` as a chunk at the logical end, with a
    /// buffered write, growing the file by zero bytes when the chunk runs past its length.
    /// Not synced.
    fn write_pending(&mut self, upto: u64) -> Result<()> {
        let keys = self.pending(upto);
        if keys.is_empty() {
            return Ok(());
        }
        if self.writing {
            // only a sync writes while a fence's chunk is in flight, and the gate holds
            // syncs back, so this is a caller outside the sync protocol
            return Err(std::io::Error::other(
                "the delta vocabulary is being written through its write-through handle",
            )
            .into());
        }
        self.forget_direct_block();
        // the header starts the file with its first chunk
        let chunk = encode_chunks(self.written, &keys, self.end == 0)?;
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
    fn sync_gated(file: &Arc<Mutex<Self>>, upto: u64) -> Result<()> {
        let (fd, end) = {
            let mut f = file.lock();
            if !f.needs_sync(upto) {
                return Ok(());
            }
            f.write_pending(upto)?;
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

    /// Write and synchronize the entries before entry `upto` (`u64::MAX`: all).
    pub(crate) fn sync(file: &Arc<Mutex<Self>>, upto: u64) -> Result<()> {
        // Never acquire this gate while retaining the mutex. Rollback uses the same
        // order, so a captured prefix cannot be cut under its fence.
        let gate = file.lock().sync_gate.clone();
        let _fence = gate.lock();
        Self::sync_gated(file, upto)
    }

    /// [`sync`](Self::sync) through the write-through handle when it can: the pending
    /// entries as one chunk that fits in the preallocated zeros, after a durable
    /// prefix. The mutex stays held for the write, so no buffered write can land in
    /// the block it rewrites.
    pub(crate) fn sync_direct(file: &Arc<Mutex<Self>>, upto: u64) -> Result<()> {
        let gate = file.lock().sync_gate.clone();
        let _fence = gate.lock();
        // The chunk is encoded under the mutex and written without it, so that the writer
        // can mark and insert while the write runs: a group commit fence writes its terms
        // while the next transaction starts (X09 item 446). The handle is taken out of the
        // file for the write and `writing` is set, so no buffered write can land in the
        // block it rewrites, and the sync gate keeps every other sync and rollback out.
        let attempt = {
            let mut guard = file.lock();
            let f = &mut *guard;
            if !f.needs_sync(upto) {
                return Ok(());
            }
            let keys = f.pending(upto);
            let mut attempt = None;
            if f.end >= HEADER as u64 && f.durable == f.end && !keys.is_empty() {
                let chunk = encode_chunks(f.written, &keys, false)?;
                if DirectLog::fits(f.end, chunk.len(), f.alloc)
                    && f.direct.handle(Some(&f.path), &f.file).is_some()
                {
                    let wal::Direct::Open(direct) =
                        std::mem::replace(&mut f.direct, wal::Direct::Untried)
                    else {
                        unreachable!("the handle was opened above")
                    };
                    f.writing = true;
                    attempt = Some((direct, chunk, f.end, keys.len() as u64));
                }
            }
            attempt
        };
        if let Some((mut direct, chunk, at, n)) = attempt {
            #[cfg(test)]
            {
                let hook = file.lock().sync_hook.clone();
                if let Some(hook) = hook {
                    hook();
                }
            }
            let written = direct.write(at, &chunk);
            let mut f = file.lock();
            f.writing = false;
            match written {
                Ok(true) => {
                    f.direct = wal::Direct::Open(direct);
                    debug_assert_eq!(f.end, at, "the vocabulary moved during a direct write");
                    f.end += chunk.len() as u64;
                    f.written += n;
                    #[cfg(test)]
                    {
                        if std::mem::take(&mut f.fail_next_sync) {
                            f.failed = true;
                            return Err(std::io::Error::other("injected sync failure").into());
                        }
                    }
                    f.durable = f.end;
                    return Ok(());
                }
                // the file system refused direct I/O: buffered from now on
                Ok(false) => f.direct = wal::Direct::Off,
                Err(e) => {
                    f.direct = wal::Direct::Open(direct);
                    f.failed = true;
                    return Err(e.into());
                }
            }
        }
        Self::sync_gated(file, upto)
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

/// Whether a file that does not start with the complete header is the first chunk a
/// crash tore, which holds no entries: an empty file, or one whose header, written with
/// the chunk, did not reach the disk, so that its first eight bytes are zeros, or a
/// prefix of the magic followed by zeros, whatever later pages of the chunk hold. A
/// pre-release file of the entries alone starts with the length of its first key, which
/// is neither. The parser and [`file_version`] share this rule.
fn torn_first_chunk(buf: &[u8]) -> bool {
    let head = &buf[..buf.len().min(MAGIC.len())];
    let magic = head
        .iter()
        .zip(MAGIC.iter())
        .take_while(|(a, b)| a == b)
        .count();
    head[magic..].iter().all(|&byte| byte == 0)
}

/// The version of the delta vocabulary at `path`: the header's version, [`VERSION`]
/// for a missing or empty file or a torn first chunk (see [`torn_first_chunk`]), which
/// holds no entries yet, and 1 for a pre-release file of the entries alone, which this
/// build refuses.
pub fn file_version(path: &Path) -> Result<u32> {
    let buf = read_file(path)?;
    if buf.len() >= 12 && buf[..MAGIC.len()] == MAGIC {
        // a header torn before its version holds no entries yet
        Ok(match u32::from_le_bytes(buf[8..12].try_into().unwrap()) {
            0 => VERSION,
            v => v,
        })
    } else if torn_first_chunk(&buf) {
        Ok(VERSION)
    } else {
        Ok(UNFRAMED_VERSION)
    }
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
    pub(crate) fn resident_bytes_limited(
        &self,
        limit: &crate::history::ReadLimit<'_>,
    ) -> Result<u64> {
        let guard = limit.read_lock(&self.inner)?;
        Ok(guard.cold_bytes() + std::mem::size_of::<RwLock<AppendVocab>>() as u64 + 64)
    }
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

    /// The read-only dictionary of a retained generation, with its wire buffers,
    /// entry index and sparse owned dictionary admitted before growth.
    pub(crate) fn open_read_only_controlled(
        path: &Path,
        budget: &mut crate::store::cold::ColdBudget<'_>,
    ) -> Result<crate::store::cold::Loaded<DeltaVocab>> {
        let start = budget.used();
        let buf = match budget.read_mutable_prefix(path) {
            Ok(bytes) => bytes,
            Err(Error::Io(error)) if error.kind() == std::io::ErrorKind::NotFound => Vec::new(),
            Err(error) => return Err(error),
        };
        let parsed = parse_delta_controlled(&buf, budget).map_err(|e| corrupt_in(path, e))?;
        let mut vocab = AppendVocab::default();
        for key in &parsed.keys {
            vocab.push_controlled(key, false, budget)?;
        }
        budget.drop_vec(parsed.keys);
        budget.drop_vec(buf);
        let bytes = vocab.cold_bytes();
        let wrapper = std::mem::size_of::<RwLock<AppendVocab>>() as u64 + 64;
        budget.charge(wrapper)?;
        debug_assert_eq!(budget.used() - start, bytes + wrapper);
        Ok(crate::store::cold::Loaded {
            value: DeltaVocab {
                inner: Arc::new(RwLock::new(vocab)),
                file: None,
            },
            charge: bytes + wrapper,
        })
    }

    /// Test support for observing the actual closed cold-reader allocation path.
    #[doc(hidden)]
    #[cfg(feature = "failpoints")]
    pub fn open_read_only_limited_for_test(
        path: &Path,
        options: &crate::history::HistoryOptions,
        memory: u64,
    ) -> Result<(DeltaVocab, u64)> {
        let limit = crate::history::ReadLimit {
            options,
            memory: Some(memory),
            held: 0,
        };
        let mut budget = crate::store::cold::ColdBudget::new(limit)?;
        let loaded = Self::open_read_only_controlled(path, &mut budget)?;
        Ok((loaded.value, loaded.charge))
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
        let end = parsed.end as u64;
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
                written,
                end,
                alloc,
                durable: end,
                prealloc: MAX_PREALLOC,
                direct: wal::Direct::Untried,
                writing: false,
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

    pub(crate) fn open_layered_controlled(
        prefix: &[(PathBuf, u64)],
        own: &Path,
        budget: &mut crate::store::cold::ColdBudget<'_>,
    ) -> Result<crate::store::cold::Loaded<DeltaVocab>> {
        let start = budget.used();
        let mut vocab = AppendVocab::default();
        for (path, end) in prefix {
            budget.check()?;
            if vocab.len() >= *end {
                continue;
            }
            extend_read_only_controlled(&mut vocab, path, Some(*end), budget)?;
            if vocab.len() < *end {
                return Err(Error::Corrupt(format!(
                    "{} has fewer delta terms than a branch's link names",
                    path.display()
                )));
            }
        }
        extend_read_only_controlled(&mut vocab, own, None, budget)?;
        budget.charge(std::mem::size_of::<RwLock<AppendVocab>>() as u64 + 64)?;
        let charge = budget.used() - start;
        Ok(crate::store::cold::Loaded {
            value: DeltaVocab {
                inner: Arc::new(RwLock::new(vocab)),
                file: None,
            },
            charge,
        })
    }

    pub fn len(&self) -> u64 {
        self.inner.read().len()
    }
    pub(crate) fn len_limited(&self, limit: &crate::history::ReadLimit<'_>) -> Result<u64> {
        Ok(limit.read_lock(&self.inner)?.len())
    }

    /// Borrow dictionary bytes only after controlled lock acquisition. The callback
    /// must admit any owned copies against its own request budget before allocation.
    pub fn with_controlled<R>(
        &self,
        control: &crate::task::Control,
        f: impl FnOnce(&AppendVocab) -> Result<R>,
    ) -> Result<R> {
        let guard = crate::task::read_lock(&self.inner, || control.check())?;
        let result = f(&guard)?;
        control.check()?;
        Ok(result)
    }

    pub(crate) fn with_prepared<R>(
        &self,
        preparation: &crate::access::Preparation,
        f: impl FnOnce(&AppendVocab) -> Result<R>,
    ) -> Result<R> {
        let guard = preparation.read_lock(&self.inner)?;
        let result = f(&guard)?;
        preparation.check()?;
        Ok(result)
    }

    #[cfg(test)]
    pub(crate) fn write_guard_for_test(&self) -> parking_lot::RwLockWriteGuard<'_, AppendVocab> {
        self.inner.write()
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

    /// The bytes that entries `from` onwards take in a chunk of the file, without the
    /// chunk's header: what a group commit's quota counts for its terms before its fence
    /// writes them.
    pub(crate) fn key_bytes(&self, from: u64) -> u64 {
        let v = self.inner.read();
        v.keys
            .get(from as usize..)
            .map_or(0, |k| k.iter().map(|k| 4 + k.len() as u64).sum())
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
                }
            }
            None => VocabMark {
                entries: self.len(),
                written: 0,
                end: 0,
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
        let to = m.end;
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
            Some(file) => DeltaFile::sync(file, u64::MAX),
            None => Ok(()),
        }
    }

    /// [`sync`](Self::sync), writing the entries through the write-through handle when
    /// they fit in the file's preallocated space (see the module documentation).
    pub fn sync_direct(&self) -> Result<()> {
        self.sync_direct_to(u64::MAX)
    }

    /// [`sync_direct`](Self::sync_direct) of the entries before entry `upto` only, or
    /// [`sync`](Self::sync) of them when they cannot go through the write-through handle.
    pub(crate) fn sync_direct_to(&self, upto: u64) -> Result<()> {
        match &self.file {
            Some(file) => DeltaFile::sync_direct(file, upto),
            None => Ok(()),
        }
    }

    /// [`sync`](Self::sync) of the entries before entry `upto` only.
    pub(crate) fn sync_to(&self, upto: u64) -> Result<()> {
        match &self.file {
            Some(file) => DeltaFile::sync(file, upto),
            None => Ok(()),
        }
    }

    /// Schedule this exact generation's sync of the entries before entry `upto` on a
    /// reusable worker, through the write-through handle when `direct` is set.
    /// Publication still waits for completion; grouped writers may append a suffix.
    ///
    /// A group commit fence passes the end of its prefix's terms. The terms that a later
    /// transaction inserted meanwhile stay unwritten, so a rollback of that transaction
    /// never cuts a chunk that holds terms of the fenced commits, which its mark, taken
    /// before they were written, would place after the cut (X09 item 446).
    pub(crate) fn sync_on(
        &self,
        worker: &mut LazySync,
        direct: bool,
        upto: u64,
    ) -> Option<PendingSync> {
        self.file
            .as_ref()
            .and_then(|file| worker.start(file.clone(), direct, upto))
    }

    /// Whether entries were inserted or written since the last [`sync`](Self::sync), that
    /// is, whether the next one writes and syncs anything.
    pub fn needs_sync(&self) -> bool {
        self.needs_sync_to(u64::MAX)
    }

    /// [`needs_sync`](Self::needs_sync) for the entries before entry `upto` only.
    pub(crate) fn needs_sync_to(&self, upto: u64) -> bool {
        self.file
            .as_ref()
            .is_some_and(|f| f.lock().needs_sync(upto))
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
        self.flush_to(u64::MAX)
    }

    /// [`flush`](Self::flush) of the entries before entry `upto` only.
    pub(crate) fn flush_to(&self, upto: u64) -> Result<u64> {
        match &self.file {
            Some(f) => {
                let mut f = f.lock();
                f.write_pending(upto)?;
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
                f.write_pending(u64::MAX)?;
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

fn extend_read_only_controlled(
    vocab: &mut AppendVocab,
    path: &Path,
    end: Option<u64>,
    budget: &mut crate::store::cold::ColdBudget<'_>,
) -> Result<()> {
    let bytes = match budget.read_mutable_prefix(path) {
        Ok(bytes) => bytes,
        Err(Error::Io(error)) if error.kind() == std::io::ErrorKind::NotFound => Vec::new(),
        Err(error) => return Err(error),
    };
    let parsed = parse_delta_controlled(&bytes, budget).map_err(|e| corrupt_in(path, e))?;
    for key in &parsed.keys {
        budget.check()?;
        if end.is_some_and(|end| vocab.len() >= end) {
            break;
        }
        vocab.push_controlled(key, true, budget)?;
    }
    budget.drop_vec(parsed.keys);
    budget.drop_vec(bytes);
    budget.check()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// One entry of a pre-release file of the entries alone, which this build refuses.
    fn entry(k: &[u8]) -> Vec<u8> {
        [&(k.len() as u32).to_le_bytes()[..], k].concat()
    }

    /// A framed file of `keys` in one chunk, as a write of them would leave it.
    fn framed(keys: &[&[u8]]) -> Vec<u8> {
        let keys: Vec<Arc<[u8]>> = keys.iter().map(|k| Arc::from(*k)).collect();
        encode_chunks(0, &keys, true).unwrap()
    }

    /// The buffer of a write spanning many chunks is allocated once, at its size.
    #[test]
    fn a_write_of_many_chunks_allocates_its_size() {
        let keys: Vec<Arc<[u8]>> = (0..8u8)
            .map(|n| Arc::from(vec![b'a' + n; MAX_CHUNK / 2 + 1]))
            .collect();
        let out = encode_chunks(0, &keys, true).unwrap();
        assert_eq!(
            out.len(),
            HEADER + 8 * (CHUNK_HEADER + 4 + MAX_CHUNK / 2 + 1)
        );
        assert_eq!(out.capacity(), out.len());
    }

    /// A write of more terms than one chunk holds, such as a group commit fence's, spans
    /// several chunks that all read back, each within the chunk limit.
    #[test]
    fn a_large_write_spans_bounded_chunks() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("delta.vocab");
        let vocab = DeltaVocab::open(&path).unwrap();
        vocab.insert(b"<urn:first>").unwrap();
        vocab.sync().unwrap();
        let big: Vec<Vec<u8>> = (0..5u8)
            .map(|n| vec![b'a' + n; MAX_CHUNK / 2 + 1000])
            .collect();
        for k in &big {
            vocab.insert(k).unwrap();
        }
        vocab.insert(b"<urn:last>").unwrap();
        vocab.sync().unwrap();
        drop(vocab);
        let bytes = std::fs::read(&path).unwrap();
        let (mut pos, mut chunks) = (HEADER, Vec::new());
        while let Some(h) = bytes.get(pos..pos + CHUNK_HEADER) {
            let word = |at: usize| u32::from_le_bytes(h[at..at + 4].try_into().unwrap());
            if word(4) == 0 {
                break;
            }
            let (n, plen) = (word(4), word(8) as usize);
            chunks.push((n, plen));
            pos += CHUNK_HEADER + plen;
        }
        // the first sync's chunk, then one per big term (two of them do not fit in one)
        assert_eq!(chunks.len(), 1 + big.len(), "{chunks:?}");
        assert!(
            chunks.iter().all(|&(_, plen)| plen <= MAX_CHUNK),
            "{chunks:?}"
        );
        let vocab = DeltaVocab::open(&path).unwrap();
        assert_eq!(vocab.len(), 2 + big.len() as u64);
        assert_eq!(vocab.get(0).unwrap(), b"<urn:first>");
        for (i, k) in big.iter().enumerate() {
            assert_eq!(&vocab.get(1 + i as u64).unwrap(), k);
        }
        assert_eq!(vocab.get(1 + big.len() as u64).unwrap(), b"<urn:last>");
    }

    #[test]
    fn controlled_read_only_dictionary_matches_wire_codecs_without_writes() {
        use crate::{
            history::{HistoryOptions, ReadLimit},
            store::cold::ColdBudget,
        };
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("delta.vocab");
        let vocab = DeltaVocab::open(&path).unwrap();
        for n in 0..2000 {
            vocab.insert(format!("<urn:term:{n}>").as_bytes()).unwrap();
        }
        vocab.sync().unwrap();
        drop(vocab);
        let before = std::fs::read(&path).unwrap();
        let options = HistoryOptions::default();
        let mut budget = ColdBudget::new(ReadLimit {
            options: &options,
            memory: Some(16 << 20),
            held: 0,
        })
        .unwrap();
        let loaded = DeltaVocab::open_read_only_controlled(&path, &mut budget).unwrap();
        assert_eq!(loaded.value.len(), 2000);
        assert_eq!(loaded.value.get(0).unwrap(), b"<urn:term:0>");
        assert_eq!(loaded.value.get(1999).unwrap(), b"<urn:term:1999>");
        assert_eq!(budget.used(), loaded.charge);
        assert_eq!(before, std::fs::read(&path).unwrap());
        drop(loaded.value);
        budget.release(loaded.charge);
        assert_eq!(budget.used(), 0);
        let mut small = ColdBudget::new(ReadLimit {
            options: &options,
            memory: Some(1024),
            held: 0,
        })
        .unwrap();
        assert!(matches!(
            DeltaVocab::open_read_only_controlled(&path, &mut small),
            Err(Error::BudgetExceeded(_))
        ));
        assert_eq!(before, std::fs::read(&path).unwrap());
    }

    #[test]
    fn controlled_parser_keeps_unframed_zero_and_unknown_version_semantics() {
        use crate::{
            history::{HistoryOptions, ReadLimit},
            store::cold::ColdBudget,
        };
        let options = HistoryOptions::default();
        let mut budget = ColdBudget::new(ReadLimit {
            options: &options,
            memory: Some(1 << 20),
            held: 0,
        })
        .unwrap();
        // a pre-release file of the entries alone is refused, as the plain parser does
        let mut source = entry(b"<urn:a>");
        source.extend(entry(b"<urn:b>"));
        source.extend([0; 64]);
        assert!(matches!(
            parse_delta_controlled(&source, &mut budget),
            Err(Error::Unsupported(_))
        ));
        assert!(matches!(parse_delta(&source), Err(Error::Unsupported(_))));
        assert_eq!(budget.used(), 0);
        // a file of zeros holds no entries yet
        let controlled = parse_delta_controlled(&[0; 64 << 10], &mut budget).unwrap();
        assert!(controlled.keys.is_empty());
        assert_eq!(controlled.end, 0);
        budget.drop_vec(controlled.keys);
        assert_eq!(budget.used(), 0);
        let mut unknown = Vec::from(MAGIC);
        unknown.extend((VERSION + 1).to_le_bytes());
        unknown.extend([0; 4]);
        assert!(matches!(
            parse_delta_controlled(&unknown, &mut budget),
            Err(Error::Unsupported(_))
        ));
        assert_eq!(budget.used(), 0);
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

    /// A pre-release file of the entries alone is refused with the dump-and-reload
    /// message, and an empty file or one of zeros opens with no entries and takes its
    /// header with the first chunk.
    #[test]
    fn an_unframed_file_is_refused_and_a_file_of_zeros_is_empty() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("delta.vocab");
        let unframed = [entry(b"<http://x/a>"), entry(b"<http://x/b>")].concat();
        std::fs::write(&path, [&unframed[..], &[0u8; 23][..]].concat()).unwrap();
        for result in [
            DeltaVocab::open(&path).map(|_| ()),
            DeltaVocab::open_read_only(&path).map(|_| ()),
            file_version(&path).and_then(|v| crate::formats::DELTA_VOCAB.check("x", v, None)),
        ] {
            let error = result.expect_err("an unframed file is refused");
            assert!(matches!(error, Error::Unsupported(_)), "{error}");
            assert!(error.to_string().contains("sparkles dump"), "{error}");
            assert!(error.to_string().contains("version 1"), "{error}");
        }
        // the refusal changes nothing
        assert_eq!(std::fs::read(&path).unwrap().len(), unframed.len() + 23);
        for bytes in [&[][..], &[0u8; 4096][..]] {
            std::fs::write(&path, bytes).unwrap();
            assert_eq!(file_version(&path).unwrap(), VERSION);
            let v = DeltaVocab::open(&path).unwrap();
            assert_eq!(v.len(), 0);
            assert_eq!(v.insert(b"<http://x/c>").unwrap(), 0);
            v.sync().unwrap();
            drop(v);
            let buf = std::fs::read(&path).unwrap();
            assert_eq!(&buf[..8], &MAGIC);
            assert_eq!(parse_delta(&buf).unwrap().chunks, 1);
            assert_eq!(DeltaVocab::open(&path).unwrap().len(), 1);
        }
    }

    /// The first chunk a crash tore holds no entries, whether its first eight bytes are
    /// zeros or a prefix of the magic followed by zeros, and whatever the later pages of
    /// the chunk hold. The parser and `file_version` agree on each form, and so a
    /// pre-release file is told apart by its first key's length alone.
    #[test]
    fn a_torn_first_chunk_with_a_prefix_of_the_magic_is_empty() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("delta.vocab");
        let later_page = [&[0u8; 4096][..], b"<later page of the chunk>"].concat();
        let mut torn: Vec<Vec<u8>> = Vec::new();
        for prefix in 1..MAGIC.len() {
            torn.push(MAGIC[..prefix].to_vec());
            torn.push([&MAGIC[..prefix], &[0u8; 64][..]].concat());
            torn.push([&MAGIC[..prefix], &later_page[prefix..]].concat());
        }
        torn.push(MAGIC.to_vec());
        torn.push([&MAGIC[..], &[0u8; 3][..]].concat());
        torn.push(later_page.clone());
        for bytes in &torn {
            std::fs::write(&path, bytes).unwrap();
            assert_eq!(file_version(&path).unwrap(), VERSION, "{bytes:?}");
            assert!(parse_delta(bytes).unwrap().keys.is_empty(), "{bytes:?}");
            assert_eq!(DeltaVocab::open(&path).unwrap().len(), 0, "{bytes:?}");
            assert_eq!(
                DeltaVocab::open_read_only(&path).unwrap().len(),
                0,
                "{bytes:?}"
            );
        }
        // a byte that is neither zero nor the next byte of the magic is a key length
        for bytes in [
            [&MAGIC[..3], &[7u8][..]].concat(),
            [&MAGIC[..3], &[0u8, 7][..]].concat(),
            [&[0u8; 7][..], &[1u8][..]].concat(),
        ] {
            std::fs::write(&path, &bytes).unwrap();
            assert_eq!(file_version(&path).unwrap(), UNFRAMED_VERSION, "{bytes:?}");
            let error = DeltaVocab::open(&path).err().expect("refused");
            assert!(matches!(error, Error::Unsupported(_)), "{error}");
        }
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

    #[test]
    fn controlled_linked_dictionary_still_refuses_truncated_required_terms() {
        use crate::{
            history::{HistoryOptions, ReadLimit},
            store::cold::ColdBudget,
        };
        use std::cell::Cell;
        let dir = tempfile::tempdir().unwrap();
        let upstream = dir.path().join("source");
        let own = dir.path().join("own");
        let first = framed(&[b"<urn:first>"]);
        let mut wire = first.clone();
        wire.extend(encode_chunks(1, &[Arc::from(&b"<urn:required-second>"[..])], false).unwrap());
        std::fs::write(&upstream, &wire).unwrap();
        let fired = Cell::new(false);
        let observer = |stage: &str, _| {
            if stage == "mutable prefix length" && !fired.replace(true) {
                OpenOptions::new()
                    .write(true)
                    .open(&upstream)
                    .unwrap()
                    .set_len(first.len() as u64)
                    .unwrap();
            }
        };
        let options = HistoryOptions::default();
        let mut budget = ColdBudget::new(ReadLimit {
            options: &options,
            memory: Some(1 << 20),
            held: 0,
        })
        .unwrap();
        budget.observe_keys(&observer);
        assert!(matches!(
            DeltaVocab::open_layered_controlled(&[(upstream.clone(), 2)], &own, &mut budget),
            Err(Error::Corrupt(_))
        ));
        assert!(fired.get());
        assert_eq!(std::fs::read(upstream).unwrap(), first);
    }

    #[test]
    fn controlled_linked_dictionary_tolerates_actual_upstream_compaction_trim() {
        use crate::{
            history::{HistoryOptions, ReadLimit},
            store::{Store, StoreOptions, cold::ColdBudget},
        };
        use std::cell::Cell;
        let root = tempfile::tempdir().unwrap();
        let main = Store::open(
            root.path(),
            StoreOptions {
                build: crate::builder::BuildOptions {
                    threads: 1,
                    ..Default::default()
                },
                ..Default::default()
            },
        )
        .unwrap();
        crate::sparql::update::update(
            &main,
            "INSERT DATA {<urn:upstream:s> <urn:upstream:p> \"payload\"}",
            &Default::default(),
        )
        .unwrap();
        main.flush_change_log().unwrap();
        main.create_branch("child", &Default::default()).unwrap();
        let child = main.branch("child").unwrap();
        child.flush_change_log().unwrap();
        let source = main.snapshot().generation.clone();
        let path = source.dir.as_ref().unwrap().join("delta.vocab");
        let (logical, _, allocated) = source.dvocab.file_state();
        assert!(
            allocated > logical,
            "fixture needs real unused preallocation"
        );
        let snapshot = child.snapshot();
        let own = snapshot
            .generation
            .dir
            .as_ref()
            .unwrap()
            .join("delta.vocab");
        let prefix = [(path.clone(), snapshot.dvocab_len)];
        let ordinary = DeltaVocab::open_layered(&prefix, &own, true).unwrap();
        let fired = Cell::new(false);
        let observer = |stage: &str, at: usize| {
            if stage == "mutable prefix length" && at as u64 == allocated && !fired.replace(true) {
                main.compact().unwrap();
                assert_eq!(std::fs::metadata(&path).unwrap().len(), logical);
            }
        };
        let options = HistoryOptions::default();
        let mut budget = ColdBudget::new(ReadLimit {
            options: &options,
            memory: Some(16 << 20),
            held: 0,
        })
        .unwrap();
        budget.observe_keys(&observer);
        let loaded = DeltaVocab::open_layered_controlled(&prefix, &own, &mut budget).unwrap();
        assert!(fired.get(), "the compaction interleaving did not execute");
        assert_eq!(loaded.value.len(), ordinary.len());
        for id in 0..ordinary.len() {
            let key = ordinary.get(id).unwrap();
            assert_eq!(loaded.value.get(id), Some(key.clone()));
            assert_eq!(loaded.value.find(&key), ordinary.find(&key));
        }
        assert_eq!(std::fs::metadata(path).unwrap().len(), logical);
    }

    #[test]
    fn controlled_layers_preserve_positional_duplicates_and_first_lookup_without_writes() {
        use crate::{
            history::{HistoryOptions, ReadLimit},
            store::cold::ColdBudget,
        };
        let dir = tempfile::tempdir().unwrap();
        let upstream = dir.path().join("upstream");
        let middle = dir.path().join("middle");
        let own = dir.path().join("own");
        let first = framed(&[b"<urn:a>", b"<urn:b>", b"<urn:a>"]);
        let second = framed(&[b"<urn:c>", b"<urn:a>"]);
        let last = framed(&[b"<urn:d>"]);
        std::fs::write(&upstream, &first).unwrap();
        std::fs::write(&middle, &second).unwrap();
        std::fs::write(&own, &last).unwrap();
        let prefix = [
            (upstream.clone(), 3),
            (middle.clone(), 5),
            (upstream.clone(), 2),
        ];
        let normal = DeltaVocab::open_layered(&prefix, &own, true).unwrap();
        let options = HistoryOptions::default();
        let limit = ReadLimit {
            options: &options,
            memory: Some(16 << 10),
            held: 0,
        };
        let mut budget = ColdBudget::new(limit).unwrap();
        let loaded = DeltaVocab::open_layered_controlled(&prefix, &own, &mut budget).unwrap();
        assert_eq!(normal.len(), 6);
        assert_eq!(loaded.value.len(), normal.len());
        for id in 0..normal.len() {
            assert_eq!(loaded.value.get(id), normal.get(id));
        }
        assert_eq!(loaded.value.find(b"<urn:a>"), Some(0));
        assert_eq!(loaded.value.find(b"<urn:c>"), Some(3));
        assert_eq!(
            loaded.charge,
            loaded.value.resident_bytes_limited(&limit).unwrap()
        );
        assert_eq!(budget.used(), loaded.charge);
        drop(loaded.value);
        budget.release(loaded.charge);
        assert_eq!(budget.used(), 0);
        assert_eq!(std::fs::read(&upstream).unwrap(), first);
        assert_eq!(std::fs::read(&middle).unwrap(), second);
        assert_eq!(std::fs::read(&own).unwrap(), last);
        let mut small = ColdBudget::new(ReadLimit {
            memory: Some(32),
            ..limit
        })
        .unwrap();
        assert!(matches!(
            DeltaVocab::open_layered_controlled(&prefix, &own, &mut small),
            Err(Error::BudgetExceeded(_))
        ));
        let mut budget = ColdBudget::new(limit).unwrap();
        assert!(matches!(
            DeltaVocab::open_layered_controlled(&[(upstream, 4)], &own, &mut budget),
            Err(Error::Corrupt(_))
        ));
    }
}
