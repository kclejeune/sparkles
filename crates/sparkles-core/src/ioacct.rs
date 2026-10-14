//! I/O accounting of the read path (spec X07 §4).
//!
//! The readers of the permutation files, the vocabulary and the numeric column count what
//! they ask the kernel for and what they decode. Per-block and per-lookup events, which many
//! threads produce by the million in a scan of the vocabulary, are counted only while a
//! [`Profile`] is alive. The counters are process-wide atomics, so
//! each costs one relaxed add and they are always on. A query's share is the difference
//! between two [`IoSnapshot`]s ([`Ctx::io_stats`](crate::sparql::Ctx::io_stats)), which is
//! exact when no other query runs at the same time, as on a cold benchmark, and includes the
//! reads of concurrent queries otherwise.
//!
//! A [`Profile`] adds what is too costly to leave on. While one is alive, the readers time
//! their vocabulary lookups, block decodes and the wait for the pages of a block, record
//! the pages they ask for or touch so that `mincore(2)` can say how many of them the query
//! brought into memory, and log the first requests in order. The major page faults come
//! from `getrusage(2)`, for the whole process.

use crate::index::Perm;
use serde::Serialize;
use std::collections::{BTreeMap, BTreeSet};
use std::sync::atomic::{
    AtomicU64, AtomicUsize,
    Ordering::{Acquire, Relaxed, Release},
};
use std::sync::{Mutex, MutexGuard};
use std::time::Instant;

/// A file the readers map, as a label for their counters.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
#[repr(u8)]
pub enum IoFile {
    Spo,
    Sop,
    Pso,
    Pos,
    Osp,
    Ops,
    Gspo,
    VocabData,
    VocabOff,
    VocabNum,
}

const FILES: usize = 10;

impl IoFile {
    pub const ALL: [IoFile; FILES] = [
        IoFile::Spo,
        IoFile::Sop,
        IoFile::Pso,
        IoFile::Pos,
        IoFile::Osp,
        IoFile::Ops,
        IoFile::Gspo,
        IoFile::VocabData,
        IoFile::VocabOff,
        IoFile::VocabNum,
    ];

    pub fn of_perm(p: Perm) -> IoFile {
        IoFile::ALL[p.index()]
    }

    pub fn name(self) -> &'static str {
        match self {
            IoFile::Spo => "spo",
            IoFile::Sop => "sop",
            IoFile::Pso => "pso",
            IoFile::Pos => "pos",
            IoFile::Osp => "osp",
            IoFile::Ops => "ops",
            IoFile::Gspo => "gspo",
            IoFile::VocabData => "vocab.dat",
            IoFile::VocabOff => "vocab.off",
            IoFile::VocabNum => "vocab.num",
        }
    }
}

#[derive(Default)]
struct Counters {
    cols_decoded: [AtomicU64; 7],
    decoded_bytes: [AtomicU64; 7],
    compressed_bytes: [AtomicU64; 7],
    willneed_requests: [AtomicU64; FILES],
    willneed_bytes: [AtomicU64; FILES],
    vocab_finds: AtomicU64,
    vocab_groups_hinted: AtomicU64,
    vocab_blocks_scanned: AtomicU64,
    num_pages: AtomicU64,
    // nanoseconds, counted only while a profile is alive
    vocab_find_ns: AtomicU64,
    vocab_read_ns: AtomicU64,
    index_decode_ns: AtomicU64,
    index_wait_ns: AtomicU64,
}

fn counters() -> &'static Counters {
    static C: std::sync::OnceLock<Counters> = std::sync::OnceLock::new();
    C.get_or_init(Counters::default)
}

static PROFILING: AtomicUsize = AtomicUsize::new(0);

/// Whether a [`Profile`] is alive, which turns the timers and the page records on.
#[inline]
pub fn profiling() -> bool {
    PROFILING.load(Acquire) != 0
}

/// A column of a block was decoded: `rows` values from `compressed` bytes.
#[inline]
pub(crate) fn column_decoded(perm: Perm, rows: usize, compressed: usize) {
    let (c, i) = (counters(), perm.index());
    c.cols_decoded[i].fetch_add(1, Relaxed);
    c.decoded_bytes[i].fetch_add(rows as u64 * 8, Relaxed);
    c.compressed_bytes[i].fetch_add(compressed as u64, Relaxed);
}

