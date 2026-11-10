//! Reading a generation's write-ahead log from any commit.
//!
//! A log is a sequence of 33-byte records, each transaction's data records followed by
//! its commit record. To read the changes of commit `s`, or to materialize the state at
//! `s`, Sparkles must know where in the file commit `s` ends. The [`WalIndex`] keeps that
//! position for every 1,024th commit, and for the first commit after each further MiB of
//! log, so a reader seeks near any commit and reads at most about a thousand commits or
//! a MiB past it. The index lives in memory: the current generation's is filled while the
//! log is replayed at open and kept up to date by each commit, and a sealed generation's
//! is built on first use by reading the log's records once, without probing the index.
//! It holds about 24 bytes per entry, so a log of a million commits needs about 24 KB.
//!
//! [`WalCursor`] reads transactions forward from an index point and numbers them the
//! way [`replay_wal`](super::replay_wal) does. [`apply_backward`] undoes a range of
//! transactions read from the end: every change a log records took effect, so applying
//! the inverse of each change in reverse order is exact.
//!
//! # Version
//!
//! A log created by this release starts with a header record: 33 bytes, like every
//! record, so the records after it keep their alignment. Its first eight bytes are
//! [`MAGIC`], whose first byte [`WAL_HEADER`] is no record type, then the log's version
//! ([`crate::formats::WAL_VERSION`]) as a little-endian `u32`. The bytes after the version
//! are zero in every version this build reads, and a newer version may use them, so a
//! reader takes the version first and refuses an unknown one before it looks at the rest.
//! A log without
//! the header was written before it existed and is version 0. Its records are the same.
//! Readers skip the header where a log starts, so a position of byte 0 still means "the
//! generation's base, before any commit". A log whose version this build does not read
//! is refused with [`Error::Unsupported`]. Existing logs are never rewritten to add the
//! header: it arrives with the next generation, at a compaction or bulk load. The
//! dataset requires reader [`HEADER_READER`] before a store writes one, so an older
//! release refuses the dataset instead of reading the header as a damaged record.
//!
//! A headerless log may hold commit records from a release that wrote no commit
//! metadata, and such a record is byte-identical to a modern one whose metadata a crash
//! tore away. The first write open of a headerless log by this release therefore records
//! the log's logical end as `walStrictFrom` in the generation's `commit.json`, once and
//! before it appends anything. It does so after replaying, truncating and syncing the log
//! and syncing the vocabulary and annotations its commits name. Records after that offset
//! follow the rules of a log with a header, and damage before it is corruption (see
//! `store::Framing`).

use super::*;

pub(super) mod probe;
use std::io::{BufReader, Seek, SeekFrom};

/// The first eight bytes of a log's header record.
pub const MAGIC: [u8; 8] = *b"\x89SPKWAL\n";

/// The dataset reader (`minimumReader` of `dataset.json`) that reads log headers.
pub const HEADER_READER: u32 = 5;

/// Constant-size transaction state for forward, reverse and cold-prefix readers.
/// Structural refusal is evaluated only after the transaction checksum succeeds,
/// preserving existing torn-tail treatment for an uncommitted damaged suffix.
#[derive(Clone, Copy, Default)]
pub(crate) struct ConfigurationRecords {
    digest: Option<[u8; 32]>,
    invalid: bool,
}

impl ConfigurationRecords {
    pub(crate) fn data(&mut self, record: &[u8; WAL_REC]) {
        match record[0] {
            WAL_CONFIGURATION => {
                if self.digest.is_some() {
                    self.invalid = true;
                }
                self.digest = Some(record[1..WAL_REC].try_into().expect("whole WAL record"));
            }
            WAL_INSERT | WAL_DELETE => {
                if self.digest.is_some() {
                    self.invalid = true;
                }
            }
            _ => self.invalid = true,
        }
    }

    pub(crate) fn finish(
        self,
        version: u32,
        flags: u8,
    ) -> Result<Option<crate::evidence::Sha256Digest>> {
        crate::formats::WAL.check("configuration transaction", version, None)?;
        let required = flags & commit::WAL_FLAG_CONFIGURATION != 0;
        let allowed = commit::WAL_FLAG_UNVALIDATED
            | commit::WAL_FLAG_ANNOTATED
            | commit::WAL_FLAG_CONFIGURATION;
        if self.invalid
            || required != self.digest.is_some()
            || (version < 2 && (required || self.digest.is_some()))
            || (version >= 2 && flags & !allowed != 0)
        {
            return Err(Error::Corrupt(
                "invalid sealed WAL configuration binding".into(),
            ));
        }
        Ok(self
            .digest
            .map(crate::evidence::Sha256Digest::from_digest_bytes))
    }
}

pub(crate) fn configuration_records(
    data: &[u8],
    version: u32,
    flags: u8,
) -> Result<Option<crate::evidence::Sha256Digest>> {
    if !data.len().is_multiple_of(WAL_REC) {
        return Err(Error::Corrupt(
            "partial configuration transaction record".into(),
        ));
    }
    let mut records = ConfigurationRecords::default();
    for record in data.as_chunks::<WAL_REC>().0 {
        records.data(record);
    }
    records.finish(version, flags)
}

pub(crate) fn append_configuration(
    data: &mut Vec<u8>,
    binding: crate::history::ConfigurationBinding,
) -> Result<u8> {
    match binding {
        crate::history::ConfigurationBinding::Known { configuration } => {
            let mut record = [0; WAL_REC];
            record[0] = WAL_CONFIGURATION;
            record[1..].copy_from_slice(configuration.generation.as_bytes());
            data.extend_from_slice(&record);
            Ok(commit::WAL_FLAG_CONFIGURATION)
        }
        crate::history::ConfigurationBinding::LegacyUnbound => Ok(0),
        crate::history::ConfigurationBinding::ImportedUnbound { .. } => Err(Error::Corrupt(
            "a new own commit cannot borrow an imported configuration binding".into(),
        )),
    }
}

/// Decode the header from the caller's actual descriptor, never another path open.
pub(crate) fn descriptor_version(file: &mut File, path: &Path) -> Result<u32> {
    let position = file.stream_position()?;
    let result = (|| {
        file.seek(SeekFrom::Start(0))?;
        let mut bytes = [0; WAL_REC];
        let mut length = 0;
        while length < WAL_REC {
            let count = file.read(&mut bytes[length..])?;
            if count == 0 {
                break;
            }
            length += count;
        }
        version_at_start(&bytes[..length], file.metadata()?.len(), path)
    })();
    let restored = file.seek(SeekFrom::Start(position));
    match result {
        Err(error) => Err(error),
        Ok(version) => {
            restored?;
            Ok(version)
        }
    }
}

/// The version of the logs that a store without configuration history writes.
/// Older releases read it, so an ordinary store stays readable by them.
pub(crate) const PLAIN_VERSION: u32 = 1;

/// The version of a new log in a store whose configuration history is
/// `activated`. Only activated stores write version 2, whose sealed
/// configuration records need the dataset reader that activation records.
pub(crate) fn version_for(activated: bool) -> u32 {
    if activated {
        crate::formats::WAL_VERSION
    } else {
        PLAIN_VERSION
    }
}

/// The header record of a new log of a store without configuration history.
#[cfg(test)]
pub(crate) fn header_record() -> [u8; WAL_REC] {
    header_record_of(PLAIN_VERSION)
}

/// The header record of a new log of an activated store.
pub(crate) fn configuration_header_record() -> [u8; WAL_REC] {
    header_record_of(crate::formats::WAL_VERSION)
}

/// The header record of a new log of `version`.
pub(crate) fn header_record_of(version: u32) -> [u8; WAL_REC] {
    let mut r = [0u8; WAL_REC];
    r[..8].copy_from_slice(&MAGIC);
    r[8..12].copy_from_slice(&version.to_le_bytes());
    r
}

/// The version in a header record (`rec[0]` is [`WAL_HEADER`]), checked against the
/// versions this build reads.
pub(crate) fn header_version(rec: &[u8], path: &Path) -> Result<u32> {
    let damaged = || Error::Corrupt(format!("{}: damaged header record", path.display()));
    if rec.len() < WAL_REC || rec[..8] != MAGIC {
        return Err(damaged());
    }
    let v = u32::from_le_bytes(rec[8..12].try_into().unwrap());
    // a newer version may define the bytes after its version, so check it first
    crate::formats::WAL.check(&path.display().to_string(), v, None)?;
    // every version this build reads leaves them zero
    if rec[12..WAL_REC].iter().any(|&b| b != 0) {
        return Err(damaged());
    }
    Ok(v)
}

