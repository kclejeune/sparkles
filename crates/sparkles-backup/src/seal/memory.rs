//! Long-lived keys occupy dedicated pages. A failed lock/dump-exclusion request
//! fails key construction rather than silently claiming protected memory. macOS
//! requires an explicitly established process-wide hard core-dump limit of zero.
use crate::{BackupError, Code, Result};
use zeroize::{Zeroize, Zeroizing};

pub(crate) struct LockedKey {
    #[cfg(any(target_os = "linux", target_os = "android", target_os = "macos"))]
    page: std::ptr::NonNull<u8>,
    #[cfg(any(target_os = "linux", target_os = "android", target_os = "macos"))]
    length: usize,
    #[cfg(any(target_os = "linux", target_os = "android", target_os = "macos"))]
    used: usize,
}
// The mapping is immutable after construction and remains owned until the last
// shared reference drops. Drop has exclusive access and zeroes before unmapping.
#[cfg(any(target_os = "linux", target_os = "android", target_os = "macos"))]
unsafe impl Send for LockedKey {}
#[cfg(any(target_os = "linux", target_os = "android", target_os = "macos"))]
unsafe impl Sync for LockedKey {}
impl LockedKey {
    pub fn new(mut bytes: Zeroizing<[u8; 32]>) -> Result<Self> {
        Self::allocate(bytes.as_mut())
    }
    pub fn from_bytes(mut bytes: Zeroizing<Vec<u8>>) -> Result<Self> {
        Self::allocate(bytes.as_mut())
    }
    fn allocate(bytes: &mut [u8]) -> Result<Self> {
        #[cfg(any(target_os = "linux", target_os = "android", target_os = "macos"))]
        {
            #[cfg(target_os = "macos")]
            if !core_dumps_disabled() {
                return Err(BackupError::new(
                    Code::InvalidConfig,
                    "macOS protected repository keys require zero soft and hard core-dump limits",
                ));
            }
            // SAFETY: anonymous private mapping; all failures unmap before returning.
            let length = unsafe { libc::sysconf(libc::_SC_PAGESIZE) };
            if length < 1 || bytes.is_empty() || bytes.len() > length as usize {
                return Err(unavailable());
            }
            let length = length as usize;
            let mapping = unsafe {
                libc::mmap(
                    std::ptr::null_mut(),
                    length,
                    libc::PROT_READ | libc::PROT_WRITE,
                    libc::MAP_PRIVATE | libc::MAP_ANONYMOUS,
                    -1,
                    0,
                )
            };
            if mapping == libc::MAP_FAILED {
                return Err(unavailable());
            }
            let locked = unsafe { libc::mlock(mapping, length) } == 0;
            #[cfg(any(target_os = "linux", target_os = "android"))]
            let excluded =
                locked && unsafe { libc::madvise(mapping, length, libc::MADV_DONTDUMP) } == 0;
            // Darwin's advice zeroes still-wired pages when the mapping is deleted.
            // Core exclusion comes from the verified process limits, not this advice.
            #[cfg(target_os = "macos")]
            let excluded = locked
                && unsafe { libc::madvise(mapping, length, libc::MADV_ZERO_WIRED_PAGES) } == 0;
            if !excluded {
                if locked {
                    unsafe { libc::munlock(mapping, length) };
                }
                unsafe { libc::munmap(mapping, length) };
                return Err(unavailable());
            }
            unsafe {
                std::ptr::copy_nonoverlapping(bytes.as_ptr(), mapping.cast::<u8>(), bytes.len())
            };
            let used = bytes.len();
            bytes.zeroize();
            Ok(Self {
                page: std::ptr::NonNull::new(mapping.cast()).expect("non-null mmap"),
                length,
                used,
            })
        }
        #[cfg(not(any(target_os = "linux", target_os = "android", target_os = "macos")))]
        {
            bytes.zeroize();
            Err(unavailable())
        }
    }
}
impl AsRef<[u8]> for LockedKey {
    fn as_ref(&self) -> &[u8] {
        #[cfg(any(target_os = "linux", target_os = "android", target_os = "macos"))]
        // SAFETY: construction allocated used initialized bytes; the mapping
        // lives for self and cannot be mutated through shared references.
        {
            unsafe { std::slice::from_raw_parts(self.page.as_ptr(), self.used) }
        }
        #[cfg(not(any(target_os = "linux", target_os = "android", target_os = "macos")))]
        {
            unreachable!("protected keys cannot be constructed on this platform")
        }
    }
}
impl Drop for LockedKey {
    fn drop(&mut self) {
        #[cfg(any(target_os = "linux", target_os = "android", target_os = "macos"))]
        // SAFETY: exclusive owner, dedicated mapping. Zeroize uses volatile writes;
        // neither munlock nor unmap can affect any other live key's page.
        unsafe {
            std::slice::from_raw_parts_mut(self.page.as_ptr(), self.length).zeroize();
            libc::munlock(self.page.as_ptr().cast(), self.length);
            libc::munmap(self.page.as_ptr().cast(), self.length);
        }
    }
}
#[cfg(target_os = "macos")]
pub(crate) fn core_dumps_disabled() -> bool {
    let mut limits = std::mem::MaybeUninit::<libc::rlimit>::uninit();
    // SAFETY: getrlimit initializes this structure on success, which is checked
    // before assume_init. A nonzero hard ceiling could allow cores to be re-enabled.
    if unsafe { libc::getrlimit(libc::RLIMIT_CORE, limits.as_mut_ptr()) } != 0 {
        return false;
    }
    let limits = unsafe { limits.assume_init() };
    limits.rlim_cur == 0 && limits.rlim_max == 0
}

