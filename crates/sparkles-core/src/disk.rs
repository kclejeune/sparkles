//! Free disk space, for the reserve a store keeps free
//! ([`StoreOptions::min_free_disk_bytes`](crate::store::StoreOptions::min_free_disk_bytes)).

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

/// Free space for an unprivileged user on the file system of `dir`.
#[cfg(unix)]
pub fn free_bytes(dir: &Path) -> std::io::Result<u64> {
    use std::os::unix::ffi::OsStrExt;
    #[cfg(test)]
    if let Some(free) = fake::free(dir) {
        return Ok(free);
    }
    let path = std::ffi::CString::new(dir.as_os_str().as_bytes())?;
    let mut s = std::mem::MaybeUninit::<libc::statvfs>::uninit();
    // SAFETY: `path` is NUL-terminated and `s` is valid for writes; statvfs initializes
    // it when it returns 0
    if unsafe { libc::statvfs(path.as_ptr(), s.as_mut_ptr()) } != 0 {
        return Err(std::io::Error::last_os_error());
    }
    // SAFETY: initialized by the successful call above
    let s = unsafe { s.assume_init() };
    #[allow(clippy::unnecessary_cast)] // the field types differ between platforms
    Ok((s.f_bavail as u64).saturating_mul(s.f_frsize as u64))
}

/// Free space is not measured on this platform: never short.
#[cfg(not(unix))]
pub fn free_bytes(_dir: &Path) -> std::io::Result<u64> {
    #[cfg(test)]
    if let Some(free) = fake::free(_dir) {
        return Ok(free);
    }
    Ok(u64::MAX)
}

/// The name of the file system that holds `dir` when it is one whose files live in
/// memory (`tmpfs`, `ramfs`) or on another machine (`nfs`, `fuse`, `9p`, `smb`, `cifs`),
/// and `None` for any other. Only Linux reports it.
#[cfg(target_os = "linux")]
pub fn special_file_system(dir: &Path) -> std::io::Result<Option<&'static str>> {
    use std::os::unix::ffi::OsStrExt;
    let path = std::ffi::CString::new(dir.as_os_str().as_bytes())?;
    let mut s = std::mem::MaybeUninit::<libc::statfs>::uninit();
    // SAFETY: `path` is NUL-terminated and `s` is valid for writes; statfs initializes it
    // when it returns 0
    if unsafe { libc::statfs(path.as_ptr(), s.as_mut_ptr()) } != 0 {
        return Err(std::io::Error::last_os_error());
    }
    // SAFETY: initialized by the successful call above
    let s = unsafe { s.assume_init() };
    #[allow(clippy::unnecessary_cast)] // the field type differs between platforms
    Ok(match (s.f_type as u64) & 0xffff_ffff {
        0x0102_1994 => Some("tmpfs"),
        0x8584_58f6 => Some("ramfs"),
        0x6969 => Some("nfs"),
        0x6573_5546 => Some("fuse"),
        0x0102_1997 => Some("9p"),
        0x517b | 0xfe53_4d42 => Some("smb"),
        0xff53_4d42 => Some("cifs"),
        _ => None,
    })
}

/// File systems are not told apart on this platform.
#[cfg(not(target_os = "linux"))]
pub fn special_file_system(_: &Path) -> std::io::Result<Option<&'static str>> {
    Ok(None)
}

/// Whether [`special_file_system`] names a file system that keeps its files in memory.
pub fn in_memory_file_system(name: &str) -> bool {
    matches!(name, "tmpfs" | "ramfs")
}

type FreeSpaceSamples = HashMap<PathBuf, (Instant, u64)>;

struct FreeSpaceCache {
    epoch: u64,
    samples: Option<FreeSpaceSamples>,
}

static FREE_SPACE: parking_lot::Mutex<FreeSpaceCache> = parking_lot::Mutex::new(FreeSpaceCache {
    epoch: 0,
    samples: None,
});

