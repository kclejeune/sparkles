//! mimalloc as a global allocator, called through its plain entry points where they give
//! the alignment a layout asks for.
//!
//! `mimalloc::MiMalloc` sends every request to `mi_malloc_aligned`. In mimalloc 3 an
//! aligned request of 64 KiB or more reserves extra room to align it, so a buffer whose
//! size is a power of two lands in the next size class: a full decoded block column of
//! 256 KiB takes 320 KiB. `mi_malloc` returns blocks aligned to [`WORD`] for every size,
//! so requests aligned to that or less use it and the rest keep the aligned calls.
//! The server, the Python, JVM and Node.js bindings and the examples that measure memory
//! all use this allocator.
//!
//! ```ignore
//! #[global_allocator]
//! static GLOBAL: sparkles_alloc::Mimalloc = sparkles_alloc::Mimalloc;
//! ```

use std::alloc::{GlobalAlloc, Layout};
use std::ffi::c_void;

pub use libmimalloc_sys as sys;

/// The mimalloc global allocator.
pub struct Mimalloc;

/// The alignment of every block `mi_malloc` returns. Blocks of 16 bytes or more are
/// aligned to 16, but smaller ones only to the word, and a layout may ask for an alignment
/// of 16 with a size below it.
pub const WORD: usize = 8;

// SAFETY: every block comes from mimalloc and goes back to it through `mi_free` or a
// reallocation. Blocks from the plain calls are aligned to `WORD`, which these calls
// only serve when the layout asks for no more. A layout keeps its alignment across a
// reallocation, so a block made by the aligned calls is only resized by them.
unsafe impl GlobalAlloc for Mimalloc {
    #[inline]
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        if layout.align() <= WORD {
            unsafe { sys::mi_malloc(layout.size()) as *mut u8 }
        } else {
            unsafe { sys::mi_malloc_aligned(layout.size(), layout.align()) as *mut u8 }
        }
    }

    #[inline]
    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        if layout.align() <= WORD {
            unsafe { sys::mi_zalloc(layout.size()) as *mut u8 }
        } else {
            unsafe { sys::mi_zalloc_aligned(layout.size(), layout.align()) as *mut u8 }
        }
    }

    #[inline]
    unsafe fn dealloc(&self, ptr: *mut u8, _layout: Layout) {
        unsafe { sys::mi_free(ptr as *mut c_void) }
    }

    #[inline]
    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        let ptr = ptr as *mut c_void;
        if layout.align() <= WORD {
            unsafe { sys::mi_realloc(ptr, new_size) as *mut u8 }
        } else {
            unsafe { sys::mi_realloc_aligned(ptr, new_size, layout.align()) as *mut u8 }
        }
    }
}

/// Return the calling thread's free heap memory to the system.
pub fn collect() {
    // SAFETY: mi_collect only returns free memory to the OS.
    unsafe { sys::mi_collect(true) }
}

/// The usable size of a block that mimalloc allocated, which is its size class.
///
/// # Safety
///
/// `ptr` must be a live block from mimalloc.
pub unsafe fn usable_size(ptr: *const u8) -> usize {
    unsafe { sys::mi_usable_size(ptr as *const c_void) }
}

#[cfg(test)]
mod tests {
    use super::{Mimalloc, usable_size};
    use std::alloc::{GlobalAlloc, Layout};

    /// A word-aligned buffer of a power-of-two size keeps its size class. Through the
    /// aligned calls, a full block column of 256 KiB took 320 KiB.
    #[test]
    fn powers_of_two_keep_their_size_class() {
        for align in [1, 8] {
            for size in [64 << 10, 128 << 10, 256 << 10, 1 << 20] {
                let layout = Layout::from_size_align(size, align).unwrap();
                // SAFETY: the layout has a non-zero size, and each block is freed with the
                // layout it has then.
                unsafe {
                    let p = Mimalloc.alloc(layout);
                    assert_eq!(usable_size(p), size, "alloc of {size} aligned to {align}");
                    Mimalloc.dealloc(p, layout);
                    let z = Mimalloc.alloc_zeroed(layout);
                    assert_eq!(usable_size(z), size, "alloc_zeroed of {size}");
                    assert!(std::slice::from_raw_parts(z, size).iter().all(|&b| b == 0));
                    let r = Mimalloc.realloc(z, layout, size * 2);
                    assert_eq!(usable_size(r), size * 2, "realloc to {}", size * 2);
                    Mimalloc.dealloc(r, Layout::from_size_align(size * 2, align).unwrap());
                }
            }
        }
    }

    /// Every block is aligned as its layout asks, through the plain and the aligned calls.
    #[test]
    fn every_block_has_its_alignment() {
        for align in [1, 2, 4, 8, 16, 32, 64, 4096] {
            for size in [1, 3, 8, 24, 100, 4096, 70_000, 300_000] {
                let layout = Layout::from_size_align(size, align).unwrap();
                let grown = Layout::from_size_align(size + 1000, align).unwrap();
                // SAFETY: the layout has a non-zero size, and each block is freed with the
                // layout it has then.
                unsafe {
                    let p = Mimalloc.alloc(layout);
                    assert_eq!(p as usize % align, 0, "alloc {size} aligned to {align}");
                    let p = Mimalloc.realloc(p, layout, grown.size());
                    assert_eq!(p as usize % align, 0, "realloc {size} aligned to {align}");
                    Mimalloc.dealloc(p, grown);
                    let z = Mimalloc.alloc_zeroed(layout);
                    assert_eq!(z as usize % align, 0, "zeroed {size} aligned to {align}");
                    Mimalloc.dealloc(z, layout);
                }
            }
        }
    }
}
