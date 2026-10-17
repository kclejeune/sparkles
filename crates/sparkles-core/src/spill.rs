//! Temporary files for cursor operators whose state outgrows the memory budget
//! ([X08](../../../docs/specs/X08-disk-spill-and-server-memory.md) §6).
//!
//! A [`Spill`] owns one directory per process under the configured spill directory,
//! named `<boot-id>-<pid>-<random>` and created with mode 0700. It holds a marker file,
//! [`MARKER`], that names the format and the process's start, and the process keeps an
//! exclusive `flock` on the marker for its life. Spill files are opened with `O_TMPFILE`
//! where the file system supports it, so they have no name and the kernel frees them
//! when they are closed or the process dies. Elsewhere a file is created as
//! `q<query>-<n>.spl` and unlinked at once. A file belongs to the operator that wrote
//! it, and dropping the operator, on completion, error, cancellation, panic or cursor
//! close, frees its space. At start, [`Spill::open`] removes the directories of dead
//! processes: a directory goes only when its name has the pattern, its marker names this
//! format, no process holds the marker's lock and it holds nothing but the marker and
//! `*.spl` files. Anything else stays, with a warning.
//!
//! Disk is reserved before it is written. A query takes 64 MiB extents from the server's
//! reservation as its files grow, up to its own limit, and the server keeps the
//! configured free space on the spill file system. A refused reservation, `ENOSPC` or
//! another I/O error fails the query with the budget kind `spill`.
//!
//! The file format is private to one process, but it is versioned like every on-disk
//! format: a 32-byte header holds the magic `SPKSPL01`, the format version, the kind of
//! operator that wrote it, the codec, an encryption flag and the column count. Frames of
//! at most [`FRAME_ROWS`] rows follow, each with a 16-byte header (payload length, row
//! count, a CRC-32 of the header and payload, and flags) and a payload of columns, each
//! a 32-bit length and the column as the index blocks encode it (zig-zag delta varints,
//! then LZ4). A reader refuses another magic, version, codec or encryption flag, and a
//! frame whose CRC, length or row count does not match, with [`Error::Corrupt`].

use crate::error::{Budget, BudgetKind, Error, Result};
use crate::id::Id;
use parking_lot::Mutex;
use std::fs::File;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

/// The name of the format in the marker file.
pub const FORMAT_NAME: &str = "sparkles-spill";
/// The version of the file format, in every file's header.
pub const FORMAT_VERSION: u16 = 1;
const MAGIC: [u8; 8] = *b"SPKSPL01";
const HEADER_BYTES: usize = 32;
const FRAME_HEADER_BYTES: usize = 16;
/// The most rows of one frame.
pub const FRAME_ROWS: usize = 4096;
/// The step in which a query reserves disk from the server.
pub const EXTENT: u64 = 64 << 20;
/// The marker file of a process directory.
pub const MARKER: &str = "SPARKLES-SPILL";
/// The codec of the columns: zig-zag delta varints, then LZ4.
const CODEC_DELTA_LZ4: u8 = 1;

/// What wrote a spill file.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum SpillKind {
    /// A sorted run of an external sort.
    SortRun = 1,
    /// One partition of a hash join's build or probe input.
    JoinPartition = 2,
    /// One partition of a grouping's partial states or input rows.
    GroupPartition = 3,
    /// One partition of a DISTINCT's rows.
    DistinctPartition = 4,
}

impl SpillKind {
    fn from_u8(v: u8) -> Option<Self> {
        Some(match v {
            1 => SpillKind::SortRun,
            2 => SpillKind::JoinPartition,
            3 => SpillKind::GroupPartition,
            4 => SpillKind::DistinctPartition,
            _ => return None,
        })
    }
}

/// Where spill files go and how much disk they may take.
#[derive(Clone, Debug)]
pub struct SpillOptions {
    /// The directory of the process directories. It must be on a local file system.
    pub dir: PathBuf,
    /// The disk all queries together may reserve.
    pub max_bytes: u64,
    /// The disk one query may reserve unless it asks for less.
    pub query_bytes: u64,
    /// The free space to leave on the spill file system.
    pub min_free_bytes: u64,
}

/// A failure the tests inject into the spill file API.
#[doc(hidden)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Fault {
    /// The n-th write of a frame fails as if the disk were full.
    Write,
    /// The n-th read of a frame finds a damaged frame.
    Read,
    /// The n-th reservation of an extent is refused.
    Reserve,
}