/// A `WILLNEED` request for `len` bytes of `file` (before it is issued).
#[inline]
pub(crate) fn willneed(file: IoFile, base: *const u8, off: usize, len: usize) {
    let (c, i) = (counters(), file as usize);
    c.willneed_requests[i].fetch_add(1, Relaxed);
    c.willneed_bytes[i].fetch_add(len as u64, Relaxed);
    if profiling() {
        profile::record(file, base, off, len, Some("willneed"));
    }
}

/// The pages of `len` bytes at `off` in a mapping are about to be read (profile only).
#[inline]
pub(crate) fn touch(file: IoFile, base: *const u8, off: usize, len: usize) {
    if profiling() {
        profile::record(file, base, off, len, None);
    }
}

/// Vocabulary lookups, counted by a profile only for the same reason as the blocks.
#[inline]
pub(crate) fn vocab_find() {
    if profiling() {
        counters().vocab_finds.fetch_add(1, Relaxed);
    }
}

#[inline]
pub(crate) fn vocab_group_hinted() {
    counters().vocab_groups_hinted.fetch_add(1, Relaxed);
}

/// Front-coded blocks scanned. Only a profile counts them: a pass over the vocabulary scans
/// millions of blocks on many threads, and one shared counter that all of them add to costs
/// more than the scans (10 to 20% of a cold `CONTAINS` over 100M triples).
#[inline]
pub(crate) fn vocab_blocks_scanned(n: u64) {
    if profiling() {
        counters().vocab_blocks_scanned.fetch_add(n, Relaxed);
    }
}

#[inline]
pub(crate) fn numeric_pages(n: u64) {
    counters().num_pages.fetch_add(n, Relaxed);
}

/// What a [`Timer`] adds its time to.
#[derive(Clone, Copy)]
pub(crate) enum Phase {
    VocabFind,
    VocabRead,
    IndexDecode,
    IndexWait,
}

/// Times a phase while a profile is alive, and adds the time to its counter when it drops.
pub(crate) struct Timer(Option<(Instant, Phase)>);

#[inline]
pub(crate) fn timer(phase: Phase) -> Timer {
    Timer(profiling().then(|| (Instant::now(), phase)))
}

impl Drop for Timer {
    #[inline]
    fn drop(&mut self) {
        if let Some((t, phase)) = self.0 {
            let c = counters();
            let ns = t.elapsed().as_nanos() as u64;
            match phase {
                Phase::VocabFind => &c.vocab_find_ns,
                Phase::VocabRead => &c.vocab_read_ns,
                Phase::IndexDecode => &c.index_decode_ns,
                Phase::IndexWait => &c.index_wait_ns,
            }
            .fetch_add(ns, Relaxed);
        }
    }
}

/// Read one byte of every page of `bytes`, so that the pages are in memory when a decoder
/// reads them. Used under a profile, to time the wait for the pages apart from the
/// decoding.
pub(crate) fn fault_in(bytes: &[u8]) -> u8 {
    let mut sum = 0u8;
    for i in (0..bytes.len()).step_by(4096) {
        sum = sum.wrapping_add(bytes[i]);
    }
    if let Some(&l) = bytes.last() {
        sum = sum.wrapping_add(l);
    }
    std::hint::black_box(sum)
}

/// The counters at one moment, or the difference of two moments.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct IoSnapshot {
    pub cols_decoded: [u64; 7],
    pub decoded_bytes: [u64; 7],
    pub compressed_bytes: [u64; 7],
    pub willneed_requests: [u64; FILES],
    pub willneed_bytes: [u64; FILES],
    pub vocab_finds: u64,
    pub vocab_groups_hinted: u64,
    pub vocab_blocks_scanned: u64,
    pub numeric_pages: u64,
    pub vocab_find_ns: u64,
    pub vocab_read_ns: u64,
    pub index_decode_ns: u64,
    pub index_wait_ns: u64,
}

fn load<const N: usize>(a: &[AtomicU64; N]) -> [u64; N] {
    std::array::from_fn(|i| a[i].load(Relaxed))
}

fn sub<const N: usize>(a: &[u64; N], b: &[u64; N]) -> [u64; N] {
    std::array::from_fn(|i| a[i].saturating_sub(b[i]))
}

