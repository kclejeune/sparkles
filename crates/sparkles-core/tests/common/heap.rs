//! The shared audit of a cursor operator's memory ledger against the allocator (item
//! 1183). A test binary that uses it declares the allocator itself:
//!
//! ```ignore
//! mod common;
//! use common::heap;
//! #[global_allocator]
//! static A: heap::Counting = heap::Counting;
//! ```
//!
//! The allocator counts a block only when a counted thread allocates it inside an open
//! window, and it remembers the block's address, so a free lowers the count only for a
//! block the window counted, whichever thread frees it. Memory that another thread
//! allocates or frees, and memory from before the window, cannot move the reading in
//! either direction. The counted threads are the one thread of the Rayon pool that runs
//! each audited query, which also takes the query's parallel work, and the engine's
//! block decoders, which a query waits for. The allocator holds its lock across the
//! system call of each counted allocation, free and reallocation, so a second counted
//! thread cannot free or allocate between a system call and its count, and the reading
//! is exact whatever the counted threads do at once. A reallocated block counts its old
//! and new sizes together, whether it moves or grows in place, as `Charged::grow`
//! admits it, and `AUDIT_REALLOC=exact` counts one that stays in place at its larger
//! size.
//!
//! While a cursor is audited, the allocator also compares the counted heap with the
//! bytes the query has admitted, which [`MemoryMeter`] reads exactly from the query's
//! own counter, at every counted allocation. A pool grant's `used` is a reservation
//! that grows in steps of 64 KiB and is given back only once it is 1 MiB above what the
//! query holds, so a comparison with it hides any shortfall below that slack. After
//! every pull the audit compares both sides again, so a charge that stays after its
//! allocation is freed is seen as well as an allocation without a charge.
//!
//! The window is one for the process, so every test that opens one holds [`serial`]
//! for its whole body.

#![allow(dead_code)]

use sparkles_core::Error;
use sparkles_core::sparql::{MemoryMeter, QueryCursor};
use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
use std::sync::atomic::{AtomicBool, AtomicI64, AtomicPtr, AtomicUsize, Ordering};

/// The counting allocator. See the module.
pub struct Counting;

thread_local! {
    /// Whether this thread's allocations count. It has no destructor and no lazy
    /// initializer, so the allocator may read it.
    static COUNTED: Cell<bool> = const { Cell::new(false) };
}

fn counted() -> bool {
    COUNTED.try_with(Cell::get).unwrap_or(false)
}

/// The counted blocks that are live, as an open-addressed table of addresses and sizes
/// that the allocator can keep without allocating. `LOCK` guards the table and the
/// counters beside it.
const SLOTS: usize = 1 << 18;
static ADDRESS: [AtomicUsize; SLOTS] = [const { AtomicUsize::new(0) }; SLOTS];
static SIZE: [AtomicUsize; SLOTS] = [const { AtomicUsize::new(0) }; SLOTS];
static LOCK: AtomicBool = AtomicBool::new(false);
/// Whether a window is open, the blocks in the table, their bytes and the most bytes
/// they reached, and whether the table ever filled, which voids the reading.
static OPEN: AtomicBool = AtomicBool::new(false);
static BLOCKS: AtomicUsize = AtomicUsize::new(0);
static LIVE: AtomicUsize = AtomicUsize::new(0);
static PEAK: AtomicUsize = AtomicUsize::new(0);
static FULL: AtomicBool = AtomicBool::new(false);
/// While a cursor's pulls are audited: the meter of its query, and the largest excess
/// of the counted heap over the admitted bytes at any counted allocation.
static METER: AtomicPtr<MemoryMeter> = AtomicPtr::new(std::ptr::null_mut());
static WITHIN: AtomicI64 = AtomicI64::new(i64::MIN);

fn locked<T>(f: impl FnOnce() -> T) -> T {
    while LOCK
        .compare_exchange_weak(false, true, Ordering::Acquire, Ordering::Relaxed)
        .is_err()
    {
        if CONTENDS.try_with(Cell::get).unwrap_or(false) {
            CONTENDED.store(true, Ordering::SeqCst);
        }
        std::hint::spin_loop();
    }
    let r = f();
    LOCK.store(false, Ordering::Release);
    r
}