/// The server-wide owner of the spill directory and its disk reservation.
pub struct Spill {
    dir: PathBuf,
    /// The marker file, which holds the process's lock for as long as it is open.
    _marker: File,
    max_bytes: u64,
    query_bytes: u64,
    min_free_bytes: u64,
    reserved: AtomicU64,
    files: AtomicU64,
    queries: AtomicU64,
    written: AtomicU64,
    spilled_queries: AtomicU64,
    fault: Mutex<Option<(Fault, u64)>>,
}

impl std::fmt::Debug for Spill {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Spill")
            .field("dir", &self.dir)
            .field("max_bytes", &self.max_bytes)
            .field("reserved", &self.reserved.load(Ordering::Relaxed))
            .finish()
    }
}

fn io(error: std::io::Error, what: &str) -> Error {
    Error::Io(std::io::Error::new(
        error.kind(),
        format!("spill {what}: {error}"),
    ))
}

/// The failure of a refused disk reservation.
fn refused(limit: u64, requested: u64) -> Error {
    Error::BudgetExceeded(Budget {
        kind: BudgetKind::Spill,
        limit,
        requested,
    })
}

/// The boot identifier of the machine, as 32 hex digits, or zeros where it is unknown.
fn boot_id() -> String {
    std::fs::read_to_string("/proc/sys/kernel/random/boot_id")
        .ok()
        .map(|s| s.trim().replace('-', ""))
        .filter(|s| s.len() == 32 && s.bytes().all(|b| b.is_ascii_hexdigit()))
        .unwrap_or_else(|| "0".repeat(32))
}

/// Whether `name` is the name of a process directory: 32 hex digits, a process number
/// and 16 hex digits, joined by dashes.
fn process_dir_name(name: &str) -> bool {
    let parts: Vec<&str> = name.split('-').collect();
    matches!(parts.as_slice(), [boot, pid, random]
        if boot.len() == 32 && boot.bytes().all(|b| b.is_ascii_hexdigit())
            && !pid.is_empty() && pid.bytes().all(|b| b.is_ascii_digit())
            && random.len() == 16 && random.bytes().all(|b| b.is_ascii_hexdigit()))
}

#[cfg(unix)]
fn try_lock(file: &File) -> bool {
    use std::os::fd::AsRawFd;
    // SAFETY: flock on a file descriptor this process owns.
    unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) == 0 }
}

#[cfg(not(unix))]
fn try_lock(_file: &File) -> bool {
    false
}

/// The free bytes of the file system that holds `dir`.
#[cfg(unix)]
fn free_bytes(dir: &Path) -> Option<u64> {
    use std::os::unix::ffi::OsStrExt;
    let path = std::ffi::CString::new(dir.as_os_str().as_bytes()).ok()?;
    // SAFETY: statvfs fills the zeroed struct for a valid NUL-terminated path.
    unsafe {
        let mut stat: libc::statvfs = std::mem::zeroed();
        (libc::statvfs(path.as_ptr(), &mut stat) == 0)
            .then(|| (stat.f_bavail as u64).saturating_mul(stat.f_frsize as u64))
    }
}

#[cfg(not(unix))]
fn free_bytes(_dir: &Path) -> Option<u64> {
    None
}

