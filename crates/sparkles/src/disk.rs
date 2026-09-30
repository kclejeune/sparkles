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
