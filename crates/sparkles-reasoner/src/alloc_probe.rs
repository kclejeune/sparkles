//! A counting allocator for this crate's unit tests. It counts the bytes each thread
//! allocates, so that a test can compare what a piece of code allocates at its peak
//! with what it charged to its memory budget, while other tests run on other threads.

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;

struct Counting;

thread_local! {
    static NOW: Cell<isize> = const { Cell::new(0) };
    static PEAK: Cell<isize> = const { Cell::new(0) };
}

fn add(n: isize) {
    let _ = NOW.try_with(|now| {
        let v = now.get() + n;
        now.set(v);
        let _ = PEAK.try_with(|peak| {
            if v > peak.get() {
                peak.set(v);
            }
        });
    });
}

unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, l: Layout) -> *mut u8 {
        let p = unsafe { System.alloc(l) };
        if !p.is_null() {
            add(l.size() as isize);
        }
        p
    }

    unsafe fn alloc_zeroed(&self, l: Layout) -> *mut u8 {
        let p = unsafe { System.alloc_zeroed(l) };
        if !p.is_null() {
            add(l.size() as isize);
        }
        p
    }

    unsafe fn dealloc(&self, p: *mut u8, l: Layout) {
        unsafe { System.dealloc(p, l) };
        add(-(l.size() as isize));
    }

    unsafe fn realloc(&self, p: *mut u8, l: Layout, size: usize) -> *mut u8 {
        let q = unsafe { System.realloc(p, l, size) };
        if !q.is_null() {
            add(size as isize - l.size() as isize);
        }
        q
    }
}

#[global_allocator]
static ALLOC: Counting = Counting;

/// The result of `f` and the most bytes this thread held allocated at once while it
/// ran, beyond what it held when `f` started.
pub(crate) fn peak_of<R>(f: impl FnOnce() -> R) -> (R, usize) {
    let start = NOW.with(Cell::get);
    PEAK.with(|p| p.set(start));
    let r = f();
    let peak = PEAK.with(Cell::get);
    (r, (peak - start).max(0) as usize)
}