fn slot(address: usize) -> usize {
    (address >> 3).wrapping_mul(0x9E37_79B9_7F4A_7C15) >> (usize::BITS - SLOTS.trailing_zeros())
}

/// The meter's own test stops a counted thread at one site of the allocator, after
/// the system call and before the count settles, to let another counted thread make
/// the opposite call there. `HOOK` names the site, 1 for `alloc`, 2 for `dealloc` and
/// 3 for `realloc`, and is cleared when the stop is taken. `REACHED` reports the stop
/// to the other thread. The stop ends when the other thread has found the meter's lock
/// held, which `CONTENDED` reports, or when its call has returned, which it reports with
/// `DONE`. `OVERLAPPED` reads `DONE` as the stop ends, which is whether the other call
/// ran inside the site.
pub static HOOK: AtomicUsize = AtomicUsize::new(0);
pub static REACHED: AtomicBool = AtomicBool::new(false);
pub static DONE: AtomicBool = AtomicBool::new(false);
pub static CONTENDED: AtomicBool = AtomicBool::new(false);
pub static OVERLAPPED: AtomicBool = AtomicBool::new(false);

thread_local! {
    /// Whether this thread takes the stop, and whether it reports finding the lock
    /// held. Like `COUNTED`, they have no destructor.
    static STOPS: Cell<bool> = const { Cell::new(false) };
    static CONTENDS: Cell<bool> = const { Cell::new(false) };
}

/// Stop here if the meter's test asked for a stop at `site` on this thread, until the
/// other thread found the lock held or its call returned. The wait is bounded by a
/// count of spins, so it reads no clock inside the allocator, and a bound that runs out
/// leaves both flags unset, which the test reports.
fn pause(site: usize) {
    if STOPS.try_with(Cell::get).unwrap_or(false)
        && HOOK
            .compare_exchange(site, 0, Ordering::SeqCst, Ordering::SeqCst)
            .is_ok()
    {
        CONTENDED.store(false, Ordering::SeqCst);
        REACHED.store(true, Ordering::SeqCst);
        let mut spins = 0u64;
        while !DONE.load(Ordering::SeqCst) && !CONTENDED.load(Ordering::SeqCst) && spins < 1 << 34 {
            std::hint::spin_loop();
            spins += 1;
        }
        OVERLAPPED.store(DONE.load(Ordering::SeqCst), Ordering::SeqCst);
    }
}

/// Count this thread's allocations, for the meter's own tests.
pub fn count_this_thread() {
    COUNTED.set(true);
}

/// Let this thread report finding the meter's lock held, for the meter's own tests.
pub fn contend_here() {
    CONTENDS.set(true);
}

/// Let this thread take the stop at `HOOK`, for the meter's own tests.
pub fn stop_here(stops: bool) {
    STOPS.set(stops);
}

/// Whether a block that grows in place raises the peak by its larger size alone, as
/// the allocator holds it, instead of by its old and new sizes together. `open` sets
/// it from `AUDIT_REALLOC=exact`.
static EXACT_REALLOC: AtomicBool = AtomicBool::new(false);

/// Read `live` bytes as a peak, and against the bytes the audited query has admitted.
/// The caller holds the lock.
fn sample(live: usize) {
    PEAK.fetch_max(live, Ordering::Relaxed);
    let m = METER.load(Ordering::Relaxed);
    if !m.is_null() {
        // SAFETY: `audit` keeps the meter alive for as long as the pointer is set, and
        // the tests that set it hold the serial lock.
        let admitted = unsafe { (*m).live_bytes() } as i64;
        WITHIN.fetch_max(live as i64 - admitted, Ordering::Relaxed);
    }
}