fn unavailable() -> BackupError {
    BackupError::new(
        Code::InvalidConfig,
        "protected repository key memory is unavailable",
    )
}

#[cfg(all(test, target_os = "macos", feature = "encryption"))]
mod macos_tests {
    use super::*;

    fn limits() -> libc::rlimit {
        let mut limits = std::mem::MaybeUninit::<libc::rlimit>::uninit();
        // SAFETY: a successful getrlimit initializes the structure.
        assert_eq!(
            unsafe { libc::getrlimit(libc::RLIMIT_CORE, limits.as_mut_ptr()) },
            0
        );
        unsafe { limits.assume_init() }
    }

    // Process policy cannot be restored after lowering its hard limit. Each case
    // runs in a separate child, never in the shared test process.
    #[test]
    fn keys_require_explicit_process_preparation() {
        const MODE: &str = "SPARKLES_MACOS_KEY_MEMORY_TEST_MODE";
        if let Ok(mode) = std::env::var(MODE) {
            if mode == "unprepared" {
                let mut before = limits();
                before.rlim_cur = 0;
                assert_ne!(before.rlim_max, 0);
                // SAFETY: only this child changes its own soft limit.
                assert_eq!(unsafe { libc::setrlimit(libc::RLIMIT_CORE, &before) }, 0);
                assert!(!core_dumps_disabled());
                assert!(LockedKey::new(Zeroizing::new([3; 32])).is_err());
                assert_eq!(limits().rlim_max, before.rlim_max);
                return;
            }
            crate::crypto::prepare_key_memory_for_process().unwrap();
            let prepared = limits();
            assert_eq!(prepared.rlim_cur, 0);
            assert_eq!(prepared.rlim_max, 0);
            assert!(core_dumps_disabled());
            // Preparation is idempotent.
            crate::crypto::prepare_key_memory_for_process().unwrap();
            let first = LockedKey::new(Zeroizing::new([4; 32])).unwrap();
            let second = LockedKey::new(Zeroizing::new([5; 32])).unwrap();
            assert_ne!(first.as_ref().as_ptr(), second.as_ref().as_ptr());
            drop(first);
            assert_eq!(second.as_ref(), &[5; 32]);
            let raised = libc::rlimit {
                rlim_cur: 1,
                rlim_max: 0,
            };
            // SAFETY: this invalid request must fail; it cannot raise the hard ceiling.
            assert_ne!(unsafe { libc::setrlimit(libc::RLIMIT_CORE, &raised) }, 0);
            assert!(core_dumps_disabled());
            return;
        }
        let before = limits();
        let mut modes = vec!["prepared"];
        if before.rlim_max != 0 {
            modes.push("unprepared");
        }
        for mode in modes {
            let output = std::process::Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "seal::memory::macos_tests::keys_require_explicit_process_preparation",
                    "--nocapture",
                ])
                .env(MODE, mode)
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "{mode}: {} {}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
        }
        let after = limits();
        assert_eq!(before.rlim_cur, after.rlim_cur);
        assert_eq!(before.rlim_max, after.rlim_max);
    }
}

#[cfg(all(test, any(target_os = "linux", target_os = "android")))]
mod tests {
    use super::*;
    #[test]
    #[cfg(any(target_os = "linux", target_os = "android"))]
    fn keys_use_independent_pages_and_one_drop_does_not_unlock_another() {
        let a = LockedKey::new(Zeroizing::new([1; 32])).unwrap();
        let b = LockedKey::new(Zeroizing::new([2; 32])).unwrap();
        assert_ne!(a.page, b.page);
        drop(a);
        assert_eq!(b.as_ref(), &[2; 32]);
        let maps = std::fs::read_to_string("/proc/self/smaps").unwrap();
        let address = b.page.as_ptr() as usize;
        // Linux may merge adjacent locked anonymous mappings from parallel tests.
        // Find the range containing this page rather than requiring its start address.
        let lines = maps.lines().collect::<Vec<_>>();
        let header = |line: &str| -> Option<(usize, usize)> {
            let (start, end) = line.split_whitespace().next()?.split_once('-')?;
            Some((
                usize::from_str_radix(start, 16).ok()?,
                usize::from_str_radix(end, 16).ok()?,
            ))
        };
        let start = lines
            .iter()
            .position(|line| {
                header(line).is_some_and(|(start, end)| start <= address && address < end)
            })
            .unwrap();
        let entry = lines[start + 1..]
            .iter()
            .take_while(|line| header(line).is_none())
            .copied()
            .collect::<Vec<_>>();
        let locked = entry
            .iter()
            .find_map(|line| line.strip_prefix("Locked:"))
            .unwrap()
            .split_whitespace()
            .next()
            .unwrap()
            .parse::<usize>()
            .unwrap();
        assert!(locked > 0);
        let flags = entry
            .iter()
            .find_map(|line| line.strip_prefix("VmFlags:"))
            .unwrap();
        assert!(flags.split_whitespace().any(|flag| flag == "lo"));
        assert!(flags.split_whitespace().any(|flag| flag == "dd"));
    }
}