impl IoSnapshot {
    /// The counters now.
    pub fn now() -> IoSnapshot {
        let c = counters();
        IoSnapshot {
            cols_decoded: load(&c.cols_decoded),
            decoded_bytes: load(&c.decoded_bytes),
            compressed_bytes: load(&c.compressed_bytes),
            willneed_requests: load(&c.willneed_requests),
            willneed_bytes: load(&c.willneed_bytes),
            vocab_finds: c.vocab_finds.load(Relaxed),
            vocab_groups_hinted: c.vocab_groups_hinted.load(Relaxed),
            vocab_blocks_scanned: c.vocab_blocks_scanned.load(Relaxed),
            numeric_pages: c.num_pages.load(Relaxed),
            vocab_find_ns: c.vocab_find_ns.load(Relaxed),
            vocab_read_ns: c.vocab_read_ns.load(Relaxed),
            index_decode_ns: c.index_decode_ns.load(Relaxed),
            index_wait_ns: c.index_wait_ns.load(Relaxed),
        }
    }

    /// What was counted since `base`.
    pub fn since(&self, base: &IoSnapshot) -> IoSnapshot {
        IoSnapshot {
            cols_decoded: sub(&self.cols_decoded, &base.cols_decoded),
            decoded_bytes: sub(&self.decoded_bytes, &base.decoded_bytes),
            compressed_bytes: sub(&self.compressed_bytes, &base.compressed_bytes),
            willneed_requests: sub(&self.willneed_requests, &base.willneed_requests),
            willneed_bytes: sub(&self.willneed_bytes, &base.willneed_bytes),
            vocab_finds: self.vocab_finds.saturating_sub(base.vocab_finds),
            vocab_groups_hinted: self
                .vocab_groups_hinted
                .saturating_sub(base.vocab_groups_hinted),
            vocab_blocks_scanned: self
                .vocab_blocks_scanned
                .saturating_sub(base.vocab_blocks_scanned),
            numeric_pages: self.numeric_pages.saturating_sub(base.numeric_pages),
            vocab_find_ns: self.vocab_find_ns.saturating_sub(base.vocab_find_ns),
            vocab_read_ns: self.vocab_read_ns.saturating_sub(base.vocab_read_ns),
            index_decode_ns: self.index_decode_ns.saturating_sub(base.index_decode_ns),
            index_wait_ns: self.index_wait_ns.saturating_sub(base.index_wait_ns),
        }
    }

    /// Columns decoded, summed over the permutations.
    pub fn columns_decoded(&self) -> u64 {
        self.cols_decoded.iter().sum()
    }

    /// Bytes asked for with `WILLNEED`, summed over the files.
    pub fn readahead_bytes(&self) -> u64 {
        self.willneed_bytes.iter().sum()
    }

    /// The JSON form (`io` in the metadata of `application/x-sparkles+json`).
    pub fn report(&self) -> IoReport {
        let mut index = BTreeMap::new();
        for p in Perm::ALL {
            let i = p.index();
            if self.cols_decoded[i] != 0 {
                index.insert(
                    p.name(),
                    IndexIo {
                        columns_decoded: self.cols_decoded[i],
                        decoded_bytes: self.decoded_bytes[i],
                        compressed_bytes: self.compressed_bytes[i],
                    },
                );
            }
        }
        let mut readahead = BTreeMap::new();
        for f in IoFile::ALL {
            let i = f as usize;
            if self.willneed_requests[i] != 0 {
                readahead.insert(
                    f.name(),
                    ReadaheadIo {
                        requests: self.willneed_requests[i],
                        bytes: self.willneed_bytes[i],
                    },
                );
            }
        }
        let ms = |ns: u64| ns as f64 / 1e6;
        IoReport {
            index,
            readahead,
            vocab: VocabIo {
                finds: self.vocab_finds,
                groups_hinted: self.vocab_groups_hinted,
                blocks_scanned: self.vocab_blocks_scanned,
            },
            numeric_pages_requested: self.numeric_pages,
            time_ms: None,
            major_faults: None,
            minor_faults: None,
            pages: None,
            events: None,
        }
        .with_times(
            ms(self.vocab_find_ns),
            ms(self.vocab_read_ns),
            ms(self.index_decode_ns),
            ms(self.index_wait_ns),
        )
    }
}

#[derive(Clone, Debug, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct IndexIo {
    pub columns_decoded: u64,
    pub decoded_bytes: u64,
    pub compressed_bytes: u64,
}

#[derive(Clone, Debug, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ReadaheadIo {
    pub requests: u64,
    pub bytes: u64,
}

