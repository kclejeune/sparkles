//! Unit-test-only extraction of the accepted selected-allocation observer.
//! Observation never changes admission or the allocator used by production.
use crate::access::Preparation;
use std::cell::Cell;
use std::marker::PhantomData;
use std::rc::Rc;
use std::sync::atomic::Ordering;

thread_local! {
    // Allocation, real retained counter, value before physical free, event count.
    static DEALLOCATION: Cell<(usize, usize, u64, usize)> = const { Cell::new((0, 0, 0, 0)) };
}

/// The real Preparation outlives its raw counter borrow, and the watch cannot
/// move to another thread. Drop clears TLS before releasing that real owner.
pub(crate) struct Watch {
    preparation: Preparation,
    _thread: PhantomData<Rc<()>>,
}
impl Watch {
    pub(crate) fn new(preparation: &Preparation) -> Self {
        DEALLOCATION.with(|watch| {
            assert_eq!(
                watch.get().1,
                0,
                "one selected allocation watch per test thread"
            )
        });
        let preparation = preparation.clone();
        DEALLOCATION.with(|watch| {
            watch.set((
                0,
                preparation.test_used_counter() as *const _ as usize,
                0,
                0,
            ))
        });
        Self {
            preparation,
            _thread: PhantomData,
        }
    }
    pub(crate) fn arm(&self, pointer: *const u8) {
        assert!(!pointer.is_null());
        DEALLOCATION.with(|watch| {
            let (old, counter, _, count) = watch.get();
            assert_eq!(old, 0);
            assert_eq!(count, 0);
            assert_eq!(
                counter,
                self.preparation.test_used_counter() as *const _ as usize
            );
            watch.set((pointer as usize, counter, 0, 0));
        });
    }
    pub(crate) fn observed(&self) -> (u64, usize) {
        DEALLOCATION.with(|watch| {
            let (_, _, used, count) = watch.get();
            (used, count)
        })
    }
}
impl Drop for Watch {
    fn drop(&mut self) {
        DEALLOCATION.with(|watch| watch.set((0, 0, 0, 0)));
    }
}

struct AllocationObserver;
// SAFETY: System receives exactly the original layouts. A selected test-thread
// watch retains its actual Preparation until cleared. Dealloc reads one atomic
// and updates Copy TLS only: no allocation, callback, borrow, panic or reentry.
unsafe impl std::alloc::GlobalAlloc for AllocationObserver {
    unsafe fn alloc(&self, layout: std::alloc::Layout) -> *mut u8 {
        unsafe { std::alloc::System.alloc(layout) }
    }
    unsafe fn dealloc(&self, pointer: *mut u8, layout: std::alloc::Layout) {
        let _ = DEALLOCATION.try_with(|watch| {
            let (watched, counter, _, count) = watch.get();
            if watched != 0 && watched == pointer as usize {
                let used = unsafe { &*(counter as *const std::sync::atomic::AtomicU64) }
                    .load(Ordering::Relaxed);
                watch.set((0, counter, used, count + 1));
            }
        });
        unsafe { std::alloc::System.dealloc(pointer, layout) };
    }
    unsafe fn realloc(&self, pointer: *mut u8, layout: std::alloc::Layout, size: usize) -> *mut u8 {
        // Watched staging byte vectors are complete and never grow after arm.
        unsafe { std::alloc::System.realloc(pointer, layout, size) }
    }
}
#[global_allocator]
static ALLOCATION_OBSERVER: AllocationObserver = AllocationObserver;
