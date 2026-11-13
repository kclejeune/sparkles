//! The allocator of the triple index's tables. A table of two megabytes or more gets a
//! mapping of its own, aligned to a huge page and advised for transparent huge pages, so
//! that the control bytes and the buckets a probe reads cost one TLB entry per two
//! megabytes whatever memory the global allocator has recycled. A table that grows in
//! batches, as the closure operators grow the index, lands in recycled slices otherwise,
//! whose page size is the one of their first touch, and a probe then pays two page walks
//! more than a probe into a table allocated once into fresh memory. Smaller tables, and
//! every table on another platform than Linux, come from the global allocator.

use allocator_api2::alloc::{AllocError, Allocator, Global};
use std::alloc::Layout;
use std::ptr::NonNull;

/// The huge page, and the size from which a table gets its own mapping.
pub(crate) const HUGE: usize = 2 << 20;

/// The allocator, a unit: every table shares the same choice.
#[derive(Clone, Copy, Default, Debug)]
pub(crate) struct Huge;

/// Whether a block of this layout is mapped on its own.
#[inline]
fn mapped(layout: &Layout) -> bool {
    cfg!(target_os = "linux") && layout.size() >= HUGE && layout.align() <= HUGE
}

/// The bytes a table of `bytes` occupies: rounded up to whole huge pages when it is
/// mapped on its own, so that a charge covers what the mapping makes resident.
pub(crate) fn rounded(bytes: usize) -> usize {
    if cfg!(target_os = "linux") && bytes >= HUGE {
        bytes.next_multiple_of(HUGE)
    } else {
        bytes
    }
}

// SAFETY: a block comes from `linux::map` or from `Global` by its layout alone, and goes
// back the same way, since `mapped` is a pure function of the layout. A mapped block is
// `HUGE`-aligned, which `mapped` requires of the layout, and at least `layout.size()`
// long. The default `grow` and `shrink` allocate, copy and deallocate by the old and
// the new layout each, which keeps every block with the allocator that made it.
unsafe impl Allocator for Huge {
    fn allocate(&self, layout: Layout) -> Result<NonNull<[u8]>, AllocError> {
        if mapped(&layout) {
            linux::map(layout.size())
        } else {
            Global.allocate(layout)
        }
    }

    unsafe fn deallocate(&self, ptr: NonNull<u8>, layout: Layout) {
        if mapped(&layout) {
            // SAFETY: `ptr` came from `linux::map` with this layout, see above
            unsafe { linux::unmap(ptr, layout.size()) }
        } else {
            // SAFETY: `ptr` came from `Global` with this layout, see above
            unsafe { Global.deallocate(ptr, layout) }
        }
    }
}

#[cfg(target_os = "linux")]
mod linux {
    use super::{AllocError, HUGE, NonNull};

    /// A fresh anonymous mapping of `size` bytes rounded up to whole huge pages,
    /// aligned to a huge page and advised for transparent huge pages. The advice is
    /// advisory: where the kernel gives none, the mapping works on small pages.
    pub(super) fn map(size: usize) -> Result<NonNull<[u8]>, AllocError> {
        let len = size.next_multiple_of(HUGE);
        // one huge page more, so that an aligned range of `len` fits, trimmed below
        let total = len.checked_add(HUGE).ok_or(AllocError)?;
        // SAFETY: an anonymous private mapping with no address hint has no preconditions
        let p = unsafe {
            libc::mmap(
                std::ptr::null_mut(),
                total,
                libc::PROT_READ | libc::PROT_WRITE,
                libc::MAP_PRIVATE | libc::MAP_ANONYMOUS,
                -1,
                0,
            )
        };
        if p == libc::MAP_FAILED {
            return Err(AllocError);
        }
        let start = p as usize;
        let aligned = start.next_multiple_of(HUGE);
        let head = aligned - start;
        let tail = total - head - len;
        // SAFETY: the head and the tail are whole pages inside the mapping just made,
        // outside the aligned range that is handed out
        unsafe {
            if head > 0 {
                libc::munmap(p, head);
            }
            if tail > 0 {
                libc::munmap((aligned + len) as *mut libc::c_void, tail);
            }
            libc::madvise(aligned as *mut libc::c_void, len, libc::MADV_HUGEPAGE);
        }
        let ptr = NonNull::new(aligned as *mut u8).ok_or(AllocError)?;
        Ok(NonNull::slice_from_raw_parts(ptr, size))
    }

    /// Unmap a block that [`map`] made for `size` bytes.
    ///
    /// # Safety
    ///
    /// `ptr` is a block [`map`] returned for `size`, not unmapped yet.
    pub(super) unsafe fn unmap(ptr: NonNull<u8>, size: usize) {
        // SAFETY: the caller's contract gives the mapping's start and its rounded length
        unsafe {
            libc::munmap(ptr.as_ptr().cast(), size.next_multiple_of(HUGE));
        }
    }
}

#[cfg(not(target_os = "linux"))]
mod linux {
    use super::{AllocError, NonNull};

    pub(super) fn map(_size: usize) -> Result<NonNull<[u8]>, AllocError> {
        unreachable!("a table is mapped on its own on Linux only")
    }

    pub(super) unsafe fn unmap(_ptr: NonNull<u8>, _size: usize) {
        unreachable!("a table is mapped on its own on Linux only")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use hashbrown::HashTable;

    /// A table grows through the mapping threshold and back below it, and every entry
    /// stays findable.
    #[test]
    fn tables_cross_the_threshold() {
        let hash = |&x: &u32| (x as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15);
        let mut t: HashTable<u32, Huge> = HashTable::with_capacity_in(16, Huge);
        // 5 bytes per bucket: two million buckets pass the threshold
        let n = 1_500_000u32;
        for x in 0..n {
            t.insert_unique(hash(&x), x, hash);
        }
        assert!(t.capacity() * 5 >= HUGE, "the table is mapped on its own");
        for x in (0..n).step_by(997) {
            assert_eq!(t.find(hash(&x), |&y| y == x), Some(&x));
        }
        assert!(t.find(hash(&n), |&y| y == n).is_none());
        t.retain(|&mut x| x < 1000);
        t.shrink_to_fit(hash);
        assert!(t.capacity() * 5 < HUGE);
        for x in 0..1000 {
            assert_eq!(t.find(hash(&x), |&y| y == x), Some(&x));
        }
        drop(t);
        let small: HashTable<u32, Huge> = HashTable::with_capacity_in(8, Huge);
        drop(small);
    }

    #[test]
    fn rounding_follows_the_threshold() {
        assert_eq!(rounded(HUGE - 1), HUGE - 1);
        if cfg!(target_os = "linux") {
            assert_eq!(rounded(HUGE), HUGE);
            assert_eq!(rounded(HUGE + 1), 2 * HUGE);
            assert_eq!(rounded(5_242_896), 3 * HUGE);
        }
    }
}