#[derive(Clone, Debug, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct VocabIo {
    pub finds: u64,
    pub groups_hinted: u64,
    pub blocks_scanned: u64,
}

/// Time spent in the read path, summed over threads. It is zero outside a profile.
#[derive(Clone, Debug, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct PhaseTimes {
    /// `Vocab::find`, which resolves the constants of a query
    pub vocab_lookup: f64,
    /// reading the keys of terms by id
    pub vocab_read: f64,
    /// decompressing and decoding index columns
    pub index_decode: f64,
    /// touching the pages of an index column before decoding it, which is the wait for
    /// the pages when they are not in memory
    pub index_wait: f64,
}

#[derive(Clone, Debug, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct PageIo {
    /// distinct pages the query asked for or read
    pub touched: u64,
    /// of those, in memory when first recorded
    pub resident_before: u64,
    /// of those, in memory at the end
    pub resident_after: u64,
    /// pages `mincore(2)` reports in memory at the end that were not at first
    pub brought_in: u64,
}

#[derive(Clone, Debug, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct IoEvent {
    /// microseconds since the profile started
    pub at_us: u64,
    pub file: &'static str,
    pub what: &'static str,
    pub bytes: u64,
    /// index columns decoded in the process before this event, from the start of the profile
    pub columns_decoded: u64,
}

/// The `io` object of a query's metadata (see [`IoSnapshot::report`]).
#[derive(Clone, Debug, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct IoReport {
    pub index: BTreeMap<&'static str, IndexIo>,
    pub readahead: BTreeMap<&'static str, ReadaheadIo>,
    pub vocab: VocabIo,
    pub numeric_pages_requested: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub time_ms: Option<PhaseTimes>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub major_faults: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub minor_faults: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pages: Option<BTreeMap<&'static str, PageIo>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub events: Option<Vec<IoEvent>>,
}

impl IoReport {
    fn with_times(mut self, find: f64, read: f64, decode: f64, wait: f64) -> IoReport {
        if find + read + decode + wait > 0.0 {
            self.time_ms = Some(PhaseTimes {
                vocab_lookup: find,
                vocab_read: read,
                index_decode: decode,
                index_wait: wait,
            });
        }
        self
    }
}

/// A profiled query: the timers and the page records are on while it is alive.
pub struct Profile {
    base: IoSnapshot,
    began: Instant,
    faults: (u64, u64),
}

impl Profile {
    /// Start profiling. Profiles are process-wide: reads of other queries that run at the
    /// same time are counted too.
    pub fn start() -> Profile {
        profile::start();
        Profile {
            base: IoSnapshot::now(),
            began: Instant::now(),
            faults: profile::faults(),
        }
    }

    /// Everything counted since [`Profile::start`], with the page counts from `mincore(2)`
    /// taken now.
    pub fn report(&self) -> IoReport {
        let mut r = IoSnapshot::now().since(&self.base).report();
        let (maj, min) = profile::faults();
        r.major_faults = Some(maj.saturating_sub(self.faults.0));
        r.minor_faults = Some(min.saturating_sub(self.faults.1));
        let pages = profile::pages();
        if !pages.is_empty() {
            r.pages = Some(pages);
        }
        let events = profile::events(self.began, self.base.columns_decoded());
        if !events.is_empty() {
            r.events = Some(events);
        }
        r
    }
}

impl Drop for Profile {
    fn drop(&mut self) {
        profile::stop();
    }
}

/// Mark a point of the query's execution in the event log of a profile.
pub fn mark(what: &'static str) {
    if profiling() {
        profile::mark(what);
    }
}

#[cfg(test)]
mod lifecycle_test {
    use std::{cell::RefCell, rc::Rc};

    #[derive(Clone, Copy, PartialEq)]
    pub(super) enum Point {
        FirstBeforeInit,
        LastBeforeReset,
        Contended,
    }
    type Callback = Rc<dyn Fn(Point)>;
    thread_local! { static HOOK: RefCell<Option<Callback>> = const { RefCell::new(None) }; }
    pub(super) struct Scope(Option<Callback>);
    pub(super) fn install(f: impl Fn(Point) + 'static) -> Scope {
        Scope(HOOK.with(|h| h.replace(Some(Rc::new(f)))))
    }
    impl Drop for Scope {
        fn drop(&mut self) {
            HOOK.with(|h| *h.borrow_mut() = self.0.take());
        }
    }
    pub(super) fn at(point: Point) {
        let hook = HOOK.with(|h| h.borrow().clone());
        if let Some(hook) = hook {
            hook(point);
        }
    }
}

