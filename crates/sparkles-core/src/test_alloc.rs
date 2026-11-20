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
    // The unadmitted-growth observation of this thread, null while none runs. Its state
    // is allocated from `System`, never through this allocator, and only the allocator
    // hooks, `Stage`, `Settled` and the credit release hook touch it, none reentrantly.
    static OBSERVATION: Cell<*mut Observation> = const { Cell::new(std::ptr::null_mut()) };
    // Stage epochs, unique on this thread across observations.
    static EPOCH: Cell<u64> = const { Cell::new(0) };
}

pub(crate) const STAGES: usize = 10;
const DEPTH: usize = 32;
const SLOTS: usize = 1 << 18;
const TABLE_FULL: u32 = 1;
const TOO_DEEP: u32 = 2;
const UNBALANCED: u32 = 4;
const SETTLEMENT: u32 = 8;
const OVER_SETTLED: u32 = 16;

/// The event at which a stage's largest excess was seen.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub(crate) enum Event {
    /// No event yet: the zeroed state of a stage's maximum.
    #[allow(dead_code)]
    None = 0,
    Alloc,
    Realloc,
    ForeignRealloc,
    Release,
    Enter,
    Exit,
}

/// A stage's largest excess, the event that set it and that event's ordinal.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Maximum {
    pub(crate) excess: i64,
    pub(crate) event: Event,
    pub(crate) ordinal: u64,
}

/// One entered stage: its epoch and number, the bytes it allocated and still holds
/// (plus the net credit its child stages acquired), the credit counter at entry, the
/// net child credit it imported, which bounds its child settlements, and the settled
/// releases of credit acquired before it entered, which its parent does not import.
#[derive(Clone, Copy)]
struct Frame {
    epoch: u64,
    stage: usize,
    live: i64,
    base: u64,
    imported: i64,
    inputs: i64,
}

/// A live allocation made while a stage was active, and that stage's epoch.
#[derive(Clone, Copy)]
struct Slot {
    pointer: usize,
    epoch: u64,
    size: usize,
}

/// Every field is valid when zeroed, which is how an observation starts.
struct Observation {
    counter: usize,
    depth: usize,
    frames: [Frame; DEPTH],
    max: [Maximum; STAGES],
    events: u64,
    entries: usize,
    peak: usize,
    failed: u32,
    settling: bool,
    origin: Origin,
    settled: u64,
    releases: u32,
    slots: [Slot; SLOTS],
}

fn home(pointer: usize) -> usize {
    ((pointer as u64 >> 4).wrapping_mul(0x9E37_79B9_7F4A_7C15) >> (64 - SLOTS.trailing_zeros()))
        as usize
}

impl Observation {
    fn used(&self) -> u64 {
        used_at(self.counter)
    }

    fn sample(&mut self, event: Event) {
        self.events += 1;
        if self.depth == 0 {
            return;
        }
        let frame = self.frames[self.depth - 1];
        let excess = frame.live - (self.used() as i64 - frame.base as i64);
        let max = &mut self.max[frame.stage];
        if excess > max.excess {
            *max = Maximum {
                excess,
                event,
                ordinal: self.events,
            };
        }
    }

    fn frame_of(&mut self, epoch: u64) -> Option<&mut Frame> {
        self.frames[..self.depth]
            .iter_mut()
            .rev()
            .find(|frame| frame.epoch == epoch)
    }

    fn insert(&mut self, pointer: usize, epoch: u64, size: usize) {
        let mut i = home(pointer);
        loop {
            let slot = &mut self.slots[i];
            if slot.pointer == pointer {
                *slot = Slot {
                    pointer,
                    epoch,
                    size,
                };
                return;
            }
            if slot.pointer == 0 {
                if self.entries + 1 >= SLOTS / 2 {
                    self.failed |= TABLE_FULL;
                    return;
                }
                *slot = Slot {
                    pointer,
                    epoch,
                    size,
                };
                self.entries += 1;
                self.peak = self.peak.max(self.entries);
                return;
            }
            i = (i + 1) & (SLOTS - 1);
        }
    }

