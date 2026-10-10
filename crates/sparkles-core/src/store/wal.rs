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

use super::*;
use std::io::{BufReader, Seek, SeekFrom};

/// Open a generation's log for writing, positioned at the end of the file. It is not
/// opened in append mode: a store that preallocates writes each commit at the log's
/// logical end, which lies before the zero bytes that follow it.
pub(crate) fn open_for_append(path: &Path) -> std::io::Result<File> {
    let mut f = OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(path)?;
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
fn write_all_at(file: &File, buf: &[u8], at: u64) -> std::io::Result<()> {
    std::os::unix::fs::FileExt::write_all_at(file, buf, at)
}

#[cfg(windows)]
fn write_all_at(file: &File, mut buf: &[u8], mut at: u64) -> std::io::Result<()> {
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
fn write_all_at(_: &File, _: &[u8], _: u64) -> std::io::Result<()> {
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
}

impl WalCursor {
    pub fn new(mut file: File, path: &Path, from: WalPoint) -> Result<WalCursor> {
        file.seek(SeekFrom::Start(from.offset))?;
        Ok(WalCursor {
            r: BufReader::with_capacity(1 << 20, file),
            path: path.to_path_buf(),
            at: from,
            txn: Vec::new(),
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

    /// The next complete transaction: its commit's number and its data records (whole
    /// 33-byte records). `None` at the end of the log, a torn final transaction
    /// included. A checksum mismatch, an unknown record type or a number out of
    /// sequence is [`Error::Corrupt`].
    pub fn next(&mut self) -> Result<Option<(u64, &[u8])>> {
        self.txn.clear();
        let mut rec = [0u8; WAL_REC];
        let mut offset = self.at.offset;
        loop {
            match self.r.read_exact(&mut rec) {
                Ok(()) => {}
                Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => return Ok(None),
                Err(e) => return Err(e.into()),
            }
            offset += WAL_REC as u64;
            match rec[0] {
                WAL_INSERT | WAL_DELETE => self.txn.extend_from_slice(&rec),
                WAL_COMMIT => break,
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
        let (seq, folding) = match commit::open_wal_commit(&rec, &self.txn) {
            Some(Ok((seq, _, _, _))) => {
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
            // legacy records before the first with metadata fold into the base
            None if self.at.folding => (prev, true),
            None => (prev + 1, false),
        };
        self.at = WalPoint {
            seq,
            offset,
            folding,
        };
        Ok(Some((seq, &self.txn)))
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
            Some(Ok((s, _, _, _))) if s == *seq => {}
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
        for rec in buf.as_chunks::<WAL_REC>().0.iter().rev() {
            match rec[0] {
                WAL_COMMIT => {
                    if let Some(c) = commit.take() {
                        undo(&c, &data, &mut seq, delta)?;
                        data.clear();
                    }
                    commit = Some(*rec);
                }
                WAL_INSERT | WAL_DELETE if commit.is_some() => data.push(*rec),
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

#[cfg(test)]
mod tests {
    use super::*;

    fn p(seq: u64, offset: u64) -> WalPoint {
        WalPoint {
            seq,
            offset,
            folding: false,
        }
    }

    /// Direct writes, buffered writes in between, and block boundaries leave the file
    /// with exactly the bytes written, in order, and zeros after them.
    #[test]
    fn direct_writes_keep_the_log_bytes_exact() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("wal.log");
        let log = open_for_append(&path).unwrap();
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