mod profile {
    use super::*;

    const MAX_EVENTS: usize = 512;
    /// ranges longer than this many pages are recorded up to it
    const MAX_PAGES: usize = 1 << 16;

    struct State {
        began: Instant,
        /// (file, mapping address, page) of every page recorded
        seen: BTreeSet<(IoFile, usize, usize)>,
        /// of the pages recorded, how many were in memory when recorded
        before: BTreeMap<IoFile, u64>,
        events: Vec<(Instant, &'static str, &'static str, u64, u64)>,
    }

    fn state() -> MutexGuard<'static, Option<State>> {
        static S: Mutex<Option<State>> = Mutex::new(None);
        #[cfg(test)]
        match S.try_lock() {
            Ok(guard) => return guard,
            Err(std::sync::TryLockError::Poisoned(e)) => return e.into_inner(),
            Err(std::sync::TryLockError::WouldBlock) => {
                lifecycle_test::at(lifecycle_test::Point::Contended)
            }
        }
        S.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Lifetime transitions own the same lock as page and event observations.
    pub(super) fn start() {
        let mut guard = state();
        let active = PROFILING.load(Relaxed);
        if active == 0 {
            #[cfg(test)]
            lifecycle_test::at(lifecycle_test::Point::FirstBeforeInit);
            reset(&mut guard);
        }
        // Publish enabled only after initialization, without locking read hot paths.
        PROFILING.store(active + 1, Release);
    }

    pub(super) fn stop() {
        let mut guard = state();
        let active = PROFILING.load(Relaxed);
        PROFILING.store(active - 1, Release);
        if active == 1 {
            #[cfg(test)]
            lifecycle_test::at(lifecycle_test::Point::LastBeforeReset);
            reset(&mut guard);
        }
    }

    fn reset(guard: &mut Option<State>) {
        *guard = Some(State {
            began: Instant::now(),
            seen: BTreeSet::new(),
            before: BTreeMap::new(),
            events: Vec::new(),
        });
    }

    pub(super) fn mark(what: &'static str) {
        if let Some(s) = state().as_mut()
            && s.events.len() < MAX_EVENTS
        {
            s.events.push((Instant::now(), "-", what, 0, columns_now()));
        }
    }

    #[cfg(unix)]
    fn page_size() -> usize {
        static P: std::sync::OnceLock<usize> = std::sync::OnceLock::new();
        *P.get_or_init(|| {
            // SAFETY: sysconf has no preconditions
            let p = unsafe { libc::sysconf(libc::_SC_PAGESIZE) };
            if p > 0 { p as usize } else { 4096 }
        })
    }

    /// How many of the pages `[first, last)` of the mapping at `addr` are in memory.
    #[cfg(unix)]
    fn resident(addr: usize, first: usize, last: usize) -> u64 {
        let ps = page_size();
        let n = last - first;
        let mut vec = vec![0u8; n];
        // SAFETY: mincore only reads the page tables for the range and writes one byte per
        // page into `vec`, which has that many. The range was inside a mapping when it was
        // recorded. If the mapping has been unmapped since (a new generation replaced it),
        // the kernel answers ENOMEM and the count is 0, so the numbers are diagnostic only.
        let r = unsafe {
            libc::mincore(
                (addr + first * ps) as *mut libc::c_void,
                n * ps,
                vec.as_mut_ptr().cast(),
            )
        };
        if r != 0 {
            return 0;
        }
        vec.iter().filter(|&&b| b & 1 != 0).count() as u64
    }

