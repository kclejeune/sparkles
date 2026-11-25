//! A counting allocator for this crate's unit tests. It counts the bytes each thread
//! allocates, so that a test can compare what a piece of code allocates at its peak
//! with what it charged to its memory budget, while other tests run on other threads.

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;

struct Counting;

thread_local! {
    static NOW: Cell<isize> = const { Cell::new(0) };
    static PEAK: Cell<isize> = const { Cell::new(0) };
    static LARGEST: Cell<usize> = const { Cell::new(0) };
}

fn note_size(size: usize) {
    let _ = LARGEST.try_with(|largest| {
        if size > largest.get() {
            largest.set(size);
        }
    });
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
            note_size(l.size());
            add(l.size() as isize);
        }
        p
    }

    unsafe fn alloc_zeroed(&self, l: Layout) -> *mut u8 {
        let p = unsafe { System.alloc_zeroed(l) };
        if !p.is_null() {
            note_size(l.size());
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
            note_size(size);
            add(size as isize - l.size() as isize);
        }
        q
    }
}

#[global_allocator]
static ALLOC: Counting = Counting;

/// This thread's live allocation balance. Only differences on the same thread
/// are meaningful.
pub(crate) fn live_bytes() -> isize {
    NOW.with(Cell::get)
}

/// The result of `f` and the most bytes this thread held allocated at once while it
/// ran, beyond what it held when `f` started.
pub(crate) fn peak_of<R>(f: impl FnOnce() -> R) -> (R, usize) {
    let (r, peak, _) = peak_and_largest_of(f);
    (r, peak)
}

/// The result of `f`, the peak live bytes beyond the start of `f`, and the largest single
/// allocation or reallocation made on this thread while `f` ran.
pub(crate) fn peak_and_largest_of<R>(f: impl FnOnce() -> R) -> (R, usize, usize) {
    let start = NOW.with(Cell::get);
    PEAK.with(|p| p.set(start));
    LARGEST.with(|l| l.set(0));
    let r = f();
    let peak = PEAK.with(Cell::get);
    let largest = LARGEST.with(Cell::get);
    (r, (peak - start).max(0) as usize, largest)
}