/// Count a block of `size` bytes, under the lock. A full table does not take it,
/// which voids the reading.
fn count(address: usize, size: usize) {
    let live = LIVE.fetch_add(size, Ordering::Relaxed) + size;
    sample(live);
    if !remember(address, size) {
        LIVE.fetch_sub(size, Ordering::Relaxed);
    }
}

/// Put a block in the table, under the lock. A full table does not take it and sets
/// `FULL`.
fn remember(address: usize, size: usize) -> bool {
    if BLOCKS.load(Ordering::Relaxed) >= SLOTS / 2 {
        FULL.store(true, Ordering::Relaxed);
        return false;
    }
    let mut at = slot(address);
    while ADDRESS[at].load(Ordering::Relaxed) != 0 {
        at = (at + 1) % SLOTS;
    }
    ADDRESS[at].store(address, Ordering::Relaxed);
    SIZE[at].store(size, Ordering::Relaxed);
    BLOCKS.fetch_add(1, Ordering::Relaxed);
    true
}

/// Take a block out of the table and its bytes out of the live bytes, under the lock,
/// and give its size, if the window counted it. Later blocks of its probe run move
/// back over the gap, so a search still ends at the first empty slot.
fn forget(address: usize) -> Option<usize> {
    if BLOCKS.load(Ordering::Relaxed) == 0 {
        return None;
    }
    let mut at = slot(address);
    loop {
        match ADDRESS[at].load(Ordering::Relaxed) {
            0 => return None,
            a if a == address => break,
            _ => at = (at + 1) % SLOTS,
        }
    }
    let size = SIZE[at].load(Ordering::Relaxed);
    LIVE.fetch_sub(size, Ordering::Relaxed);
    BLOCKS.fetch_sub(1, Ordering::Relaxed);
    let mut gap = at;
    let mut next = (at + 1) % SLOTS;
    loop {
        let a = ADDRESS[next].load(Ordering::Relaxed);
        if a == 0 {
            break;
        }
        // The block stays if its home slot lies after the gap and up to its place.
        let home = slot(a);
        let stays = if gap <= next {
            gap < home && home <= next
        } else {
            gap < home || home <= next
        };
        if !stays {
            ADDRESS[gap].store(a, Ordering::Relaxed);
            SIZE[gap].store(SIZE[next].load(Ordering::Relaxed), Ordering::Relaxed);
            gap = next;
        }
        next = (next + 1) % SLOTS;
    }
    ADDRESS[gap].store(0, Ordering::Relaxed);
    Some(size)
}

/// Whether a free or a reallocation must take the lock: while a window is open, or
/// while the table holds a block that a window counted.
fn watched() -> bool {
    OPEN.load(Ordering::Relaxed) || BLOCKS.load(Ordering::Relaxed) > 0
}

// The meter holds `LOCK` across the system call of every counted allocation, and of
// every free and reallocation while a block could be counted. A counted block's life
// is then one ordered sequence of system calls and counts, so the live bytes are
// exactly what the system holds of the counted blocks at each count, whatever another
// counted thread does at the same time. Without it, a free on another thread between a
// system call and its count hides the peak, and an allocation on another thread
// between a count and the free it stands for is read without the block the system
// still holds. The system allocator never calls back into `Counting`, so the lock is
// never taken twice by one thread. Allocations on threads that do not count take no
// lock. The lock serializes the counted threads' allocations while a window is open,
// which the audit's fixtures accept.
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, l: Layout) -> *mut u8 {
        if !(OPEN.load(Ordering::Relaxed) && counted()) {
            return unsafe { System.alloc(l) };
        }
        locked(|| {
            let p = unsafe { System.alloc(l) };
            pause(1);
            // A window that closed while this thread waited for the lock counts no
            // more blocks.
            if !p.is_null() && OPEN.load(Ordering::Relaxed) {
                count(p as usize, l.size());
            }
            p
        })
    }
    unsafe fn dealloc(&self, p: *mut u8, l: Layout) {
        if !watched() {
            return unsafe { System.dealloc(p, l) };
        }
        locked(|| {
            unsafe { System.dealloc(p, l) };
            pause(2);
            forget(p as usize);
        });
    }
    unsafe fn realloc(&self, p: *mut u8, l: Layout, new: usize) -> *mut u8 {
        if !watched() {
            return unsafe { System.realloc(p, l, new) };
        }
        locked(|| {
            let q = unsafe { System.realloc(p, l, new) };
            pause(3);
            if q.is_null() {
                // The block is still live at its address, with the bytes it had.
                return q;
            }
            let old = forget(p as usize);
            // A block the window counted stays counted at its new address, whichever
            // thread reallocates it, as a free lowers the count whichever thread frees.
            if OPEN.load(Ordering::Relaxed) && (counted() || old.is_some()) {
                // A block that moved was live beside its copy while the bytes moved.
                // One that stayed in place reads the same, old and new together, which
                // is what `Charged::grow` admits: whether a block can stay in place
                // depends on what the allocator holds beside it, which earlier tests
                // and queries set, so an operator must admit both sizes.
                // `AUDIT_REALLOC=exact` reads such a block at its larger size.
                let held = old.unwrap_or(0);
                let rest = LIVE.load(Ordering::Relaxed);
                if q == p && EXACT_REALLOC.load(Ordering::Relaxed) {
                    sample(rest + held.max(new));
                } else {
                    sample(rest + held + new);
                }
                count(q as usize, new);
            }
            q
        })
    }
}