    #[cfg(unix)]
    pub(super) fn record(
        file: IoFile,
        base: *const u8,
        off: usize,
        len: usize,
        what: Option<&'static str>,
    ) {
        if len == 0 || base.is_null() {
            return;
        }
        let ps = page_size();
        let addr = base as usize;
        let (p0, p1) = (off / ps, (off + len).div_ceil(ps));
        let p1 = p1.min(p0 + MAX_PAGES);
        let mut guard = state();
        let Some(s) = guard.as_mut() else { return };
        if let Some(what) = what
            && s.events.len() < MAX_EVENTS
        {
            s.events
                .push((Instant::now(), file.name(), what, len as u64, columns_now()));
        }
        // only pages not recorded yet, in runs, so that a page that an earlier request of
        // this query brought in does not count as in memory before the query
        let mut run: Option<(usize, usize)> = None;
        let mut before = 0u64;
        for p in p0..p1 {
            if s.seen.insert((file, addr, p)) {
                run = match run {
                    Some((a, b)) if b == p => Some((a, p + 1)),
                    Some((a, b)) => {
                        before += resident(addr, a, b);
                        Some((p, p + 1))
                    }
                    None => Some((p, p + 1)),
                };
            }
        }
        if let Some((a, b)) = run {
            before += resident(addr, a, b);
        }
        *s.before.entry(file).or_default() += before;
    }

    #[cfg(not(unix))]
    pub(super) fn record(_: IoFile, _: *const u8, _: usize, _: usize, _: Option<&'static str>) {}

    #[cfg(unix)]
    pub(super) fn pages() -> BTreeMap<&'static str, PageIo> {
        let guard = state();
        let Some(s) = guard.as_ref() else {
            return BTreeMap::new();
        };
        let mut out: BTreeMap<IoFile, (u64, u64)> = BTreeMap::new();
        let mut it = s.seen.iter().peekable();
        while let Some(&(file, addr, first)) = it.next() {
            let mut last = first + 1;
            while let Some(&&(f, a, p)) = it.peek() {
                if (f, a, p) != (file, addr, last) {
                    break;
                }
                last += 1;
                it.next();
            }
            let e = out.entry(file).or_default();
            e.0 += (last - first) as u64;
            e.1 += resident(addr, first, last);
        }
        out.into_iter()
            .map(|(f, (touched, after))| {
                let before = s.before.get(&f).copied().unwrap_or(0).min(touched);
                (
                    f.name(),
                    PageIo {
                        touched,
                        resident_before: before,
                        resident_after: after,
                        brought_in: after.saturating_sub(before),
                    },
                )
            })
            .collect()
    }