impl Spill {
    /// Create this process's directory under `options.dir`, after removing the
    /// directories that dead processes left there.
    pub fn open(options: &SpillOptions) -> Result<Arc<Spill>> {
        if !cfg!(unix) {
            return Err(Error::Unsupported(
                "disk spill needs a Unix file system".into(),
            ));
        }
        std::fs::create_dir_all(&options.dir).map_err(|e| io(e, "directory"))?;
        sweep(&options.dir);
        let random: u64 = rand::random();
        let name = format!("{}-{}-{random:016x}", boot_id(), std::process::id());
        let dir = options.dir.join(name);
        let mut builder = std::fs::DirBuilder::new();
        #[cfg(unix)]
        {
            use std::os::unix::fs::DirBuilderExt;
            builder.mode(0o700);
        }
        builder
            .create(&dir)
            .map_err(|e| io(e, "process directory"))?;
        let marker_path = dir.join(MARKER);
        let started = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_secs());
        std::fs::write(
            &marker_path,
            format!(
                "{FORMAT_NAME} {FORMAT_VERSION}\nstarted {started}\npid {}\n",
                std::process::id()
            ),
        )
        .map_err(|e| io(e, "marker"))?;
        let marker = File::open(&marker_path).map_err(|e| io(e, "marker"))?;
        if !try_lock(&marker) {
            return Err(Error::invalid(format!(
                "could not lock the spill marker in {}",
                dir.display()
            )));
        }
        Ok(Arc::new(Spill {
            dir,
            _marker: marker,
            max_bytes: options.max_bytes,
            query_bytes: options.query_bytes,
            min_free_bytes: options.min_free_bytes,
            reserved: AtomicU64::new(0),
            files: AtomicU64::new(0),
            queries: AtomicU64::new(0),
            written: AtomicU64::new(0),
            spilled_queries: AtomicU64::new(0),
            fault: Mutex::new(None),
        }))
    }

    /// The directory of this process's spill files.
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// The disk all queries together may reserve.
    pub fn capacity(&self) -> u64 {
        self.max_bytes
    }

    /// The disk the queries hold now.
    pub fn reserved_bytes(&self) -> u64 {
        self.reserved.load(Ordering::Relaxed)
    }

    /// The bytes written to spill files since the start.
    pub fn written_bytes(&self) -> u64 {
        self.written.load(Ordering::Relaxed)
    }

    /// How many queries wrote a spill file.
    pub fn spilled_queries(&self) -> u64 {
        self.spilled_queries.load(Ordering::Relaxed)
    }

    /// The reservation of one query, whose files may take `limit` bytes when it is
    /// given and lower than the server's limit for one query.
    pub fn query(self: &Arc<Self>, limit: Option<u64>) -> QuerySpill {
        QuerySpill {
            spill: self.clone(),
            limit: limit.map_or(self.query_bytes, |l| l.min(self.query_bytes)),
            number: self.queries.fetch_add(1, Ordering::Relaxed),
            state: Mutex::new(QueryState::default()),
        }
    }

    /// Make the `after`-th next operation of kind `fault` fail (tests only).
    #[doc(hidden)]
    pub fn inject_fault(&self, fault: Fault, after: u64) {
        *self.fault.lock() = Some((fault, after));
    }

    fn faulted(&self, op: Fault) -> bool {
        let mut fault = self.fault.lock();
        match &mut *fault {
            Some((kind, n)) if *kind == op => {
                if *n <= 1 {
                    *fault = None;
                    true
                } else {
                    *n -= 1;
                    false
                }
            }
            _ => false,
        }
    }

    /// Take one extent of the server's reservation.
    fn take_extent(&self) -> Result<()> {
        if self.faulted(Fault::Reserve) {
            return Err(refused(self.max_bytes, self.reserved_bytes() + EXTENT));
        }
        let mut current = self.reserved.load(Ordering::Acquire);
        loop {
            let next = current.saturating_add(EXTENT);
            if next > self.max_bytes {
                return Err(refused(self.max_bytes, next));
            }
            match self.reserved.compare_exchange_weak(
                current,
                next,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => break,
                Err(seen) => current = seen,
            }
        }
        if self.min_free_bytes > 0
            && let Some(free) = free_bytes(&self.dir)
            && free < self.min_free_bytes.saturating_add(EXTENT)
        {
            self.reserved.fetch_sub(EXTENT, Ordering::AcqRel);
            return Err(refused(free.saturating_sub(self.min_free_bytes), EXTENT));
        }
        Ok(())
    }

    /// Open an unnamed file in the process directory.
    fn create(&self, query: u64) -> Result<File> {
        let n = self.files.fetch_add(1, Ordering::Relaxed);
        #[cfg(target_os = "linux")]
        {
            use std::os::unix::fs::OpenOptionsExt;
            let opened = std::fs::OpenOptions::new()
                .read(true)
                .write(true)
                .mode(0o600)
                .custom_flags(libc::O_TMPFILE)
                .open(&self.dir);
            match opened {
                Ok(file) => return Ok(file),
                // A file system without O_TMPFILE takes the named path below.
                Err(e)
                    if matches!(
                        e.raw_os_error(),
                        Some(libc::EOPNOTSUPP) | Some(libc::EISDIR) | Some(libc::EINVAL)
                    ) => {}
                Err(e) => return Err(io(e, "file")),
            }
        }
        let path = self.dir.join(format!("q{query}-{n}.spl"));
        let mut options = std::fs::OpenOptions::new();
        options.read(true).write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let file = options.open(&path).map_err(|e| io(e, "file"))?;
        std::fs::remove_file(&path).map_err(|e| io(e, "file"))?;
        Ok(file)
    }
}

impl Drop for Spill {
    fn drop(&mut self) {
        // Every file was unlinked when it was made, so only the marker is left.
        let _ = std::fs::remove_file(self.dir.join(MARKER));
        let _ = std::fs::remove_dir(&self.dir);
    }
}