/// Discard the observation made before a replay step allocated disk space.
pub(crate) fn invalidate_free_bytes(dir: &Path) {
    let mut cache = FREE_SPACE.lock();
    cache.epoch = cache.epoch.wrapping_add(1);
    if let Some(samples) = &mut cache.samples {
        samples.remove(dir);
    }
}

/// [`free_bytes`], measured at most once per `max_age` for each directory (small
/// commits check it under the writer lock, many times a second).
pub fn free_bytes_cached(dir: &Path, max_age: Duration) -> std::io::Result<u64> {
    #[cfg(test)]
    let max_age = match fake::disk(dir) {
        Some(disk) if disk.cached => Duration::MAX,
        Some(disk) => return Ok(disk.free()),
        None => max_age,
    };
    free_bytes_cached_with(dir, max_age, free_bytes)
}

fn free_bytes_cached_with(
    dir: &Path,
    max_age: Duration,
    measure: impl FnOnce(&Path) -> std::io::Result<u64>,
) -> std::io::Result<u64> {
    let now = Instant::now();
    let cache = FREE_SPACE.lock();
    if let Some((at, free)) = cache.samples.as_ref().and_then(|samples| samples.get(dir))
        && now.duration_since(*at) < max_age
    {
        return Ok(*free);
    }
    let epoch = cache.epoch;
    drop(cache);

    let free = measure(dir)?;
    let mut cache = FREE_SPACE.lock();
    // An invalidation fences off measurements already in flight.
    if cache.epoch == epoch {
        let samples = cache.samples.get_or_insert_with(HashMap::new);
        if samples.len() > 1024 {
            samples.clear();
        }
        samples.insert(dir.to_path_buf(), (now, free));
    }
    Ok(free)
}

/// [`Error::StorageFull`](crate::Error::StorageFull) when writing `need` more bytes in
/// `dir` would leave less than `reserve` free.
pub fn check_reserve(dir: &Path, reserve: u64, need: u64, cached: bool) -> crate::Result<()> {
    let free = if cached {
        free_bytes_cached(dir, Duration::from_secs(1))?
    } else {
        free_bytes(dir)?
    };
    if free < reserve.saturating_add(need) {
        let h = crate::error::human_bytes;
        return Err(crate::Error::StorageFull(format!(
            "not enough free disk space: {} free, {} must stay free{}",
            h(free),
            h(reserve),
            if need > 0 {
                format!(" after writing {}", h(need))
            } else {
                String::new()
            }
        )));
    }
    Ok(())
}

/// Make sure the process may hold `need` open files: when the soft limit is lower, raise
/// it, up to the hard limit. Many systems start processes with a soft limit of 1,024
/// while the hard limit is far higher, and a bulk load of a billion triples merges more
/// files than that at once. The error says how to raise the hard limit.
#[cfg(unix)]
#[allow(clippy::unnecessary_cast)] // rlim_t is not u64 on every unix
pub fn ensure_open_files(need: u64) -> crate::Result<()> {
    let mut lim = libc::rlimit {
        rlim_cur: 0,
        rlim_max: 0,
    };
    // SAFETY: getrlimit and setrlimit only read and write the struct passed to them
    if unsafe { libc::getrlimit(libc::RLIMIT_NOFILE, &mut lim) } != 0 {
        return Ok(());
    }
    let (soft, hard) = (lim.rlim_cur as u64, lim.rlim_max as u64);
    if soft >= need {
        return Ok(());
    }
    if hard >= need {
        lim.rlim_cur = need.max(soft) as libc::rlim_t;
        if unsafe { libc::setrlimit(libc::RLIMIT_NOFILE, &lim) } == 0 {
            return Ok(());
        }
    }
    Err(crate::Error::invalid(format!(
        "this needs {need} open files, and the limit is {soft} (hard limit {hard}): raise it \
         with `ulimit -n {need}` or LimitNOFILE= in the service"
    )))
}

