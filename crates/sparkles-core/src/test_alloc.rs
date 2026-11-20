//! Unit-test-only extraction of the accepted selected-allocation observer.
//! Observation never changes admission or the allocator used by production.
use crate::access::Preparation;
use std::cell::Cell;
use std::marker::PhantomData;
use std::rc::Rc;
use std::sync::atomic::Ordering;

thread_local! {
    // Allocation, real retained counter, value before physical free, event count.
    pub(crate) static DEALLOCATION: Cell<(usize, usize, u64, usize)> = const { Cell::new((0, 0, 0, 0)) };
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

thread_local! {
    // Whether this thread records, and the largest allocation it has recorded.
    static LARGEST: Cell<(bool, usize)> = const { Cell::new((false, 0)) };
}

/// The largest allocation `f` makes on this thread.
pub(crate) fn record_largest(f: impl FnOnce()) -> usize {
    LARGEST.with(|cell| cell.set((true, 0)));
    f();
    LARGEST.with(|cell| cell.replace((false, 0)).1)
}

thread_local! {
    // Selected size, original live-counter address, count, lowest and highest admission.
    static ADMITTED: Cell<(usize, usize, usize, u64, u64)> =
        const { Cell::new((0, 0, 0, u64::MAX, 0)) };
}

pub(crate) fn record_admitted_allocations(
    preparation: &Preparation,
    size: usize,
    f: impl FnOnce(),
) -> (usize, u64, u64) {
    struct Reset;
    impl Drop for Reset {
        fn drop(&mut self) {
            ADMITTED.with(|cell| cell.set((0, 0, 0, u64::MAX, 0)));
        }
    }
    let _reset = Reset;
    ADMITTED.with(|cell| {
        assert_eq!(cell.get().0, 0);
        let counter = preparation.test_used_counter() as *const _ as usize;
        cell.set((size, counter, 0, u64::MAX, 0));
    });
    f();
    ADMITTED.with(|cell| {
        let (_, _, count, low, high) = cell.get();
        (count, low, high)
    })
}

pub(crate) fn admitted_allocations_seen() -> usize {
    ADMITTED.with(|cell| cell.get().2)
}

thread_local! {
    // Unadmitted-growth observer: original live-counter address, active stage,
    // live bytes allocated in the stage, admitted bytes at its start, and the
    // largest excess of live over newly admitted bytes per stage.
    static EXCESS: Cell<(usize, usize, i64, u64, [i64; STAGES])> =
        const { Cell::new((0, usize::MAX, 0, 0, [0; STAGES])) };
}

pub(crate) const STAGES: usize = 8;

/// The largest excess, per stage, of bytes a planner stage allocated and still holds
/// over the Preparation credit it acquired meanwhile, while `f` runs on this thread.
pub(crate) fn record_unadmitted(preparation: &Preparation, f: impl FnOnce()) -> [i64; STAGES] {
    struct Reset;
    impl Drop for Reset {
        fn drop(&mut self) {
            EXCESS.with(|cell| cell.set((0, usize::MAX, 0, 0, [0; STAGES])));
        }
    }
    let _reset = Reset;
    EXCESS.with(|cell| {
        assert_eq!(cell.get().0, 0);
        let counter = preparation.test_used_counter() as *const _ as usize;
        cell.set((counter, usize::MAX, 0, 0, [0; STAGES]));
    });
    f();
    EXCESS.with(|cell| cell.get().4)
}

/// A planner stage observed by [`record_unadmitted`]. A nested stage pauses the
/// outer one, whose excess then excludes the inner stage's unadmitted bytes.
pub(crate) struct Stage(Option<(usize, i64, u64)>);

fn used_at(counter: usize) -> u64 {
    unsafe { &*(counter as *const std::sync::atomic::AtomicU64) }.load(Ordering::Relaxed)
}

impl Stage {
    pub(crate) fn enter(stage: usize) -> Self {
        let outer = EXCESS.try_with(|cell| {
            let (counter, active, live, base, max) = cell.get();
            if counter == 0 {
                return None;
            }
            cell.set((counter, stage, 0, used_at(counter), max));
            Some((active, live, base))
        });
        Self(outer.ok().flatten())
    }
}

impl Drop for Stage {
    fn drop(&mut self) {
        if let Some((active, live, base)) = self.0 {
            let _ = EXCESS.try_with(|cell| {
                let (counter, _, _, inner, max) = cell.get();
                // The outer stage sees only the credit the inner stage acquired.
                let admitted = used_at(counter) as i64 - inner as i64;
                cell.set((counter, active, live + admitted, base, max));
            });
        }
    }
}

fn observe(delta: i64) {
    let _ = EXCESS.try_with(|cell| {
        let (counter, active, live, base, mut max) = cell.get();
        if counter == 0 || active == usize::MAX {
            return;
        }
        let live = live + delta;
        let excess = live - (used_at(counter) as i64 - base as i64);
        max[active] = max[active].max(excess);
        cell.set((counter, active, live, base, max));
    });
}

struct AllocationObserver;
// SAFETY: System receives exactly the original layouts. A selected test-thread
// watch retains its actual Preparation until cleared. Dealloc reads one atomic
// and updates Copy TLS only, and alloc updates const Copy TLS only: no
// allocation, callback, borrow, panic or reentry.
unsafe impl std::alloc::GlobalAlloc for AllocationObserver {
    unsafe fn alloc(&self, layout: std::alloc::Layout) -> *mut u8 {
        let _ = LARGEST.try_with(|cell| {
            let (on, largest) = cell.get();
            if on {
                cell.set((on, largest.max(layout.size())));
            }
        });
        let _ = ADMITTED.try_with(|cell| {
            let (size, counter, count, low, high) = cell.get();
            if size != 0 && size == layout.size() {
                let used = unsafe { &*(counter as *const std::sync::atomic::AtomicU64) }
                    .load(Ordering::Relaxed);
                cell.set((size, counter, count + 1, low.min(used), high.max(used)));
            }
        });
        observe(layout.size() as i64);
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
        observe(-(layout.size() as i64));
        unsafe { std::alloc::System.dealloc(pointer, layout) };
    }
    unsafe fn realloc(&self, pointer: *mut u8, layout: std::alloc::Layout, size: usize) -> *mut u8 {
        // Watched staging byte vectors are complete and never grow after arm.
        observe(size as i64 - layout.size() as i64);
        unsafe { std::alloc::System.realloc(pointer, layout, size) }
    }
}
#[global_allocator]
static ALLOCATION_OBSERVER: AllocationObserver = AllocationObserver;