/// The version of a log without a header record, the pre-release form that this build
/// refuses with the dump-and-reload message (spec X16 Phase 1).
pub(crate) const HEADERLESS_VERSION: u32 = 0;

/// The version of a log whose first bytes are `first` and whose length is `total`. A
/// log whose first record is not a header record is the pre-release headerless form,
/// version 0, and is refused. A header that a crash at the log's creation tore before
/// its bytes reached the disk, fewer bytes than a record or a record of zeros, holds no
/// committed record and reads as an empty log of [`PLAIN_VERSION`], which the open
/// creates again with its header. `wal::create` syncs the header before any record
/// follows it, so a record of zeros followed by more bytes is not that case: it is
/// damage, and refusing it keeps the records after it from being read as a torn tail.
fn version_at_start(first: &[u8], total: u64, path: &Path) -> Result<u32> {
    match first.first() {
        None => Ok(PLAIN_VERSION),
        Some(_) if first.len() < WAL_REC => Ok(PLAIN_VERSION),
        Some(&WAL_HEADER) => header_version(first, path),
        Some(_) if is_zero_record(&first[..WAL_REC]) => {
            if total <= WAL_REC as u64 {
                Ok(PLAIN_VERSION)
            } else {
                Err(Error::Corrupt(format!(
                    "{}: the header record is zeros, but {total} bytes follow it",
                    path.display()
                )))
            }
        }
        Some(_) => {
            Err(crate::formats::WAL.refuse(&path.display().to_string(), HEADERLESS_VERSION, None))
        }
    }
}

/// The version of the log whose first bytes are `buf` (see [`version_at_start`]).
#[cfg(test)]
pub(crate) fn version_of(buf: &[u8], path: &Path) -> Result<u32> {
    version_at_start(buf, buf.len() as u64, path)
}

/// The version of a log for replay, from the whole log `buf`. A header record cut short
/// by a crash is a torn tail with no committed record, as it is to the record-by-record
/// readers, so it reads as an empty log, and so does a log of zeros alone, whose length
/// grew for its header before the bytes reached the disk.
pub(crate) fn committed_version_of(buf: &[u8], path: &Path) -> Result<u32> {
    if buf.first().is_some_and(|&b| b != WAL_HEADER) && buf.iter().all(|&b| b == 0) {
        return Ok(PLAIN_VERSION);
    }
    version_at_start(buf, buf.len() as u64, path)
}

/// The version of the log at `path` ([`PLAIN_VERSION`] for a missing or empty log,
/// [`HEADERLESS_VERSION`] for the pre-release headerless log, which the registry then
/// refuses), for `sparkles migrate --check`.
pub fn file_version(path: &Path) -> Result<u32> {
    let mut buf = [0u8; WAL_REC];
    let (n, total) = match File::open(path) {
        Ok(mut f) => {
            let mut n = 0;
            while n < WAL_REC {
                match f.read(&mut buf[n..])? {
                    0 => break,
                    k => n += k,
                }
            }
            (n, f.metadata()?.len())
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => (0, 0),
        Err(e) => return Err(e.into()),
    };
    if n == WAL_REC && buf[0] != WAL_HEADER && !is_zero_record(&buf) {
        return Ok(HEADERLESS_VERSION);
    }
    version_at_start(&buf[..n], total, path)
}

/// The bytes a new log starts with: its header record.
pub(crate) const HEADER_BYTES: u64 = WAL_REC as u64;

/// Create a new, empty log at `path` (which must not exist): its header record, synced.
/// The caller syncs the directory and has already made the dataset require
/// [`HEADER_READER`] (every persistent store does when it opens).
pub(crate) fn create(path: &Path, version: u32) -> std::io::Result<()> {
    let mut f = OpenOptions::new().write(true).create_new(true).open(path)?;
    f.write_all(&header_record_of(version))?;
    f.sync_all()
}

/// Open a generation's log for writing, positioned at the end of the file, creating it
/// with its header (see [`create`]) if it does not exist. It is not opened in append
/// mode: a store that preallocates writes each commit at the log's logical end, which
/// lies before the zero bytes that follow it.
pub(crate) fn open_for_append(path: &Path, version: u32) -> std::io::Result<File> {
    match create(path, version) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(e) => return Err(e),
    }
    let mut f = OpenOptions::new().write(true).open(path)?;
    f.seek(SeekFrom::End(0))?;
    Ok(f)
}

/// The smallest preallocation step: a small log grows by this much at a time.
const PREALLOC_MIN: u64 = 64 << 10;

/// Write a transaction's records `data` at byte `at` of the log file, its logical end,
/// without syncing. `alloc` is the file's length. With preallocation (`prealloc` above 0,
/// the largest step), a write past `alloc` also writes zero bytes after the records, so
/// the file grows by a step, as long as the log at first and then by its own size up
/// to `prealloc`. The commit's sync makes them durable with the records. The commits
/// after it overwrite allocated bytes, and their `fdatasync` needs no journal commit
/// on file systems that update blocks in place.
pub(crate) fn write_commit(
    file: &File,
    at: u64,
    alloc: &mut u64,
    data: &[u8],
    prealloc: u64,
) -> std::io::Result<()> {
    let end = at + data.len() as u64;
    write_all_at(file, data, at)?;
    if end <= *alloc {
        return Ok(());
    }
    if prealloc == 0 {
        *alloc = end;
        return Ok(());
    }
    let step = (*alloc).max(PREALLOC_MIN).min(prealloc);
    let to = (end + step).next_multiple_of(4096).min(end + prealloc);
    let zeros = vec![0u8; ((to - end) as usize).min(1 << 20)];
    let mut pos = end;
    while pos < to {
        let n = (to - pos).min(zeros.len() as u64) as usize;
        write_all_at(file, &zeros[..n], pos)?;
        pos += n as u64;
    }
    *alloc = to;
    Ok(())
}

#[cfg(unix)]
pub(crate) fn write_all_at(file: &File, buf: &[u8], at: u64) -> std::io::Result<()> {
    std::os::unix::fs::FileExt::write_all_at(file, buf, at)
}

#[cfg(windows)]
pub(crate) fn write_all_at(file: &File, mut buf: &[u8], mut at: u64) -> std::io::Result<()> {
    while !buf.is_empty() {
        let n = std::os::windows::fs::FileExt::seek_write(file, buf, at)?;
        if n == 0 {
            return Err(std::io::ErrorKind::WriteZero.into());
        }
        buf = &buf[n..];
        at += n as u64;
    }
    Ok(())
}

#[cfg(not(any(unix, windows)))]
pub(crate) fn write_all_at(_: &File, _: &[u8], _: u64) -> std::io::Result<()> {
    Err(std::io::ErrorKind::Unsupported.into())
}

/// The unit of a direct write: its offset and length are multiples of this, which covers
/// devices with 512-byte and 4 KiB logical blocks.
const BLOCK: u64 = 4096;

/// The most bytes one direct write covers, from the start of the log's last block to the
/// end of the commit. A larger commit takes the buffered path.
const DIRECT_MAX: usize = 64 << 10;

/// The write-through path of a log (Linux only): a second handle on the same file, opened
/// with `O_DIRECT | O_DSYNC`, that writes a small commit into space the log has already
/// allocated. The delta vocabulary writes its chunks through one too.
///
/// A write with `O_DSYNC` returns once its data, and any metadata needed to read it back,
/// are durable, the same promise as a write followed by `fdatasync`. On ext4 and XFS a
/// direct write that overwrites allocated, written blocks needs no metadata, so the
/// kernel sends it with the FUA flag when the device supports it, and the drive persists
/// that one block instead of flushing its whole volatile cache. A device without FUA gets
/// a cache flush after the write, which is what `fdatasync` sends anyway.
///
/// Direct I/O wants aligned offsets and lengths, so each write covers whole 4 KiB blocks
/// from the start of the block that holds the log's logical end. The handle keeps a copy
/// of that block's committed bytes and writes them again in front of the new records.
/// Those bytes are already durable and unchanged, so a torn block write can only damage
/// the new records, which the commit's checksum and replay treat as a torn tail, just as
/// they do for a buffered write whose pages reach the disk in any order.
pub(crate) struct DirectLog {
    file: File,
    /// an aligned window of `DIRECT_MAX` bytes inside `raw`
    raw: Vec<u8>,
    /// the start of the aligned window in `raw`
    off: usize,
    /// the log offset of the window's first byte (a multiple of `BLOCK`)
    block: u64,
    /// the window holds the log's bytes from `block` up to here (`u64::MAX`: none)
    end: u64,
}