/// Remove the process directories under `root` that dead processes left. A directory
/// goes only when its name has the pattern, its marker names this format, its marker's
/// lock is free, and it holds nothing but the marker and `*.spl` files.
pub fn sweep(root: &Path) -> usize {
    let Ok(entries) = std::fs::read_dir(root) else {
        return 0;
    };
    let mut removed = 0;
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(name) = name.to_str() else { continue };
        let path = entry.path();
        if !entry.file_type().is_ok_and(|t| t.is_dir()) || !process_dir_name(name) {
            tracing::warn!(path = %path.display(), "leaving a foreign entry in the spill directory");
            continue;
        }
        let marker_path = path.join(MARKER);
        let ok = (|| {
            let text = std::fs::read_to_string(&marker_path).ok()?;
            if !text.starts_with(&format!("{FORMAT_NAME} ")) {
                return None;
            }
            let marker = File::open(&marker_path).ok()?;
            if !try_lock(&marker) {
                // A live process holds it.
                return None;
            }
            let mut files = Vec::new();
            for item in std::fs::read_dir(&path).ok()?.flatten() {
                let item_name = item.file_name();
                let item_name = item_name.to_str()?;
                if item_name == MARKER {
                    continue;
                }
                if !item_name.ends_with(".spl") || !item.file_type().ok()?.is_file() {
                    return None;
                }
                files.push(item.path());
            }
            for file in files {
                std::fs::remove_file(file).ok()?;
            }
            std::fs::remove_file(&marker_path).ok()?;
            drop(marker);
            std::fs::remove_dir(&path).ok()
        })();
        if ok.is_some() {
            removed += 1;
        } else if path.exists() {
            tracing::warn!(path = %path.display(), "leaving a spill directory that is in use or not a spill directory");
        }
    }
    removed
}

#[derive(Default)]
struct QueryState {
    /// The bytes the query's live files take.
    used: u64,
    /// The extents the query holds of the server's reservation.
    reserved: u64,
    /// The bytes the query has written, and its files.
    written: u64,
    files: u64,
}

/// One query's reservation of spill disk. Dropping it gives its extents back.
pub struct QuerySpill {
    spill: Arc<Spill>,
    limit: u64,
    number: u64,
    state: Mutex<QueryState>,
}

impl std::fmt::Debug for QuerySpill {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("QuerySpill")
            .field("limit", &self.limit)
            .finish()
    }
}

impl QuerySpill {
    /// The bytes the query has written to spill files.
    pub fn written_bytes(&self) -> u64 {
        self.state.lock().written
    }

    /// How many spill files the query has made.
    pub fn files(&self) -> u64 {
        self.state.lock().files
    }

    /// The disk this query may take.
    pub fn limit(&self) -> u64 {
        self.limit
    }

    fn reserve(&self, bytes: u64) -> Result<()> {
        let mut state = self.state.lock();
        let used = state.used.saturating_add(bytes);
        if used > self.limit {
            return Err(refused(self.limit, used));
        }
        while used > state.reserved {
            self.spill.take_extent()?;
            state.reserved += EXTENT;
        }
        state.used = used;
        state.written = state.written.saturating_add(bytes);
        drop(state);
        self.spill.written.fetch_add(bytes, Ordering::Relaxed);
        Ok(())
    }

    fn release(&self, bytes: u64) {
        let mut state = self.state.lock();
        state.used = state.used.saturating_sub(bytes);
    }

    /// Start a spill file of `columns` columns.
    pub fn create(self: &Arc<Self>, kind: SpillKind, columns: usize) -> Result<SpillWriter> {
        let columns_u32 =
            u32::try_from(columns).map_err(|_| Error::invalid("too many spill columns"))?;
        self.reserve(HEADER_BYTES as u64)?;
        let file = match self.spill.create(self.number) {
            Ok(file) => file,
            Err(e) => {
                self.release(HEADER_BYTES as u64);
                return Err(e);
            }
        };
        let first = {
            let mut state = self.state.lock();
            state.files += 1;
            state.files == 1
        };
        if first {
            self.spill.spilled_queries.fetch_add(1, Ordering::Relaxed);
        }
        let mut header = [0u8; HEADER_BYTES];
        header[..8].copy_from_slice(&MAGIC);
        header[8..10].copy_from_slice(&FORMAT_VERSION.to_le_bytes());
        header[10] = kind as u8;
        header[11] = CODEC_DELTA_LZ4;
        // header[12]: no encryption
        header[16..20].copy_from_slice(&columns_u32.to_le_bytes());
        let mut writer = SpillWriter {
            file: Some(file),
            query: self.clone(),
            kind,
            columns,
            rows: 0,
            frames: 0,
            bytes: 0,
            buffer: Vec::new(),
            scratch: Vec::new(),
            frame_rows: FRAME_ROWS,
        };
        writer.bytes = HEADER_BYTES as u64;
        writer.write_bytes(&header)?;
        Ok(writer)
    }
}

