//! Temporary files for cursor operators whose state outgrows the memory budget
//! ([X08](../../../docs/specs/X08-disk-spill-and-server-memory.md) §6).
//!
//! A [`Spill`] owns one directory per process under the configured spill directory,
//! named `<boot-id>-<pid>-<random>` and created with mode 0700. The spill directory itself
//! must belong to the server's user and be writable by no one else, and it is created
//! with mode 0700 when it does not exist. The process directory holds a marker file,
//! [`MARKER`], that names the format and the process's start. The process locks the
//! marker with an exclusive `flock` before it writes the format into it and keeps the
//! lock for its life. Spill files are opened with `O_TMPFILE`
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
//! reservation as its files grow, up to its own limit, and gives back the extents its
//! open files no longer need whenever a file closes. The server keeps the configured
//! free space on the spill file system. A refused reservation, or a write that fails
//! with `ENOSPC`, `EDQUOT` or `EFBIG`, fails the query with the budget kind `spill`.
//! Another I/O error fails it with that error.
//!
//! The file format is private to one process, but it is versioned like every on-disk
//! format: a 32-byte header holds the magic `SPKSPL01`, the format version, the kind of
//! operator that wrote it, the codec, the encryption, the column count and the file's
//! number. Frames of at most [`FRAME_ROWS`] rows follow, each with a 16-byte header
//! (payload length, row count, a CRC-32 of the header and payload, and flags) and a
//! payload of columns, each a 32-bit length and the column as the index blocks encode it
//! (zig-zag delta varints, then LZ4). A reader refuses another magic, version, codec or
//! encryption, and a frame whose CRC, length or row count does not match, with
//! [`Error::Corrupt`].
//!
//! With [`SpillOptions::encrypted`] (`serve --spill encrypted`, X08 §6.7) every file has
//! its own random 256-bit AES-GCM key, made when the file is created and held only in
//! memory by its writer and its finished run, so a file is unreadable once the process
//! that wrote it is gone. Each frame's payload is sealed with AES-256-GCM through
//! `aws-lc-rs`. Its nonce is the file's number and the frame's number, and its
//! additional data is the file header's kind, column count and number with the frame's
//! number, length and row count, so a frame that is changed, or moved to another place
//! in its file or to another file, fails to open. The reader knows from memory how many
//! frames the file holds, so a truncated file fails too. The CRC of an encrypted frame
//! covers the sealed bytes, so it reveals nothing of the plain text.

use crate::error::{Budget, BudgetKind, Error, Result};
use crate::id::Id;
use parking_lot::Mutex;
use std::fs::File;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

/// The name of the format in the marker file.
pub const FORMAT_NAME: &str = "sparkles-spill";
/// The version of the file format, in every file's header, registered as
/// [`crate::formats::SPILL`].
pub const FORMAT_VERSION: u16 = 2;
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
/// The encryption byte of a plain file.
const PLAIN: u8 = 0;
/// The encryption byte of a file whose frames are sealed with AES-256-GCM.
const AES_256_GCM: u8 = 1;
/// The bytes of an AES-GCM tag after each sealed payload.
const TAG_BYTES: usize = 16;

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
    /// The directory of the process directories. It must be on a local file system,
    /// belong to the server's user and be writable by no other user.
    pub dir: PathBuf,
    /// The disk all queries together may reserve.
    pub max_bytes: u64,
    /// The disk one query may reserve unless it asks for less.
    pub query_bytes: u64,
    /// The free space to leave on the spill file system.
    pub min_free_bytes: u64,
    /// Whether every frame is encrypted with a key that only this process holds
    /// (`serve --spill encrypted`).
    pub encrypted: bool,
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
    encrypted: bool,
    reserved: AtomicU64,
    files: AtomicU64,
    queries: AtomicU64,
    written: AtomicU64,
    spilled_queries: AtomicU64,
    retries: AtomicU64,
    fault: Mutex<Option<(Fault, u64)>>,
    /// Whether a fault is injected, read before `fault` is locked.
    faults: std::sync::atomic::AtomicBool,
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

/// The failure of a write to a spill file: the `spill` budget when the file system has
/// no room for it (`ENOSPC`, `EDQUOT` or `EFBIG`), and an I/O error otherwise.
fn written(error: std::io::Error, limit: u64, bytes: u64) -> Error {
    failed(error, "write", limit, bytes)
}