    /// Remove `pointer`'s entry, shifting later entries of its probe run back so that
    /// the table never holds tombstones.
    fn remove(&mut self, pointer: usize) -> Option<Slot> {
        let mask = SLOTS - 1;
        let mut i = home(pointer);
        loop {
            match self.slots[i].pointer {
                0 => return None,
                p if p == pointer => break,
                _ => i = (i + 1) & mask,
            }
        }
        let found = self.slots[i];
        let mut j = i;
        loop {
            j = (j + 1) & mask;
            let slot = self.slots[j];
            if slot.pointer == 0 {
                break;
            }
            let k = home(slot.pointer);
            let stays = if i <= j {
                i < k && k <= j
            } else {
                i < k || k <= j
            };
            if !stays {
                self.slots[i] = slot;
                i = j;
            }
        }
        self.slots[i] = Slot {
            pointer: 0,
            epoch: 0,
            size: 0,
        };
        self.entries -= 1;
        Some(found)
    }
}

/// `f` on this thread's observation, if one runs. No reference to it outlives `f`, and
/// `f` neither allocates nor reaches another user of the observation.
fn with<R>(f: impl FnOnce(&mut Observation) -> R) -> Option<R> {
    let observation = OBSERVATION.try_with(Cell::get).ok()?;
    if observation.is_null() {
        return None;
    }
    // SAFETY: the observation lives until `record_unadmitted_events` clears the pointer,
    // and the users above never hold a reference across one another.
    Some(f(unsafe { &mut *observation }))
}

/// The largest excess, per stage, of bytes a planner stage allocated and still holds
/// over the Preparation credit it acquired meanwhile, while `f` runs on this thread.
pub(crate) fn record_unadmitted(preparation: &Preparation, f: impl FnOnce()) -> [i64; STAGES] {
    let (max, peak) = record_unadmitted_events(preparation, f);
    for (stage, max) in max.iter().enumerate() {
        if max.excess > 0 {
            println!(
                "observer: stage {stage} excess {} at {:?} event {} (peak {peak} entries)",
                max.excess, max.event, max.ordinal
            );
        }
    }
    max.map(|max| max.excess)
}

/// [`record_unadmitted`] with the event that set each stage's largest excess, and the
/// largest number of live allocations the observation tracked at once.
pub(crate) fn record_unadmitted_events(
    preparation: &Preparation,
    f: impl FnOnce(),
) -> ([Maximum; STAGES], usize) {
    let layout = std::alloc::Layout::new::<Observation>();
    struct Reset(*mut Observation, std::alloc::Layout);
    impl Drop for Reset {
        fn drop(&mut self) {
            OBSERVATION.with(|cell| cell.set(std::ptr::null_mut()));
            // SAFETY: allocated below from System with this layout, and no longer reachable.
            unsafe { std::alloc::GlobalAlloc::dealloc(&std::alloc::System, self.0.cast(), self.1) };
        }
    }
    assert!(
        OBSERVATION.with(Cell::get).is_null(),
        "one unadmitted-growth observation per thread"
    );
    // SAFETY: a nonzero layout, and an all-zero Observation is valid.
    let observation = unsafe { std::alloc::GlobalAlloc::alloc_zeroed(&std::alloc::System, layout) }
        .cast::<Observation>();
    assert!(!observation.is_null());
    let reset = Reset(observation, layout);
    // SAFETY: valid and not yet shared.
    unsafe { (*observation).counter = preparation.test_used_counter() as *const _ as usize };
    OBSERVATION.with(|cell| cell.set(observation));
    f();
    let (max, peak, failed, depth) = with(|o| (o.max, o.peak, o.failed, o.depth)).unwrap();
    drop(reset);
    assert_eq!(
        (failed, depth),
        (0, 0),
        "unadmitted-growth observer failure (peak {peak} entries)"
    );
    (max, peak)
}

/// A planner stage observed by [`record_unadmitted`]. A nested stage pauses the
/// outer one. The rules:
///
/// - A stage owns the allocations made while it is the active stage, under a fresh
///   epoch per entry. Its live bytes count its own allocations and frees only, so a
///   free of storage allocated before it, by an earlier entry, in another observation,
///   or by a child stage after that child returned never reduces them.
/// - A reallocation of a stage's own storage counts the change in size. Any other
///   reallocation counts the whole new size, conservatively, and takes the storage over.
/// - When a child stage returns, its whole net credit change enters the parent's live
///   bytes, so the parent's excess is the same across the child: a child's surplus credit
///   covers nothing of the parent's. Settled releases of credit acquired before the child
///   entered are left out, since every frame they reached reconciled them itself.
/// - A credit release is sampled after it lowers the counter, as are stage entry and
///   exit. A release is never assumed to belong to anything: a release of credit the
///   stage did not acquire reads as excess unless it is the drop of one named
///   reservation wrapped in [`Settled`], which neutralizes exactly that release.
pub(crate) struct Stage(Option<u64>);

