//! Long-lived keys occupy dedicated pages. A failed lock/dump-exclusion request
//! fails key construction rather than silently claiming protected memory.
use crate::{BackupError, Code, Result};
use zeroize::{Zeroize, Zeroizing};

pub(crate) struct LockedKey {
    #[cfg(any(target_os = "linux", target_os = "android"))]
    page: std::ptr::NonNull<u8>,
    #[cfg(any(target_os = "linux", target_os = "android"))]
    length: usize,
    #[cfg(any(target_os = "linux", target_os = "android"))]
    used: usize,
}
// The mapping is immutable after construction and remains owned until the last
// shared reference drops. Drop has exclusive access and zeroes before unmapping.
#[cfg(any(target_os = "linux", target_os = "android"))]
unsafe impl Send for LockedKey {}
#[cfg(any(target_os = "linux", target_os = "android"))]
unsafe impl Sync for LockedKey {}
impl LockedKey {
    pub fn new(mut bytes: Zeroizing<[u8; 32]>) -> Result<Self> {
        Self::allocate(bytes.as_mut())
    }
    pub fn from_bytes(mut bytes: Zeroizing<Vec<u8>>) -> Result<Self> {
        Self::allocate(bytes.as_mut())
    }
    fn allocate(bytes: &mut [u8]) -> Result<Self> {
        #[cfg(any(target_os = "linux", target_os = "android"))]
        {
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
            let excluded =
                locked && unsafe { libc::madvise(mapping, length, libc::MADV_DONTDUMP) } == 0;
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
        #[cfg(not(any(target_os = "linux", target_os = "android")))]
        {
            bytes.zeroize();
            Err(unavailable())
        }
    }
}
impl AsRef<[u8]> for LockedKey {
    fn as_ref(&self) -> &[u8] {
        #[cfg(any(target_os = "linux", target_os = "android"))]
        // SAFETY: construction allocated used initialized bytes; the mapping
        // lives for self and cannot be mutated through shared references.
        {
            unsafe { std::slice::from_raw_parts(self.page.as_ptr(), self.used) }
        }
        #[cfg(not(any(target_os = "linux", target_os = "android")))]
        {
            unreachable!("protected keys cannot be constructed on this platform")
        }
    }
}
impl Drop for LockedKey {
    fn drop(&mut self) {
        #[cfg(any(target_os = "linux", target_os = "android"))]
        // SAFETY: exclusive owner, dedicated mapping. Zeroize uses volatile writes;
        // neither munlock nor unmap can affect any other live key's page.
        unsafe {
            std::slice::from_raw_parts_mut(self.page.as_ptr(), self.length).zeroize();
            libc::munlock(self.page.as_ptr().cast(), self.length);
            libc::munmap(self.page.as_ptr().cast(), self.length);
        }
    }
}
fn unavailable() -> BackupError {
    BackupError::new(
        Code::InvalidConfig,
        "protected repository key memory is unavailable",
    )
}

#[cfg(test)]
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