impl DirectLog {
    /// Open the write-through handle of the file at `path`, which `log` has open: `None`
    /// where direct I/O is not available (another OS, a file system without `O_DIRECT`) or
    /// the path is not the same file.
    pub fn open(path: &Path, log: &File) -> Option<DirectLog> {
        #[cfg(target_os = "linux")]
        {
            use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
            let file = OpenOptions::new()
                .read(true)
                .write(true)
                .custom_flags(libc::O_DIRECT | libc::O_DSYNC)
                .open(path)
                .ok()?;
            let (a, b) = (file.metadata().ok()?, log.metadata().ok()?);
            if (a.dev(), a.ino()) != (b.dev(), b.ino()) {
                return None;
            }
            let raw = vec![0u8; DIRECT_MAX + BLOCK as usize];
            let off = raw.as_ptr().align_offset(BLOCK as usize);
            Some(DirectLog {
                file,
                raw,
                off,
                block: 0,
                end: u64::MAX,
            })
        }
        #[cfg(not(target_os = "linux"))]
        {
            let _ = (path, log);
            None
        }
    }

    /// Whether a commit of `len` bytes at the logical end `at` can be written through
    /// this handle: every block it touches lies inside the log's allocated length `alloc`
    /// (so the write changes no file size), and the write is at most `DIRECT_MAX` bytes.
    pub fn fits(at: u64, len: usize, alloc: u64) -> bool {
        let start = at - at % BLOCK;
        let end = (at + len as u64).next_multiple_of(BLOCK);
        end <= alloc && end - start <= DIRECT_MAX as u64
    }

    /// Forget the copy of the last block: the file was written another way, and its
    /// bytes may differ even where a later write starts at the same offset (a delta
    /// vocabulary cut back by a rollback).
    pub fn invalidate(&mut self) {
        self.end = u64::MAX;
    }

    /// Write `data` at the log's logical end `at`, durably, through the direct handle.
    /// The caller checked [`fits`](Self::fits), and every byte of the log after `at` up
    /// to its allocated length is a preallocated zero. When the window does not hold the
    /// committed bytes of the last block (the first write, or after commits written
    /// another way), they are read from the file first.
    ///
    /// `Ok(false)` when the file system refused a direct read or write with `EINVAL`,
    /// which it returns for an alignment it does not accept. The caller then writes the
    /// commit the buffered way and stops using this handle. Even if part of the write
    /// had reached the file, the buffered path writes the same bytes at the same offset
    /// and syncs them. Any other error leaves the log in an unknown state, as a failed
    /// buffered write does.
    pub fn write(&mut self, at: u64, data: &[u8]) -> std::io::Result<bool> {
        let start = at - at % BLOCK;
        let end = at + data.len() as u64;
        let len = (end.next_multiple_of(BLOCK) - start) as usize;
        let head = (at - start) as usize;
        let (off, block, cached) = (self.off, self.block, self.end);
        let win = &mut self.raw[off..off + DIRECT_MAX];
        self.end = u64::MAX;
        if head > 0 && (block != start || cached != at) {
            // A direct read of the whole block. The kernel writes back any of its
            // pages still dirty in the page cache first, so this sees what buffered
            // writes put there.
            if let Err(e) = read_exact_at(&self.file, &mut win[..BLOCK as usize], start) {
                return refused(e);
            }
        }
        win[head..head + data.len()].copy_from_slice(data);
        win[head + data.len()..len].fill(0);
        self.block = start;
        if let Err(e) = write_all_at(&self.file, &win[..len], start) {
            return refused(e);
        }
        // keep the block the next commit starts in at the front of the window
        let last = end - end % BLOCK;
        if last > start {
            let from = (last - start) as usize;
            let tail = (end - last) as usize;
            win.copy_within(from..from + tail, 0);
            self.block = last;
        }
        self.end = end;
        Ok(true)
    }
}

/// The writer's write-through handle of the current log, opened on the first commit
/// that can use it. A new log (a compaction or bulk commit switching generations)
/// starts again from `Untried`.
#[derive(Default)]
pub(crate) enum Direct {
    #[default]
    Untried,
    Open(Box<DirectLog>),
    /// not available here, refused by the file system, or turned off
    Off,
}

impl Direct {
    /// The handle for a write to `log`, the file at `path` (`None`: no direct writes),
    /// opening it if this is the first try.
    pub fn handle(&mut self, path: Option<&Path>, log: &File) -> Option<&mut DirectLog> {
        if matches!(self, Direct::Untried) {
            *self = match path.and_then(|p| DirectLog::open(p, log)) {
                Some(d) => Direct::Open(Box::new(d)),
                None => Direct::Off,
            };
        }
        match self {
            Direct::Open(d) => Some(d),
            _ => None,
        }
    }
}

/// The written end of the current generation's log, which every write to the log's file
/// goes through. The writer's ordinary path and the group commit leader share it. A grouped
/// transaction does not write its records itself. It hands them to the queue, and the
/// leader of its fence writes the whole sealed prefix at the tail, through the
/// write-through handle when the prefix fits in preallocated space, and with a buffered
/// write and `fdatasync` otherwise (X09 item 446). So one thread writes at a time, each
/// fence's bytes start where the last durable write ended, and nothing lies after `end`
/// but preallocated zeros, as [`DirectLog::write`] requires. An ordinary commit runs only
/// when no grouped commit is in flight, so it never waits for the lock.
pub(crate) struct Tail {
    io: Mutex<TailIo>,
    /// the file's length as of the last finished write, for a transaction that only
    /// decides how much to preallocate and must not wait for a fence's I/O
    alloc: AtomicU64,
}

pub(crate) struct TailIo {
    /// the file's length: past `end` it holds preallocated zero bytes
    pub alloc: u64,
    /// the bytes of the log written to the file, where the next write starts
    pub end: u64,
    /// the write-through handle (`Untried` for a new log)
    pub direct: Direct,
}

impl Tail {
    /// The tail of a log whose file is `alloc` bytes long and whose records end at `end`.
    pub fn new(alloc: u64, end: u64) -> Arc<Tail> {
        Arc::new(Tail {
            io: Mutex::new(TailIo {
                alloc,
                end,
                direct: Direct::Untried,
            }),
            alloc: AtomicU64::new(alloc),
        })
    }

    /// Hold the tail to write at it. [`Tail::release`] records the file's new length.
    pub fn lock(&self) -> MutexGuard<'_, TailIo> {
        self.io.lock()
    }

    /// Record the file's length after a write under `io`.
    pub fn release(&self, io: &TailIo) {
        self.alloc.store(io.alloc, Ordering::Release);
    }

    /// The file's length as of the last finished write.
    pub fn alloc(&self) -> u64 {
        self.alloc.load(Ordering::Acquire)
    }
}

/// The outcome of a failed direct read or write: `EINVAL`, which a file system returns
/// for an alignment it does not accept before it writes anything, makes the caller fall
/// back to the buffered path. Any other error is the commit's error.
fn refused(e: std::io::Error) -> std::io::Result<bool> {
    if e.kind() == std::io::ErrorKind::InvalidInput {
        Ok(false)
    } else {
        Err(e)
    }
}

#[cfg(unix)]
fn read_exact_at(file: &File, buf: &mut [u8], at: u64) -> std::io::Result<()> {
    std::os::unix::fs::FileExt::read_exact_at(file, buf, at)
}

#[cfg(not(unix))]
fn read_exact_at(_: &File, _: &mut [u8], _: u64) -> std::io::Result<()> {
    Err(std::io::ErrorKind::Unsupported.into())
}

/// Whether a record is all zero bytes: preallocated space past the log's last commit,
/// or a part of the last transaction that did not reach the disk before a crash.
#[inline]
pub(crate) fn is_zero_record(rec: &[u8]) -> bool {
    rec.iter().all(|&b| b == 0)
}

