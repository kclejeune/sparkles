//! Free disk space, for the reserve a store keeps free
//! ([`StoreOptions::min_free_disk_bytes`](crate::store::StoreOptions::min_free_disk_bytes)).

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

/// Free space for an unprivileged user on the file system of `dir`.
#[cfg(unix)]
pub fn free_bytes(dir: &Path) -> std::io::Result<u64> {
    use std::os::unix::ffi::OsStrExt;
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
pub fn free_bytes(_: &Path) -> std::io::Result<u64> {
    Ok(u64::MAX)
}

/// [`free_bytes`], measured at most once per `max_age` for each directory (small
/// commits check it under the writer lock, many times a second).
pub fn free_bytes_cached(dir: &Path, max_age: Duration) -> std::io::Result<u64> {
    static CACHE: parking_lot::Mutex<Option<HashMap<PathBuf, (Instant, u64)>>> =
        parking_lot::Mutex::new(None);
    let now = Instant::now();
    if let Some((at, free)) = CACHE.lock().as_ref().and_then(|c| c.get(dir).copied())
        && now.duration_since(at) < max_age
    {
        return Ok(free);
    }
    let free = free_bytes(dir)?;
    let mut cache = CACHE.lock();
    let cache = cache.get_or_insert_with(HashMap::new);
    if cache.len() > 1024 {
        cache.clear();
    }
    cache.insert(dir.to_path_buf(), (now, free));
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

/// Whether files written through [`WritebackFile`] send their data to the device as
/// they grow (on by default, `off` in `SPARKLES_WRITEBACK` turns it off for a process).
pub fn steady_writeback() -> bool {
    STEADY_WRITEBACK.get()
}

/// Turn [`steady_writeback`] on or off for writes from now on.
pub fn set_steady_writeback(on: bool) {
    STEADY_WRITEBACK.set(on);
}

static STEADY_WRITEBACK: crate::index::EnvSwitch =
    crate::index::EnvSwitch::new("SPARKLES_WRITEBACK");

/// A [`WritebackFile`] starts writeback of its data every this many bytes.
const WRITEBACK_EVERY: u64 = 8 << 20;
/// A [`WritebackFile`] waits until the data this many bytes behind the end has reached
/// the device before it writes on.
const WRITEBACK_BEHIND: u64 = 64 << 20;

/// A file written front to back that sends its data to the device as it grows.
///
/// Linux keeps written data in dirty pages until a share of memory is dirty and then
/// throttles every thread that writes, whatever file it writes to. A build that writes
/// a large index in a few streams fills that share quickly and is then held in bursts.
/// This file starts writeback of every [`WRITEBACK_EVERY`] bytes as soon as they are
/// written (`sync_file_range`), and waits for the data [`WRITEBACK_BEHIND`] bytes
/// back, so each file keeps a bounded amount of dirty data and its final `fsync` has
/// little left to do. Elsewhere it is a plain file.
pub struct WritebackFile {
    f: std::fs::File,
    written: u64,
    /// the end of the data whose writeback was started
    started: u64,
}

impl WritebackFile {
    pub fn new(f: std::fs::File) -> WritebackFile {
        WritebackFile {
            f,
            written: 0,
            started: 0,
        }
    }

    pub fn get_ref(&self) -> &std::fs::File {
        &self.f
    }

    #[cfg(target_os = "linux")]
    fn writeback(&mut self) {
        use std::os::fd::AsRawFd;
        if self.written - self.started < WRITEBACK_EVERY || !steady_writeback() {
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