    #[cfg(not(unix))]
    pub(super) fn pages() -> BTreeMap<&'static str, PageIo> {
        BTreeMap::new()
    }

    fn columns_now() -> u64 {
        IoSnapshot::now().columns_decoded()
    }

    pub(super) fn events(began: Instant, base_columns: u64) -> Vec<IoEvent> {
        let guard = state();
        guard.as_ref().map_or_else(Vec::new, |s| {
            s.events
                .iter()
                .map(|&(at, file, what, bytes, cols)| IoEvent {
                    at_us: at.saturating_duration_since(began).as_micros() as u64,
                    file,
                    what,
                    bytes,
                    columns_decoded: cols.saturating_sub(base_columns),
                })
                .collect()
        })
    }

    /// (major, minor) page faults of the process so far.
    pub(super) fn faults() -> (u64, u64) {
        #[cfg(unix)]
        {
            // SAFETY: an all-zero rusage is valid, and getrusage fills it
            let mut ru: libc::rusage = unsafe { std::mem::zeroed() };
            if unsafe { libc::getrusage(libc::RUSAGE_SELF, &mut ru) } == 0 {
                return (ru.ru_majflt as u64, ru.ru_minflt as u64);
            }
        }
        (0, 0)
    }

    #[allow(dead_code)]
    pub(super) fn began() -> Option<Instant> {
        state().as_ref().map(|s| s.began)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs::File;

    /// Exact process isolation keeps process-wide profiling out of other tests.
    fn isolated_profile_test(name: &str) -> bool {
        if std::env::var_os("SPARKLES_PROFILE_LIFETIME_CHILD").is_some() {
            return false;
        }
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", &format!("ioacct::tests::{name}"), "--nocapture"])
            .env("SPARKLES_PROFILE_LIFETIME_CHILD", "1")
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        true
    }

    #[derive(Default)]
    struct Pause {
        open: (Mutex<bool>, std::sync::Condvar),
        expired: std::sync::atomic::AtomicBool,
    }
    impl Pause {
        fn wait(&self) {
            let guard = self.open.0.lock().unwrap();
            let (guard, timeout) = self
                .open
                .1
                .wait_timeout_while(guard, std::time::Duration::from_secs(5), |open| !*open)
                .unwrap();
            if timeout.timed_out() && !*guard {
                self.expired.store(true, Relaxed);
            }
        }
        fn release(&self) {
            *self.open.0.lock().unwrap() = true;
            self.open.1.notify_all();
        }
    }
    impl Drop for Pause {
        fn drop(&mut self) {
            self.release();
        }
    }

    fn profile_mapping() -> (File, std::sync::Arc<memmap2::Mmap>, usize) {
        #[cfg(unix)]
        // SAFETY: sysconf has no pointer arguments or other preconditions.
        let page = usize::try_from(unsafe { libc::sysconf(libc::_SC_PAGESIZE) }).unwrap();
        #[cfg(not(unix))]
        let page = 4096;
        let f = tempfile::tempfile().unwrap();
        f.set_len((2 * page) as u64).unwrap();
        // SAFETY: this private file stays unchanged and alive while mapped.
        let mapping = unsafe { memmap2::Mmap::map(&f).unwrap() };
        (f, std::sync::Arc::new(mapping), page)
    }

    fn has_mark(report: &IoReport, name: &str) -> bool {
        report
            .events
            .as_ref()
            .is_some_and(|events| events.iter().any(|e| e.what == name))
    }
    fn assert_observation(report: &IoReport, name: &str, pages: u64) {
        assert!(has_mark(report, name), "missing {name}: {report:?}");
        #[cfg(unix)]
        assert_eq!(
            report.pages.as_ref().unwrap()["vocab.dat"].touched,
            pages,
            "{report:?}"
        );
        #[cfg(not(unix))]
        let _ = pages;
    }

    #[test]
    fn profile_first_start_initializes_before_enabling() {
        if isolated_profile_test("profile_first_start_initializes_before_enabling") {
            return;
        }
        use lifecycle_test::Point;
        use std::sync::{Arc, mpsc};
        // Leave the same inactive state a real preceding last drop leaves behind.
        drop(Profile::start());
        let (_file, mapping, _) = profile_mapping();
        let pause = Arc::new(Pause::default());
        let (tx, rx) = mpsc::channel();
        let worker = std::thread::spawn({
            let pause = pause.clone();
            move || {
                let _hook = lifecycle_test::install(move |point| {
                    if point == Point::FirstBeforeInit {
                        let _ = tx.send(());
                        pause.wait();
                    }
                });
                Profile::start()
            }
        });
        let entered = rx.recv_timeout(std::time::Duration::from_secs(3));
        let enabled_during_init = entered.is_ok() && profiling();
        let mut recorded_before_reset = false;
        if enabled_during_init {
            mark("during-first-init");
            touch(IoFile::VocabData, mapping.as_ptr(), 0, 1);
            recorded_before_reset = profile::events(Instant::now(), 0)
                .iter()
                .any(|e| e.what == "during-first-init");
        }
        pause.release();
        let joined = worker.join();
        // Cleanup and observation precede assertions, including on the old failure.
        let p = joined.unwrap();
        mark("after-first-init");
        touch(IoFile::VocabData, mapping.as_ptr(), 0, 1);
        let report = p.report();
        drop(p);
        eprintln!(
            "first init paused: enabled={enabled_during_init}, recorded during pause={recorded_before_reset}, retained during-pause mark={}; {report:?}",
            has_mark(&report, "during-first-init")
        );
        assert!(entered.is_ok());
        assert!(!pause.expired.load(Relaxed));
        assert!(
            !enabled_during_init,
            "profiling enabled before first initialization"
        );
        assert_observation(&report, "after-first-init", 1);
        assert!(!profiling());
    }

    #[test]
    fn profile_last_drop_cannot_erase_a_new_epoch() {
        if isolated_profile_test("profile_last_drop_cannot_erase_a_new_epoch") {
            return;
        }
        use lifecycle_test::Point;
        use std::sync::{Arc, mpsc};
        enum Event {
            Contended,
            Ready(Box<(Profile, IoReport)>),
        }
        let (_file, mapping, _) = profile_mapping();
        let old = Profile::start();
        let pause = Arc::new(Pause::default());
        let (paused_tx, paused_rx) = mpsc::channel();
        let dropping = std::thread::spawn({
            let pause = pause.clone();
            move || {
                let _hook = lifecycle_test::install(move |point| {
                    if point == Point::LastBeforeReset {
                        let _ = paused_tx.send(());
                        pause.wait();
                    }
                });
                drop(old);
            }
        });
        let entered = paused_rx.recv_timeout(std::time::Duration::from_secs(3));
        let (tx, rx) = mpsc::channel();
        let starting = std::thread::spawn(move || {
            let hook_tx = tx.clone();
            let _hook = lifecycle_test::install(move |point| {
                if point == Point::Contended {
                    let _ = hook_tx.send(Event::Contended);
                }
            });
            let fresh = Profile::start();
            mark("fresh-epoch");
            touch(IoFile::VocabData, mapping.as_ptr(), 0, 1);
            let before = fresh.report();
            let _ = tx.send(Event::Ready(Box::new((fresh, before))));
        });
        let first = rx.recv_timeout(std::time::Duration::from_secs(3));
        let contended = matches!(first, Ok(Event::Contended));
        // The repaired start is blocked by the old transition's mutex. Release it
        // before awaiting Ready; the old implementation is already Ready here.
        pause.release();
        let dropped = dropping.join();
        let ready = match first {
            Ok(Event::Ready(pair)) => Some(*pair),
            _ => match rx.recv_timeout(std::time::Duration::from_secs(3)) {
                Ok(Event::Ready(pair)) => Some(*pair),
                _ => None,
            },
        };
        let started = starting.join();
        let (p, before) = ready.expect("new profile did not finish after releasing old drop");
        let after = p.report();
        drop(p);
        eprintln!(
            "last-drop schedule: contended={contended}, fresh startup report={before:?}, after={after:?}"
        );
        assert!(entered.is_ok());
        assert!(dropped.is_ok() && started.is_ok());
        assert!(!pause.expired.load(Relaxed));
        assert_observation(&before, "fresh-epoch", 1);
        assert_observation(&after, "fresh-epoch", 1);
        assert!(contended, "new epoch bypassed the old transition lock");
        assert!(!profiling());
    }

    #[test]
    fn overlapping_profiles_preserve_the_live_epoch() {
        if isolated_profile_test("overlapping_profiles_preserve_the_live_epoch") {
            return;
        }
        let (_file, mapping, page) = profile_mapping();
        let first = Profile::start();
        mark("first-live");
        touch(IoFile::VocabData, mapping.as_ptr(), 0, 1);
        let worker = std::thread::spawn(move || {
            let second = Profile::start();
            mark("second-live");
            touch(IoFile::VocabData, mapping.as_ptr(), page, 1);
            let report = second.report();
            drop(second);
            report
        });
        let during = worker.join().unwrap();
        let enabled_with_first_alive = profiling();
        mark("first-still-live");
        let after = first.report();
        drop(first);
        assert!(enabled_with_first_alive);
        for name in ["first-live", "second-live"] {
            assert_observation(&during, name, 2);
            assert_observation(&after, name, 2);
        }
        assert_observation(&after, "first-still-live", 2);
        assert!(!profiling());
        let fresh = Profile::start();
        let empty = fresh.report();
        drop(fresh);
        assert!(empty.events.is_none() && empty.pages.is_none());
    }

    #[test]
    fn snapshots_subtract() {
        let a = IoSnapshot::now();
        column_decoded(Perm::Pos, 100, 40);
        willneed(IoFile::VocabData, std::ptr::null(), 0, 4096);
        let d = IoSnapshot::now().since(&a);
        assert!(d.cols_decoded[Perm::Pos.index()] >= 1);
        assert!(d.decoded_bytes[Perm::Pos.index()] >= 800);
        assert!(d.willneed_bytes[IoFile::VocabData as usize] >= 4096);
        let r = d.report();
        assert!(r.index.contains_key("pos"));
        assert!(r.readahead.contains_key("vocab.dat"));
    }

    #[test]
    fn profile_counts_resident_pages() {
        if isolated_profile_test("profile_counts_resident_pages") {
            return;
        }
        use std::io::Write;
        let mut f = tempfile::tempfile().unwrap();
        f.write_all(&vec![7u8; 64 * 4096]).unwrap();
        f.sync_all().unwrap();
        // SAFETY: the file is private to this test and not modified while mapped
        let m = unsafe { memmap2::Mmap::map(&f).unwrap() };
        let p = Profile::start();
        touch(IoFile::VocabData, m.as_ptr(), 0, 8 * 4096);
        fault_in(&m[..8 * 4096]);
        let r = p.report();
        let pages = r.pages.expect("pages");
        let io = &pages["vocab.dat"];
        assert_eq!(io.touched, 8);
        assert!(io.resident_after >= 8, "{io:?}");
        drop(p);
    }
}