fn used_at(counter: usize) -> u64 {
    unsafe { &*(counter as *const std::sync::atomic::AtomicU64) }.load(Ordering::Relaxed)
}

impl Stage {
    /// [`Stage::enter`], unless `stage` is the active stage already, which then goes on
    /// unpaused: a stage reached from within itself is observed once, as a whole.
    pub(crate) fn within(stage: usize) -> Self {
        let active = with(|o| o.depth > 0 && o.frames[o.depth - 1].stage == stage);
        if active.unwrap_or(false) {
            Self(None)
        } else {
            Self::enter(stage)
        }
    }

    pub(crate) fn enter(stage: usize) -> Self {
        assert!(stage < STAGES);
        let epoch = EPOCH.with(|epoch| {
            let next = epoch.get() + 1;
            epoch.set(next);
            next
        });
        Self(
            with(|o| {
                o.sample(Event::Enter);
                if o.depth == DEPTH {
                    o.failed |= TOO_DEEP;
                    return None;
                }
                o.frames[o.depth] = Frame {
                    epoch,
                    stage,
                    live: 0,
                    base: o.used(),
                    imported: 0,
                    inputs: 0,
                };
                o.depth += 1;
                Some(epoch)
            })
            .flatten(),
        )
    }
}

impl Drop for Stage {
    fn drop(&mut self) {
        let Some(epoch) = self.0 else {
            return;
        };
        with(|o| {
            if o.depth == 0 || o.frames[o.depth - 1].epoch != epoch {
                o.failed |= UNBALANCED;
                return;
            }
            o.sample(Event::Exit);
            o.depth -= 1;
            let child = o.frames[o.depth];
            if o.depth > 0 {
                // The child's net credit change plus the magnitude of its settled
                // releases of credit acquired before it, which each frame reconciled
                // for itself.
                let delta = o.used() as i64 - child.base as i64 + child.inputs;
                let parent = &mut o.frames[o.depth - 1];
                parent.live += delta;
                parent.imported += delta.max(0);
            }
        });
    }
}

/// Where a reservation's credit was acquired: the epoch of the stage active then (0
/// when none was) and the last stage epoch entered before it. A [`Settled`] drop names
/// it, so that each live frame reconciles the release by how it saw the credit.
#[derive(Clone, Copy, Default)]
pub(crate) struct Origin {
    frame: u64,
    entered: u64,
}

/// A frame epoch no stage has, for credit a returned child stage handed up.
const HANDED_UP: u64 = u64::MAX;

impl Origin {
    /// Credit acquired now, in the active stage.
    pub(crate) fn now() -> Self {
        Self {
            frame: with(|o| o.depth.checked_sub(1).map_or(0, |top| o.frames[top].epoch))
                .unwrap_or(0),
            entered: EPOCH.with(Cell::get),
        }
    }

    /// Credit that a child stage acquired and handed up to the active stage when it
    /// returned. The active stage imported it as child credit.
    pub(crate) fn handed_up() -> Self {
        Self {
            frame: HANDED_UP,
            entered: EPOCH.with(Cell::get),
        }
    }
}

/// One named reservation whose drop the observer settles, with the [`Origin`] of its
/// credit. Each live frame entered after the credit was acquired saw it as credit from
/// before the frame: the release is neutralized there and not imported by its parent.
/// The deepest frame entered before it acquired the credit itself, which needs nothing,
/// or imported it from a returned child, which the release retires from its imported
/// child credit, failing the observation if that goes below zero. The drop must release
/// exactly the reservation's bytes, once, on the observed counter.
pub(crate) struct Settled {
    reservation: Option<crate::access::Reservation>,
    origin: Origin,
}

impl Settled {
    pub(crate) fn new(origin: Origin, reservation: Option<crate::access::Reservation>) -> Self {
        Self {
            reservation,
            origin,
        }
    }
}

impl Drop for Settled {
    fn drop(&mut self) {
        let Some(reservation) = self.reservation.take() else {
            return;
        };
        let bytes = reservation.bytes();
        let origin = self.origin;
        let began = with(|o| {
            if o.settling {
                o.failed |= SETTLEMENT;
                return false;
            }
            o.settling = true;
            o.origin = origin;
            o.settled = 0;
            o.releases = 0;
            true
        });
        drop(reservation);
        if began == Some(true) {
            with(|o| {
                if o.releases != 1 || o.settled != bytes {
                    o.failed |= SETTLEMENT;
                }
                o.settling = false;
            });
        }
    }
}

