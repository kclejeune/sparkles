//! Isolated caller-thread accounting for actual component allocations.
use std::alloc::{GlobalAlloc, Layout, System};

#[derive(Clone, Copy)]
struct Counts {
    active: bool,
    live: i64,
    base: i64,
    peak: i64,
    largest: usize,
}
thread_local! {
    static COUNTS: std::cell::Cell<Counts> = const {
        std::cell::Cell::new(Counts { active: false, live: 0, base: 0, peak: 0, largest: 0 })
    };
}
struct Counting;
fn allocate(bytes: usize) {
    let _ = COUNTS.try_with(|cell| {
        let mut counts = cell.get();
        counts.live = counts.live.saturating_add(bytes as i64);
        if counts.active {
            counts.peak = counts.peak.max(counts.live);
            counts.largest = counts.largest.max(bytes);
        }
        cell.set(counts);
    });
}
fn release(bytes: usize) {
    let _ = COUNTS.try_with(|cell| {
        let mut counts = cell.get();
        counts.live = counts.live.saturating_sub(bytes as i64);
        cell.set(counts);
    });
}
// SAFETY: every operation delegates unchanged to System. Const caller TLS only
// records sizes; neither callbacks nor tracing nor allocation run in the observer.
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let pointer = unsafe { System.alloc(layout) };
        if !pointer.is_null() {
            allocate(layout.size());
        }
        pointer
    }
    unsafe fn dealloc(&self, pointer: *mut u8, layout: Layout) {
        unsafe { System.dealloc(pointer, layout) };
        release(layout.size());
    }
    unsafe fn realloc(&self, pointer: *mut u8, layout: Layout, size: usize) -> *mut u8 {
        let pointer = unsafe { System.realloc(pointer, layout, size) };
        if !pointer.is_null() {
            release(layout.size());
            allocate(size);
        }
        pointer
    }
}
#[global_allocator]
static ALLOCATOR: Counting = Counting;

pub struct Measurement {
    pub largest: usize,
    pub peak: usize,
}
pub fn measure<T>(operation: impl FnOnce() -> T) -> (T, Measurement) {
    COUNTS.with(|cell| {
        let mut c = cell.get();
        c.active = true;
        c.base = c.live;
        c.peak = c.live;
        c.largest = 0;
        cell.set(c);
    });
    struct Measuring;
    impl Drop for Measuring {
        fn drop(&mut self) {
            let _ = COUNTS.try_with(|cell| {
                let mut c = cell.get();
                c.active = false;
                cell.set(c);
            });
        }
    }
    let measuring = Measuring;
    let result = operation();
    drop(measuring);
    let counts = COUNTS.with(std::cell::Cell::get);
    (
        result,
        Measurement {
            largest: counts.largest,
            peak: counts.peak.saturating_sub(counts.base) as usize,
        },
    )
}