/// The failure of `what` on a spill file: the `spill` budget when the file system has no
/// room or the quota is spent, with the cause logged since the budget error cannot carry
/// it, and an I/O error otherwise.
fn failed(error: std::io::Error, what: &str, limit: u64, bytes: u64) -> Error {
    #[cfg(unix)]
    let full = matches!(
        error.raw_os_error(),
        Some(libc::ENOSPC) | Some(libc::EDQUOT) | Some(libc::EFBIG)
    );
    #[cfg(not(unix))]
    let full = error.kind() == std::io::ErrorKind::StorageFull;
    if full {
        tracing::warn!(target: "sparkles::spill", "spill {what} failed: {error}");
        refused(limit, bytes)
    } else {
        io(error, what)
    }
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

/// Create the spill directory `dir` with mode 0700 where it does not exist, and check
/// that it is a directory this process's user owns and that no other user may write,
/// so that no one else can place or swap entries in it. A symbolic link is resolved
/// once here, and the directory it leads to must pass the same checks.
fn private_root(dir: &Path) -> Result<PathBuf> {
    let mut builder = std::fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    builder.create(dir).map_err(|e| io(e, "directory"))?;
    let root = dir.canonicalize().map_err(|e| io(e, "directory"))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        let meta = std::fs::metadata(&root).map_err(|e| io(e, "directory"))?;
        // SAFETY: geteuid has no preconditions and cannot fail.
        let uid = unsafe { libc::geteuid() };
        if !meta.is_dir() || meta.uid() != uid {
            return Err(Error::invalid(format!(
                "the spill directory {} must be a directory owned by user {uid}; give \
                 --spill-dir a new subdirectory, which the server creates with mode 0700",
                root.display()
            )));
        }
        if meta.mode() & 0o022 != 0 {
            return Err(Error::invalid(format!(
                "the spill directory {} must not be writable by other users (mode {:o}); \
                 run chmod go-w on it, or give --spill-dir a new subdirectory, which the \
                 server creates with mode 0700",
                root.display(),
                meta.mode() & 0o7777
            )));
        }
    }
    Ok(root)
}

impl Spill {
    /// Create this process's directory under `options.dir`, after removing the
    /// directories that dead processes left there.
    pub fn open(options: &SpillOptions) -> Result<Arc<Spill>> {
        Self::open_with(options, |_| {})
    }

    /// [`Spill::open`], calling `before_marker` with the marker's path before the marker
    /// is made, so that tests can make that step fail.
    fn open_with(options: &SpillOptions, before_marker: impl FnOnce(&Path)) -> Result<Arc<Spill>> {
        if !cfg!(unix) {
            return Err(Error::Unsupported(
                "disk spill needs a Unix file system".into(),
            ));
        }
        let root = private_root(&options.dir)?;
        sweep(&root);
        let random: u64 = rand::random();
        let name = format!("{}-{}-{random:016x}", boot_id(), std::process::id());
        let dir = root.join(name);
        let mut builder = std::fs::DirBuilder::new();
        #[cfg(unix)]
        {
            use std::os::unix::fs::DirBuilderExt;
            builder.mode(0o700);
        }
        builder
            .create(&dir)
            .map_err(|e| io(e, "process directory"))?;
        before_marker(&dir.join(MARKER));
        let marker = Self::mark(&dir).inspect_err(|_| {
            // A directory without a written marker would stay until a sweep finds it old.
            // It was made by this call with mode 0700, so nothing else is in it.
            let _ = std::fs::remove_dir_all(&dir);
        })?;
        Ok(Arc::new(Spill {
            dir,
            _marker: marker,
            max_bytes: options.max_bytes,
            query_bytes: options.query_bytes,
            min_free_bytes: options.min_free_bytes,
            encrypted: options.encrypted,
            reserved: AtomicU64::new(0),
            files: AtomicU64::new(0),
            queries: AtomicU64::new(0),
            written: AtomicU64::new(0),
            spilled_queries: AtomicU64::new(0),
            retries: AtomicU64::new(0),
            fault: Mutex::new(None),
            faults: std::sync::atomic::AtomicBool::new(false),
        }))
    }