/// The lock every test that opens a window holds for its whole body.
pub fn serial() -> std::sync::MutexGuard<'static, ()> {
    static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
    LOCK.lock().unwrap_or_else(|e| e.into_inner())
}

/// The engine's start hook for its long-lived threads. A query waits for the block
/// decoders, so they count, and the store's other workers do not.
fn engine_thread() {
    if std::thread::current()
        .name()
        .is_some_and(|name| name.starts_with("sparkles-decode"))
    {
        COUNTED.set(true);
    }
}

/// The one-thread pool whose thread is counted. A query that runs in it keeps its
/// parallel work on that thread.
pub fn pool() -> &'static rayon::ThreadPool {
    static POOL: std::sync::OnceLock<rayon::ThreadPool> = std::sync::OnceLock::new();
    POOL.get_or_init(|| {
        sparkles_core::threads::set_start_hook(engine_thread);
        rayon::ThreadPoolBuilder::new()
            .num_threads(1)
            .start_handler(|_| COUNTED.set(true))
            .build()
            .unwrap()
    })
}

fn open() -> Window {
    // Read before the window opens, since reading the variable allocates.
    let exact = std::env::var("AUDIT_REALLOC").is_ok_and(|v| v == "exact");
    EXACT_REALLOC.store(exact, Ordering::Relaxed);
    locked(|| {
        for address in &ADDRESS {
            address.store(0, Ordering::Relaxed);
        }
        BLOCKS.store(0, Ordering::Relaxed);
        LIVE.store(0, Ordering::Relaxed);
        PEAK.store(0, Ordering::Relaxed);
        WITHIN.store(i64::MIN, Ordering::Relaxed);
        FULL.store(false, Ordering::Relaxed);
        OPEN.store(true, Ordering::Relaxed);
    });
    Window { open: true }
}

/// An open window. It is closed under the meter's lock, so no allocator call is half
/// done when its reading is taken, and it closes when it is dropped too, also while a
/// panic unwinds, so that no later test finds it open.
struct Window {
    open: bool,
}

impl Window {
    /// Close the window and give its peak and the largest excess inside a pull.
    fn close(mut self) -> (usize, i64) {
        self.open = false;
        let (peak, within, full) = locked(|| {
            OPEN.store(false, Ordering::Relaxed);
            (
                PEAK.load(Ordering::Relaxed),
                WITHIN.load(Ordering::Relaxed),
                FULL.load(Ordering::Relaxed),
            )
        });
        assert!(!full, "the block table filled");
        (peak, within)
    }
}

impl Drop for Window {
    fn drop(&mut self) {
        if self.open {
            locked(|| OPEN.store(false, Ordering::Relaxed));
        }
    }
}