#[cfg(not(unix))]
pub fn ensure_open_files(_: u64) -> crate::Result<()> {
    Ok(())
}

/// Raise the soft limit on open files to the hard limit, as servers and loaders that
/// hold many files do. Returns the new soft limit.
#[cfg(unix)]
#[allow(clippy::unnecessary_cast)]
pub fn raise_open_file_limit() -> Option<u64> {
    let mut lim = libc::rlimit {
        rlim_cur: 0,
        rlim_max: 0,
    };
    // SAFETY: as in ensure_open_files
    unsafe {
        if libc::getrlimit(libc::RLIMIT_NOFILE, &mut lim) != 0 {
            return None;
        }
        if lim.rlim_cur < lim.rlim_max {
            let want = libc::rlimit {
                rlim_cur: lim.rlim_max,
                rlim_max: lim.rlim_max,
            };
            if libc::setrlimit(libc::RLIMIT_NOFILE, &want) == 0 {
                return Some(want.rlim_cur as u64);
            }
        }
    }
    Some(lim.rlim_cur as u64)
}

#[cfg(not(unix))]
pub fn raise_open_file_limit() -> Option<u64> {
    None
}

/// A [`WritebackFile`] starts writeback of its data every this many bytes.
#[cfg(target_os = "linux")]
const WRITEBACK_EVERY: u64 = 8 << 20;
/// A [`WritebackFile`] waits until the data this many bytes behind the end has reached
/// the device before it writes on.
#[cfg(target_os = "linux")]
const WRITEBACK_BEHIND: u64 = 64 << 20;

/// A file written front to back that sends its data to the device as it grows.
///
/// Linux keeps written data in dirty pages until a share of memory is dirty and then
/// throttles every thread that writes, whatever file it writes to. A build that writes
/// a large index in a few streams fills that share quickly and is then held in bursts.
/// This file starts writeback every 8 MiB as soon as those bytes are
/// written (`sync_file_range`), and waits for the data 64 MiB
/// back, so each file keeps a bounded amount of dirty data and its final `fsync` has
/// little left to do. Elsewhere it is a plain file.
pub struct WritebackFile {
    f: std::fs::File,
    written: u64,
    /// the end of the data whose writeback was started
    #[cfg(target_os = "linux")]
    started: u64,
}

impl WritebackFile {
    pub fn new(f: std::fs::File) -> WritebackFile {
        WritebackFile {
            f,
            written: 0,
            #[cfg(target_os = "linux")]
            started: 0,
        }
    }

    pub fn get_ref(&self) -> &std::fs::File {
        &self.f
    }

    #[cfg(target_os = "linux")]
    fn writeback(&mut self) {
        use std::os::fd::AsRawFd;
        if self.written - self.started < WRITEBACK_EVERY {
            return;
        }
        let fd = self.f.as_raw_fd();
        // SAFETY: sync_file_range only reads the open descriptor; a failure (a file
        // system without it) leaves the data to the kernel's own writeback
        unsafe {
            libc::sync_file_range(
                fd,
                self.started as libc::off64_t,
                (self.written - self.started) as libc::off64_t,
                libc::SYNC_FILE_RANGE_WRITE,
            );
            // a length of 0 would mean the whole file
            if let Some(end) = self.started.checked_sub(WRITEBACK_BEHIND)
                && end > 0
            {
                libc::sync_file_range(
                    fd,
                    0,
                    end as libc::off64_t,
                    libc::SYNC_FILE_RANGE_WAIT_BEFORE,
                );
            }
        }
        self.started = self.written;
    }

    #[cfg(not(target_os = "linux"))]
    fn writeback(&mut self) {}
}

impl std::io::Write for WritebackFile {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        let n = self.f.write(buf)?;
        self.written += n as u64;
        self.writeback();
        Ok(n)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.f.flush()
    }
}

#[cfg(test)]
mod free_space_tests {
    use super::*;
    use std::sync::Barrier;