impl Drop for QuerySpill {
    fn drop(&mut self) {
        let reserved = self.state.get_mut().reserved;
        self.spill.reserved.fetch_sub(reserved, Ordering::AcqRel);
    }
}

/// A spill file being written. Dropping it removes the file.
pub struct SpillWriter {
    file: Option<File>,
    query: Arc<QuerySpill>,
    kind: SpillKind,
    columns: usize,
    rows: u64,
    frames: u64,
    /// The bytes reserved for the file so far.
    bytes: u64,
    buffer: Vec<u8>,
    scratch: Vec<u8>,
    /// The most rows a frame holds, which bounds what a reader decodes at once.
    frame_rows: usize,
}

impl SpillWriter {
    /// Write frames of at most `rows` rows, between 1 and [`FRAME_ROWS`], so that a
    /// merge reading many files at once holds a smaller frame of each.
    pub fn set_frame_rows(&mut self, rows: usize) {
        self.frame_rows = rows.clamp(1, FRAME_ROWS);
    }

    fn write_bytes(&mut self, bytes: &[u8]) -> Result<()> {
        use std::io::Write;
        let file = self.file.as_mut().expect("an open spill file");
        file.write_all(bytes).map_err(|e| {
            if e.raw_os_error() == Some(28) {
                refused(self.query.limit, self.bytes)
            } else {
                io(e, "write")
            }
        })
    }

    /// The rows written so far.
    pub fn rows(&self) -> u64 {
        self.rows
    }

    /// The bytes written so far.
    pub fn bytes(&self) -> u64 {
        self.bytes
    }

    /// Append `rows` rows, given as one slice per column, in frames of at most
    /// [`FRAME_ROWS`] rows, or fewer after [`SpillWriter::set_frame_rows`].
    pub fn write(&mut self, columns: &[&[Id]], rows: usize) -> Result<()> {
        if columns.len() != self.columns || columns.iter().any(|c| c.len() < rows) {
            return Err(Error::invalid("a spill frame does not match its file"));
        }
        let mut at = 0;
        while at < rows {
            let n = (rows - at).min(self.frame_rows);
            self.frame(columns, at..at + n)?;
            at += n;
        }
        Ok(())
    }

    fn frame(&mut self, columns: &[&[Id]], range: std::ops::Range<usize>) -> Result<()> {
        let mut payload = std::mem::take(&mut self.buffer);
        payload.clear();
        let mut values: Vec<u64> = Vec::with_capacity(range.len());
        for column in columns {
            values.clear();
            values.extend(column[range.clone()].iter().map(|id| id.0));
            let encoded = crate::index::encode_column(&values, &mut self.scratch);
            let len = u32::try_from(encoded.len())
                .map_err(|_| Error::invalid("a spill column is too large"))?;
            payload.extend_from_slice(&len.to_le_bytes());
            payload.extend_from_slice(&encoded);
        }
        let len = u32::try_from(payload.len())
            .map_err(|_| Error::invalid("a spill frame is too large"))?;
        let rows = range.len() as u32;
        let mut header = [0u8; FRAME_HEADER_BYTES];
        header[..4].copy_from_slice(&len.to_le_bytes());
        header[4..8].copy_from_slice(&rows.to_le_bytes());
        header[12..16].copy_from_slice(&0u32.to_le_bytes());
        let crc = frame_crc(&header, &payload);
        header[8..12].copy_from_slice(&crc.to_le_bytes());
        let total = (FRAME_HEADER_BYTES + payload.len()) as u64;
        self.query.reserve(total)?;
        self.bytes += total;
        if self.query.spill.faulted(Fault::Write) {
            self.buffer = payload;
            return Err(refused(self.query.limit, self.bytes));
        }
        self.write_bytes(&header)?;
        self.write_bytes(&payload)?;
        self.buffer = payload;
        self.rows += u64::from(rows);
        self.frames += 1;
        Ok(())
    }

    /// Finish the file for reading.
    pub fn finish(mut self) -> Result<SpillRun> {
        let file = self.file.take().expect("an open spill file");
        Ok(SpillRun {
            file,
            query: self.query.clone(),
            kind: self.kind,
            columns: self.columns,
            rows: self.rows,
            frames: self.frames,
            bytes: std::mem::take(&mut self.bytes),
        })
    }
}

impl Drop for SpillWriter {
    fn drop(&mut self) {
        self.query.release(self.bytes);
    }
}