/// The bytes the window has counted and still holds.
pub fn live() -> usize {
    LIVE.load(Ordering::Relaxed)
}

/// The most bytes that `f`, run on the counted thread, held of what it allocated.
pub fn peak<T: Send>(f: impl FnOnce() -> T + Send) -> (T, usize) {
    pool().install(|| {
        let window = open();
        let r = f();
        let (peak, _) = window.close();
        (r, peak)
    })
}

/// Ends the in-pull comparison when it is dropped, also while a panic unwinds, so that
/// no later allocation reads a meter that is gone. It borrows the meter, so the meter
/// outlives it, and it sets and clears the pointer under the meter's lock, which every
/// read of the pointer holds.
struct Watch<'a>(std::marker::PhantomData<&'a MemoryMeter>);

impl<'a> Watch<'a> {
    fn new(meter: &'a MemoryMeter) -> Watch<'a> {
        let at = std::ptr::from_ref(meter).cast_mut();
        locked(|| METER.store(at, Ordering::Relaxed));
        Watch(std::marker::PhantomData)
    }
}

impl Drop for Watch<'_> {
    fn drop(&mut self) {
        locked(|| METER.store(std::ptr::null_mut(), Ordering::Relaxed));
    }
}

/// What an audited cursor showed. `within` is the largest excess of the counted heap
/// over the admitted bytes at any counted allocation inside a pull, `after` that excess
/// after a pull, and `over` the largest excess of the admitted bytes over the counted
/// heap after a pull. The heap includes what the cursor allocated before its first pull,
/// such as its plan, which is never admitted, so the bounds on `within` and `after`
/// carry an allowance for it. `peak` is the counted heap's peak and `ledger` the
/// cursor's own reported peak.
#[derive(Debug, Clone)]
pub struct Audit {
    pub rows: usize,
    pub batches: usize,
    pub within: i64,
    pub after: i64,
    pub over: i64,
    pub peak: usize,
    pub ledger: u64,
    pub plan: String,
}

/// How far an audited cursor may stand from its ledger. `under` bounds the heap above
/// the ledger inside and after a pull, which includes what the cursor allocated for its
/// plan before its first pull. `over` bounds the ledger above the heap after a pull.
/// `peak` bounds the ledger's peak above five quarters of the heap's peak, which is
/// where an overcharge inside a pull shows, since such a charge refuses a budget that
/// the operator's real state fits.
#[derive(Debug, Clone, Copy)]
pub struct Allowance {
    pub under: i64,
    pub over: i64,
    pub peak: i64,
}

impl Allowance {
    /// What an operator whose charges follow its allocations measures: a few KiB of
    /// plan and cursor outside the ledger, and rounding.
    pub const EXACT: Allowance = Allowance {
        under: 16 << 10,
        over: 16 << 10,
        peak: 64 << 10,
    };
}

impl Audit {
    /// The findings against an allowance: the heap above the ledger by more than
    /// `under`, inside or after a pull, is an undercharge, and the ledger above the heap
    /// by more than `over` after a pull, or at its peak, is an overcharge.
    pub fn check(&self, name: &str, allowed: &Allowance) -> Vec<String> {
        let mut found = Vec::new();
        if self.within > allowed.under {
            found.push(format!(
                "{name}: the heap passed the ledger inside a pull by {} bytes",
                self.within
            ));
        }
        if self.after > allowed.under {
            found.push(format!(
                "{name}: the heap passed the ledger after a pull by {} bytes",
                self.after
            ));
        }
        if self.over > allowed.over {
            found.push(format!(
                "{name}: the ledger passed the heap after a pull by {} bytes",
                self.over
            ));
        }
        let peak = self.ledger as i64 - (self.peak as i64 * 5 / 4);
        if peak > allowed.peak {
            found.push(format!(
                "{name}: the ledger's peak of {} bytes passed five quarters of the heap's peak of {} by {peak} bytes",
                self.ledger, self.peak
            ));
        }
        found
    }
}