/// A position in a log: right after the last record of commit `seq`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct WalPoint {
    pub seq: u64,
    /// the byte offset after the commit's last record
    pub offset: u64,
    /// legacy transactions (without commit metadata) read from here fold into the base
    pub folding: bool,
}

/// An index entry every this many commits.
const INDEX_COMMITS: u64 = 1024;
/// And for the first commit after this many bytes of log since the last entry.
const INDEX_BYTES: u64 = 1 << 20;

/// A sparse index of one generation's log (see the module documentation).
#[derive(Clone, Debug)]
pub(crate) struct WalIndex {
    /// ascending; the first is the generation's base
    points: Vec<WalPoint>,
    /// the newest commit indexed
    last: WalPoint,
    /// commits since the newest entry of `points`
    since: u64,
}

impl WalIndex {
    /// The index of an empty log whose generation's base index holds commit `base`.
    pub fn new(base: u64, fold_legacy: bool) -> WalIndex {
        let p = WalPoint {
            seq: base,
            offset: 0,
            folding: fold_legacy,
        };
        WalIndex {
            points: vec![p],
            last: p,
            since: 0,
        }
    }

    /// Record the end of a complete transaction (in log order).
    pub fn note(&mut self, p: WalPoint) {
        if p.seq < self.last.seq || p.offset < self.last.offset {
            return;
        }
        if p.seq == self.last.seq {
            // a legacy transaction folded into the base: the base's state ends later
            self.last = p;
            if let Some(l) = self.points.last_mut()
                && l.seq == p.seq
            {
                *l = p;
            }
            return;
        }
        self.last = p;
        self.since += 1;
        let anchor = self.points.last().map_or(0, |a| a.offset);
        if self.since >= INDEX_COMMITS || p.offset - anchor >= INDEX_BYTES {
            self.points.push(p);
            self.since = 0;
        }
    }

    /// The newest indexed position at or before the end of commit `seq` (the base if
    /// nothing nearer is known).
    pub fn floor(&self, seq: u64) -> WalPoint {
        if self.last.seq <= seq {
            return self.last;
        }
        let i = self.points.partition_point(|p| p.seq <= seq);
        self.points[i.saturating_sub(1)]
    }

    /// The newest commit indexed and where it ends.
    #[cfg(test)]
    fn last(&self) -> WalPoint {
        self.last
    }

    /// Entries held.
    #[cfg(test)]
    fn len(&self) -> usize {
        self.points.len()
    }

    /// Build the index of a log by reading its records once (a sealed generation's, or
    /// one not indexed yet). It ends at a torn final transaction, and at damage: the
    /// index only says where to start reading, and the reader that reaches the damage
    /// reports it.
    pub fn scan(path: &Path, base: u64, fold_legacy: bool) -> Result<WalIndex> {
        let mut ix = WalIndex::new(base, fold_legacy);
        let file = match File::open(path) {
            Ok(f) => f,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(ix),
            Err(e) => return Err(e.into()),
        };
        let mut c = WalCursor::new(file, path, ix.last)?;
        loop {
            match c.next() {
                Ok(Some(_)) => ix.note(c.position()),
                Ok(None) => break,
                Err(e) => {
                    tracing::debug!(target: "sparkles::store::wal", "indexing {} stopped: {e}", path.display());
                    break;
                }
            }
        }
        Ok(ix)
    }
}

/// Reads the transactions of a log forward from a [`WalPoint`].
pub(crate) struct WalCursor {
    r: BufReader<File>,
    path: PathBuf,
    /// after the last complete transaction read
    at: WalPoint,
    /// the data records of the transaction last read
    txn: Vec<u8>,
    version: u32,
    configuration: Option<crate::evidence::Sha256Digest>,
}

impl WalCursor {
    /// A bounded reader keeps its I/O buffer small and refuses transaction growth
    /// before allocation. `held` includes the caller's retained delta.
    pub(crate) fn open_limited(
        path: &Path,
        from: WalPoint,
        limit: &crate::history::ReadLimit<'_>,
        held: u64,
    ) -> Result<WalCursor> {
        limit.check(held)?;
        let capacity = limit
            .remaining()
            .saturating_sub(held)
            .saturating_div(8)
            .min(64 << 10)
            .max(WAL_REC as u64);
        limit.check(held.saturating_add(capacity.max(WAL_REC as u64)))?;
        let mut file = File::open(path)?;
        let version = descriptor_version(&mut file, path)?;
        file.seek(SeekFrom::Start(from.offset))?;
        Ok(WalCursor {
            r: BufReader::with_capacity(capacity as usize, file),
            path: path.to_path_buf(),
            at: from,
            txn: Vec::new(),
            version,
            configuration: None,
        })
    }

    pub(crate) fn buffer_bytes(&self) -> u64 {
        (self.r.capacity() + self.txn.capacity()) as u64
    }

    pub fn new(mut file: File, path: &Path, from: WalPoint) -> Result<WalCursor> {
        let version = descriptor_version(&mut file, path)?;
        file.seek(SeekFrom::Start(from.offset))?;
        Ok(WalCursor {
            r: BufReader::with_capacity(1 << 20, file),
            path: path.to_path_buf(),
            at: from,
            txn: Vec::new(),
            version,
            configuration: None,
        })
    }

    pub fn open(path: &Path, from: WalPoint) -> Result<WalCursor> {
        Self::new(File::open(path)?, path, from)
    }

    /// The log's path.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Where the cursor stands: after the last transaction read.
    pub fn position(&self) -> WalPoint {
        self.at
    }

    pub(crate) fn configuration(&self) -> Option<crate::evidence::Sha256Digest> {
        self.configuration
    }

    /// The next complete transaction: its commit's number and its data records (whole
    /// 33-byte records). `None` at the end of the log, a torn final transaction
    /// included. A checksum mismatch, an unknown record type or a number out of
    /// sequence is [`Error::Corrupt`].
    pub fn next(&mut self) -> Result<Option<(u64, &[u8])>> {
        Ok(self.next_inner(None)?.map(|(seq, txn, _)| (seq, txn)))
    }

    pub(crate) fn next_limited(
        &mut self,
        limit: &crate::history::ReadLimit<'_>,
        held: u64,
    ) -> Result<Option<(u64, &[u8], u64)>> {
        self.next_inner(Some((limit, held)))
    }