    /// Create, lock and write the marker of the process directory `dir`.
    fn mark(dir: &Path) -> Result<File> {
        // The marker is locked before it names the format, so that a sweep by another
        // process starting at the same time never takes this directory for a dead one.
        let mut marker = {
            let mut options = std::fs::OpenOptions::new();
            options.write(true).create_new(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt;
                options.mode(0o600);
            }
            options
                .open(dir.join(MARKER))
                .map_err(|e| io(e, "marker"))?
        };
        if !try_lock(&marker) {
            return Err(Error::invalid(format!(
                "could not lock the spill marker in {}",
                dir.display()
            )));
        }
        let started = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_secs());
        std::io::Write::write_all(
            &mut marker,
            format!(
                "{FORMAT_NAME} {FORMAT_VERSION}\nstarted {started}\npid {}\n",
                std::process::id()
            )
            .as_bytes(),
        )
        .map_err(|e| io(e, "marker"))?;
        Ok(marker)
    }

    /// The directory of this process's spill files.
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// Whether spill files are encrypted.
    pub fn encrypted(&self) -> bool {
        self.encrypted
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

    /// How many eager SELECT queries exceeded their memory budget and ran again as
    /// cursors that may spill.
    pub fn eager_retries(&self) -> u64 {
        self.retries.load(Ordering::Relaxed)
    }

    /// Count an eager query that runs again as a cursor.
    pub fn count_eager_retry(&self) {
        self.retries.fetch_add(1, Ordering::Relaxed);
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
        self.faults.store(true, Ordering::Release);
    }

    fn faulted(&self, op: Fault) -> bool {
        if !self.faults.load(Ordering::Acquire) {
            return false;
        }
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

    /// Open an unnamed file in the process directory, with its number.
    fn create(&self, query: u64) -> Result<(File, u64)> {
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
                Ok(file) => return Ok((file, n)),
                // A file system without O_TMPFILE takes the named path below.
                Err(e)
                    if matches!(
                        e.raw_os_error(),
                        Some(libc::EOPNOTSUPP) | Some(libc::EISDIR) | Some(libc::EINVAL)
                    ) => {}
                Err(e) => return Err(failed(e, "file", self.max_bytes, self.reserved_bytes())),
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
        let file = options
            .open(&path)
            .map_err(|e| failed(e, "file", self.max_bytes, self.reserved_bytes()))?;
        std::fs::remove_file(&path).map_err(|e| io(e, "file"))?;
        Ok((file, n))
    }
}

impl Drop for Spill {
    fn drop(&mut self) {
        // Every file was unlinked when it was made, so only the marker is left.
        let _ = std::fs::remove_file(self.dir.join(MARKER));
        let _ = std::fs::remove_dir(&self.dir);
    }
}

/// How long a process directory may go without a written marker before a sweep takes
/// it for the remains of a start that died.
const UNWRITTEN_GRACE: std::time::Duration = std::time::Duration::from_secs(60);

/// Whether the directory `dir`, and its marker when there is one, were last changed more
/// than [`UNWRITTEN_GRACE`] ago.
fn unwritten_long_ago(dir: &Path, marker: Option<&Path>) -> bool {
    let old = |p: &Path| {
        std::fs::metadata(p)
            .and_then(|m| m.modified())
            .ok()
            .and_then(|t| t.elapsed().ok())
            .is_some_and(|age| age > UNWRITTEN_GRACE)
    };
    old(dir) && marker.is_none_or(old)
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
            tracing::warn!(target: "sparkles::spill", path = %path.display(), "leaving a foreign entry in the spill directory");
            continue;
        }
        let marker_path = path.join(MARKER);
        let ok = (|| {
            let marker = match std::fs::read_to_string(&marker_path) {
                Ok(text) if text.starts_with(&format!("{FORMAT_NAME} ")) => {
                    Some(File::open(&marker_path).ok()?)
                }
                // A start that died before it wrote its marker, long enough ago that no
                // process is still filling it in.
                Ok(text) if text.is_empty() && unwritten_long_ago(&path, Some(&marker_path)) => {
                    Some(File::open(&marker_path).ok()?)
                }
                Err(e)
                    if e.kind() == std::io::ErrorKind::NotFound
                        && unwritten_long_ago(&path, None) =>
                {
                    None
                }
                _ => return None,
            };
            if marker.as_ref().is_some_and(|m| !try_lock(m)) {
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
            if marker.is_some() {
                std::fs::remove_file(&marker_path).ok()?;
            }
            drop(marker);
            std::fs::remove_dir(&path).ok()
        })();
        if ok.is_some() {
            removed += 1;
        } else if path.exists() {
            tracing::warn!(target: "sparkles::spill", path = %path.display(), "leaving a spill directory that is in use or not a spill directory");
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

    /// Free `bytes` of the query's files, and give the server back the extents the
    /// files still open no longer need.
    fn release(&self, bytes: u64) {
        let mut state = self.state.lock();
        state.used = state.used.saturating_sub(bytes);
        let needed = state.used.div_ceil(EXTENT) * EXTENT;
        if state.reserved > needed {
            let spare = state.reserved - needed;
            state.reserved = needed;
            self.spill.reserved.fetch_sub(spare, Ordering::AcqRel);
        }
    }

    /// Start a spill file of `columns` columns.
    pub fn create(self: &Arc<Self>, kind: SpillKind, columns: usize) -> Result<SpillWriter> {
        let columns_u32 =
            u32::try_from(columns).map_err(|_| Error::invalid("too many spill columns"))?;
        self.reserve(HEADER_BYTES as u64)?;
        let key = if self.spill.encrypted {
            match FrameKey::generate() {
                Ok(key) => Some(Arc::new(key)),
                Err(e) => {
                    self.release(HEADER_BYTES as u64);
                    return Err(e);
                }
            }
        } else {
            None
        };
        let (file, number) = match self.spill.create(self.number) {
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
        header[12] = if key.is_some() { AES_256_GCM } else { PLAIN };
        header[16..20].copy_from_slice(&columns_u32.to_le_bytes());
        header[20..28].copy_from_slice(&number.to_le_bytes());
        let mut writer = SpillWriter {
            file: Some(file),
            query: self.clone(),
            kind,
            number,
            key,
            columns,
            rows: 0,
            frames: 0,
            max_frame_rows: 0,
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
    /// The file's number in the process, part of every frame's nonce.
    number: u64,
    /// The file's key when frames are encrypted.
    key: Option<Arc<FrameKey>>,
    columns: usize,
    rows: u64,
    frames: u64,
    /// The largest successfully written frame, retained for reader admission.
    max_frame_rows: usize,
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
        file.write_all(bytes)
            .map_err(|e| written(e, self.query.limit, self.bytes))
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
        let rows = range.len() as u32;
        let sealed = payload.len() + if self.key.is_some() { TAG_BYTES } else { 0 };
        let len =
            u32::try_from(sealed).map_err(|_| Error::invalid("a spill frame is too large"))?;
        let mut header = [0u8; FRAME_HEADER_BYTES];
        header[..4].copy_from_slice(&len.to_le_bytes());
        header[4..8].copy_from_slice(&rows.to_le_bytes());
        header[12..16].copy_from_slice(&0u32.to_le_bytes());
        if let Some(key) = &self.key {
            let aad = frame_aad(self.kind, self.columns, self.number, self.frames, &header);
            key.seal(self.number, self.frames, &aad, &mut payload)?;
        }
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
        self.max_frame_rows = self.max_frame_rows.max(rows as usize);
        Ok(())
    }

    /// Finish the file for reading.
    pub fn finish(mut self) -> Result<SpillRun> {
        let file = self.file.take().expect("an open spill file");
        Ok(SpillRun {
            file: Some(file),
            query: self.query.clone(),
            kind: self.kind,
            number: self.number,
            key: self.key.take(),
            columns: self.columns,
            rows: self.rows,
            frames: self.frames,
            max_frame_rows: self.max_frame_rows,
            bytes: std::mem::take(&mut self.bytes),
        })
    }
}

impl Drop for SpillWriter {
    fn drop(&mut self) {
        // Closing frees the disk, which must come before the extent is given back.
        drop(self.file.take());
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

/// The additional data a sealed frame authenticates: the file's kind, column count and
/// number, the frame's number, and the frame header without its CRC.
fn frame_aad(
    kind: SpillKind,
    columns: usize,
    number: u64,
    frame: u64,
    header: &[u8; FRAME_HEADER_BYTES],
) -> [u8; 33] {
    let mut aad = [0u8; 33];
    aad[0] = kind as u8;
    aad[1..5].copy_from_slice(&(columns as u32).to_le_bytes());
    aad[5..13].copy_from_slice(&number.to_le_bytes());
    aad[13..21].copy_from_slice(&frame.to_le_bytes());
    aad[21..29].copy_from_slice(&header[..8]);
    aad[29..33].copy_from_slice(&header[12..16]);
    aad
}

/// One spill file's AES-256-GCM key, made from the system's random source and held
/// only in memory. aws-lc-rs clears the key's bytes when it drops.
struct FrameKey(aws_lc_rs::aead::LessSafeKey);

impl FrameKey {
    fn generate() -> Result<Self> {
        use aws_lc_rs::aead::{AES_256_GCM, LessSafeKey, UnboundKey};
        let mut bytes = [0u8; 32];
        aws_lc_rs::rand::fill(&mut bytes)
            .map_err(|_| Error::invalid("spill encryption: no random key"))?;
        let key = UnboundKey::new(&AES_256_GCM, &bytes)
            .map_err(|_| Error::invalid("spill encryption: a bad key"));
        // The plain copy of the key is cleared before the key is used.
        for byte in &mut bytes {
            // SAFETY: a write through a valid reference to a byte of a local array.
            unsafe { std::ptr::write_volatile(byte, 0) };
        }
        Ok(Self(LessSafeKey::new(key?)))
    }

    /// The nonce of frame `frame` of file `file`: the file's number in the first four
    /// bytes and the frame's in the last eight. The key is the file's own, so the
    /// frame's number alone keeps nonces apart.
    fn nonce(file: u64, frame: u64) -> aws_lc_rs::aead::Nonce {
        let mut nonce = [0u8; 12];
        nonce[..4].copy_from_slice(&(file as u32).to_le_bytes());
        nonce[4..].copy_from_slice(&frame.to_le_bytes());
        aws_lc_rs::aead::Nonce::assume_unique_for_key(nonce)
    }

    /// Encrypt `payload` in place and append its tag.
    fn seal(&self, file: u64, frame: u64, aad: &[u8], payload: &mut Vec<u8>) -> Result<()> {
        self.0
            .seal_in_place_append_tag(
                Self::nonce(file, frame),
                aws_lc_rs::aead::Aad::from(aad),
                payload,
            )
            .map_err(|_| Error::invalid("spill encryption failed"))
    }

    /// Decrypt `payload` in place, which ends with its tag, and give the length of the
    /// plain text, which stays at its start.
    fn open(&self, file: u64, frame: u64, aad: &[u8], payload: &mut [u8]) -> Result<usize> {
        self.0
            .open_in_place(
                Self::nonce(file, frame),
                aws_lc_rs::aead::Aad::from(aad),
                payload,
            )
            .map(|plain| plain.len())
            .map_err(|_| Error::Corrupt("spill frame: it fails authentication".into()))
    }
}

/// A finished spill file. Dropping it closes the file, which frees its space.
pub struct SpillRun {
    /// Always open until the run drops.
    file: Option<File>,
    query: Arc<QuerySpill>,
    kind: SpillKind,
    number: u64,
    key: Option<Arc<FrameKey>>,
    columns: usize,
    rows: u64,
    frames: u64,
    max_frame_rows: usize,
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
    fn file(&self) -> &File {
        self.file.as_ref().expect("an open spill run")
    }

    pub fn rows(&self) -> u64 {
        self.rows
    }

    pub fn bytes(&self) -> u64 {
        self.bytes
    }

    pub fn frames(&self) -> u64 {
        self.frames
    }

    /// The largest frame actually written, independent of the writer's last limit.
    pub(crate) fn max_frame_rows(&self) -> usize {
        self.max_frame_rows
    }

    /// A reader from the first frame. Several readers of one file are independent.
    pub fn reader(self: &Arc<Self>) -> Result<SpillReader> {
        let mut header = [0u8; HEADER_BYTES];
        read_at(self.file(), &mut header, 0)?;
        if header[..8] != MAGIC {
            return Err(Error::Corrupt("spill file: not a spill file".into()));
        }
        let version = u16::from_le_bytes([header[8], header[9]]);
        if !crate::formats::SPILL.reads(u32::from(version)) {
            return Err(Error::Corrupt(format!(
                "spill file: format version {version}, this build reads {}",
                crate::formats::SPILL.readable()
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
        let expected = if self.key.is_some() {
            AES_256_GCM
        } else {
            PLAIN
        };
        if header[12] != expected {
            return Err(Error::Corrupt(format!(
                "spill file: encryption {}, the writer used {expected}",
                header[12]
            )));
        }
        let columns = u32::from_le_bytes(header[16..20].try_into().unwrap()) as usize;
        if columns != self.columns {
            return Err(Error::Corrupt("spill file: column count differs".into()));
        }
        if u64::from_le_bytes(header[20..28].try_into().unwrap()) != self.number {
            return Err(Error::Corrupt("spill file: file number differs".into()));
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
        // Closing frees the disk, which must come before the extent is given back.
        drop(self.file.take());
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
        read_at(self.run.file(), &mut header, self.offset)?;
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
            self.run.file(),
            &mut self.payload,
            self.offset + FRAME_HEADER_BYTES as u64,
        )?;
        if self.run.query.spill.faulted(Fault::Read) {
            self.payload[0] ^= 0xff;
        }
        if frame_crc(&header, &self.payload) != crc {
            return Err(Error::Corrupt("spill frame: CRC mismatch".into()));
        }
        let len = if let Some(key) = &self.run.key {
            let aad = frame_aad(
                self.run.kind,
                self.run.columns,
                self.run.number,
                self.frames,
                &header,
            );
            key.open(self.run.number, self.frames, &aad, &mut self.payload)?
        } else {
            len
        };
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
        self.offset += (FRAME_HEADER_BYTES + self.payload.len()) as u64;
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
            encrypted: false,
        })
        .unwrap()
    }

    fn encrypted_spill(dir: &Path) -> Arc<Spill> {
        Spill::open(&SpillOptions {
            dir: dir.to_path_buf(),
            max_bytes: 1 << 30,
            query_bytes: 1 << 30,
            min_free_bytes: 0,
            encrypted: true,
        })
        .unwrap()
    }

    /// Every frame of `run`: its offset and its bytes, header included.
    fn frames_of(run: &SpillRun) -> Vec<(u64, Vec<u8>)> {
        use std::os::unix::fs::FileExt;
        let mut frames = Vec::new();
        let mut at = HEADER_BYTES as u64;
        while at < run.bytes {
            let mut header = [0u8; FRAME_HEADER_BYTES];
            run.file().read_exact_at(&mut header, at).unwrap();
            let len = u32::from_le_bytes(header[..4].try_into().unwrap()) as usize;
            let mut bytes = vec![0u8; FRAME_HEADER_BYTES + len];
            run.file().read_exact_at(&mut bytes, at).unwrap();
            frames.push((at, bytes));
            at += (FRAME_HEADER_BYTES + len) as u64;
        }
        frames
    }

    fn read_all(run: &Arc<SpillRun>) -> Result<Vec<Id>> {
        let mut reader = run.reader()?;
        let mut all = Vec::new();
        while let Some((columns, _)) = reader.next_frame()? {
            all.extend_from_slice(&columns[0]);
        }
        Ok(all)
    }

    #[test]
    fn encrypted_frames_round_trip_and_hide_the_plain_text() {
        let dir = tempfile::tempdir().unwrap();
        let plain = spill(dir.path(), 1 << 30, 1 << 30);
        let sealed = encrypted_spill(dir.path());
        assert!(sealed.encrypted() && !plain.encrypted());
        let column: Vec<Id> = (0..9000u64)
            .map(|i| Id(0x4142_4344_4546_0000 + i * 977))
            .collect();
        let write = |spill: &Arc<Spill>| {
            let query = Arc::new(spill.query(None));
            let mut writer = query.create(SpillKind::JoinPartition, 1).unwrap();
            writer.write(&[&column], column.len()).unwrap();
            Arc::new(writer.finish().unwrap())
        };
        let (a, b) = (write(&plain), write(&sealed));
        assert_eq!(read_all(&a).unwrap(), column);
        assert_eq!(read_all(&b).unwrap(), column);
        // Each sealed frame is the plain frame and a tag.
        assert_eq!(b.bytes, a.bytes + TAG_BYTES as u64 * a.frames);
        // No 16 bytes of a plain payload appear in the sealed file.
        let sealed_bytes: Vec<u8> = frames_of(&b).into_iter().flat_map(|(_, f)| f).collect();
        for (_, frame) in frames_of(&a) {
            for window in frame[FRAME_HEADER_BYTES..]
                .as_chunks::<16>()
                .0
                .iter()
                .take(64)
            {
                assert!(
                    !sealed_bytes.windows(16).any(|w| w == window),
                    "plain bytes in a sealed file"
                );
            }
        }
    }

    #[test]
    fn tampered_reordered_moved_or_truncated_encrypted_frames_are_refused() {
        use std::os::unix::fs::FileExt;
        let dir = tempfile::tempdir().unwrap();
        let spill = encrypted_spill(dir.path());
        let query = Arc::new(spill.query(None));
        // Frames of equal length, so that swapping two keeps the file's layout.
        let column: Vec<Id> = vec![Id(42); 3 * FRAME_ROWS];
        let make = || {
            let mut writer = query.create(SpillKind::SortRun, 1).unwrap();
            writer.write(&[&column], column.len()).unwrap();
            Arc::new(writer.finish().unwrap())
        };
        let corrupt = |run: &Arc<SpillRun>| {
            let error = read_all(run).unwrap_err();
            assert!(matches!(error, Error::Corrupt(_)), "{error}");
            error.to_string()
        };
        // A changed byte of the sealed payload, with its CRC made right again.
        let run = make();
        let frames = frames_of(&run);
        let (at, mut frame) = frames[1].clone();
        frame[FRAME_HEADER_BYTES + 3] ^= 0x01;
        let header: [u8; FRAME_HEADER_BYTES] = frame[..FRAME_HEADER_BYTES].try_into().unwrap();
        let crc = frame_crc(&header, &frame[FRAME_HEADER_BYTES..]);
        frame[8..12].copy_from_slice(&crc.to_le_bytes());
        run.file().write_all_at(&frame, at).unwrap();
        assert!(corrupt(&run).contains("authentication"));
        // Two frames swapped.
        let run = make();
        let frames = frames_of(&run);
        assert_eq!(frames[0].1.len(), frames[2].1.len());
        run.file().write_all_at(&frames[2].1, frames[0].0).unwrap();
        run.file().write_all_at(&frames[0].1, frames[2].0).unwrap();
        assert!(corrupt(&run).contains("authentication"));
        // A frame copied from another file of the same query, at the same place.
        let (one, two) = (make(), make());
        let theirs = frames_of(&two);
        one.file().write_all_at(&theirs[1].1, theirs[1].0).unwrap();
        assert!(corrupt(&one).contains("authentication"));
        // The last frame cut off.
        let run = make();
        let frames = frames_of(&run);
        run.file().set_len(frames[2].0).unwrap();
        assert!(corrupt(&run).contains("truncated"));
        // A plain reader's encryption byte in an encrypted file.
        let run = make();
        run.file().write_all_at(&[PLAIN], 12).unwrap();
        assert!(corrupt(&run).contains("encryption"));
        assert_eq!(read_all(&make()).unwrap(), column);
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
        assert_eq!(run.max_frame_rows(), FRAME_ROWS);
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
        run.file().write_all_at(&9u16.to_le_bytes(), 8).unwrap();
        assert!(matches!(run.reader(), Err(Error::Corrupt(m)) if m.contains("version 9")));
        run.file()
            .write_all_at(&FORMAT_VERSION.to_le_bytes(), 8)
            .unwrap();
        // An unknown operator kind, codec or encryption flag.
        for (at, byte) in [(10u64, 99u8), (11, 9), (12, 1)] {
            let mut was = [0u8];
            run.file().read_exact_at(&mut was, at).unwrap();
            run.file().write_all_at(&[byte], at).unwrap();
            assert!(matches!(run.reader(), Err(Error::Corrupt(_))), "byte {at}");
            run.file().write_all_at(&was, at).unwrap();
        }
        assert!(run.reader().is_ok());
        // A flipped payload byte.
        run.file()
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
            encrypted: false,
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

    #[test]
    fn a_query_returns_extents_once_its_files_close() {
        let dir = tempfile::tempdir().unwrap();
        let spill = spill(dir.path(), 1 << 30, 1 << 30);
        let query = Arc::new(spill.query(None));
        let column = ids(4096, 1 << 50);
        let mut writer = query.create(SpillKind::SortRun, 1).unwrap();
        writer.write(&[&column], 4096).unwrap();
        let run = writer.finish().unwrap();
        assert_eq!(spill.reserved_bytes(), EXTENT);
        drop(run);
        // The query is still running, but holds no file.
        assert_eq!(spill.reserved_bytes(), 0);
        let mut writer = query.create(SpillKind::SortRun, 1).unwrap();
        writer.write(&[&column], 4096).unwrap();
        assert_eq!(spill.reserved_bytes(), EXTENT);
        drop(writer);
        assert_eq!(spill.reserved_bytes(), 0);
        drop(query);
        assert_eq!(spill.reserved_bytes(), 0);
    }

    #[test]
    fn a_full_disk_or_quota_fails_with_the_spill_budget() {
        for errno in [libc::ENOSPC, libc::EDQUOT, libc::EFBIG] {
            let error = written(std::io::Error::from_raw_os_error(errno), 64, 32);
            assert!(
                matches!(&error, Error::BudgetExceeded(b) if b.kind == BudgetKind::Spill),
                "{errno}: {error}"
            );
        }
        let error = written(std::io::Error::from_raw_os_error(libc::EIO), 64, 32);
        assert!(matches!(error, Error::Io(_)), "{error}");
    }

    #[test]
    fn the_spill_root_must_be_private_to_the_server() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let shared = dir.path().join("shared");
        std::fs::create_dir(&shared).unwrap();
        std::fs::set_permissions(&shared, std::fs::Permissions::from_mode(0o777)).unwrap();
        let open = |root: &Path| {
            Spill::open(&SpillOptions {
                dir: root.to_path_buf(),
                max_bytes: 1 << 30,
                query_bytes: 1 << 30,
                min_free_bytes: 0,
                encrypted: false,
            })
        };
        let error = open(&shared).unwrap_err();
        assert!(error.to_string().contains("writable"), "{error}");
        // A root it creates is private.
        let private = dir.path().join("private");
        drop(open(&private).unwrap());
        let mode = std::fs::metadata(&private).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o700);
        // A link to a private directory opens that directory.
        let link = dir.path().join("link");
        std::os::unix::fs::symlink(&shared, &link).unwrap();
        assert!(open(&link).is_err());
        std::fs::remove_file(&link).unwrap();
        std::os::unix::fs::symlink(&private, &link).unwrap();
        let spill = open(&link).unwrap();
        assert!(spill.dir().starts_with(private.canonicalize().unwrap()));
    }

    #[test]
    fn processes_starting_together_never_sweep_each_other() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().to_path_buf();
        let threads: Vec<_> = (0..8)
            .map(|_| {
                let root = root.clone();
                std::thread::spawn(move || {
                    for _ in 0..400 {
                        let spill = spill(&root, 1 << 30, 1 << 30);
                        let query = Arc::new(spill.query(None));
                        let mut writer = query.create(SpillKind::SortRun, 1).unwrap();
                        writer.write(&[&ids(10, 1)], 10).unwrap();
                        drop(writer.finish().unwrap());
                        assert!(spill.dir().join(MARKER).exists());
                    }
                })
            })
            .collect();
        for t in threads {
            t.join().unwrap();
        }
        assert_eq!(std::fs::read_dir(&root).unwrap().count(), 0);
    }

    #[test]
    fn sweep_removes_old_directories_whose_marker_was_never_written() {
        let dir = tempfile::tempdir().unwrap();
        let name = |r: &str| format!("{}-1-{r}", "0".repeat(32));
        // No marker, and an empty marker, both from a start that died long ago.
        let bare = dir.path().join(name(&"a".repeat(16)));
        std::fs::create_dir(&bare).unwrap();
        let empty = dir.path().join(name(&"b".repeat(16)));
        std::fs::create_dir(&empty).unwrap();
        std::fs::write(empty.join(MARKER), b"").unwrap();
        // The same, just made, which a process may still be filling in.
        let fresh = dir.path().join(name(&"c".repeat(16)));
        std::fs::create_dir(&fresh).unwrap();
        std::fs::write(fresh.join(MARKER), b"").unwrap();
        let old = std::time::SystemTime::now() - std::time::Duration::from_secs(3600);
        for d in [&bare, &empty] {
            File::open(d).unwrap().set_modified(old).unwrap();
        }
        File::options()
            .write(true)
            .open(empty.join(MARKER))
            .unwrap()
            .set_modified(old)
            .unwrap();
        assert_eq!(sweep(dir.path()), 2);
        assert!(!bare.exists() && !empty.exists());
        assert!(fresh.exists());
    }

    #[test]
    fn a_failed_open_leaves_no_process_directory() {
        let dir = tempfile::tempdir().unwrap();
        // A directory where the marker would go makes its creation fail.
        let error = Spill::open_with(
            &SpillOptions {
                dir: dir.path().to_path_buf(),
                max_bytes: 1 << 30,
                query_bytes: 1 << 30,
                min_free_bytes: 0,
                encrypted: false,
            },
            |marker: &Path| std::fs::create_dir(marker).unwrap(),
        )
        .unwrap_err();
        assert!(error.to_string().contains("marker"), "{error}");
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 0);
    }
}