/// Build a cursor with `make` and pull it to its end on the counted thread, comparing
/// the counted heap with the query's admitted bytes inside and after every pull. Each
/// batch is dropped before the next pull, so the consumer holds nothing. `make` runs
/// inside the window, so what the cursor allocates for its plan counts.
pub fn audit(make: impl FnOnce() -> QueryCursor + Send) -> Audit {
    pool().install(|| {
        let window = open();
        let mut c = make();
        let meter = c.memory_meter();
        let mut a = Audit {
            rows: 0,
            batches: 0,
            within: i64::MIN,
            after: i64::MIN,
            over: i64::MIN,
            peak: 0,
            ledger: 0,
            plan: String::new(),
        };
        {
            let _watch = Watch::new(&meter);
            loop {
                let ended = match c.next_batch().unwrap() {
                    Some(b) => {
                        a.rows += b.len();
                        a.batches += 1;
                        false
                    }
                    None => true,
                };
                let heap = live() as i64;
                let admitted = meter.live_bytes() as i64;
                a.after = a.after.max(heap - admitted);
                a.over = a.over.max(admitted - heap);
                if ended {
                    break;
                }
            }
        }
        (a.peak, a.within) = window.close();
        a.ledger = c.stats().mem_peak_bytes;
        a.plan = c.plan_json().unwrap_or_default();
        drop(meter);
        a
    })
}

/// Whether `e` is the refusal of a memory budget.
pub fn refused(e: &Error) -> bool {
    matches!(e, Error::BudgetExceeded(b) if b.kind == sparkles_core::BudgetKind::Memory)
}

/// The budgets of `range` in steps of `step` that `run` fits, as a sorted list. The fit
/// of a streamed operator is not always monotone in its budget, since some operators
/// size their batches from the room they find, so the budgets are swept and not
/// bisected. Every error other than a memory refusal fails the test.
pub fn fits(
    range: std::ops::Range<u64>,
    step: u64,
    mut run: impl FnMut(u64) -> Result<(), Error>,
) -> Vec<u64> {
    let mut fit = Vec::new();
    for budget in range.step_by(step as usize) {
        match run(budget) {
            Ok(()) => fit.push(budget),
            Err(e) => assert!(refused(&e), "{budget} bytes: {e}"),
        }
    }
    fit
}

/// The budget-parity sweep: for every budget of `range` (in steps of `step`) at which
/// `before` fits, `after` must fit at that budget or within `slack` bytes above it, and
/// both must give the same answer. `before` is the run with a switch off, or the run
/// that a migration replaced; `after` is the run with the switch on, or the migrated
/// one. It returns the largest extra the after run needed, and fails the test when the
/// sweep never crosses the before run's smallest fit, since such a sweep checks nothing.
pub fn parity<T: PartialEq + std::fmt::Debug>(
    name: &str,
    range: std::ops::Range<u64>,
    step: u64,
    slack: u64,
    mut before: impl FnMut(u64) -> Result<T, Error>,
    mut after: impl FnMut(u64) -> Result<T, Error>,
) -> u64 {
    let (mut fit, mut fail, mut worst) = (0, 0, 0);
    for budget in range.step_by(step as usize) {
        let expected = match before(budget) {
            Ok(answer) => answer,
            Err(e) => {
                assert!(refused(&e), "{name}, {budget} bytes: {e}");
                fail += 1;
                continue;
            }
        };
        fit += 1;
        let more =
            (0..=slack)
                .step_by(step.max(1) as usize)
                .find(|more| match after(budget + more) {
                    Ok(answer) => {
                        assert_eq!(answer, expected, "{name}, {budget} bytes");
                        true
                    }
                    Err(e) => {
                        assert!(refused(&e), "{name}, {budget} bytes: {e}");
                        false
                    }
                });
        let more = more.unwrap_or_else(|| {
            panic!("{name}: fits {budget} bytes before, and after not within {slack} bytes more")
        });
        worst = worst.max(more);
    }
    assert!(
        fit > 0 && fail > 0,
        "{name}: {fit} fit and {fail} fail, so the sweep crosses no threshold"
    );
    worst
}