/// Sample after a release of `bytes` of credit lowered `counter`, settling it first
/// when it is the drop of a [`Settled`] reservation.
pub(crate) fn released(counter: &std::sync::atomic::AtomicU64, bytes: u64) {
    with(|o| {
        if o.counter != counter as *const _ as usize {
            return;
        }
        if o.settling {
            o.settled += bytes;
            o.releases += 1;
            let origin = o.origin;
            let bytes = bytes as i64;
            let mut over = false;
            for frame in o.frames[..o.depth].iter_mut().rev() {
                if frame.epoch > origin.entered {
                    frame.live -= bytes;
                    frame.inputs += bytes;
                    continue;
                }
                if frame.epoch != origin.frame {
                    frame.live -= bytes;
                    frame.imported -= bytes;
                    over = frame.imported < 0;
                }
                break;
            }
            if over {
                o.failed |= OVER_SETTLED;
            }
        }
        o.sample(Event::Release);
    });
}

/// Recheck the unadmitted-growth observer's excess now, after credit was released
/// without an allocation, so that the release is checked as an allocation would be.
pub(crate) fn observe_release() {
    with(|o| o.sample(Event::Release));
}

fn allocated(pointer: *mut u8, size: usize) {
    if pointer.is_null() {
        return;
    }
    with(|o| {
        if o.depth == 0 {
            o.remove(pointer as usize);
            return;
        }
        let top = o.depth - 1;
        o.insert(pointer as usize, o.frames[top].epoch, size);
        o.frames[top].live += size as i64;
        o.sample(Event::Alloc);
    });
}

fn freed(pointer: *mut u8) {
    with(|o| {
        if let Some(slot) = o.remove(pointer as usize)
            && let Some(frame) = o.frame_of(slot.epoch)
        {
            frame.live -= slot.size as i64;
        }
    });
}

/// A successful reallocation of `old` to `new` of `size` bytes. A failed one changes
/// nothing, since the old storage stays as it was.
fn reallocated(old: *mut u8, new: *mut u8, size: usize) {
    if new.is_null() {
        return;
    }
    with(|o| {
        let was = o.remove(old as usize);
        if o.depth == 0 {
            o.remove(new as usize);
            return;
        }
        let top = o.depth - 1;
        let epoch = o.frames[top].epoch;
        let event = match was {
            Some(slot) if slot.epoch == epoch => {
                o.frames[top].live += size as i64 - slot.size as i64;
                Event::Realloc
            }
            was => {
                if let Some(slot) = was
                    && let Some(frame) = o.frame_of(slot.epoch)
                {
                    frame.live -= slot.size as i64;
                }
                o.frames[top].live += size as i64;
                Event::ForeignRealloc
            }
        };
        o.insert(new as usize, epoch, size);
        o.sample(event);
    });
}

/// Count an acquisition of `bytes` for [`record_admitted_allocations`].
fn admitted(bytes: usize) {
    let _ = ADMITTED.try_with(|cell| {
        let (size, counter, count, low, high) = cell.get();
        if size != 0 && size == bytes {
            let used = unsafe { &*(counter as *const std::sync::atomic::AtomicU64) }
                .load(Ordering::Relaxed);
            cell.set((size, counter, count + 1, low.min(used), high.max(used)));
        }
    });
}

struct AllocationObserver;
// SAFETY: System receives exactly the original layouts, and the hooks only read the
// returned pointers. A selected test-thread watch retains its actual Preparation until
// cleared. The hooks read atomics and update Copy TLS and the observation, which lives
// in System memory: no allocation, callback, borrow, panic or reentry.
unsafe impl std::alloc::GlobalAlloc for AllocationObserver {
    unsafe fn alloc(&self, layout: std::alloc::Layout) -> *mut u8 {
        let _ = LARGEST.try_with(|cell| {
            let (on, largest) = cell.get();
            if on {
                cell.set((on, largest.max(layout.size())));
            }
        });
        admitted(layout.size());
        let pointer = unsafe { std::alloc::System.alloc(layout) };
        allocated(pointer, layout.size());
        pointer
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
        freed(pointer);
        unsafe { std::alloc::System.dealloc(pointer, layout) };
    }
    unsafe fn realloc(&self, pointer: *mut u8, layout: std::alloc::Layout, size: usize) -> *mut u8 {
        // Watched staging byte vectors are complete and never grow after arm. A
        // reallocation to the selected size acquires storage of that size.
        admitted(size);
        let new = unsafe { std::alloc::System.realloc(pointer, layout, size) };
        reallocated(pointer, new, size);
        new
    }
}
#[global_allocator]
static ALLOCATION_OBSERVER: AllocationObserver = AllocationObserver;