/// The CRC-32 of a frame's header, with its CRC field zero, and its payload.
fn frame_crc(header: &[u8; FRAME_HEADER_BYTES], payload: &[u8]) -> u32 {
    let mut crc = flate2::Crc::new();
    crc.update(&header[..8]);
    crc.update(&header[12..16]);
    crc.update(payload);
    crc.sum()
}

/// A finished spill file. Dropping it closes the file, which frees its space.
pub struct SpillRun {
    file: File,
    query: Arc<QuerySpill>,
    kind: SpillKind,
    columns: usize,
    rows: u64,
    frames: u64,
    bytes: u64,
}

impl std::fmt::Debug for SpillRun {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SpillRun")
            .field("rows", &self.rows)
            .field("bytes", &self.bytes)
            .finish()
    }
}

impl SpillRun {
    pub fn rows(&self) -> u64 {
        self.rows
    }

    pub fn bytes(&self) -> u64 {
        self.bytes
    }

    pub fn frames(&self) -> u64 {
        self.frames
    }

    /// A reader from the first frame. Several readers of one file are independent.
    pub fn reader(self: &Arc<Self>) -> Result<SpillReader> {
        let mut header = [0u8; HEADER_BYTES];
        read_at(&self.file, &mut header, 0)?;
        if header[..8] != MAGIC {
            return Err(Error::Corrupt("spill file: not a spill file".into()));
        }
        let version = u16::from_le_bytes([header[8], header[9]]);
        if version != FORMAT_VERSION {
            return Err(Error::Corrupt(format!(
                "spill file: format version {version}, this build reads {FORMAT_VERSION}"
            )));
        }
        if SpillKind::from_u8(header[10]) != Some(self.kind) {
            return Err(Error::Corrupt("spill file: unknown operator kind".into()));
        }
        if header[11] != CODEC_DELTA_LZ4 {
            return Err(Error::Corrupt(format!(
                "spill file: unknown codec {}",
                header[11]
            )));
        }
        if header[12] != 0 {
            return Err(Error::Corrupt(
                "spill file: encrypted frames are not supported by this build".into(),
            ));
        }
        let columns = u32::from_le_bytes(header[16..20].try_into().unwrap()) as usize;
        if columns != self.columns {
            return Err(Error::Corrupt("spill file: column count differs".into()));
        }
        Ok(SpillReader {
            run: self.clone(),
            offset: HEADER_BYTES as u64,
            frames: 0,
            payload: Vec::new(),
        })
    }
}

impl Drop for SpillRun {
    fn drop(&mut self) {
        self.query.release(self.bytes);
    }
}

#[cfg(unix)]
fn read_at(file: &File, buf: &mut [u8], offset: u64) -> Result<()> {
    use std::os::unix::fs::FileExt;
    file.read_exact_at(buf, offset).map_err(|e| {
        if e.kind() == std::io::ErrorKind::UnexpectedEof {
            Error::Corrupt("spill file: truncated".into())
        } else {
            io(e, "read")
        }
    })
}

#[cfg(not(unix))]
fn read_at(_file: &File, _buf: &mut [u8], _offset: u64) -> Result<()> {
    Err(Error::Unsupported(
        "disk spill needs a Unix file system".into(),
    ))
}

/// Reads a spill file's frames in order.
pub struct SpillReader {
    run: Arc<SpillRun>,
    offset: u64,
    frames: u64,
    payload: Vec<u8>,
}