    fn next_inner(
        &mut self,
        limited: Option<(&crate::history::ReadLimit<'_>, u64)>,
    ) -> Result<Option<(u64, &[u8], u64)>> {
        self.txn.clear();
        self.configuration = None;
        let mut rec = [0u8; WAL_REC];
        let mut offset = self.at.offset;
        loop {
            if let Some((limit, held)) = limited {
                limit.check(held.saturating_add(self.buffer_bytes()))?;
            }
            match self.r.read_exact(&mut rec) {
                Ok(()) => {}
                Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => return Ok(None),
                Err(e) => return Err(e.into()),
            }
            offset += WAL_REC as u64;
            match rec[0] {
                WAL_INSERT | WAL_DELETE | WAL_CONFIGURATION => {
                    if let Some((limit, held)) = limited
                        && self.txn.len() + WAL_REC > self.txn.capacity()
                    {
                        let next = (self.txn.capacity().saturating_mul(2)).max(WAL_REC);
                        // Old and replacement allocations can coexist during relocation.
                        limit.check(
                            held.saturating_add(self.buffer_bytes())
                                .saturating_add(next as u64),
                        )?;
                        self.txn
                            .try_reserve_exact(next - self.txn.len())
                            .map_err(|_| {
                                Error::BudgetExceeded(crate::Budget {
                                    kind: crate::BudgetKind::Memory,
                                    limit: limit.memory.unwrap_or(u64::MAX),
                                    requested: limit.memory.unwrap_or(u64::MAX).saturating_add(1),
                                })
                            })?;
                    }
                    self.txn.extend_from_slice(&rec);
                }
                WAL_COMMIT => break,
                // the header record where the log starts
                WAL_HEADER if offset == WAL_REC as u64 => {
                    if header_version(&rec, &self.path)? != self.version {
                        return Err(Error::Corrupt(
                            "WAL header changed during cursor read".into(),
                        ));
                    }
                }
                // the log's end: its preallocated space, or a torn last transaction
                _ if is_zero_record(&rec) => return Ok(None),
                op => {
                    return Err(Error::Corrupt(format!(
                        "{}: unknown record type {op} at byte {}",
                        self.path.display(),
                        offset - WAL_REC as u64
                    )));
                }
            }
        }
        let prev = self.at.seq;
        let metadata = match limited {
            Some((limit, held)) => commit::open_wal_commit_with_crc(
                &rec,
                replay_crc_limited(
                    &rec,
                    &self.txn,
                    limit,
                    held.saturating_add(self.buffer_bytes()),
                )?,
            ),
            None => commit::open_wal_commit(&rec, &self.txn),
        };
        let (seq, folding) = match metadata {
            Some(Ok((seq, _, _, flags))) => {
                self.configuration = configuration_records(&self.txn, self.version, flags)?;
                if seq != prev + 1 {
                    return Err(Error::Corrupt(format!(
                        "{}: commit {seq} follows commit {prev}",
                        self.path.display()
                    )));
                }
                (seq, false)
            }
            Some(Err(())) => {
                return Err(Error::Corrupt(format!(
                    "{}: checksum mismatch in the transaction ending at byte {offset}",
                    self.path.display()
                )));
            }
            None if self.version >= 1 => {
                return Err(Error::Corrupt(
                    "framed WAL transaction has no commit metadata".into(),
                ));
            }
            // legacy records before the first with metadata fold into the base
            None if self.at.folding => {
                configuration_records(&self.txn, self.version, 0)?;
                (prev, true)
            }
            None => {
                configuration_records(&self.txn, self.version, 0)?;
                (prev + 1, false)
            }
        };
        self.at = WalPoint {
            seq,
            offset,
            folding,
        };
        Ok(Some((seq, &self.txn, self.buffer_bytes())))
    }

    /// Move to the end of commit `seq` (at or after the cursor's position): the
    /// position after the last transaction numbered at most `seq`. A log that ends
    /// before `seq` is [`Error::Corrupt`].
    pub fn seek_to(mut self, seq: u64) -> Result<WalPoint> {
        loop {
            let before = self.at;
            if before.seq > seq {
                return Err(Error::Corrupt(format!(
                    "{}: read past commit {seq}",
                    self.path.display()
                )));
            }
            match self.next()? {
                Some((s, _)) if s > seq => return Ok(before),
                Some(_) => {}
                None if before.seq == seq => return Ok(before),
                None => {
                    return Err(Error::Corrupt(format!(
                        "{}: the log ends before commit {seq}",
                        self.path.display()
                    )));
                }
            }
        }
    }
}

/// The quad `[s, p, o, g]` of a data record.
#[inline]
pub(crate) fn record_quad(rec: &[u8]) -> [Id; 4] {
    std::array::from_fn(|j| {
        Id(u64::from_le_bytes(
            rec[1 + j * 8..9 + j * 8].try_into().unwrap(),
        ))
    })
}

/// Apply the transactions of a log range to `delta`, forward, from `from` through the
/// end of commit `to.seq` (`to` is where that commit ends). `check` runs every 64 Ki
/// records with the delta so far.
pub(crate) fn apply_forward(
    path: &Path,
    generation: &Generation,
    cache: &BlockCache,
    delta: &mut Delta,
    from: WalPoint,
    to: WalPoint,
    check: &mut dyn FnMut(&Delta) -> Result<()>,
) -> Result<u64> {
    if from == to {
        return Ok(0);
    }
    let mut c = WalCursor::open(path, from)?;
    let spo = generation.perm(Perm::Spo);
    let mut records = 0u64;
    while c.position().offset < to.offset {
        let Some((_, txn)) = c.next()? else {
            return Err(Error::Corrupt(format!(
                "{}: the log ends before commit {}",
                path.display(),
                to.seq
            )));
        };
        for rec in txn.as_chunks::<WAL_REC>().0 {
            if rec[0] == WAL_CONFIGURATION {
                continue;
            }
            let q = record_quad(rec);
            let in_base = spo.contains(cache, &Perm::Spo.to_key(&q))?;
            apply(delta, &q, rec[0] == WAL_INSERT, in_base);
            records += 1;
            if records & 0xFFFF == 0 {
                check(delta)?;
            }
        }
    }
    if c.position() != to {
        return Err(Error::Corrupt(format!(
            "{}: commit {} does not end at byte {}",
            path.display(),
            to.seq,
            to.offset
        )));
    }
    Ok(records)
}

/// Bounded forward replay checks transaction relocation and each delta mutation.
#[allow(clippy::too_many_arguments)]
pub(crate) fn apply_forward_limited(
    path: &Path,
    generation: &Generation,
    cache: &BlockCache,
    delta: &mut Delta,
    from: WalPoint,
    to: WalPoint,
    limit: &crate::history::ReadLimit<'_>,
) -> Result<u64> {
    limit.check(super::history_limited::preparation_delta_bytes(delta))?;
    if from == to {
        return Ok(0);
    }
    let mut cursor = WalCursor::open_limited(
        path,
        from,
        limit,
        super::history_limited::preparation_delta_bytes(delta),
    )?;
    let spo = generation.perm(Perm::Spo);
    let mut records = 0;
    while cursor.position().offset < to.offset {
        let Some((_, txn, buffers)) = cursor.next_limited(
            limit,
            super::history_limited::preparation_delta_bytes(delta),
        )?
        else {
            return Err(Error::Corrupt(format!(
                "{}: the log ends before commit {}",
                path.display(),
                to.seq
            )));
        };
        for record in txn.as_chunks::<WAL_REC>().0 {
            if record[0] == WAL_CONFIGURATION {
                continue;
            }
            // Each changed quad can add one key to every permutation. Check before
            // mutation, while the validated transaction remains in memory.
            limit.check(
                super::history_limited::preparation_mutation_bytes(delta).saturating_add(buffers),
            )?;
            let quad = record_quad(record);
            let in_base = spo.contains_limited(
                cache,
                &Perm::Spo.to_key(&quad),
                limit,
                super::history_limited::preparation_mutation_bytes(delta).saturating_add(buffers),
            )?;
            apply(delta, &quad, record[0] == WAL_INSERT, in_base);
            records += 1;
        }
    }
    limit.check(super::history_limited::preparation_delta_bytes(delta))?;
    if cursor.position() != to {
        return Err(Error::Corrupt(format!(
            "{}: commit {} does not end at byte {}",
            path.display(),
            to.seq,
            to.offset
        )));
    }
    Ok(records)
}

/// Why a backward replay could not run.
pub(crate) enum Backward {
    /// a legacy transaction in the range: its number is not in the log, so replay
    /// forward instead
    Legacy,
    Failed(Error),
}

impl From<Error> for Backward {
    fn from(e: Error) -> Backward {
        Backward::Failed(e)
    }
}

impl From<std::io::Error> for Backward {
    fn from(e: std::io::Error) -> Backward {
        Backward::Failed(e.into())
    }
}

/// Undo the transactions of a log range on `delta`, the state at the end of `hi`, so it
/// becomes the state at the end of `lo`. The range is read from its end in chunks, and
/// each transaction's checksum is verified before it is undone.
pub(crate) fn apply_backward(
    path: &Path,
    generation: &Generation,
    cache: &BlockCache,
    delta: &mut Delta,
    lo: WalPoint,
    hi: WalPoint,
    check: &mut dyn FnMut(&Delta) -> Result<()>,
) -> std::result::Result<u64, Backward> {
    const CHUNK: u64 = (4 << 20) / WAL_REC as u64 * WAL_REC as u64;
    let corrupt = |m: String| Backward::Failed(Error::Corrupt(format!("{}: {m}", path.display())));
    if hi.offset < lo.offset || !(hi.offset - lo.offset).is_multiple_of(WAL_REC as u64) {
        return Err(corrupt(format!(
            "no whole records between bytes {} and {}",
            lo.offset, hi.offset
        )));
    }
    let mut file = File::open(path)?;
    let version = descriptor_version(&mut file, path)?;
    let spo = generation.perm(Perm::Spo);
    // the transaction being collected: its commit record, then its data records from
    // the last to the first
    let mut commit: Option<[u8; WAL_REC]> = None;
    let mut data: Vec<[u8; WAL_REC]> = Vec::new();
    let mut fwd: Vec<u8> = Vec::new();
    let (mut seq, mut records) = (hi.seq, 0u64);
    let mut undo = |commit: &[u8; WAL_REC],
                    data: &[[u8; WAL_REC]],
                    seq: &mut u64,
                    delta: &mut Delta|
     -> std::result::Result<(), Backward> {
        fwd.clear();
        for r in data.iter().rev() {
            fwd.extend_from_slice(r);
        }
        match commit::open_wal_commit(commit, &fwd) {
            Some(Ok((s, _, _, flags))) if s == *seq => {
                configuration_records(&fwd, version, flags).map_err(Backward::Failed)?;
            }
            Some(Ok((s, _, _, _))) => {
                return Err(corrupt(format!("found commit {s} where {seq} ends")));
            }
            Some(Err(())) => {
                return Err(corrupt(format!(
                    "checksum mismatch in the transaction of commit {seq}"
                )));
            }
            None => return Err(Backward::Legacy),
        }
        for r in data {
            if r[0] == WAL_CONFIGURATION {
                continue;
            }
            let q = record_quad(r);
            let in_base = spo.contains(cache, &Perm::Spo.to_key(&q))?;
            apply(delta, &q, r[0] != WAL_INSERT, in_base);
            records += 1;
            if records & 0xFFFF == 0 {
                check(delta)?;
            }
        }
        *seq -= 1;
        Ok(())
    };
    let mut end = hi.offset;
    let mut buf = Vec::new();
    while end > lo.offset {
        let start = end.saturating_sub(CHUNK).max(lo.offset);
        buf.resize((end - start) as usize, 0);
        file.seek(SeekFrom::Start(start))?;
        file.read_exact(&mut buf)?;
        for (i, rec) in buf.as_chunks::<WAL_REC>().0.iter().enumerate().rev() {
            match rec[0] {
                // the header record where the log starts
                WAL_HEADER if start == 0 && i == 0 => {}
                WAL_COMMIT => {
                    if let Some(c) = commit.take() {
                        undo(&c, &data, &mut seq, delta)?;
                        data.clear();
                    }
                    commit = Some(*rec);
                }
                WAL_INSERT | WAL_DELETE | WAL_CONFIGURATION if commit.is_some() => data.push(*rec),
                op => {
                    return Err(corrupt(format!(
                        "unexpected record type {op} before byte {end}"
                    )));
                }
            }
        }
        end = start;
    }
    if let Some(c) = commit.take() {
        undo(&c, &data, &mut seq, delta)?;
    }
    if seq != lo.seq {
        return Err(corrupt(format!(
            "undoing commits from {} reached commit {seq}, not {}",
            hi.seq, lo.seq
        )));
    }
    Ok(records)
}

/// Bounded reverse replay retains only one transaction and preflights both orders.\n#[allow(clippy::too_many_arguments)]
pub(crate) fn apply_backward_limited(
    path: &Path,
    generation: &Generation,
    cache: &BlockCache,
    delta: &mut Delta,
    lo: WalPoint,
    hi: WalPoint,
    limit: &crate::history::ReadLimit<'_>,
) -> std::result::Result<u64, Backward> {
    const CHUNK: u64 = (64 << 10) / WAL_REC as u64 * WAL_REC as u64;
    limit.check(super::history_limited::preparation_delta_bytes(delta))?;
    let corrupt = |m: String| Backward::Failed(Error::Corrupt(format!("{}: {m}", path.display())));
    if hi.offset < lo.offset || !(hi.offset - lo.offset).is_multiple_of(WAL_REC as u64) {
        return Err(corrupt(format!(
            "no whole records between bytes {} and {}",
            lo.offset, hi.offset
        )));
    }
    let mut file = File::open(path)?;
    let version = descriptor_version(&mut file, path)?;
    let spo = generation.perm(Perm::Spo);
    // the transaction being collected: its commit record, then its data records from
    // the last to the first
    let mut commit: Option<[u8; WAL_REC]> = None;
    let mut data: Vec<[u8; WAL_REC]> = Vec::new();
    let mut fwd: Vec<u8> = Vec::new();
    let (mut seq, mut records) = (hi.seq, 0u64);
    let mut undo = |commit: &[u8; WAL_REC],
                    data: &[[u8; WAL_REC]],
                    seq: &mut u64,
                    delta: &mut Delta,
                    held_buffers: u64|
     -> std::result::Result<(), Backward> {
        fwd.clear();
        let need = data.len().saturating_mul(WAL_REC);
        if need > fwd.capacity() {
            limit.check(
                super::history_limited::preparation_delta_bytes(delta)
                    .saturating_add(held_buffers)
                    .saturating_add(fwd.capacity() as u64)
                    .saturating_add(need as u64),
            )?;
            fwd.try_reserve_exact(need)
                .map_err(|_| allocation_refused(limit))?;
        }
        for r in data.iter().rev() {
            limit.check(
                super::history_limited::preparation_delta_bytes(delta)
                    .saturating_add(held_buffers)
                    .saturating_add(fwd.capacity() as u64),
            )?;
            fwd.extend_from_slice(r);
        }
        match commit::open_wal_commit_with_crc(
            commit,
            replay_crc_limited(
                commit,
                &fwd,
                limit,
                super::history_limited::preparation_delta_bytes(delta)
                    .saturating_add(held_buffers)
                    .saturating_add(fwd.capacity() as u64),
            )?,
        ) {
            Some(Ok((s, _, _, flags))) if s == *seq => {
                configuration_records(&fwd, version, flags).map_err(Backward::Failed)?;
            }
            Some(Ok((s, _, _, _))) => {
                return Err(corrupt(format!("found commit {s} where {seq} ends")));
            }
            Some(Err(())) => {
                return Err(corrupt(format!(
                    "checksum mismatch in the transaction of commit {seq}"
                )));
            }
            None => return Err(Backward::Legacy),
        }
        for r in data {
            if r[0] == WAL_CONFIGURATION {
                continue;
            }
            limit.check(
                super::history_limited::preparation_mutation_bytes(delta)
                    .saturating_add(held_buffers)
                    .saturating_add(fwd.capacity() as u64),
            )?;
            let q = record_quad(r);
            let in_base = spo.contains_limited(
                cache,
                &Perm::Spo.to_key(&q),
                limit,
                super::history_limited::preparation_mutation_bytes(delta)
                    .saturating_add(held_buffers)
                    .saturating_add(fwd.capacity() as u64),
            )?;
            apply(delta, &q, r[0] != WAL_INSERT, in_base);
            records += 1;
        }
        *seq -= 1;
        Ok(())
    };
    let mut end = hi.offset;
    let mut buf: Vec<u8> = Vec::new();
    while end > lo.offset {
        limit.check(
            super::history_limited::preparation_delta_bytes(delta)
                .saturating_add(buf.capacity() as u64)
                .saturating_add((data.capacity() * WAL_REC * 2) as u64),
        )?;
        let remaining = limit
            .remaining()
            .saturating_sub(super::history_limited::preparation_delta_bytes(delta))
            .saturating_sub((data.capacity() * WAL_REC * 2) as u64);
        let chunk = CHUNK
            .min(remaining / 8 / WAL_REC as u64 * WAL_REC as u64)
            .max(WAL_REC as u64);
        let start = end.saturating_sub(chunk).max(lo.offset);
        let need = (end - start) as usize;
        if need > buf.capacity() {
            limit.check(
                super::history_limited::preparation_delta_bytes(delta)
                    .saturating_add((data.capacity() * WAL_REC * 2) as u64)
                    .saturating_add(buf.capacity() as u64)
                    .saturating_add(need as u64),
            )?;
            buf.try_reserve_exact(need.saturating_sub(buf.len()))
                .map_err(|_| allocation_refused(limit))?;
        }
        buf.resize(need, 0);
        file.seek(SeekFrom::Start(start))?;
        file.read_exact(&mut buf)?;
        for (i, rec) in buf.as_chunks::<WAL_REC>().0.iter().enumerate().rev() {
            limit.check(
                super::history_limited::preparation_delta_bytes(delta)
                    .saturating_add(buf.capacity() as u64)
                    .saturating_add((data.capacity() * WAL_REC * 2) as u64),
            )?;
            match rec[0] {
                // the header record where the log starts
                WAL_HEADER if start == 0 && i == 0 => {}
                WAL_COMMIT => {
                    if let Some(c) = commit.take() {
                        undo(
                            &c,
                            &data,
                            &mut seq,
                            delta,
                            (buf.capacity() + data.capacity() * WAL_REC) as u64,
                        )?;
                        data.clear();
                    }
                    commit = Some(*rec);
                }
                WAL_INSERT | WAL_DELETE | WAL_CONFIGURATION if commit.is_some() => {
                    if data.len() == data.capacity() {
                        let next = data.capacity().saturating_mul(2).max(1);
                        limit.check(
                            super::history_limited::preparation_delta_bytes(delta)
                                .saturating_add(buf.capacity() as u64)
                                .saturating_add((data.capacity() * WAL_REC * 2) as u64)
                                .saturating_add((next * WAL_REC * 2) as u64),
                        )?;
                        data.try_reserve_exact(next - data.len())
                            .map_err(|_| allocation_refused(limit))?;
                    }
                    data.push(*rec);
                }
                op => {
                    return Err(corrupt(format!(
                        "unexpected record type {op} before byte {end}"
                    )));
                }
            }
        }
        end = start;
    }
    if let Some(c) = commit.take() {
        undo(
            &c,
            &data,
            &mut seq,
            delta,
            (buf.capacity() + data.capacity() * WAL_REC) as u64,
        )?;
    }
    if seq != lo.seq {
        return Err(corrupt(format!(
            "undoing commits from {} reached commit {seq}, not {}",
            hi.seq, lo.seq
        )));
    }
    Ok(records)
}

/// The ordinary codec consumes this scalar without allocating another buffer.
/// Legacy records have no modern checksum and retain their replay fallback.
fn replay_crc_limited(
    record: &[u8],
    data: &[u8],
    limit: &crate::history::ReadLimit<'_>,
    held: u64,
) -> Result<flate2::Crc> {
    limit.check(held)?;
    let mut crc = flate2::Crc::new();
    if record[26] == commit::WAL_COMMIT_V2 {
        #[cfg(test)]
        let mut processed = 0;
        for chunk in data.chunks(commit::WAL_CRC_CHUNK) {
            limit.check(held)?;
            crc.update(chunk);
            #[cfg(test)]
            {
                processed += chunk.len();
                commit::observe_wal_crc(
                    u64::from_le_bytes(record[9..17].try_into().unwrap()),
                    processed,
                    data.len(),
                );
            }
            limit.check(held)?;
        }
    }
    limit.check(held)?;
    Ok(crc)
}

fn allocation_refused(limit: &crate::history::ReadLimit<'_>) -> Error {
    let memory = limit.memory.unwrap_or(u64::MAX);
    Error::BudgetExceeded(crate::Budget {
        kind: crate::BudgetKind::Memory,
        limit: memory,
        requested: memory.saturating_add(1),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn configuration_wal_checks_sealed_placement_flags_and_version() {
        let temporary = tempfile::tempdir().unwrap();
        let path = temporary.path().join("wal.log");
        let mut configuration = [7; WAL_REC];
        configuration[0] = WAL_CONFIGURATION;
        let mut quad = [0; WAL_REC];
        quad[0] = WAL_INSERT;
        let identity = crate::evidence::Sha256Digest::from_digest_bytes([7; 32]);
        let valid = [quad, configuration].concat();
        let fixture = |version: u32, data: &[u8], flags: u8| {
            let mut header = header_record();
            header[8..12].copy_from_slice(&version.to_le_bytes());
            let mut seal = [0; WAL_REC];
            seal[0] = WAL_COMMIT;
            commit::seal_wal_commit(&mut seal, 1, 10, commit::CommitKind::Update, flags, data);
            [header.as_slice(), data, seal.as_slice()].concat()
        };
        for (version, data, flags, accepted) in [
            (2, valid.clone(), commit::WAL_FLAG_CONFIGURATION, true),
            (2, quad.to_vec(), 0, true),
            (2, valid.clone(), 0, false),
            (2, quad.to_vec(), commit::WAL_FLAG_CONFIGURATION, false),
            (
                2,
                [configuration, configuration].concat(),
                commit::WAL_FLAG_CONFIGURATION,
                false,
            ),
            (
                2,
                [configuration, quad].concat(),
                commit::WAL_FLAG_CONFIGURATION,
                false,
            ),
            (1, valid.clone(), commit::WAL_FLAG_CONFIGURATION, false),
            (0, valid.clone(), commit::WAL_FLAG_CONFIGURATION, false),
            (
                2,
                valid.clone(),
                commit::WAL_FLAG_CONFIGURATION | 128,
                false,
            ),
        ] {
            let bytes = if version == 0 {
                fixture(version, &data, flags)[WAL_REC..].to_vec()
            } else {
                fixture(version, &data, flags)
            };
            std::fs::write(&path, bytes).unwrap();
            if version == 0 {
                // the pre-release headerless log is refused before any record is read
                let error = WalCursor::open(&path, p(0, 0)).err().expect("version 0");
                assert!(matches!(error, Error::Unsupported(_)), "{error}");
                assert!(error.to_string().contains("wal version 0"), "{error}");
                continue;
            }
            let mut cursor = WalCursor::open(&path, p(0, 0)).unwrap();
            let result = cursor.next().map(|r| r.map(|(seq, _)| seq));
            assert_eq!(
                result.is_ok(),
                accepted,
                "version={version} flags={flags} data={data:?}"
            );
            if accepted {
                assert_eq!(result.unwrap(), Some(1));
                assert_eq!(
                    cursor.configuration(),
                    if flags == 0 { None } else { Some(identity) }
                );
            }
        }
        // A changed payload with the old seal remains a checksum failure, rather
        // than being accepted because the metadata's placement happens to be valid.
        let mut damaged = fixture(2, &valid, commit::WAL_FLAG_CONFIGURATION);
        damaged[WAL_REC + 1] ^= 1;
        std::fs::write(&path, damaged).unwrap();
        assert!(WalCursor::open(&path, p(0, 0)).unwrap().next().is_err());
    }

    fn p(seq: u64, offset: u64) -> WalPoint {
        WalPoint {
            seq,
            offset,
            folding: false,
        }
    }

    fn crc_fixture(backward: bool) -> (tempfile::TempDir, Store, PathBuf, WalPoint, WalPoint) {
        let root = tempfile::tempdir().unwrap();
        let store = Store::open(root.path(), Default::default()).unwrap();
        for (commit, count) in [(1, 6_000), (2, if backward { 3_000 } else { 9_000 })] {
            let mut body = String::from("INSERT DATA {");
            use std::fmt::Write;
            for n in 0..count {
                write!(body, " <urn:c{commit}:{n}> <urn:p> {n} .").unwrap();
            }
            body.push('}');
            crate::sparql::update::update(&store, &body, &Default::default()).unwrap();
        }
        store.flush_change_log().unwrap();
        let snap = store.snapshot();
        let path = snap.generation.dir.as_ref().unwrap().join("wal.log");
        let base = p(0, 0);
        let mut cursor = WalCursor::open(&path, base).unwrap();
        assert_eq!(cursor.next().unwrap().unwrap().0, 1);
        let first = cursor.position();
        assert_eq!(cursor.next().unwrap().unwrap().0, 2);
        let second = cursor.position();
        // These are verified real boundaries. Dense fixture points exclude
        // selector discovery from the checksum observer's replay scope.
        *snap.generation.wal_index.lock() = Some(WalIndex {
            points: vec![base, first, second],
            last: second,
            since: 0,
        });
        assert_eq!(backward, second.offset - first.offset < first.offset);
        (root, store, path, first, second)
    }

    fn durable_files(root: &Path) -> std::collections::BTreeMap<PathBuf, Vec<u8>> {
        let mut pending = vec![root.to_path_buf()];
        let mut files = std::collections::BTreeMap::new();
        while let Some(dir) = pending.pop() {
            for entry in std::fs::read_dir(dir).unwrap() {
                let entry = entry.unwrap();
                if entry.file_type().unwrap().is_dir() {
                    pending.push(entry.path());
                } else {
                    files.insert(entry.path(), std::fs::read(entry.path()).unwrap());
                }
            }
        }
        files
    }

    fn interrupt_crc(backward: bool, deadline: bool, damage: bool) {
        use std::sync::{
            Arc,
            atomic::{AtomicBool, AtomicUsize, Ordering},
        };
        use std::time::{Duration, Instant};
        let (root, store, path, first, second) = crc_fixture(backward);
        let seq = if backward { 2 } else { 1 };
        let end = if backward {
            second.offset
        } else {
            first.offset
        };
        if damage {
            let mut bytes = std::fs::read(&path).unwrap();
            bytes[end as usize - 1] ^= 1;
            std::fs::write(&path, bytes).unwrap();
        }
        let before = durable_files(root.path());
        let cache_before = store.history.as_ref().unwrap().lock().cache.len();
        let cancel = Arc::new(AtomicBool::new(false));
        let until = deadline.then(|| Instant::now() + Duration::from_secs(2));
        let options = crate::history::HistoryOptions {
            cancel: Some(cancel.clone()),
            deadline: until,
        };
        let processed = Arc::new(AtomicUsize::new(0));
        let total = Arc::new(AtomicUsize::new(0));
        let progress = processed.clone();
        let size = total.clone();
        let observer = Arc::new(move |found: u64, bytes: usize, length: usize| {
            assert_eq!(found, seq, "fixture selected the wrong replay direction");
            progress.store(bytes, Ordering::SeqCst);
            size.store(length, Ordering::SeqCst);
            if bytes < length {
                if let Some(until) = until {
                    while Instant::now() < until {
                        std::thread::sleep(Duration::from_millis(1));
                    }
                } else {
                    cancel.store(true, Ordering::SeqCst);
                }
            }
        });
        let result = crate::commit::with_wal_crc_observer(observer, || {
            store.snapshot_at_limited(&crate::history::At::Commit(1), &options, 128 << 20)
        });
        let worked = processed.load(Ordering::SeqCst);
        let complete = total.load(Ordering::SeqCst);
        assert!(
            complete > crate::commit::WAL_CRC_CHUNK,
            "checksum was never reached"
        );
        assert_eq!(durable_files(root.path()), before);
        assert_eq!(
            store.history.as_ref().unwrap().lock().cache.len(),
            cache_before
        );
        assert!(
            worked < complete,
            "checksum ignored the mid-pass control: processed {worked}/{complete} bytes"
        );
        if deadline {
            assert!(matches!(result, Err(Error::Timeout)));
        } else {
            assert!(matches!(result, Err(Error::Cancelled)));
        }
    }

    #[test]
    fn bounded_crc_forward_cancel() {
        interrupt_crc(false, false, false);
    }
    #[test]
    fn bounded_crc_reverse_cancel() {
        interrupt_crc(true, false, false);
    }
    #[test]
    fn bounded_crc_forward_deadline() {
        interrupt_crc(false, true, false);
    }
    #[test]
    fn bounded_crc_reverse_deadline() {
        interrupt_crc(true, true, false);
    }
    #[test]
    fn bounded_crc_damaged_forward_cancel() {
        interrupt_crc(false, false, true);
    }
    #[test]
    fn bounded_crc_damaged_reverse_cancel() {
        interrupt_crc(true, false, true);
    }

    #[test]
    fn bounded_crc_state_matches_ordinary_in_both_directions() {
        for backward in [false, true] {
            let (root, store, _, _, _) = crc_fixture(backward);
            let before = durable_files(root.path());
            let at = crate::history::At::Commit(1);
            let (bounded, resolved) = store
                .snapshot_at_limited(&at, &Default::default(), 128 << 20)
                .unwrap();
            let mut bounded_quads = Vec::new();
            bounded
                .for_each_quad(|q| {
                    bounded_quads.push(*q);
                    Ok(())
                })
                .unwrap();
            {
                let mut history = store.history.as_ref().unwrap().lock();
                history.cache.clear();
                history.settle();
            }
            let (ordinary, expected) = store.snapshot_at(&at, &Default::default()).unwrap();
            let mut ordinary_quads = Vec::new();
            ordinary
                .for_each_quad(|q| {
                    ordinary_quads.push(*q);
                    Ok(())
                })
                .unwrap();
            bounded_quads.sort_unstable();
            ordinary_quads.sort_unstable();
            assert_eq!(bounded_quads.len(), 6_000);
            assert_eq!(bounded_quads, ordinary_quads);
            assert_eq!(resolved.commit, expected.commit);
            assert_eq!(durable_files(root.path()), before);
        }
    }

    #[test]
    fn bounded_crc_damage_still_refuses_in_both_directions() {
        for backward in [false, true] {
            let (root, store, path, first, second) = crc_fixture(backward);
            let end = if backward {
                second.offset
            } else {
                first.offset
            };
            let mut bytes = std::fs::read(&path).unwrap();
            bytes[end as usize - 1] ^= 1;
            std::fs::write(&path, bytes).unwrap();
            let before = durable_files(root.path());
            let result = store.snapshot_at_limited(
                &crate::history::At::Commit(1),
                &Default::default(),
                128 << 20,
            );
            assert!(matches!(result, Err(Error::Corrupt(_))));
            assert!(store.history.as_ref().unwrap().lock().cache.is_empty());
            assert_eq!(durable_files(root.path()), before);
        }
    }

    /// Direct writes, buffered writes in between, and block boundaries leave the file
    /// with exactly the bytes written, in order, and zeros after them.
    #[test]
    fn direct_writes_keep_the_log_bytes_exact() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("wal.log");
        let log = open_for_append(&path, PLAIN_VERSION).unwrap();
        let Some(mut direct) = DirectLog::open(&path, &log) else {
            // no direct I/O on this file system (or OS): commits take the buffered path
            return;
        };
        let (mut at, mut alloc) = (0u64, 0u64);
        let mut expect = Vec::new();
        let rec = |i: usize, n: usize| -> Vec<u8> {
            (0..n * 33).map(|j| (1 + (i * 7 + j) % 251) as u8).collect()
        };
        for i in 0..400 {
            // mostly one-quad commits, some longer, and every 50th written buffered
            let data = rec(i, if i % 9 == 0 { 40 } else { 2 });
            if i % 50 == 0 || !DirectLog::fits(at, data.len(), alloc) {
                write_commit(&log, at, &mut alloc, &data, 64 << 10).unwrap();
                log.sync_data().unwrap();
            } else {
                assert!(direct.write(at, &data).unwrap(), "commit {i}");
            }
            at += data.len() as u64;
            expect.extend_from_slice(&data);
        }
        // a commit too long for one direct write
        assert!(!DirectLog::fits(at, DIRECT_MAX, u64::MAX));
        let file = std::fs::read(&path).unwrap();
        assert_eq!(file.len() as u64, alloc);
        assert_eq!(&file[..expect.len()], &expect[..]);
        assert!(file[expect.len()..].iter().all(|&b| b == 0));
        // a write may not change the file's size
        assert!(!DirectLog::fits(alloc - 10, 33, alloc));
        let last = alloc - alloc % BLOCK;
        assert!(DirectLog::fits(last - BLOCK, 33, alloc));
    }

    #[test]
    fn the_index_keeps_sparse_points() {
        let mut ix = WalIndex::new(10, false);
        for s in 11..=3000u64 {
            ix.note(p(s, (s - 10) * 66));
        }
        // every 1024 commits (66 bytes each: the byte rule does not apply)
        assert_eq!(ix.len(), 3);
        assert_eq!(ix.floor(10), p(10, 0));
        assert_eq!(ix.floor(1033), p(10, 0));
        assert_eq!(ix.floor(1034), p(1034, 1024 * 66));
        assert_eq!(ix.floor(2999), p(2058, 2048 * 66));
        assert_eq!(ix.floor(5000), p(3000, 2990 * 66));
        // large transactions: a point per MiB
        let mut ix = WalIndex::new(0, false);
        for s in 1..=10u64 {
            ix.note(p(s, s * (600 << 10)));
        }
        assert_eq!(ix.len(), 6);
        // folded legacy transactions move the base's end
        let mut ix = WalIndex::new(5, true);
        ix.note(WalPoint {
            seq: 5,
            offset: 66,
            folding: true,
        });
        ix.note(p(6, 132));
        assert_eq!(ix.floor(5).offset, 66);
        assert_eq!(ix.floor(6).offset, 132);
        // out-of-order notes are ignored
        ix.note(p(4, 10));
        assert_eq!(ix.last(), p(6, 132));
    }
}