    #[test]
    fn invalidation_prevents_an_in_flight_sample_from_being_cached() {
        const CAPACITY: u64 = 1024;
        let dir = tempfile::tempdir().unwrap();
        let dir = dir.path();
        let _disk = fake::install_cached(dir, CAPACITY);
        let measured = Barrier::new(2);
        let invalidated = Barrier::new(2);

        let (old_free, write) = std::thread::scope(|scope| {
            let check = scope.spawn(|| {
                free_bytes_cached_with(dir, Duration::MAX, |dir| {
                    let free = free_bytes(dir);
                    measured.wait();
                    invalidated.wait();
                    free
                })
            });
            measured.wait();
            let write = std::fs::write(dir.join("growth"), b"x");
            invalidate_free_bytes(dir);
            invalidated.wait();
            (check.join().unwrap().unwrap(), write)
        });

        write.unwrap();
        assert_eq!(old_free, CAPACITY);
        assert_eq!(free_bytes_cached(dir, Duration::MAX).unwrap(), CAPACITY - 1);
    }
}

#[cfg(all(test, unix))]
mod open_file_tests {
    use super::*;

    #[test]
    #[allow(clippy::unnecessary_cast)]
    fn the_soft_limit_rises_up_to_the_hard_limit() {
        let mut lim = libc::rlimit {
            rlim_cur: 0,
            rlim_max: 0,
        };
        assert_eq!(unsafe { libc::getrlimit(libc::RLIMIT_NOFILE, &mut lim) }, 0);
        let (soft, hard) = (lim.rlim_cur as u64, lim.rlim_max as u64);
        ensure_open_files(soft).unwrap();
        if hard < u64::MAX / 2 {
            let e = ensure_open_files(hard + 1).unwrap_err().to_string();
            assert!(e.contains("ulimit -n"), "{e}");
        }
        assert!(raise_open_file_limit().unwrap() >= soft);
    }
}

/// A small disk for tests: the free space under a directory is a capacity less what the
/// directory holds, measured at every call.
#[cfg(test)]
pub(crate) mod fake {
    use std::path::{Path, PathBuf};

    static DISKS: parking_lot::Mutex<Vec<FakeDisk>> = parking_lot::Mutex::new(Vec::new());

    #[derive(Clone)]
    pub(super) struct FakeDisk {
        root: PathBuf,
        capacity: u64,
        pub(super) cached: bool,
    }

    impl FakeDisk {
        pub(super) fn free(&self) -> u64 {
            self.capacity
                .saturating_sub(crate::store::dir_size(&self.root))
        }
    }

    /// Removes its disk when dropped.
    pub(crate) struct Disk(PathBuf, bool);

    impl Drop for Disk {
        fn drop(&mut self) {
            DISKS.lock().retain(|disk| disk.root != self.0);
            if self.1 {
                super::invalidate_free_bytes(&self.0);
            }
        }
    }

    /// Make `root` a disk of `capacity` bytes.
    pub(crate) fn install(root: &Path, capacity: u64) -> Disk {
        install_with_cache(root, capacity, false)
    }

    /// Make a sample stay warm until it is invalidated, regardless of test timing.
    pub(crate) fn install_cached(root: &Path, capacity: u64) -> Disk {
        super::invalidate_free_bytes(root);
        install_with_cache(root, capacity, true)
    }

    fn install_with_cache(root: &Path, capacity: u64, cached: bool) -> Disk {
        DISKS.lock().push(FakeDisk {
            root: root.to_path_buf(),
            capacity,
            cached,
        });
        Disk(root.to_path_buf(), cached)
    }

    pub(super) fn disk(dir: &Path) -> Option<FakeDisk> {
        DISKS
            .lock()
            .iter()
            .find(|disk| dir.starts_with(&disk.root))
            .cloned()
    }

    pub(super) fn free(dir: &Path) -> Option<u64> {
        disk(dir).map(|disk| disk.free())
    }
}