impl SpillReader {
    /// The next frame as one column of ids per file column, or `None` after the last.
    pub fn next_frame(&mut self) -> Result<Option<(Vec<Vec<Id>>, usize)>> {
        if self.frames == self.run.frames {
            return Ok(None);
        }
        let mut header = [0u8; FRAME_HEADER_BYTES];
        read_at(&self.run.file, &mut header, self.offset)?;
        let len = u32::from_le_bytes(header[..4].try_into().unwrap()) as usize;
        let rows = u32::from_le_bytes(header[4..8].try_into().unwrap()) as usize;
        let crc = u32::from_le_bytes(header[8..12].try_into().unwrap());
        if rows == 0 || rows > FRAME_ROWS {
            return Err(Error::Corrupt(format!("spill frame of {rows} rows")));
        }
        if self.offset + (FRAME_HEADER_BYTES + len) as u64 > self.run.bytes {
            return Err(Error::Corrupt("spill frame runs past its file".into()));
        }
        self.payload.resize(len, 0);
        read_at(
            &self.run.file,
            &mut self.payload,
            self.offset + FRAME_HEADER_BYTES as u64,
        )?;
        if self.run.query.spill.faulted(Fault::Read) {
            self.payload[0] ^= 0xff;
        }
        if frame_crc(&header, &self.payload) != crc {
            return Err(Error::Corrupt("spill frame: CRC mismatch".into()));
        }
        let mut columns = Vec::with_capacity(self.run.columns);
        let mut at = 0usize;
        for _ in 0..self.run.columns {
            let size = self
                .payload
                .get(at..at + 4)
                .map(|b| u32::from_le_bytes(b.try_into().unwrap()) as usize)
                .ok_or_else(|| Error::Corrupt("spill frame: short column".into()))?;
            let bytes = self
                .payload
                .get(at + 4..at + 4 + size)
                .ok_or_else(|| Error::Corrupt("spill frame: short column".into()))?;
            let values = crate::index::decode_column(bytes, rows)?;
            columns.push(values.iter().map(|&v| Id(v)).collect());
            at += 4 + size;
        }
        if at != len {
            return Err(Error::Corrupt("spill frame: trailing bytes".into()));
        }
        self.offset += (FRAME_HEADER_BYTES + len) as u64;
        self.frames += 1;
        Ok(Some((columns, rows)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spill(dir: &Path, max: u64, query: u64) -> Arc<Spill> {
        Spill::open(&SpillOptions {
            dir: dir.to_path_buf(),
            max_bytes: max,
            query_bytes: query,
            min_free_bytes: 0,
        })
        .unwrap()
    }

    fn ids(n: usize, step: u64) -> Vec<Id> {
        (0..n as u64).map(|i| Id(i * step + 7)).collect()
    }

    #[test]
    fn frames_round_trip_across_frame_boundaries() {
        let dir = tempfile::tempdir().unwrap();
        let spill = spill(dir.path(), 1 << 30, 1 << 30);
        let query = Arc::new(spill.query(None));
        let (a, b) = (ids(10_000, 3), ids(10_000, 1 << 40));
        let mut writer = query.create(SpillKind::SortRun, 2).unwrap();
        writer.write(&[&a, &b], 10_000).unwrap();
        writer.write(&[&a[..5], &b[..5]], 5).unwrap();
        let run = Arc::new(writer.finish().unwrap());
        assert_eq!(run.rows(), 10_005);
        assert_eq!(run.frames(), 4);
        for _ in 0..2 {
            let mut reader = run.reader().unwrap();
            let (mut x, mut y) = (Vec::new(), Vec::new());
            while let Some((columns, rows)) = reader.next_frame().unwrap() {
                assert!(rows <= FRAME_ROWS);
                x.extend_from_slice(&columns[0]);
                y.extend_from_slice(&columns[1]);
            }
            assert_eq!(&x[..10_000], &a[..]);
            assert_eq!(&y[10_000..], &b[..5]);
        }
        assert!(spill.reserved_bytes() >= EXTENT);
        drop(run);
        assert_eq!(query.state.lock().used, 0);
        drop(query);
        assert_eq!(spill.reserved_bytes(), 0);
        // Unnamed files leave nothing but the marker behind.
        let names: Vec<_> = std::fs::read_dir(spill.dir())
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .collect();
        assert_eq!(names, vec![std::ffi::OsString::from(MARKER)]);
    }

    #[test]
    fn zero_column_frames_keep_their_row_counts() {
        let dir = tempfile::tempdir().unwrap();
        let spill = spill(dir.path(), 1 << 30, 1 << 30);
        let query = Arc::new(spill.query(None));
        let mut writer = query.create(SpillKind::DistinctPartition, 0).unwrap();
        writer.write(&[], 5000).unwrap();
        let run = Arc::new(writer.finish().unwrap());
        let mut reader = run.reader().unwrap();
        let mut rows = 0;
        while let Some((columns, n)) = reader.next_frame().unwrap() {
            assert!(columns.is_empty());
            rows += n;
        }
        assert_eq!(rows, 5000);
    }

    #[test]
    fn damaged_versions_and_frames_are_refused() {
        let dir = tempfile::tempdir().unwrap();
        let spill = spill(dir.path(), 1 << 30, 1 << 30);
        let query = Arc::new(spill.query(None));
        let column = ids(100, 5);
        let mut writer = query.create(SpillKind::SortRun, 1).unwrap();
        writer.write(&[&column], 100).unwrap();
        let run = Arc::new(writer.finish().unwrap());
        use std::os::unix::fs::FileExt;
        // An unknown version.
        run.file.write_all_at(&9u16.to_le_bytes(), 8).unwrap();
        assert!(matches!(run.reader(), Err(Error::Corrupt(m)) if m.contains("version 9")));
        run.file
            .write_all_at(&FORMAT_VERSION.to_le_bytes(), 8)
            .unwrap();
        // A flipped payload byte.
        run.file
            .write_all_at(&[0xff], (HEADER_BYTES + FRAME_HEADER_BYTES + 6) as u64)
            .unwrap();
        let mut reader = run.reader().unwrap();
        assert!(matches!(reader.next_frame(), Err(Error::Corrupt(m)) if m.contains("CRC")));
        // An injected read fault.
        let mut writer = query.create(SpillKind::SortRun, 1).unwrap();
        writer.write(&[&column], 100).unwrap();
        let run = Arc::new(writer.finish().unwrap());
        spill.inject_fault(Fault::Read, 1);
        let mut reader = run.reader().unwrap();
        assert!(matches!(reader.next_frame(), Err(Error::Corrupt(_))));
    }

    #[test]
    fn reservations_fail_with_the_spill_budget() {
        let dir = tempfile::tempdir().unwrap();
        // A query limit below one frame.
        let spill = spill(dir.path(), 1 << 30, 64);
        let query = Arc::new(spill.query(None));
        let column = ids(4096, 1 << 50);
        let mut writer = query.create(SpillKind::SortRun, 1).unwrap();
        let error = writer.write(&[&column], 4096).unwrap_err();
        assert!(matches!(error, Error::BudgetExceeded(b) if b.kind == BudgetKind::Spill));
        drop(writer);
        assert_eq!(query.state.lock().used, 0);
        // A server reservation smaller than one extent.
        let spill2 = Spill::open(&SpillOptions {
            dir: dir.path().to_path_buf(),
            max_bytes: EXTENT - 1,
            query_bytes: 1 << 40,
            min_free_bytes: 0,
        })
        .unwrap();
        let query = Arc::new(spill2.query(None));
        assert!(matches!(
            query.create(SpillKind::SortRun, 1),
            Err(Error::BudgetExceeded(b)) if b.kind == BudgetKind::Spill
        ));
        // Injected faults of a reservation and of a write.
        let query = Arc::new(spill.query(Some(1 << 40)));
        spill.inject_fault(Fault::Reserve, 1);
        assert!(query.create(SpillKind::SortRun, 1).is_err());
        let mut writer = Arc::new(spill.query(Some(1 << 30)))
            .create(SpillKind::SortRun, 1)
            .unwrap();
        spill.inject_fault(Fault::Write, 1);
        assert!(matches!(
            writer.write(&[&column], 4096),
            Err(Error::BudgetExceeded(b)) if b.kind == BudgetKind::Spill
        ));
        drop(writer);
        drop(query);
        assert_eq!(spill2.reserved_bytes(), 0);
    }

    #[test]
    fn sweep_removes_only_dead_spill_directories() {
        let dir = tempfile::tempdir().unwrap();
        let live = spill(dir.path(), 1 << 30, 1 << 30);
        // A dead process's directory with a leftover named file.
        let dead = dir
            .path()
            .join(format!("{}-1-{:016x}", "a".repeat(32), 5u64));
        std::fs::create_dir(&dead).unwrap();
        std::fs::write(dead.join(MARKER), format!("{FORMAT_NAME} 1\n")).unwrap();
        std::fs::write(dead.join("q1-1.spl"), b"x").unwrap();
        // A directory with a foreign file, a foreign name and a foreign marker.
        let odd = dir
            .path()
            .join(format!("{}-2-{:016x}", "b".repeat(32), 6u64));
        std::fs::create_dir(&odd).unwrap();
        std::fs::write(odd.join(MARKER), format!("{FORMAT_NAME} 1\n")).unwrap();
        std::fs::write(odd.join("notes.txt"), b"keep").unwrap();
        let foreign = dir.path().join("mine");
        std::fs::create_dir(&foreign).unwrap();
        let other = dir
            .path()
            .join(format!("{}-3-{:016x}", "c".repeat(32), 7u64));
        std::fs::create_dir(&other).unwrap();
        std::fs::write(other.join(MARKER), b"something else").unwrap();
        assert_eq!(sweep(dir.path()), 1);
        assert!(!dead.exists());
        assert!(odd.exists() && foreign.exists() && other.exists());
        // The live process's directory stays while it holds its lock.
        assert!(live.dir().exists());
        let live_dir = live.dir().to_path_buf();
        drop(live);
        assert!(!live_dir.exists());
    }

    #[test]
    fn process_directory_names_have_the_pattern() {
        assert!(process_dir_name(&format!(
            "{}-123-{}",
            "0".repeat(32),
            "f".repeat(16)
        )));
        assert!(!process_dir_name("mine"));
        assert!(!process_dir_name(&format!(
            "{}-x-{}",
            "0".repeat(32),
            "f".repeat(16)
        )));
    }
}
