//! Permutation indexes (QLever `CompressedRelation`-style).
//!
//! Every quad `(s, p, o, g)` is stored in several *permutations*, each a fully sorted
//! sequence of 4-column keys. A permutation file is a sequence of blocks of up to
//! [`BLOCK_ROWS`] rows. Each block stores its four columns separately
//! (delta + zig-zag varint, then LZ4). Block metadata (first/last key, row offset, byte
//! range) is kept in RAM, so a scan with a bound prefix touches only the blocks that can
//! contain it, and exact range counts need at most two block decodes.
//!
//! Since index format 3 a column is a run of independent LZ4 frames, one per segment of
//! [`SEG_ROWS`] rows, and `<perm>.seg` holds each segment's first key and frame lengths
//! (spec X07 §6.5). A reader of a few rows of a block decodes only the segments that can
//! hold them, and a reader of whole blocks decodes every frame into one column as before.
//! Format 2 generations, whose columns are one frame each and which have no `.seg` file,
//! are read as before.

use crate::error::{Error, Result};
use crate::id::Id;
use crate::ioacct::IoFile;
use crate::vocab::write_varint;
use memmap2::Mmap;
use std::fs::File;
use std::io::{BufWriter, Write};
use std::path::Path;
use std::sync::Arc;

pub const BLOCK_ROWS: usize = 32 * 1024;

/// Rows of a segment in the generations this build writes. The `.seg` header records the
/// size, and a reader accepts any size from [`MIN_SEG_ROWS`] to [`MAX_SEG_ROWS`] that
/// divides [`BLOCK_ROWS`].
pub const SEG_ROWS: usize = 2048;
const MIN_SEG_ROWS: usize = 512;
const MAX_SEG_ROWS: usize = 4096;
const SEG_MAGIC: &[u8; 8] = b"SPKSEG01";
/// Bytes of the `.seg` header: the magic, the segment size and four reserved bytes.
const SEG_HEADER: usize = 16;
/// Bytes of one segment's directory entry: its first key and its frame length in each
/// column.
pub(crate) const SEG_ENTRY: usize = 32 + 16;
/// The most segments of a block a range read decodes one by one, as a share of the
/// block's segments. A range that spans more reads whole columns.
const SPARSE_SEGMENTS_DIV: usize = 4;

/// Read hints for the memory-mapped index and vocabulary files (on by default, `off` in
/// `SPARKLES_IO_HINTS` turns them off for a process).
///
/// Without hints, Linux answers a page fault on a file mapping by reading the
/// device's whole read-ahead window around the page, 128 KiB to several MiB. A point
/// lookup on a cold server faults on a few pages of a vocabulary of tens of MiB, and a
/// window around each of them, so a query that needs 30 terms read 40 MiB. With the
/// hints, the vocabulary data is mapped for random access, so a fault reads its page
/// alone, and the reads that cover many terms ask for their whole range ahead. An index
/// file is mapped for random access too, and a block column about to be decoded asks
/// the kernel for exactly its bytes and those of the block's later columns, in one
/// request, before the decoder touches them.
pub fn io_hints() -> bool {
    IO_HINTS.get()
}

/// Turn the read hints of [`io_hints`] on or off for files mapped from now on, and for
/// the ranges read from the files already mapped.
pub fn set_io_hints(on: bool) {
    IO_HINTS.set(on);
}

/// Whether vocabularies opened from now on use their sparse index (`vocab.idx`, see
/// [`crate::vocab::Vocab`]) when they have one. On by default, `off` in
/// `SPARKLES_SPARSE_VOCAB` turns it off for a process.
pub fn sparse_vocab() -> bool {
    SPARSE_VOCAB.get()
}

/// Turn the use of [`sparse_vocab`] on or off for vocabularies opened from now on.
pub fn set_sparse_vocab(on: bool) {
    SPARSE_VOCAB.set(on);
}

/// Whether vocabularies opened from now on read the values of numeric literals from
/// their numeric column (`vocab.num`, see [`crate::vocab::numeric`]) when they have one.
/// On by default, `off` in `SPARKLES_NUMERIC_COLUMN` turns it off for a process.
pub fn numeric_column() -> bool {
    NUMERIC_COLUMN.get()
}

/// Turn the use of [`numeric_column`] on or off for vocabularies opened from now on.
pub fn set_numeric_column(on: bool) {
    NUMERIC_COLUMN.set(on);
}

static NUMERIC_COLUMN: EnvSwitch = EnvSwitch::new("SPARKLES_NUMERIC_COLUMN");
static IO_HINTS: EnvSwitch = EnvSwitch::new("SPARKLES_IO_HINTS");
static SPARSE_VOCAB: EnvSwitch = EnvSwitch::new("SPARKLES_SPARSE_VOCAB");

/// A process-wide switch, on unless its environment variable says `off`, `0` or
/// `false`, and settable for tests and A/B runs.
pub(crate) struct EnvSwitch {
    var: &'static str,
    /// 0 = not read from the environment yet, 1 = on, 2 = off
    state: std::sync::atomic::AtomicU8,
}

impl EnvSwitch {
    pub(crate) const fn new(var: &'static str) -> EnvSwitch {
        EnvSwitch {
            var,
            state: std::sync::atomic::AtomicU8::new(0),
        }
    }
    pub(crate) fn get(&self) -> bool {
        use std::sync::atomic::Ordering::Relaxed;
        match self.state.load(Relaxed) {
            0 => {
                let on = std::env::var(self.var).map_or(true, |v| {
                    !matches!(
                        v.trim().to_ascii_lowercase().as_str(),
                        "off" | "0" | "false"
                    )
                });
                self.set(on);
                on
            }
            v => v == 1,
        }
    }
    pub(crate) fn set(&self, on: bool) {
        self.state
            .store(if on { 1 } else { 2 }, std::sync::atomic::Ordering::Relaxed);
    }
}

/// Map a whole file read-only for random access (see [`io_hints`]).
pub(crate) fn map_random(f: &File) -> std::io::Result<Mmap> {
    // SAFETY: index and vocabulary files are immutable once written; generations are
    // never modified in place.
    let m = unsafe { Mmap::map(f)? };
    #[cfg(unix)]
    if io_hints() {
        // a hint only: a kernel that refuses it reads as before
        let _ = m.advise(memmap2::Advice::Random);
    }
    Ok(m)
}

#[cfg(test)]
type HintSpans = std::rc::Rc<std::cell::RefCell<Vec<(usize, usize)>>>;
#[cfg(test)]
type HintSink = (usize, HintSpans);
#[cfg(test)]
thread_local! {
    static HINT_TRACE: std::cell::RefCell<Option<HintSink>> = const { std::cell::RefCell::new(None) };
}

#[cfg(test)]
pub(crate) struct HintTrace {
    spans: HintSpans,
    previous: Option<HintSink>,
}
#[cfg(test)]
impl HintTrace {
    pub(crate) fn spans(&self) -> Vec<(usize, usize)> {
        self.spans.borrow().clone()
    }
}
#[cfg(test)]
impl Drop for HintTrace {
    fn drop(&mut self) {
        HINT_TRACE.with(|trace| {
            *trace.borrow_mut() = self.previous.take();
        });
    }
}

#[cfg(test)]
thread_local! {
    /// [`ReadAhead::before`] calls on this thread (for tests of warm-path overhead)
    static READ_AHEAD_CALLS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

#[cfg(test)]
pub(crate) fn read_ahead_fixture(blocks: usize) -> (tempfile::TempDir, PermIndex) {
    let dir = tempfile::tempdir().unwrap();
    let mut writer = PermWriter::create(dir.path(), Perm::Spo).unwrap();
    let mut random = 0x123456789abcdefu64;
    for i in 0..(blocks * BLOCK_ROWS) as u64 {
        random ^= random << 13;
        random ^= random >> 7;
        random ^= random << 17;
        let x = random;
        random ^= random << 13;
        random ^= random >> 7;
        random ^= random << 17;
        writer.push([i, x, random, 0]).unwrap();
    }
    writer.finish(dir.path(), Perm::Spo).unwrap();
    let idx = PermIndex::open(dir.path(), Perm::Spo).unwrap();
    (dir, idx)
}

#[cfg(test)]
pub(crate) fn read_ahead_reference(rows: usize) -> Vec<u64> {
    let mut random = 0x123456789abcdefu64;
    (0..rows)
        .map(|_| {
            random ^= random << 13;
            random ^= random >> 7;
            random ^= random << 17;
            let value = random;
            random ^= random << 13;
            random ^= random >> 7;
            random ^= random << 17;
            value
        })
        .collect()
}

/// Independent test oracle over raw attempted advice, without production coverage state.
#[cfg(test)]
pub(crate) fn advice_covers_span(spans: &[(usize, usize)], (from, to): (usize, usize)) -> bool {
    if from >= to {
        return true;
    }
    let mut sorted = spans.to_vec();
    sorted.sort_unstable();
    let mut through = from;
    for (start, end) in sorted {
        if end <= through {
            continue;
        }
        if start > through {
            return false;
        }
        through = end;
        if through >= to {
            return true;
        }
    }
    false
}

#[cfg(all(test, unix))]
pub(crate) fn advice_page_size() -> usize {
    // SAFETY: sysconf has no pointer arguments or other preconditions.
    usize::try_from(unsafe { libc::sysconf(libc::_SC_PAGESIZE) }).unwrap()
}

/// Ask the kernel to start reading `len` bytes of `m` at `off` (see [`io_hints`]).
#[inline]
pub(crate) fn will_need(file: IoFile, m: &Mmap, off: usize, len: usize) -> bool {
    #[cfg(unix)]
    if len > 0 && off <= m.len() && len <= m.len() - off && io_hints() {
        #[cfg(test)]
        HINT_TRACE.with(|trace| {
            if let Some((mapping, spans)) = &*trace.borrow()
                && *mapping == m.as_ptr() as usize
            {
                spans.borrow_mut().push((off, off + len));
            }
        });
        crate::ioacct::willneed(file, m.as_ptr(), off, len);
        // Linux caps a WILLNEED request's read-ahead. A single hint for an export's
        // tens of MiB leaves its tail to synchronous faults on a Random mapping.
        // Issue bounded requests so the entire requested range is covered.
        const WINDOW: usize = 256 << 10;
        for start in (off..off + len).step_by(WINDOW) {
            let _ = m.advise_range(
                memmap2::Advice::WillNeed,
                start,
                WINDOW.min(off + len - start),
            );
        }
        return true;
    }
    #[cfg(not(unix))]
    let _ = (file, m, off, len);
    false
}

pub type Key = [u64; 4];

/// Target compressed bytes requested ahead by one [`ReadAhead`] pass. Whole-block
/// progress may extend it through one next block and its coalesced gap.
/// `SPARKLES_READ_WINDOW_KB` changes it for experiments, and 0 turns the windows off: every
/// block then asks for its own columns just before it is decoded, as before the windows.
pub fn read_window() -> usize {
    static W: std::sync::OnceLock<usize> = std::sync::OnceLock::new();
    *W.get_or_init(|| {
        std::env::var("SPARKLES_READ_WINDOW_KB")
            .ok()
            .and_then(|v| v.trim().parse::<usize>().ok())
            .map_or(
                4 << 20,
                |kb| if kb == 0 { 0 } else { (kb << 10).max(64 << 10) },
            )
    })
}

/// Read-ahead of one pass over consecutive blocks of a permutation (spec X07 §5.3).
///
/// The files are mapped for random access (see [`io_hints`]), so the kernel reads nothing
/// ahead of a page fault. A pass asks for its own bytes in a window that runs ahead of it:
/// the first request covers about 1 MiB and each refill, made when the pass has used half of
/// what was requested, doubles up to [`read_window`]. A short range is read by the first
/// request alone, which overlaps the reads of its blocks as the plan-time prefetch does. A
/// long one keeps a window of reads in flight, as the kernel's read-ahead would have, and
/// a scan that stops early has requested at most the target plus one next-block extension,
/// including its coalesced gap. This counts requested bytes, not physical I/O completion.
/// The window belongs to the pass, so concurrent passes over one file share no state and the
/// mapping's advice never changes.
#[derive(Default)]
pub struct ReadAhead {
    /// Blocks before this one were examined, including decoded cache skips.
    requested: usize,
    /// Actual requested compressed byte union still ahead of this pass.
    covered: std::collections::VecDeque<(usize, usize)>,
    /// request more when the pass reaches this block
    refill_at: usize,
    /// Target outstanding advice bytes, including existing unconsumed coverage.
    window: usize,
    #[cfg(test)]
    lookup_inspections: usize,
}

impl ReadAhead {
    const FIRST: usize = 1 << 20;

    /// The pass reads block `b` next, of the blocks up to `end`. `columns` gives the
    /// columns the pass reads of a block. Returns whether a cache miss needs current-block
    /// fallback advice. Such fallback never requests blocks beyond the one being reached.
    pub fn before(
        &mut self,
        idx: &PermIndex,
        cache: &BlockCache,
        b: usize,
        end: usize,
        columns: &dyn Fn(usize) -> ColMask,
    ) -> bool {
        #[cfg(test)]
        READ_AHEAD_CALLS.with(|n| n.set(n.get() + 1));
        let limit = read_window();
        if !io_hints() || limit == 0 {
            return true;
        }
        let Some(data) = idx.data.as_ref() else {
            return true;
        };
        let Some(block) = idx.blocks.get(b) else {
            return true;
        };
        let at = block.offset as usize;
        while self.covered.front().is_some_and(|&(_, z)| z <= at) {
            self.covered.pop_front();
        }
        if let Some((a, _)) = self.covered.front_mut() {
            *a = (*a).max(at);
        }
        if b >= self.refill_at && self.requested < end {
            self.window = if self.window == 0 {
                Self::FIRST.min(limit)
            } else {
                self.window.saturating_mul(2).min(limit)
            };
            let mut used = self.covered.iter().map(|&(a, z)| z - a).sum::<usize>();
            let mut examined = 0usize;
            let mut blk = b.max(self.requested);
            let mut run: Option<(usize, usize)> = None;
            const GAP: usize = 64 << 10;
            // Existing outstanding advice consumes the budget. The one addition that
            // crosses it may extend through one next block and a bridged gap, never
            // another full window. A separate traversal cap also bounds cached/empty
            // spans, each of which consumes at least one unit of examined work.
            while blk < end && used < self.window && examined < self.window {
                let mask = columns(blk);
                let span = idx.span(blk, mask);
                examined = examined.saturating_add(span.map_or(1, |(a, z)| (z - a).max(1)));
                if let Some((from, to)) = span
                    && !cache.has_cols(idx, blk, mask)
                {
                    run = match run {
                        Some((a, z)) if from <= z.saturating_add(GAP) => {
                            used = used.saturating_add(to.saturating_sub(z));
                            Some((a, z.max(to)))
                        }
                        previous => {
                            if let Some((a, z)) = previous {
                                self.hint(idx, data, a, z);
                            }
                            used = used.saturating_add(to - from);
                            Some((from, to))
                        }
                    };
                }
                blk += 1;
            }
            if let Some((a, z)) = run {
                self.hint(idx, data, a, z);
            }
            self.requested = blk;
            self.refill_at = if blk >= end {
                usize::MAX
            } else {
                b.saturating_add(((blk.saturating_sub(b)) / 2).max(1))
            };
        }
        // A cached column skipped at look-ahead may have been evicted since. Only
        // actual advice, not the examined endpoint, can suppress miss-time fallback.
        !idx.span(b, columns(b))
            .is_none_or(|(a, z)| self.covers(a, z))
    }

    fn covers(&mut self, from: usize, to: usize) -> bool {
        // Coverage is sorted and merged. A future interval cannot cover a hole
        // before its start, so do not walk the rest of the future window there.
        for &(start, end) in &self.covered {
            #[cfg(test)]
            {
                self.lookup_inspections += 1;
            }
            if start > from {
                return false;
            }
            if end >= to {
                return true;
            }
        }
        false
    }

    fn hint(&mut self, idx: &PermIndex, data: &Mmap, from: usize, to: usize) {
        if will_need(IoFile::of_perm(idx.perm), data, from, to - from) {
            if let Some((_, end)) = self.covered.back_mut()
                && from <= *end
            {
                *end = (*end).max(to);
            } else {
                self.covered.push_back((from, to));
            }
        }
    }
}

/// Position of each component in a quad array `[s, p, o, g]`.
pub const S: usize = 0;
pub const P: usize = 1;
pub const O: usize = 2;
pub const G: usize = 3;

#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash, Ord, PartialOrd)]
pub enum Perm {
    Spo,
    Sop,
    Pso,
    Pos,
    Osp,
    Ops,
    Gspo,
}

impl Perm {
    pub const ALL: [Perm; 7] = [
        Perm::Spo,
        Perm::Sop,
        Perm::Pso,
        Perm::Pos,
        Perm::Osp,
        Perm::Ops,
        Perm::Gspo,
    ];

    /// Key column `i` holds quad component `order()[i]`.
    #[inline]
    pub const fn order(self) -> [usize; 4] {
        match self {
            Perm::Spo => [S, P, O, G],
            Perm::Sop => [S, O, P, G],
            Perm::Pso => [P, S, O, G],
            Perm::Pos => [P, O, S, G],
            Perm::Osp => [O, S, P, G],
            Perm::Ops => [O, P, S, G],
            Perm::Gspo => [G, S, P, O],
        }
    }

    pub const fn index(self) -> usize {
        self as usize
    }

    pub fn name(self) -> &'static str {
        match self {
            Perm::Spo => "spo",
            Perm::Sop => "sop",
            Perm::Pso => "pso",
            Perm::Pos => "pos",
            Perm::Osp => "osp",
            Perm::Ops => "ops",
            Perm::Gspo => "gspo",
        }
    }

    #[inline]
    pub fn to_key(self, quad: &[Id; 4]) -> Key {
        let o = self.order();
        [quad[o[0]].0, quad[o[1]].0, quad[o[2]].0, quad[o[3]].0]
    }

    #[inline]
    pub fn to_quad(self, key: &Key) -> [Id; 4] {
        let o = self.order();
        let mut q = [Id::UNDEF; 4];
        for i in 0..4 {
            q[o[i]] = Id(key[i]);
        }
        q
    }

    /// Which key column holds quad component `c`.
    #[inline]
    pub fn col_of(self, c: usize) -> usize {
        self.order().iter().position(|&x| x == c).unwrap()
    }
}

/// Set of key columns (bit `c` = column `c`) a reader needs from a block.
pub type ColMask = u8;
/// Every column.
pub const ALL_COLS: ColMask = 0b1111;

/// Mask of the leading key columns that decide whether a key lies in `[lo, hi]`: the
/// columns after them are bounded by 0 and `u64::MAX`, which every key satisfies.
pub fn bound_cols(lo: &Key, hi: &Key) -> ColMask {
    let n = (0..4)
        .rev()
        .find(|&c| lo[c] != 0 || hi[c] != u64::MAX)
        .map_or(0, |c| c + 1);
    ((1u16 << n) - 1) as ColMask
}

/// A decoded column, shared by the block cache and its readers.
///
/// The values live in an allocation of their own, apart from the reference count. A full
/// column is 256 KiB, and a shared slice with its counts in front would ask for 16 bytes
/// more, which mimalloc serves from its 320 KiB size class.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Col(Arc<Box<[u64]>>);

impl Col {
    fn new(values: Box<[u64]>) -> Col {
        Col(Arc::new(values))
    }
}

impl std::ops::Deref for Col {
    type Target = [u64];
    #[inline]
    fn deref(&self) -> &[u64] {
        &self.0
    }
}

impl From<Vec<u64>> for Col {
    fn from(values: Vec<u64>) -> Col {
        Col::new(values.into_boxed_slice())
    }
}

impl FromIterator<u64> for Col {
    fn from_iter<I: IntoIterator<Item = u64>>(iter: I) -> Col {
        Col::new(iter.into_iter().collect())
    }
}

/// A decoded block: four columns of equal length. Columns a reader did not ask for (see
/// [`BlockCache::get_cols`]) are empty and read as 0 through [`Block::key`].
#[derive(Clone)]
pub struct Block {
    pub cols: [Col; 4],
    rows: usize,
}

impl Block {
    #[inline]
    pub fn len(&self) -> usize {
        self.rows
    }
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
    #[inline]
    pub fn key(&self, i: usize) -> Key {
        let c = |c: usize| self.cols[c].get(i).copied().unwrap_or(0);
        [c(0), c(1), c(2), c(3)]
    }
    #[inline]
    fn cmp_prefix(&self, i: usize, prefix: &[u64]) -> std::cmp::Ordering {
        for (c, &v) in prefix.iter().enumerate() {
            match self.cols[c][i].cmp(&v) {
                std::cmp::Ordering::Equal => continue,
                o => return o,
            }
        }
        std::cmp::Ordering::Equal
    }
    /// Row range `[s, e)` whose keys lie in `[lo, hi]`; needs the columns of
    /// [`bound_cols`].
    pub fn key_range(&self, lo: &Key, hi: &Key) -> (usize, usize) {
        let n = self.len();
        let d = bound_cols(lo, hi).count_ones() as usize;
        let lead = |i: usize| -> Key {
            let mut k = [0; 4];
            for (c, x) in k.iter_mut().enumerate().take(d) {
                *x = self.cols[c][i];
            }
            k
        };
        let (mut l, mut h) = ([0; 4], [0; 4]);
        l[..d].copy_from_slice(&lo[..d]);
        h[..d].copy_from_slice(&hi[..d]);
        let s = partition(n, |i| lead(i) < l);
        let e = s + partition(n - s, |i| lead(s + i) <= h);
        (s, e)
    }
    /// Row range `[lo, hi)` whose keys start with `prefix`.
    pub fn prefix_range(&self, prefix: &[u64]) -> (usize, usize) {
        if prefix.is_empty() {
            return (0, self.len());
        }
        let n = self.len();
        let (mut lo, mut hi) = (0, n);
        while lo < hi {
            let m = (lo + hi) / 2;
            if self.cmp_prefix(m, prefix).is_lt() {
                lo = m + 1
            } else {
                hi = m
            }
        }
        let start = lo;
        let mut hi = n;
        while lo < hi {
            let m = (lo + hi) / 2;
            if self.cmp_prefix(m, prefix).is_le() {
                lo = m + 1
            } else {
                hi = m
            }
        }
        (start, lo)
    }
    /// Bytes of the decoded columns held.
    pub fn bytes(&self) -> usize {
        self.cols.iter().map(|c| c.len() * 8).sum()
    }
}

/// Number of leading indexes in `0..n` for which `pred` holds (`pred` must be monotone).
fn partition(n: usize, pred: impl Fn(usize) -> bool) -> usize {
    let (mut lo, mut hi) = (0, n);
    while lo < hi {
        let m = (lo + hi) / 2;
        if pred(m) { lo = m + 1 } else { hi = m }
    }
    lo
}

#[derive(Clone, Debug)]
pub struct BlockMeta {
    pub first: Key,
    pub last: Key,
    pub offset: u64,
    /// compressed byte length of each column
    pub col_len: [u32; 4],
    pub rows: u32,
    /// cumulative row number of the first row of this block
    pub row_start: u64,
}

/// Bytes of one block's metadata record in a `.meta` file.
pub(crate) const META_BYTES: usize = 8 * 4 * 2 + 8 + 4 * 4 + 4 + 8;

impl BlockMeta {
    fn write(&self, out: &mut Vec<u8>) {
        for v in self.first.iter().chain(self.last.iter()) {
            out.extend_from_slice(&v.to_le_bytes());
        }
        out.extend_from_slice(&self.offset.to_le_bytes());
        for l in self.col_len {
            out.extend_from_slice(&l.to_le_bytes());
        }
        out.extend_from_slice(&self.rows.to_le_bytes());
        out.extend_from_slice(&self.row_start.to_le_bytes());
    }
    pub(crate) fn read(b: &[u8]) -> BlockMeta {
        let u64_at = |i: usize| u64::from_le_bytes(b[i..i + 8].try_into().unwrap());
        let u32_at = |i: usize| u32::from_le_bytes(b[i..i + 4].try_into().unwrap());
        BlockMeta {
            first: [u64_at(0), u64_at(8), u64_at(16), u64_at(24)],
            last: [u64_at(32), u64_at(40), u64_at(48), u64_at(56)],
            offset: u64_at(64),
            col_len: [u32_at(72), u32_at(76), u32_at(80), u32_at(84)],
            rows: u32_at(88),
            row_start: u64_at(92),
        }
    }
}

pub(crate) fn encode_column(col: &[u64], scratch: &mut Vec<u8>) -> Vec<u8> {
    scratch.clear();
    let mut prev = 0u64;
    for &v in col {
        let d = v.wrapping_sub(prev) as i64;
        write_varint(scratch, ((d << 1) ^ (d >> 63)) as u64);
        prev = v;
    }
    lz4_flex::compress_prepend_size(scratch)
}

/// Encode `col` as one frame per `seg_rows` rows, each with its deltas restarting at 0,
/// appended to `out`. Pushes each frame's length to `lens`.
pub(crate) fn encode_segmented(
    col: &[u64],
    seg_rows: usize,
    scratch: &mut Vec<u8>,
    out: &mut Vec<u8>,
    lens: &mut Vec<u32>,
) {
    for part in col.chunks(seg_rows) {
        let frame = encode_column(part, scratch);
        lens.push(frame.len() as u32);
        out.extend_from_slice(&frame);
    }
}

/// Decode one compressed column of `rows` values. Damaged input is an error, never a
/// panic: the size prefix must fit `rows` varints, and the values must use exactly the
/// decompressed bytes.
pub(crate) fn decode_column(bytes: &[u8], rows: usize) -> Result<Col> {
    // Allocated as the buffer the block cache keeps, so that nothing is copied after
    // decoding.
    let mut col: Box<[u64]> = vec![0u64; rows].into_boxed_slice();
    decode_frame_into(bytes, &mut col)?;
    Ok(Col::new(col))
}

/// Decode the frame `bytes` (a size prefix, then LZ4 of zig-zag delta varints starting
/// from 0) into all of `out`, with the checks of [`decode_column`].
pub(crate) fn decode_frame_into(bytes: &[u8], out: &mut [u64]) -> Result<()> {
    let rows = out.len();
    let bad = |m: &str| Error::Corrupt(format!("block column: {m}"));
    let size = bytes
        .get(..4)
        .map(|b| u32::from_le_bytes(b.try_into().unwrap()) as usize)
        .ok_or_else(|| bad("shorter than its size prefix"))?;
    // a varint takes at most 10 bytes: a larger size is damage (and would be allocated)
    if size > rows.saturating_mul(10) {
        return Err(bad(&format!(
            "decompressed size {size} is too large for {rows} rows"
        )));
    }
    // The varints are decompressed into a buffer each thread keeps. A fresh one per
    // column made every cold block decode fault in new pages, which was a tenth of a
    // cold point lookup. A thread keeps at most one column's varints, BLOCK_ROWS * 10
    // bytes and usually far less.
    thread_local! {
        static RAW: std::cell::RefCell<Vec<u8>> = const { std::cell::RefCell::new(Vec::new()) };
    }
    RAW.with_borrow_mut(|raw| {
        if raw.len() < size {
            raw.resize(size, 0);
        }
        let raw = &mut raw[..size];
        let n = lz4_flex::block::decompress_into(&bytes[4..], raw)
            .map_err(|e| Error::Corrupt(format!("block decompression: {e}")))?;
        if n != size {
            return Err(Error::Corrupt(format!(
                "block decompression: {n} bytes instead of {size}"
            )));
        }
        decode_varints_into(raw, out)
    })
}

/// The `out.len()` zigzag-delta varints that make up all of `raw`, written to `out`.
fn decode_varints_into(raw: &[u8], out: &mut [u64]) -> Result<()> {
    let rows = out.len();
    let bad = |m: &str| Error::Corrupt(format!("block column: {m}"));
    let mut pos = 0;
    let mut prev = 0u64;
    let mut i = 0;
    while i < rows {
        // Sorted key columns mostly hold deltas below 64, one byte each. Eight such
        // bytes in a row are decoded together, without a branch per value.
        if let (Some(dst), Some(s)) = (out.get_mut(i..i + 8), raw.get(pos..pos + 8)) {
            let w = u64::from_le_bytes(s.try_into().unwrap());
            if w & 0x8080_8080_8080_8080 == 0 {
                for (o, z) in dst.iter_mut().zip(w.to_le_bytes()) {
                    let d = ((z >> 1) as i64) ^ -((z & 1) as i64);
                    prev = prev.wrapping_add(d as u64);
                    *o = prev;
                }
                pos += 8;
                i += 8;
                continue;
            }
        }
        // a varint takes at most 10 bytes: while 10 remain, read them without checking
        // for the end of the buffer at each byte
        let z = match raw.get(pos..pos + 10) {
            Some(s) => read_varint10(s.try_into().unwrap(), &mut pos),
            None => read_varint_checked(raw, &mut pos),
        }
        .ok_or_else(|| bad(&format!("holds fewer than {rows} values")))?;
        let d = ((z >> 1) as i64) ^ -((z & 1) as i64);
        prev = prev.wrapping_add(d as u64);
        out[i] = prev;
        i += 1;
    }
    if pos != raw.len() {
        return Err(bad(&format!(
            "{} bytes left after {rows} values",
            raw.len() - pos
        )));
    }
    Ok(())
}

/// [`read_varint_checked`] on a buffer known to hold the 10 bytes a varint can take.
#[inline(always)]
fn read_varint10(s: &[u8; 10], pos: &mut usize) -> Option<u64> {
    let mut v = 0u64;
    for (i, &b) in s.iter().enumerate() {
        v |= ((b & 0x7F) as u64) << (7 * i);
        if b < 0x80 {
            *pos += i + 1;
            return Some(v);
        }
    }
    None
}

/// [`read_varint`](crate::vocab::read_varint) that returns `None` at the end of `buf` or
/// on a value longer than 64 bits.
#[inline]
pub(crate) fn read_varint_checked(buf: &[u8], pos: &mut usize) -> Option<u64> {
    let mut v = 0u64;
    let mut shift = 0;
    loop {
        let b = *buf.get(*pos)?;
        *pos += 1;
        if shift > 63 {
            return None;
        }
        v |= ((b & 0x7F) as u64) << shift;
        if b < 0x80 {
            return Some(v);
        }
        shift += 7;
    }
}

/// Streaming writer for one permutation. Keys must arrive sorted and deduplicated.
pub struct PermWriter {
    data: BufWriter<crate::disk::WritebackFile>,
    /// the segment directory (`<perm>.seg`), `None` for an unsegmented (format 2) index
    seg: Option<BufWriter<File>>,
    meta: Vec<u8>,
    pos: u64,
    rows: u64,
    buf: [Vec<u64>; 4],
    last: Option<Key>,
    scratch: Vec<u8>,
    /// one block's encoded columns and their frame lengths, reused
    enc: Vec<u8>,
    lens: [Vec<u32>; 4],
}

impl PermWriter {
    /// A writer of a segmented permutation, the format this build writes.
    pub fn create(dir: &Path, perm: Perm) -> Result<PermWriter> {
        Self::create_format(dir, perm, true)
    }

    /// A writer of a segmented permutation or, with `segmented` false, of one in the
    /// format 2 layout without segments (for tests and fixtures of that format).
    pub(crate) fn create_format(dir: &Path, perm: Perm, segmented: bool) -> Result<PermWriter> {
        let seg = if segmented {
            let mut w = BufWriter::with_capacity(
                64 << 10,
                File::create(dir.join(format!("{}.seg", perm.name())))?,
            );
            w.write_all(SEG_MAGIC)?;
            w.write_all(&(SEG_ROWS as u32).to_le_bytes())?;
            w.write_all(&[0u8; 4])?;
            Some(w)
        } else {
            None
        };
        Ok(PermWriter {
            data: BufWriter::with_capacity(
                1 << 20,
                crate::disk::WritebackFile::new(File::create(
                    dir.join(format!("{}.dat", perm.name())),
                )?),
            ),
            seg,
            meta: Vec::new(),
            pos: 0,
            rows: 0,
            buf: Default::default(),
            last: None,
            scratch: Vec::new(),
            enc: Vec::new(),
            lens: Default::default(),
        })
    }

    /// Push a key; exact duplicates of the previous key are dropped.
    #[inline]
    pub fn push(&mut self, key: Key) -> Result<()> {
        if self.last == Some(key) {
            return Ok(());
        }
        debug_assert!(self.last.is_none_or(|l| l < key), "keys out of order");
        self.last = Some(key);
        for (col, v) in self.buf.iter_mut().zip(key) {
            col.push(v);
        }
        if self.buf[0].len() == BLOCK_ROWS {
            self.flush_block()?;
        }
        Ok(())
    }

    fn flush_block(&mut self) -> Result<()> {
        let n = self.buf[0].len();
        if n == 0 {
            return Ok(());
        }
        let first = [
            self.buf[0][0],
            self.buf[1][0],
            self.buf[2][0],
            self.buf[3][0],
        ];
        let last = [
            self.buf[0][n - 1],
            self.buf[1][n - 1],
            self.buf[2][n - 1],
            self.buf[3][n - 1],
        ];
        let mut col_len = [0u32; 4];
        let offset = self.pos;
        for ((col, lens), len) in self.buf.iter().zip(&mut self.lens).zip(&mut col_len) {
            self.enc.clear();
            lens.clear();
            if self.seg.is_some() {
                encode_segmented(col, SEG_ROWS, &mut self.scratch, &mut self.enc, lens);
            } else {
                self.enc = encode_column(col, &mut self.scratch);
            }
            self.data.write_all(&self.enc)?;
            *len = self.enc.len() as u32;
            self.pos += self.enc.len() as u64;
        }
        if let Some(seg) = self.seg.as_mut() {
            let mut entry = [0u8; SEG_ENTRY];
            for s in 0..n.div_ceil(SEG_ROWS) {
                for c in 0..4 {
                    entry[c * 8..c * 8 + 8]
                        .copy_from_slice(&self.buf[c][s * SEG_ROWS].to_le_bytes());
                    entry[32 + c * 4..36 + c * 4].copy_from_slice(&self.lens[c][s].to_le_bytes());
                }
                seg.write_all(&entry)?;
            }
        }
        for col in self.buf.iter_mut() {
            col.clear();
        }
        BlockMeta {
            first,
            last,
            offset,
            col_len,
            rows: n as u32,
            row_start: self.rows,
        }
        .write(&mut self.meta);
        self.rows += n as u64;
        Ok(())
    }

    /// End the current block here, however few rows it holds (a block of a partial
    /// compaction, see [`crate::store`]'s compaction).
    pub(crate) fn end_block(&mut self) -> Result<()> {
        self.flush_block()
    }

    /// Append block `b` of `src` as one block after the current one, which is ended
    /// first. Its keys must follow the keys pushed before. When `src` has this writer's
    /// layout, the block's bytes and directory entries are copied as they are. Otherwise
    /// (a format 2 block written into a segmented index, or a different segment size) the
    /// block is decoded and encoded again.
    pub(crate) fn push_block(&mut self, src: &PermIndex, b: usize) -> Result<()> {
        self.flush_block()?;
        let m = &src.blocks[b];
        debug_assert!(self.last.is_none_or(|l| l < m.first), "blocks out of order");
        let same = match (&self.seg, &src.segs) {
            (None, None) => true,
            (Some(_), Some(d)) => d.seg_rows == SEG_ROWS,
            _ => false,
        };
        if !same {
            let block = src.decode_block(b)?;
            for i in 0..block.len() {
                self.push(block.key(i))?;
            }
            return self.flush_block();
        }
        let bytes = src.raw_block(b)?;
        debug_assert_eq!(
            bytes.len() as u64,
            m.col_len.iter().map(|&l| l as u64).sum::<u64>()
        );
        self.data.write_all(bytes)?;
        if let (Some(seg), Some(d)) = (self.seg.as_mut(), src.segs.as_ref()) {
            seg.write_all(d.entries(b)?)?;
        }
        BlockMeta {
            offset: self.pos,
            row_start: self.rows,
            ..m.clone()
        }
        .write(&mut self.meta);
        self.pos += bytes.len() as u64;
        self.rows += m.rows as u64;
        self.last = Some(m.last);
        Ok(())
    }

    pub fn finish(mut self, dir: &Path, perm: Perm) -> Result<u64> {
        self.flush_block()?;
        self.data.flush()?;
        self.data.get_ref().get_ref().sync_all()?;
        if let Some(mut seg) = self.seg.take() {
            seg.flush()?;
            seg.get_ref().sync_all()?;
        }
        let mut m = File::create(dir.join(format!("{}.meta", perm.name())))?;
        m.write_all(&self.meta)?;
        m.sync_all()?;
        Ok(self.rows)
    }
}

/// The segment directory of a segmented permutation (`<perm>.seg`, spec X07 §6.5).
pub(crate) struct Segments {
    map: Mmap,
    /// rows of a segment
    pub(crate) seg_rows: usize,
    /// the index of each block's first entry, and the total at the end
    start: Vec<usize>,
    /// a bit per block whose entries a reader has asked the kernel for, so that a warm
    /// pass makes no read hint for them again
    hinted: Box<[std::sync::atomic::AtomicU64]>,
    /// per block, the segments decoded one by one so far, up to the block's limit
    /// ([`Segments::dense_limit`]), after which readers decode the block whole
    sparse_reads: Box<[std::sync::atomic::AtomicU8]>,
}

impl Segments {
    fn open(path: &Path, perm: Perm, blocks: &[BlockMeta]) -> Result<Segments> {
        let bad = |m: String| Error::Corrupt(format!("{}.seg: {m}", perm.name()));
        let f = File::open(path)?;
        let map = map_random(&f)?;
        if map.len() < SEG_HEADER || &map[..8] != SEG_MAGIC {
            return Err(bad("not a segment directory".into()));
        }
        let seg_rows = u32::from_le_bytes(map[8..12].try_into().unwrap()) as usize;
        if !(MIN_SEG_ROWS..=MAX_SEG_ROWS).contains(&seg_rows)
            || !BLOCK_ROWS.is_multiple_of(seg_rows)
        {
            return Err(bad(format!("segment size {seg_rows} is not supported")));
        }
        let mut start = Vec::with_capacity(blocks.len() + 1);
        let mut n = 0usize;
        for m in blocks {
            start.push(n);
            n += (m.rows as usize).div_ceil(seg_rows);
        }
        start.push(n);
        if map.len() != SEG_HEADER + n * SEG_ENTRY {
            return Err(bad(format!(
                "{} bytes, {} expected for {n} segments",
                map.len(),
                SEG_HEADER + n * SEG_ENTRY
            )));
        }
        Ok(Segments {
            map,
            seg_rows,
            hinted: (0..blocks.len().div_ceil(64))
                .map(|_| Default::default())
                .collect(),
            sparse_reads: (0..blocks.len()).map(|_| Default::default()).collect(),
            start,
        })
    }

    /// Segments of block `b`.
    #[inline]
    pub(crate) fn count(&self, b: usize) -> usize {
        self.start[b + 1] - self.start[b]
    }

    /// The segments of block `b` that readers may decode one by one before they read it
    /// whole: a quarter of them. Past that, decoding the block once costs less than
    /// decoding more of its segments, one request each.
    #[inline]
    fn dense_limit(&self, b: usize) -> u8 {
        (self.count(b) / SPARSE_SEGMENTS_DIV).max(1) as u8
    }

    /// Whether readers have decoded enough of block `b`'s segments one by one to read it
    /// whole from now on.
    #[inline]
    fn dense(&self, b: usize) -> bool {
        self.sparse_reads[b].load(std::sync::atomic::Ordering::Relaxed) >= self.dense_limit(b)
    }

    /// Count a segment of block `b` decoded on its own.
    fn note_sparse_read(&self, b: usize) {
        use std::sync::atomic::Ordering::Relaxed;
        let limit = self.dense_limit(b);
        let _ =
            self.sparse_reads[b].fetch_update(Relaxed, Relaxed, |n| (n < limit).then_some(n + 1));
    }

    /// Have readers read block `b` whole from now on.
    fn mark_dense(&self, b: usize) {
        self.sparse_reads[b].store(self.dense_limit(b), std::sync::atomic::Ordering::Relaxed);
    }

    /// The directory entries of block `b`.
    pub(crate) fn entries(&self, b: usize) -> Result<&[u8]> {
        let from = SEG_HEADER + self.start[b] * SEG_ENTRY;
        let to = SEG_HEADER + self.start[b + 1] * SEG_ENTRY;
        self.map
            .get(from..to)
            .ok_or_else(|| Error::Corrupt("segment directory is short".into()))
    }

    #[inline]
    fn entry(&self, b: usize, s: usize) -> &[u8] {
        let at = SEG_HEADER + (self.start[b] + s) * SEG_ENTRY;
        &self.map[at..at + SEG_ENTRY]
    }

    /// The first key of segment `s` of block `b`.
    #[inline]
    pub(crate) fn first_key(&self, b: usize, s: usize) -> Key {
        let e = self.entry(b, s);
        std::array::from_fn(|c| u64::from_le_bytes(e[c * 8..c * 8 + 8].try_into().unwrap()))
    }

    /// The frame length of segment `s` of block `b` in column `c`.
    #[inline]
    pub(crate) fn frame_len(&self, b: usize, s: usize, c: usize) -> usize {
        let e = self.entry(b, s);
        u32::from_le_bytes(e[32 + c * 4..36 + c * 4].try_into().unwrap()) as usize
    }

    /// The bytes of the file that hold block `b`'s entries: `(offset, length)`.
    fn span(&self, b: usize) -> (usize, usize) {
        let from = SEG_HEADER + self.start[b] * SEG_ENTRY;
        (from, self.count(b) * SEG_ENTRY)
    }
}

/// Read side of one permutation.
#[derive(Clone)]
pub struct PermIndex {
    pub perm: Perm,
    data: Option<Arc<Mmap>>,
    pub blocks: Arc<Vec<BlockMeta>>,
    pub rows: u64,
    /// unique id of this permutation instance, used for cache keys
    pub uid: u64,
    /// the segment directory, `None` for a format 2 permutation
    pub(crate) segs: Option<Arc<Segments>>,
}

impl PermIndex {
    pub fn empty(perm: Perm) -> PermIndex {
        PermIndex {
            perm,
            data: None,
            blocks: Arc::new(Vec::new()),
            rows: 0,
            uid: next_uid(),
            segs: None,
        }
    }

    /// Open the permutation in `dir`, segmented when its `.seg` file exists.
    pub fn open(dir: &Path, perm: Perm) -> Result<PermIndex> {
        let segmented = dir.join(format!("{}.seg", perm.name())).exists();
        Self::open_layout(dir, perm, segmented)
    }

    /// Open the permutation of a generation in index format `format`: format 3 is
    /// segmented and needs its `.seg` file, format 2 is not.
    pub fn open_format(dir: &Path, perm: Perm, format: u32) -> Result<PermIndex> {
        Self::open_layout(dir, perm, format >= 3)
    }

    fn open_layout(dir: &Path, perm: Perm, segmented: bool) -> Result<PermIndex> {
        let meta = std::fs::read(dir.join(format!("{}.meta", perm.name())))?;
        if meta.len() % META_BYTES != 0 {
            return Err(Error::Corrupt(format!("{} metadata size", perm.name())));
        }
        let blocks: Vec<BlockMeta> = meta
            .as_chunks::<META_BYTES>()
            .0
            .iter()
            .map(|c| BlockMeta::read(c))
            .collect();
        let rows = blocks.last().map_or(0, |b| b.row_start + b.rows as u64);
        let f = File::open(dir.join(format!("{}.dat", perm.name())))?;
        let data = if f.metadata()?.len() > 0 {
            Some(Arc::new(map_random(&f)?))
        } else {
            None
        };
        let segs = if segmented {
            let path = dir.join(format!("{}.seg", perm.name()));
            if !path.exists() {
                return Err(Error::Corrupt(format!(
                    "{}.seg is missing from a segmented index",
                    perm.name()
                )));
            }
            Some(Arc::new(Segments::open(&path, perm, &blocks)?))
        } else {
            None
        };
        Ok(PermIndex {
            perm,
            data,
            blocks: Arc::new(blocks),
            rows,
            uid: next_uid(),
            segs,
        })
    }

    pub fn disk_bytes(&self) -> u64 {
        self.data.as_ref().map_or(0, |d| d.len() as u64)
            + (self.blocks.len() * META_BYTES) as u64
            + self.segs.as_ref().map_or(0, |d| d.map.len() as u64)
    }

    /// Whether the permutation is segmented (index format 3).
    pub fn segmented(&self) -> bool {
        self.segs.is_some()
    }

    /// The rows `[from, to)` of block `b` that segment `s` holds.
    #[inline]
    pub(crate) fn segment_rows(&self, b: usize, s: usize) -> (usize, usize) {
        let n = self.segs.as_ref().map_or(BLOCK_ROWS, |d| d.seg_rows);
        let rows = self.blocks[b].rows as usize;
        ((s * n).min(rows), ((s + 1) * n).min(rows))
    }

    /// The segments `[s0, s1)` of block `b` to decode one by one for the rows of the
    /// block with keys in `[lo, hi]`, or `None` when the reader decodes whole columns:
    /// the permutation is not segmented, the range covers the whole block, it spans more
    /// than a quarter of the block's segments, or readers have already decoded a quarter
    /// of them one by one (spec X07 §6.5). A prefetch and the reader after it agree, since
    /// only a prefetch or a decode changes the answer.
    pub(crate) fn segment_span(&self, b: usize, lo: &Key, hi: &Key) -> Option<(usize, usize)> {
        let d = self.segs.as_ref()?;
        let m = &self.blocks[b];
        if (m.first >= *lo && m.last <= *hi) || d.dense(b) {
            return None;
        }
        let n = d.count(b);
        // the last segment that starts at or before `lo` holds its first row, and the
        // segments that start after `hi` hold none of the range
        let s0 = partition(n, |s| d.first_key(b, s) <= *lo).saturating_sub(1);
        let s1 = partition(n, |s| d.first_key(b, s) <= *hi).max(s0 + 1);
        (s1 - s0 <= (n / SPARSE_SEGMENTS_DIV).max(1) && s1 - s0 < n).then_some((s0, s1))
    }

    /// [`segment_span`](Self::segment_span), unless the cache holds block `b`'s columns
    /// whole (judged by the first column of `mask`): a reader then uses them, as cheap as
    /// a segment, instead of cutting segments from them on every request.
    pub(crate) fn read_span(
        &self,
        cache: &BlockCache,
        b: usize,
        lo: &Key,
        hi: &Key,
        mask: ColMask,
    ) -> Option<(usize, usize)> {
        let span = self.segment_span(b, lo, hi)?;
        (!cache.holds_whole(self, b, mask)).then_some(span)
    }

    /// Have readers of block `b` decode it whole from now on: a prefetch found that a
    /// request reads more than a quarter of its segments.
    pub(crate) fn mark_dense(&self, b: usize) {
        if let Some(d) = &self.segs {
            d.mark_dense(b);
        }
    }

    /// The segments of block `b` a request may read one by one before it reads the block
    /// whole (see [`mark_dense`](Self::mark_dense)).
    pub(crate) fn dense_limit(&self, b: usize) -> usize {
        self.segs.as_ref().map_or(0, |d| d.dense_limit(b) as usize)
    }

    /// Whether segment `s` of block `b` is the block's last.
    #[inline]
    pub(crate) fn last_segment(&self, b: usize, s: usize) -> bool {
        self.segs.as_ref().is_none_or(|d| s + 1 >= d.count(b))
    }

    /// The first key of segment `s` of block `b` (the block's first key when the
    /// permutation is not segmented).
    pub(crate) fn segment_first(&self, b: usize, s: usize) -> Key {
        match &self.segs {
            Some(d) => d.first_key(b, s),
            None => self.blocks[b].first,
        }
    }

    /// Ask the kernel for block `b`'s directory entries, before a reader searches them,
    /// once per process: the entries are small and stay in the page cache.
    pub(crate) fn prefetch_directory(&self, b: usize) {
        use std::sync::atomic::Ordering::Relaxed;
        if let Some(d) = &self.segs {
            let bit = 1u64 << (b % 64);
            let word = &d.hinted[b / 64];
            if word.load(Relaxed) & bit == 0 && word.fetch_or(bit, Relaxed) & bit == 0 {
                let (off, len) = d.span(b);
                will_need(IoFile::of_perm(self.perm), &d.map, off, len);
            }
        }
    }

    /// The bytes of column `c` of block `b`: `(offset, length)` in the data file.
    fn column_span(&self, b: usize, c: usize) -> (usize, usize) {
        let m = &self.blocks[b];
        let off = m.offset as usize + m.col_len[..c].iter().map(|&l| l as usize).sum::<usize>();
        (off, m.col_len[c] as usize)
    }

    /// The bytes of the frames of segments `[s0, s1)` of block `b` in column `c`:
    /// `(offset, length)`, checked to lie inside the column.
    fn frames_span(&self, b: usize, c: usize, s0: usize, s1: usize) -> Result<(usize, usize)> {
        let d = self
            .segs
            .as_ref()
            .ok_or_else(|| Error::Corrupt("no segment directory".into()))?;
        let (off, len) = self.column_span(b, c);
        let before: usize = (0..s0).map(|s| d.frame_len(b, s, c)).sum();
        let span: usize = (s0..s1).map(|s| d.frame_len(b, s, c)).sum();
        if before + span > len {
            return Err(Error::Corrupt(format!(
                "{}.seg: block {b} column {c} frames end past the column",
                self.perm.name()
            )));
        }
        Ok((off + before, span))
    }

    /// Ask the kernel for the frames of segments `[s0, s1)` of block `b` in the columns of
    /// `mask`, unless the cache holds them. Returns whether it lacked one.
    pub(crate) fn prefetch_segments(
        &self,
        cache: &BlockCache,
        b: usize,
        (s0, s1): (usize, usize),
        mask: ColMask,
    ) -> bool {
        let mask = mask & ALL_COLS;
        let missing = (s0..s1).any(|s| !cache.has_segment(self, b, s, mask));
        if !missing || !io_hints() {
            return missing;
        }
        if let Some(data) = self.data.as_ref() {
            for c in (0..4).filter(|c| mask & (1 << c) != 0) {
                if let Ok((off, len)) = self.frames_span(b, c, s0, s1) {
                    will_need(IoFile::of_perm(self.perm), data, off, len);
                }
            }
        }
        true
    }

    /// Whether the cache holds what a reader of the rows of block `b` in `[lo, hi]`
    /// decodes, the segments of [`segment_span`](Self::segment_span) or the whole columns
    /// in `mask`.
    pub(crate) fn range_cached(
        &self,
        cache: &BlockCache,
        b: usize,
        lo: &Key,
        hi: &Key,
        mask: ColMask,
    ) -> bool {
        match self.read_span(cache, b, lo, hi, mask) {
            Some((s0, s1)) => (s0..s1).all(|s| cache.has_segment(self, b, s, mask)),
            None => cache.has_cols(self, b, mask),
        }
    }

    /// [`prefetch`](Self::prefetch) of what a reader of the rows of block `b` in
    /// `[lo, hi]` decodes: the segments of [`segment_span`](Self::segment_span), or the
    /// whole columns in `mask`.
    pub(crate) fn prefetch_range(
        &self,
        cache: &BlockCache,
        b: usize,
        lo: &Key,
        hi: &Key,
        mask: ColMask,
    ) -> bool {
        match self.read_span(cache, b, lo, hi, mask) {
            Some(span) => self.prefetch_segments(cache, b, span, mask),
            None => self.prefetch(cache, b, mask),
        }
    }

    /// Decode segment `s` of column `c` of block `b`, asking the kernel for its bytes
    /// first when `hint`.
    pub fn decode_segment(&self, b: usize, c: usize, s: usize, hint: bool) -> Result<Col> {
        let data = self
            .data
            .as_ref()
            .ok_or_else(|| Error::Corrupt("no data".into()))?;
        let (off, len) = self.frames_span(b, c, s, s + 1)?;
        if hint {
            will_need(IoFile::of_perm(self.perm), data, off, len);
        }
        let bytes = data.get(off..off + len).ok_or_else(|| {
            Error::Corrupt(format!(
                "{}.dat: block {b} ends past the end of the file",
                self.perm.name()
            ))
        })?;
        let (r0, r1) = self.segment_rows(b, s);
        crate::ioacct::column_decoded(self.perm, r1 - r0, len);
        if crate::ioacct::profiling() {
            crate::ioacct::touch(IoFile::of_perm(self.perm), data.as_ptr(), off, len);
            let _wait = crate::ioacct::timer(crate::ioacct::Phase::IndexWait);
            crate::ioacct::fault_in(bytes);
        }
        let _decode = crate::ioacct::timer(crate::ioacct::Phase::IndexDecode);
        decode_column(bytes, r1 - r0)
    }

    /// The encoded bytes of block `b` (its four columns).
    pub(crate) fn raw_block(&self, b: usize) -> Result<&[u8]> {
        let m = &self.blocks[b];
        let len: usize = m.col_len.iter().map(|&l| l as usize).sum();
        self.data
            .as_ref()
            .and_then(|d| d.get(m.offset as usize..m.offset as usize + len))
            .ok_or_else(|| {
                Error::Corrupt(format!(
                    "{}.dat: block {b} ends past the end of the file",
                    self.perm.name()
                ))
            })
    }

    pub fn decode_block(&self, b: usize) -> Result<Block> {
        let m = &self.blocks[b];
        let mut cols: [Col; 4] = std::array::from_fn(|_| empty_col());
        for (c, col) in cols.iter_mut().enumerate() {
            *col = self.decode_col(b, c)?;
        }
        Ok(Block {
            cols,
            rows: m.rows as usize,
        })
    }

    /// Ask the kernel to start reading the columns in `mask` of block `b` (and those
    /// between them), unless the cache holds them decoded. A reader that knows the
    /// blocks it will visit asks for all of them first, so that a cold server reads them
    /// in parallel instead of one after another (see [`io_hints`]). Returns whether the
    /// cache lacked a column of `mask` in block `b`, whether or not hints are on, so that
    /// a caller can skip its other cold-path work for blocks that are warm.
    pub fn prefetch(&self, cache: &BlockCache, b: usize, mask: ColMask) -> bool {
        let mask = mask & ALL_COLS;
        if mask == 0 || cache.has_cols(self, b, mask) {
            return false;
        }
        if !io_hints() {
            return true;
        }
        if let (Some(data), Some((from, to))) = (self.data.as_ref(), self.span(b, mask)) {
            will_need(IoFile::of_perm(self.perm), data, from, to - from);
        }
        true
    }

    /// The bytes of the file that hold the columns in `mask` of block `b` (and the columns
    /// between them): `[from, to)`.
    fn span(&self, b: usize, mask: ColMask) -> Option<(usize, usize)> {
        let mask = mask & ALL_COLS;
        let m = self.blocks.get(b)?;
        if mask == 0 {
            return None;
        }
        let first = mask.trailing_zeros() as usize;
        let last = 7 - mask.leading_zeros() as usize;
        let at = |c: usize| {
            m.offset as usize + m.col_len[..c].iter().map(|&l| l as usize).sum::<usize>()
        };
        Some((at(first), at(last + 1)))
    }

    /// Decode one column of a block (columns are compressed separately).
    pub fn decode_col(&self, b: usize, c: usize) -> Result<Col> {
        self.decode_col_hinted(b, c, true)
    }

    /// [`decode_col`](Self::decode_col), asking the kernel for the column's bytes first
    /// only when `hint`: a pass that has read ahead (see [`ReadAhead`]) has asked already.
    pub fn decode_col_hinted(&self, b: usize, c: usize, hint: bool) -> Result<Col> {
        let m = &self.blocks[b];
        let data = self
            .data
            .as_ref()
            .ok_or_else(|| Error::Corrupt("no data".into()))?;
        let off = m.offset as usize + m.col_len[..c].iter().map(|&l| l as usize).sum::<usize>();
        // this column and the block's later ones, which a scan usually decodes next: one
        // read on a cold file instead of a page fault per page
        if hint {
            will_need(
                IoFile::of_perm(self.perm),
                data,
                off,
                m.col_len[c..].iter().map(|&l| l as usize).sum::<usize>(),
            );
        }
        let bytes = data.get(off..off + m.col_len[c] as usize).ok_or_else(|| {
            Error::Corrupt(format!(
                "{}.dat: block {b} ends past the end of the file",
                self.perm.name()
            ))
        })?;
        crate::ioacct::column_decoded(self.perm, m.rows as usize, bytes.len());
        if crate::ioacct::profiling() {
            crate::ioacct::touch(IoFile::of_perm(self.perm), data.as_ptr(), off, bytes.len());
            // the wait for the pages, apart from the decoding
            let _wait = crate::ioacct::timer(crate::ioacct::Phase::IndexWait);
            crate::ioacct::fault_in(bytes);
        }
        let _decode = crate::ioacct::timer(crate::ioacct::Phase::IndexDecode);
        match &self.segs {
            None => decode_column(bytes, m.rows as usize),
            Some(d) => {
                // every frame into one buffer, as the block cache keeps it
                let rows = m.rows as usize;
                let mut col: Box<[u64]> = vec![0u64; rows].into_boxed_slice();
                let mut at = 0usize;
                for (s, out) in col.chunks_mut(d.seg_rows).enumerate() {
                    let len = d.frame_len(b, s, c);
                    let frame = bytes.get(at..at + len).ok_or_else(|| {
                        Error::Corrupt(format!(
                            "{}.seg: block {b} column {c} frames end past the column",
                            self.perm.name()
                        ))
                    })?;
                    decode_frame_into(frame, out)?;
                    at += len;
                }
                if at != bytes.len() {
                    return Err(Error::Corrupt(format!(
                        "{}.seg: block {b} column {c} frames cover {at} of {} bytes",
                        self.perm.name(),
                        bytes.len()
                    )));
                }
                Ok(Col::new(col))
            }
        }
    }

    /// Block index range `[lo, hi)` that may contain keys with this prefix
    /// (QLever-style block skipping on in-RAM first/last keys).
    pub fn block_range(&self, prefix: &[u64]) -> (usize, usize) {
        self.key_block_range(&pad(prefix, 0), &pad(prefix, u64::MAX))
    }

    /// Block index range `[lo, hi)` that may contain keys in `[lo_key, hi_key]`.
    pub fn key_block_range(&self, lo_key: &Key, hi_key: &Key) -> (usize, usize) {
        let lo = self.blocks.partition_point(|b| b.last < *lo_key);
        let hi = self.blocks.partition_point(|b| b.first <= *hi_key);
        (lo, hi.max(lo))
    }
}

#[inline]
pub fn pad(prefix: &[u64], fill: u64) -> Key {
    let mut k = [fill; 4];
    k[..prefix.len()].copy_from_slice(prefix);
    k
}

pub(crate) fn next_uid() -> u64 {
    use std::sync::atomic::{AtomicU64, Ordering};
    static UID: AtomicU64 = AtomicU64::new(1);
    UID.fetch_add(1, Ordering::Relaxed)
}

/// Cache of decoded block columns, keyed by (permutation instance, block and segment,
/// column and namespace): readers decode and keep only the columns they use.
///
/// The second key field holds the block above its low 7 bits and, in them, the segment
/// plus one, or [`WHOLE`] for the whole column. The third holds the column in its low 8
/// bits and the namespace of the store that decoded it above them. A permutation instance (`uid`) is unique in the
/// process, so the namespace only attributes an entry to a dataset. It never separates
/// entries that would otherwise collide.
type ColumnCache = quick_cache::sync::Cache<(u64, u32, u32), Col, BlockWeighter>;

/// The segment field of the key of a whole column (see [`ColumnCache`]).
const WHOLE: u32 = 0;
/// Bits of the segment field in the second key field.
const SEG_KEY_BITS: u32 = 7;

/// The decoded-column storage that stores can share. A server holds one for all its
/// datasets, and each store reads through its own [`BlockCache`] view of it.
pub struct SharedBlocks {
    cache: Arc<ColumnCache>,
}

/// What one namespace holds in a shared cache.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct NamespaceUsage {
    pub bytes: u64,
    pub entries: u64,
}

impl SharedBlocks {
    pub fn new(bytes: u64) -> SharedBlocks {
        SharedBlocks {
            cache: Arc::new(new_column_cache(bytes)),
        }
    }

    /// The bytes the cache may hold.
    pub fn capacity(&self) -> u64 {
        self.cache.capacity()
    }

    /// The bytes it holds now, for all namespaces.
    pub fn bytes(&self) -> u64 {
        self.cache.weight()
    }

    pub fn entries(&self) -> usize {
        self.cache.len()
    }

    /// Bytes and entries by namespace, in one pass over the cache.
    pub fn usage(&self) -> std::collections::HashMap<u32, NamespaceUsage> {
        let mut out: std::collections::HashMap<u32, NamespaceUsage> = Default::default();
        for ((_, _, c), v) in self.cache.iter() {
            let u = out.entry(c >> 8).or_default();
            u.bytes += (v.len() * 8).max(1) as u64;
            u.entries += 1;
        }
        out
    }
}

/// The cache of decoded columns keeps quick_cache's default of four shards per CPU.
/// That is safe here, unlike in the result and SERVICE caches, because quick_cache halves
/// the shard count until each shard expects at least 32 items, and the expected items
/// are the budget over one column's bytes, or 64 for a small budget. A shard therefore
/// holds about 32 columns, or half the budget when the floor of 64 applies, so a column
/// of [`BLOCK_ROWS`] ids fits its shard in every budget of 1 MiB or more.
fn new_column_cache(bytes: u64) -> ColumnCache {
    quick_cache::sync::Cache::with_weighter(
        (bytes / (BLOCK_ROWS as u64 * 8)).max(64) as usize,
        bytes,
        BlockWeighter,
    )
}

/// A store's view of the cache of decoded block columns. Hits and misses are the view's
/// own. A view made by [`BlockCache::new`] owns its storage, and a view made by
/// [`BlockCache::attach`] shares it with the other views of a [`SharedBlocks`].
pub struct BlockCache {
    cache: Arc<ColumnCache>,
    /// the namespace in the keys of the entries this view decodes
    ns: u32,
    /// the storage is shared with other views, so `bytes` and `entries` count the
    /// namespace's entries only
    shared: bool,
    hits: std::sync::atomic::AtomicU64,
    misses: std::sync::atomic::AtomicU64,
    /// decoded blocks are added (false: a read-through view, see [`BlockCache::read_through`])
    fill: bool,
    #[cfg(test)]
    decode_hook: Option<Arc<dyn Fn() + Send + Sync>>,
}

#[cfg(test)]
thread_local! {
    static DECODE_POOL_CALLS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    /// [`BlockCache::has_cols`] calls on this thread
    static HAS_COLS_CALLS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// The threads that decode blocks ahead of a reader (see [`BlockCache::warm`]). They are
/// not the global pool, so a parallel scan that uses it does not queue point lookups. The
/// pool starts at the first request that decodes ahead, not with a cache, so a process
/// that never does holds no threads. `None` when the threads could not be started, and the
/// caller then decodes on its own thread.
fn decode_pool() -> Option<&'static rayon::ThreadPool> {
    #[cfg(test)]
    DECODE_POOL_CALLS.with(|n| n.set(n.get() + 1));
    static POOL: std::sync::OnceLock<Option<rayon::ThreadPool>> = std::sync::OnceLock::new();
    POOL.get_or_init(|| {
        let cpus = std::thread::available_parallelism().map_or(2, |n| n.get());
        rayon::ThreadPoolBuilder::new()
            .num_threads((cpus / 2).clamp(2, 8))
            .thread_name(|i| format!("sparkles-decode-{i}"))
            .build()
            .ok()
    })
    .as_ref()
}

/// The smallest budget of one warm request in a cache of at least four times its size,
/// see [`warm_budget`].
const WARM_FLOOR: u64 = 64 << 20;

/// The most decoded bytes one prefetch request may add to a cache of `capacity` bytes: an
/// eighth of the cache, so that one request cannot push out what the other queries hold,
/// but at least [`WARM_FLOOR`] or a quarter of the cache, whichever is smaller. A request
/// over its budget warms nothing, so with an eighth alone the 256 MiB cache of the `small`
/// memory profile declined the parallel decode of a cold `star-lookup` at 10.5M triples,
/// which the 1 GiB default admits, and the query took 1.6 to 1.7 times as long (roadmap
/// item 97). Caches of 512 MiB and more keep the eighth.
fn warm_budget(capacity: u64) -> u64 {
    (capacity / 8).max(WARM_FLOOR.min(capacity / 4))
}

#[derive(Clone)]
struct BlockWeighter;
impl quick_cache::Weighter<(u64, u32, u32), Col> for BlockWeighter {
    fn weight(&self, _k: &(u64, u32, u32), v: &Col) -> u64 {
        (v.len() * 8).max(1) as u64
    }
}

fn empty_col() -> Col {
    static EMPTY: std::sync::OnceLock<Col> = std::sync::OnceLock::new();
    EMPTY.get_or_init(|| Col::from(Vec::new())).clone()
}

impl BlockCache {
    /// The key of column `c` of block `b`, whole (`seg` [`WHOLE`]) or of segment
    /// `seg - 1`.
    #[inline]
    fn key(&self, idx: &PermIndex, b: usize, c: usize, seg: u32) -> (u64, u32, u32) {
        debug_assert!(seg < 1 << SEG_KEY_BITS);
        (
            idx.uid,
            (b as u32) << SEG_KEY_BITS | seg,
            self.ns << 8 | c as u32,
        )
    }

    /// Whether the cache holds the first column of `mask` of block `b` whole, one lookup
    /// that tells a reader of a few rows to use the whole columns.
    #[inline]
    pub(crate) fn holds_whole(&self, idx: &PermIndex, b: usize, mask: ColMask) -> bool {
        let c = (mask & ALL_COLS).trailing_zeros() as usize;
        c < 4 && self.cache.peek(&self.key(idx, b, c, WHOLE)).is_some()
    }

    /// Whether the columns in `mask` of segment `s` of block `b` are in the cache, as the
    /// segment or in the whole column, without counting a hit or miss.
    pub(crate) fn has_segment(&self, idx: &PermIndex, b: usize, s: usize, mask: ColMask) -> bool {
        (0..4).all(|c| {
            mask & (1 << c) == 0
                || self
                    .cache
                    .peek(&self.key(idx, b, c, s as u32 + 1))
                    .is_some()
                || self.cache.peek(&self.key(idx, b, c, WHOLE)).is_some()
        })
    }

    /// Segment `s` of block `b` with the columns in `mask` (the others are empty), as a
    /// block of the segment's rows. A column the cache holds whole is cut from it, and a
    /// missing one is decoded from its frame alone. Either is kept as the segment.
    pub fn get_segment(
        &self,
        idx: &PermIndex,
        b: usize,
        s: usize,
        mask: ColMask,
        hint: bool,
    ) -> Result<Block> {
        self.segment_counted(idx, b, s, mask, hint, true)
    }

    /// [`get_segment`](Self::get_segment), counting a decode toward the block's limit of
    /// segments read one by one when `count` (a prefetch counts its segments itself).
    fn segment_counted(
        &self,
        idx: &PermIndex,
        b: usize,
        s: usize,
        mask: ColMask,
        hint: bool,
        count: bool,
    ) -> Result<Block> {
        let (r0, r1) = idx.segment_rows(b, s);
        let mut decoded = false;
        let mut cols: [Col; 4] = std::array::from_fn(|_| empty_col());
        for (c, col) in cols.iter_mut().enumerate() {
            if mask & (1 << c) == 0 {
                continue;
            }
            let key = self.key(idx, b, c, s as u32 + 1);
            if let Some(v) = self.cache.get(&key) {
                self.hits.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                *col = v;
                continue;
            }
            if let Some(whole) = self.cache.get(&self.key(idx, b, c, WHOLE)) {
                // cut once and kept, so that later requests find the segment
                self.hits.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                let v = Col::from(whole[r0..r1].to_vec());
                if self.fill {
                    self.cache.insert(key, v.clone());
                }
                *col = v;
                continue;
            }
            self.misses
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            #[cfg(test)]
            if let Some(hook) = &self.decode_hook {
                hook();
            }
            let v = idx.decode_segment(b, c, s, hint)?;
            if self.fill {
                self.cache.insert(key, v.clone());
            }
            *col = v;
            decoded = true;
        }
        if decoded
            && count
            && let Some(d) = &idx.segs
        {
            d.note_sparse_read(b);
        }
        Ok(Block {
            cols,
            rows: r1 - r0,
        })
    }

    /// A cache of its own, of at most `bytes`.
    pub fn new(bytes: u64) -> BlockCache {
        BlockCache {
            cache: Arc::new(new_column_cache(bytes)),
            ns: 0,
            shared: false,
            hits: Default::default(),
            misses: Default::default(),
            fill: true,
            #[cfg(test)]
            decode_hook: None,
        }
    }

    /// A view of `shared` whose entries belong to namespace `ns` (at most 2^24 - 1).
    pub fn attach(shared: &SharedBlocks, ns: u32) -> BlockCache {
        BlockCache {
            cache: shared.cache.clone(),
            ns: ns & 0xff_ffff,
            shared: true,
            hits: Default::default(),
            misses: Default::default(),
            fill: true,
            #[cfg(test)]
            decode_hook: None,
        }
    }

    /// A view of the same cache that uses its blocks but never adds any: for one-off
    /// full scans (exports, backups) that would otherwise evict the blocks queries use.
    /// Its hit and miss counts are its own.
    pub fn read_through(&self) -> BlockCache {
        BlockCache {
            cache: self.cache.clone(),
            ns: self.ns,
            shared: self.shared,
            hits: Default::default(),
            misses: Default::default(),
            fill: false,
            #[cfg(test)]
            decode_hook: None,
        }
    }

    /// A block with all four columns.
    pub fn get(&self, idx: &PermIndex, b: usize) -> Result<Block> {
        self.get_cols(idx, b, ALL_COLS)
    }

    /// A block with the columns in `mask` (the others are empty).
    pub fn get_cols(&self, idx: &PermIndex, b: usize, mask: ColMask) -> Result<Block> {
        self.get_cols_hinted(idx, b, mask, true)
    }

    /// [`get_cols`](Self::get_cols) for a pass that has read the block ahead (`hint`
    /// false, see [`ReadAhead`]).
    pub fn get_cols_hinted(
        &self,
        idx: &PermIndex,
        b: usize,
        mask: ColMask,
        hint: bool,
    ) -> Result<Block> {
        let mut cols: [Col; 4] = std::array::from_fn(|_| empty_col());
        for (c, col) in cols.iter_mut().enumerate() {
            if mask & (1 << c) == 0 {
                continue;
            }
            let key = self.key(idx, b, c, WHOLE);
            *col = match self.cache.get(&key) {
                Some(v) => {
                    self.hits.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                    v
                }
                None => {
                    self.misses
                        .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                    #[cfg(test)]
                    if let Some(hook) = &self.decode_hook {
                        hook();
                    }
                    let v = idx.decode_col_hinted(b, c, hint)?;
                    if self.fill {
                        self.cache.insert(key, v.clone());
                    }
                    v
                }
            };
        }
        Ok(Block {
            cols,
            rows: idx.blocks[b].rows as usize,
        })
    }

    /// A block with the columns in `mask` if the cache holds all of them decoded, counting
    /// a hit for each, and nothing otherwise (no miss, no hit, no decode). A pass uses it
    /// to skip its read-ahead bookkeeping for warm blocks.
    pub fn get_cols_cached(&self, idx: &PermIndex, b: usize, mask: ColMask) -> Option<Block> {
        let mut cols: [Col; 4] = std::array::from_fn(|_| empty_col());
        let mut found = 0u64;
        for (c, col) in cols.iter_mut().enumerate() {
            if mask & (1 << c) == 0 {
                continue;
            }
            *col = self.cache.get(&self.key(idx, b, c, WHOLE))?;
            found += 1;
        }
        self.hits
            .fetch_add(found, std::sync::atomic::Ordering::Relaxed);
        Some(Block {
            cols,
            rows: idx.blocks[b].rows as usize,
        })
    }

    /// Decode the columns in `mask` of `blocks` that the cache lacks, in parallel on a pool
    /// of its own, and keep them. A scan or join that has asked the kernel for these
    /// blocks decodes them one by one otherwise, and on a lookup that touches dozens of
    /// blocks for a few rows each the decoding is most of the time. Only columns the reader
    /// decodes anyway are decoded. Does nothing for a read-through view, which keeps no
    /// blocks, for few columns, or when the distinct missing decoded bytes exceed cache
    /// capacity. Checks stop optional work cooperatively. Already-started decodes may finish.
    pub fn warm(
        &self,
        idx: &PermIndex,
        blocks: &[usize],
        mask: ColMask,
        check: &(dyn Fn() -> Result<()> + Sync),
    ) -> Result<()> {
        check()?;
        let mut work = self.warm_columns(idx);
        for &b in blocks {
            check()?;
            work.add(b, mask);
        }
        work.finish(check)
    }

    /// A bounded candidate set shared with join prefetch, so even enumeration stops
    /// retaining work once the distinct missing columns exceed the decoded byte budget.
    pub(crate) fn warm_columns<'a>(&'a self, idx: &'a PermIndex) -> WarmColumns<'a> {
        WarmColumns {
            cache: self,
            idx,
            work: Vec::new(),
            seen: Default::default(),
            bytes: 0,
            segments: 0,
            budget: warm_budget(self.cache.capacity()),
            eligible: self.fill,
        }
    }

    #[cfg(test)]
    pub(crate) fn evict_column(&self, idx: &PermIndex, b: usize, c: u8) {
        assert!(
            self.cache
                .remove(&self.key(idx, b, c as usize, WHOLE))
                .is_some()
        );
    }

    /// Whether the columns in `mask` of block `b` are decoded in the cache, without
    /// decoding them or counting a hit or miss.
    pub fn has_cols(&self, idx: &PermIndex, b: usize, mask: ColMask) -> bool {
        #[cfg(test)]
        HAS_COLS_CALLS.with(|n| n.set(n.get() + 1));
        (0..4)
            .all(|c| mask & (1 << c) == 0 || self.cache.peek(&self.key(idx, b, c, WHOLE)).is_some())
    }

    /// The bytes this view's namespace holds. A cache of its own counts all its entries.
    pub fn bytes(&self) -> u64 {
        if !self.shared {
            return self.cache.weight();
        }
        self.usage().bytes
    }
    /// The entries this view's namespace holds.
    pub fn entries(&self) -> usize {
        if !self.shared {
            return self.cache.len();
        }
        self.usage().entries as usize
    }
    /// The bytes the underlying cache may hold, shared with other views or not.
    pub fn capacity(&self) -> u64 {
        self.cache.capacity()
    }
    /// The namespace of this view's entries.
    pub fn namespace(&self) -> u32 {
        self.ns
    }
    fn usage(&self) -> NamespaceUsage {
        let mut u = NamespaceUsage::default();
        for ((_, _, c), v) in self.cache.iter() {
            if c >> 8 == self.ns {
                u.bytes += (v.len() * 8).max(1) as u64;
                u.entries += 1;
            }
        }
        u
    }
    pub fn hits(&self) -> u64 {
        self.hits.load(std::sync::atomic::Ordering::Relaxed)
    }
    pub fn misses(&self) -> u64 {
        self.misses.load(std::sync::atomic::Ordering::Relaxed)
    }
}

/// Optional decoded columns for one permutation and one prefetch request. This is
/// an admission estimate, not a reservation against other cache users or eviction.
pub(crate) struct WarmColumns<'a> {
    cache: &'a BlockCache,
    idx: &'a PermIndex,
    /// (block, column, segment field of the key: [`WHOLE`] or the segment plus one)
    work: Vec<(usize, u8, u32)>,
    seen: rustc_hash::FxHashSet<(usize, u8, u32)>,
    bytes: u64,
    /// the segments among `work`
    segments: usize,
    /// the most decoded bytes one request may add, see [`warm_budget`]
    budget: u64,
    eligible: bool,
}

impl WarmColumns<'_> {
    pub(crate) fn add(&mut self, b: usize, mask: ColMask) {
        for c in 0..4u8 {
            if mask & (1 << c) == 0
                || self
                    .cache
                    .cache
                    .peek(&self.cache.key(self.idx, b, c as usize, WHOLE))
                    .is_some()
            {
                continue;
            }
            self.push(b, c, WHOLE, self.idx.blocks[b].rows as u64);
        }
    }

    /// Add segments `[s0, s1)` of block `b` in the columns of `mask`, those the cache
    /// holds neither as segments nor whole.
    pub(crate) fn add_segments(&mut self, b: usize, (s0, s1): (usize, usize), mask: ColMask) {
        for s in s0..s1 {
            for c in 0..4u8 {
                if mask & (1 << c) == 0 || self.cache.has_segment(self.idx, b, s, 1 << c) {
                    continue;
                }
                let (r0, r1) = self.idx.segment_rows(b, s);
                self.push(b, c, s as u32 + 1, (r1 - r0) as u64);
            }
        }
    }

    fn push(&mut self, b: usize, c: u8, seg: u32, rows: u64) {
        if !self.eligible {
            return;
        }
        let key = (b, c, seg);
        if self.seen.contains(&key) {
            return;
        }
        // Match BlockWeighter exactly, including partial tails and empty columns.
        // Overflow declines the entire optional request before any decode occurs.
        let next = rows
            .checked_mul(8)
            .and_then(|weight| self.bytes.checked_add(weight.max(1)));
        let Some(bytes) = next.filter(|&n| n <= self.budget) else {
            self.eligible = false;
            self.work = Vec::new();
            self.seen = Default::default();
            self.bytes = 0;
            self.segments = 0;
            return;
        };
        self.bytes = bytes;
        self.segments += usize::from(seg != WHOLE);
        self.seen.insert(key);
        self.work.push(key);
    }

    pub(crate) fn finish(self, check: &(dyn Fn() -> Result<()> + Sync)) -> Result<()> {
        use rayon::prelude::*;
        check()?;
        // Below this the hand-off costs about what decoding saves: sixteen whole columns,
        // or 32 segments, which decode in a few microseconds each. Capacity admission
        // never warms a partial prefix of an oversized set.
        const MIN_COLUMNS: usize = 16;
        const MIN_SEGMENTS: usize = 32;
        let whole = self.work.len() - self.segments;
        if self.eligible && (whole >= MIN_COLUMNS || self.segments >= MIN_SEGMENTS) {
            let decode = |&(b, c, seg): &(usize, u8, u32)| -> Result<()> {
                check()?;
                // Only check errors propagate: an advisory decode error remains
                // for the consuming reader to encounter if it needs this column.
                let _ = if seg == WHOLE {
                    self.cache.get_cols_hinted(self.idx, b, 1 << c, false)
                } else {
                    self.cache
                        .segment_counted(self.idx, b, seg as usize - 1, 1 << c, false, false)
                };
                Ok(())
            };
            match decode_pool() {
                Some(pool) => pool.install(|| self.work.par_iter().try_for_each(decode))?,
                None => self.work.iter().try_for_each(decode)?,
            }
            // each segment decoded counts once toward its block's limit, whatever its
            // columns (the work lists a segment's columns one after another)
            if let Some(d) = &self.idx.segs {
                let mut last = None;
                for &(b, _, seg) in &self.work {
                    if seg != WHOLE && last != Some((b, seg)) {
                        last = Some((b, seg));
                        d.note_sparse_read(b);
                    }
                }
            }
        }
        // Cancellation during the final already-started decode must not be hidden.
        check()
    }
}

impl PermIndex {
    #[cfg(test)]
    pub(crate) fn trace_hints(&self) -> HintTrace {
        let spans = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
        let mapping = self.data.as_ref().unwrap().as_ptr() as usize;
        let previous = HINT_TRACE.with(|trace| trace.replace(Some((mapping, spans.clone()))));
        HintTrace { spans, previous }
    }

    /// Exact number of base rows with the given key prefix.
    pub fn count(&self, cache: &BlockCache, prefix: &[u64]) -> Result<u64> {
        if prefix.is_empty() {
            return Ok(self.rows);
        }
        self.count_between(cache, &pad(prefix, 0), &pad(prefix, u64::MAX))
    }

    /// Exact number of base rows with keys in `[lo_key, hi_key]` (at most two decodes).
    pub fn count_between(&self, cache: &BlockCache, lo_key: &Key, hi_key: &Key) -> Result<u64> {
        let (lo, hi) = self.key_block_range(lo_key, hi_key);
        if lo >= hi {
            return Ok(0);
        }
        let (lo_key, hi_key) = (*lo_key, *hi_key);
        // the blocks at the two ends of the range are decoded one after the other below:
        // ask for both first, so that a cold server reads them together
        let bc = bound_cols(&lo_key, &hi_key);
        let partial = |e: usize| {
            let m = &self.blocks[e];
            !(m.first >= lo_key && m.last <= hi_key)
        };
        // each end block that bounds the range: a cached one is taken from the cache here,
        // once, and a cold one is asked for before either is decoded
        let ends = if hi - 1 == lo { 1 } else { 2 };
        let mut cached: [Option<Block>; 2] = [None, None];
        let spans = [lo, hi - 1].map(|e| self.read_span(cache, e, &lo_key, &hi_key, bc));
        for ((slot, e), span) in cached.iter_mut().zip([lo, hi - 1]).zip(spans).take(ends) {
            if partial(e) {
                if let Some(span) = span {
                    self.prefetch_segments(cache, e, span, bc);
                    continue;
                }
                *slot = cache.get_cols_cached(self, e, bc);
                if slot.is_none() {
                    self.prefetch(cache, e, bc);
                }
            }
        }
        let mut total = 0u64;
        // interior blocks fully contained in the range need no decoding
        let mut b = lo;
        while b < hi {
            let m = &self.blocks[b];
            if m.first >= lo_key && m.last <= hi_key {
                // run of fully covered blocks
                let mut e = b;
                while e + 1 < hi && {
                    let n = &self.blocks[e + 1];
                    n.first >= lo_key && n.last <= hi_key
                } {
                    e += 1;
                }
                total += self.blocks[e].row_start + self.blocks[e].rows as u64 - m.row_start;
                b = e + 1;
            } else if let Some(span) =
                if b == lo { spans[0] } else { spans[1] }.filter(|_| b == lo || b == hi - 1)
            {
                self.for_each_segment_rows(cache, b, span, &lo_key, &hi_key, bc, |_, s, e| {
                    total += (e - s) as u64;
                    Ok(true)
                })?;
                b += 1;
            } else {
                let held = if b == lo {
                    cached[0].take()
                } else if b == hi - 1 {
                    cached[1].take()
                } else {
                    None
                };
                let blk = match held {
                    Some(blk) => blk,
                    None => cache.get_cols(self, b, bc)?,
                };
                let (s, e) = blk.key_range(&lo_key, &hi_key);
                total += (e - s) as u64;
                b += 1;
            }
        }
        Ok(total)
    }

    /// Estimated number of rows with a prefix, without decoding any block
    /// (midpoint between lower and upper bound, as QLever does).
    pub fn estimate(&self, prefix: &[u64]) -> u64 {
        if prefix.is_empty() {
            return self.rows;
        }
        self.estimate_between(&pad(prefix, 0), &pad(prefix, u64::MAX))
    }

    /// Estimated number of rows with keys in `[lo_key, hi_key]`, without decoding.
    pub fn estimate_between(&self, lo_key: &Key, hi_key: &Key) -> u64 {
        let (lo, hi) = self.key_block_range(lo_key, hi_key);
        if lo >= hi {
            return 0;
        }
        let upper: u64 = self.blocks[lo..hi].iter().map(|m| m.rows as u64).sum();
        let lower: u64 = self.blocks[lo..hi]
            .iter()
            .filter(|m| m.first >= *lo_key && m.last <= *hi_key)
            .map(|m| m.rows as u64)
            .sum();
        (lower + upper).div_ceil(2).max(1)
    }

    /// Visit every base row with the given prefix, block-slice at a time:
    /// `f(block, start_row, end_row)`.
    pub fn for_each_range(
        &self,
        cache: &BlockCache,
        prefix: &[u64],
        mut f: impl FnMut(&Block, usize, usize) -> Result<()>,
    ) -> Result<()> {
        self.for_each_range_until(cache, prefix, |b, s, e| f(b, s, e).map(|()| true))
    }

    /// Like [`for_each_range`](Self::for_each_range), but stops (without decoding further
    /// blocks) as soon as the callback returns `false`.
    pub fn for_each_range_until(
        &self,
        cache: &BlockCache,
        prefix: &[u64],
        f: impl FnMut(&Block, usize, usize) -> Result<bool>,
    ) -> Result<()> {
        self.for_each_key_range_until(cache, &pad(prefix, 0), &pad(prefix, u64::MAX), f)
    }

    /// Visit every base row whose key lies in `[lo_key, hi_key]`, block-slice at a time,
    /// until the callback returns `false`. Blocks outside the range are never decoded.
    pub fn for_each_key_range_until(
        &self,
        cache: &BlockCache,
        lo_key: &Key,
        hi_key: &Key,
        f: impl FnMut(&Block, usize, usize) -> Result<bool>,
    ) -> Result<()> {
        self.for_each_key_range_cols(cache, lo_key, hi_key, ALL_COLS, f)
    }

    /// [`for_each_key_range_until`](Self::for_each_key_range_until) decoding only the
    /// columns in `mask` (plus the leading columns needed to find a partial range).
    pub fn for_each_key_range_cols(
        &self,
        cache: &BlockCache,
        lo_key: &Key,
        hi_key: &Key,
        mask: ColMask,
        f: impl FnMut(&Block, usize, usize) -> Result<bool>,
    ) -> Result<()> {
        self.for_each_key_range_masked(cache, lo_key, hi_key, |_| mask, f)
    }

    /// [`for_each_key_range_cols`](Self::for_each_key_range_cols) with the columns to
    /// decode chosen per block (by its number).
    pub fn for_each_key_range_masked(
        &self,
        cache: &BlockCache,
        lo_key: &Key,
        hi_key: &Key,
        mask_of: impl Fn(usize) -> ColMask,
        mut f: impl FnMut(&Block, usize, usize) -> Result<bool>,
    ) -> Result<()> {
        let (lo, hi) = self.key_block_range(lo_key, hi_key);
        let bounds = bound_cols(lo_key, hi_key);
        let mut ahead = ReadAhead::default();
        let columns = |b: usize| {
            let m = &self.blocks[b];
            let mask = mask_of(b);
            if m.first >= *lo_key && m.last <= *hi_key {
                mask
            } else {
                mask | bounds
            }
        };
        for b in lo..hi {
            let m = &self.blocks[b];
            let mask = columns(b);
            let whole = m.first >= *lo_key && m.last <= *hi_key;
            if !whole && let Some(span) = self.read_span(cache, b, lo_key, hi_key, mask) {
                // a few rows of the block: the segments that can hold them, one by one
                if !self.for_each_segment_rows(cache, b, span, lo_key, hi_key, mask, &mut f)? {
                    break;
                }
                continue;
            }
            let blk = match cache.get_cols_cached(self, b, mask) {
                Some(blk) => blk,
                None => {
                    let hint = ahead.before(self, cache, b, hi, &columns);
                    cache.get_cols_hinted(self, b, mask, hint)?
                }
            };
            let go_on = if whole {
                f(&blk, 0, blk.len())?
            } else {
                let (s, e) = blk.key_range(lo_key, hi_key);
                s >= e || f(&blk, s, e)?
            };
            if !go_on {
                break;
            }
        }
        Ok(())
    }

    /// Visit the rows with keys in `[lo_key, hi_key]` of segments `[s0, s1)` of block
    /// `b`, one segment at a time, decoding the columns in `mask` (which must include
    /// the bound columns of the range): `f(segment, start_row, end_row)` with rows of the
    /// segment, until it returns `false`. Returns whether it never did.
    #[allow(clippy::too_many_arguments)]
    fn for_each_segment_rows(
        &self,
        cache: &BlockCache,
        b: usize,
        (s0, s1): (usize, usize),
        lo_key: &Key,
        hi_key: &Key,
        mask: ColMask,
        mut f: impl FnMut(&Block, usize, usize) -> Result<bool>,
    ) -> Result<bool> {
        for s in s0..s1 {
            let blk = cache.get_segment(self, b, s, mask, true)?;
            let (a, z) = blk.key_range(lo_key, hi_key);
            if a < z && !f(&blk, a, z)? {
                return Ok(false);
            }
        }
        Ok(true)
    }

    /// Point lookup of a full key.
    pub fn contains(&self, cache: &BlockCache, key: &Key) -> Result<bool> {
        let b = self.blocks.partition_point(|m| m.last < *key);
        if b >= self.blocks.len() || self.blocks[b].first > *key {
            return Ok(false);
        }
        if let Some(span) = self.read_span(cache, b, key, key, ALL_COLS) {
            let mut found = false;
            self.for_each_segment_rows(cache, b, span, key, key, ALL_COLS, |_, _, _| {
                found = true;
                Ok(false)
            })?;
            return Ok(found);
        }
        let blk = cache.get(self, b)?;
        let (s, e) = blk.prefix_range(key);
        Ok(s < e)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A full column fits its shard of the column cache in budgets from 1 MiB to 4 GiB,
    /// whatever the host's CPU count.
    #[test]
    fn column_cache_keeps_a_full_column() {
        for mib in [1u64, 32, 256, 4096] {
            let c = new_column_cache(mib << 20);
            assert!(
                c.shard_capacity() * 97 / 100 >= BLOCK_ROWS as u64 * 8,
                "{mib} MiB: {} shards of {} bytes",
                c.num_shards(),
                c.shard_capacity()
            );
        }
    }

    #[test]
    fn write_read_scan() {
        let dir = tempfile::tempdir().unwrap();
        let mut w = PermWriter::create(dir.path(), Perm::Spo).unwrap();
        let mut keys = Vec::new();
        for s in 0..300u64 {
            for p in 0..10u64 {
                for o in 0..40u64 {
                    keys.push([s, p, o * 3, 0]);
                }
            }
        }
        for k in &keys {
            w.push(*k).unwrap();
            w.push(*k).unwrap(); // duplicates dropped
        }
        assert_eq!(w.finish(dir.path(), Perm::Spo).unwrap(), keys.len() as u64);
        let idx = PermIndex::open(dir.path(), Perm::Spo).unwrap();
        let cache = BlockCache::new(1 << 24);
        assert!(idx.blocks.len() > 1);
        assert_eq!(idx.count(&cache, &[]).unwrap(), keys.len() as u64);
        assert_eq!(idx.count(&cache, &[7]).unwrap(), 400);
        assert_eq!(idx.count(&cache, &[7, 3]).unwrap(), 40);
        assert_eq!(idx.count(&cache, &[7, 3, 6]).unwrap(), 1);
        assert_eq!(idx.count(&cache, &[7, 3, 7]).unwrap(), 0);
        assert_eq!(idx.count(&cache, &[1000]).unwrap(), 0);
        let mut seen = 0;
        idx.for_each_range(&cache, &[299], |b, s, e| {
            for i in s..e {
                assert_eq!(b.cols[0][i], 299);
            }
            seen += e - s;
            Ok(())
        })
        .unwrap();
        assert_eq!(seen, 400);
        assert!(idx.contains(&cache, &[5, 5, 9, 0]).unwrap());
        assert!(!idx.contains(&cache, &[5, 5, 10, 0]).unwrap());
        let est = idx.estimate(&[7]);
        assert!(est > 0);
    }

    /// The same sorted keys written segmented (format 3) and not (format 2), in blocks of
    /// irregular lengths: two full blocks, then a block ended early at a row that is not
    /// a segment boundary, then a full one and a short tail.
    fn both_layouts(keys: &[Key]) -> (tempfile::TempDir, PermIndex, tempfile::TempDir, PermIndex) {
        let write = |segmented: bool| {
            let dir = tempfile::tempdir().unwrap();
            let mut w = PermWriter::create_format(dir.path(), Perm::Spo, segmented).unwrap();
            for (i, k) in keys.iter().enumerate() {
                w.push(*k).unwrap();
                if i + 1 == 2 * BLOCK_ROWS + 5_000 {
                    w.end_block().unwrap();
                }
            }
            w.finish(dir.path(), Perm::Spo).unwrap();
            let idx = PermIndex::open(dir.path(), Perm::Spo).unwrap();
            (dir, idx)
        };
        let (d3, i3) = write(true);
        let (d2, i2) = write(false);
        assert!(i3.segmented() && !i2.segmented());
        (d3, i3, d2, i2)
    }

    /// Keys of runs of 1 to 40 rows per subject, with large id gaps, over five blocks.
    fn run_keys() -> Vec<Key> {
        let mut keys = Vec::new();
        let mut r = 0x9e3779b97f4a7c15u64;
        let mut s = 1u64;
        while keys.len() < 4 * BLOCK_ROWS + 7_000 {
            r ^= r << 13;
            r ^= r >> 7;
            r ^= r << 17;
            let n = 1 + r % 40;
            for o in 0..n {
                keys.push([s, 7 + o % 3, (r >> 20) + o * 1_000_003, (o % 2) * 5]);
            }
            s += 1 + (r >> 50);
        }
        keys.sort();
        keys.dedup();
        keys
    }

    #[test]
    fn segmented_columns_decode_whole_and_by_segment() {
        let keys = run_keys();
        let (_d3, i3, _d2, i2) = both_layouts(&keys);
        assert_eq!(i3.blocks.len(), i2.blocks.len());
        for b in 0..i3.blocks.len() {
            let n = i3.segs.as_ref().unwrap().count(b);
            assert_eq!(n, (i3.blocks[b].rows as usize).div_ceil(SEG_ROWS));
            for c in 0..4 {
                let whole = i3.decode_col(b, c).unwrap();
                assert_eq!(&*whole, &*i2.decode_col(b, c).unwrap());
                for s in 0..n {
                    let (r0, r1) = i3.segment_rows(b, s);
                    assert_eq!(&*i3.decode_segment(b, c, s, false).unwrap(), &whole[r0..r1]);
                    if c == 0 {
                        assert_eq!(i3.segment_first(b, s)[0], whole[r0]);
                    }
                }
            }
        }
        // the directory adds 48 bytes per segment
        let segs: usize = (0..i3.blocks.len())
            .map(|b| i3.segs.as_ref().unwrap().count(b))
            .sum();
        assert_eq!(
            i3.segs.as_ref().unwrap().map.len(),
            SEG_HEADER + segs * SEG_ENTRY
        );
    }

    #[test]
    fn segmented_reads_answer_as_whole_block_reads() {
        let keys = run_keys();
        let (d3, _i3, _d2, i2) = both_layouts(&keys);
        let (c3, c2) = (BlockCache::new(1 << 26), BlockCache::new(1 << 26));
        let rows = |idx: &PermIndex, cache: &BlockCache, lo: &Key, hi: &Key| {
            let mut out = Vec::new();
            idx.for_each_key_range_cols(cache, lo, hi, ALL_COLS, |b, s, e| {
                out.extend((s..e).map(|i| b.key(i)));
                Ok(true)
            })
            .unwrap();
            out
        };
        let mut r = 12345u64;
        for t in 0..400 {
            // opened afresh, so that no block has turned to whole reads and every lookup
            // reads segments where it can
            let i3 = PermIndex::open(d3.path(), Perm::Spo).unwrap();
            r ^= r << 13;
            r ^= r >> 7;
            r ^= r << 17;
            let k = keys[(r % keys.len() as u64) as usize];
            // a subject, a subject and predicate, a full key, and a span of subjects
            let (lo, hi) = match t % 4 {
                0 => (pad(&k[..1], 0), pad(&k[..1], u64::MAX)),
                1 => (pad(&k[..2], 0), pad(&k[..2], u64::MAX)),
                2 => (k, k),
                _ => (pad(&k[..1], 0), pad(&[k[0] + (r >> 54)], u64::MAX)),
            };
            let want = rows(&i2, &c2, &lo, &hi);
            assert_eq!(rows(&i3, &c3, &lo, &hi), want, "{lo:?}..{hi:?}");
            assert_eq!(i3.count_between(&c3, &lo, &hi).unwrap(), want.len() as u64);
            assert_eq!(i2.count_between(&c2, &lo, &hi).unwrap(), want.len() as u64);
            assert!(i3.contains(&c3, &k).unwrap());
            let mut absent = k;
            absent[2] += 1;
            assert_eq!(
                i3.contains(&c3, &absent).unwrap(),
                i2.contains(&c2, &absent).unwrap()
            );
        }
        // a cache holding whole columns serves segment reads, and the reverse
        let i3 = PermIndex::open(d3.path(), Perm::Spo).unwrap();
        let (lo, hi) = (
            pad(&keys[40_000][..1], 0),
            pad(&keys[40_000][..1], u64::MAX),
        );
        let warm = BlockCache::new(1 << 26);
        for b in 0..i3.blocks.len() {
            warm.get(&i3, b).unwrap();
        }
        assert_eq!(rows(&i3, &warm, &lo, &hi), rows(&i2, &c2, &lo, &hi));
    }

    #[test]
    fn a_point_lookup_decodes_one_segment_per_column() {
        let keys = run_keys();
        let (_d3, i3, _d2, _i2) = both_layouts(&keys);
        let cache = BlockCache::new(1 << 26);
        let k = keys[3 * BLOCK_ROWS / 2];
        let (lo, hi) = (pad(&k[..2], 0), pad(&k[..2], u64::MAX));
        let mut n = 0;
        i3.for_each_key_range_cols(&cache, &lo, &hi, 1 << O, |_, s, e| {
            n += e - s;
            Ok(true)
        })
        .unwrap();
        assert!(n > 0);
        // the bound columns (subject and predicate) and the object, of one or two
        // segments: a small share of the block's decoded bytes
        assert!(cache.misses() <= 6, "{}", cache.misses());
        assert!(
            cache.bytes() <= 6 * SEG_ROWS as u64 * 8,
            "{}",
            cache.bytes()
        );
        // a range over most of a block reads it whole
        let b = 1;
        let m = &i3.blocks[b];
        let (lo, hi) = (
            [m.first[0] + 1, 0, 0, 0],
            [m.last[0] - 1, u64::MAX, u64::MAX, u64::MAX],
        );
        assert_eq!(i3.segment_span(b, &lo, &hi), None);
    }

    #[test]
    fn a_block_read_by_many_segments_is_read_whole_after_a_quarter_of_them() {
        let keys = run_keys();
        let (_d3, i3, _d2, _i2) = both_layouts(&keys);
        let cache = BlockCache::new(1 << 26);
        let b = 1;
        let n = i3.segs.as_ref().unwrap().count(b);
        let limit = i3.dense_limit(b);
        assert!(limit >= 1 && limit < n);
        // one lookup in each segment of the block, in order
        let mut sparse = 0;
        for s in 0..n {
            let (r0, _) = i3.segment_rows(b, s);
            let k = i3.decode_col(b, 0).unwrap()[r0 + 1];
            let (lo, hi) = (pad(&[k], 0), pad(&[k], u64::MAX));
            if i3.read_span(&cache, b, &lo, &hi, 1).is_some() {
                sparse += 1;
            }
            let mut rows = 0;
            i3.for_each_key_range_cols(&cache, &lo, &hi, 1 << O, |_, a, z| {
                rows += z - a;
                Ok(true)
            })
            .unwrap();
            assert!(rows > 0);
        }
        // the first `limit` lookups decoded their segments, and the rest the whole block
        assert_eq!(sparse, limit);
        assert!(cache.holds_whole(&i3, b, 1));
    }

    #[test]
    fn partial_compaction_copies_or_re_encodes_blocks_by_layout() {
        let keys = run_keys();
        let (_d3, i3, _d2, i2) = both_layouts(&keys);
        for src in [&i3, &i2] {
            let dir = tempfile::tempdir().unwrap();
            let mut w = PermWriter::create(dir.path(), Perm::Spo).unwrap();
            for b in 0..src.blocks.len() {
                w.push_block(src, b).unwrap();
            }
            w.finish(dir.path(), Perm::Spo).unwrap();
            let out = PermIndex::open(dir.path(), Perm::Spo).unwrap();
            assert!(out.segmented());
            assert_eq!(out.blocks.len(), src.blocks.len());
            for b in 0..out.blocks.len() {
                assert_eq!(out.blocks[b].rows, src.blocks[b].rows);
                for c in 0..4 {
                    assert_eq!(
                        &*out.decode_col(b, c).unwrap(),
                        &*src.decode_col(b, c).unwrap()
                    );
                }
                for s in 0..out.segs.as_ref().unwrap().count(b) {
                    assert_eq!(out.segment_first(b, s), i3.segment_first(b, s));
                }
            }
        }
    }

    /// The segment size sweep of spec X07 §6.5: the size of every permutation of the
    /// generation in `SPARKLES_SEGMENT_SWEEP` (any format) encoded with no segments and
    /// with segments of 512 to 4,096 rows, directories included. Run with
    /// `cargo test --release -p sparkles-core --lib segment_size_sweep -- --ignored --nocapture`.
    #[test]
    #[ignore]
    fn segment_size_sweep() {
        let dir = std::path::PathBuf::from(std::env::var("SPARKLES_SEGMENT_SWEEP").unwrap());
        let sizes = [0usize, 512, 1024, 2048, 4096];
        let mut total = [0u64; 5];
        for &p in &Perm::ALL {
            let idx = PermIndex::open(&dir, p).unwrap();
            let mut bytes = [0u64; 5];
            let mut scratch = Vec::new();
            for b in 0..idx.blocks.len() {
                let rows = idx.blocks[b].rows as usize;
                for c in 0..4 {
                    let col = idx.decode_col(b, c).unwrap();
                    for (i, &n) in sizes.iter().enumerate() {
                        if n == 0 {
                            bytes[i] += encode_column(&col, &mut scratch).len() as u64;
                        } else {
                            let (mut out, mut lens) = (Vec::new(), Vec::new());
                            encode_segmented(&col, n, &mut scratch, &mut out, &mut lens);
                            bytes[i] += out.len() as u64;
                            if c == 0 {
                                bytes[i] += (rows.div_ceil(n) * SEG_ENTRY) as u64;
                            }
                        }
                    }
                }
            }
            println!("{} {bytes:?}", p.name());
            for i in 0..5 {
                total[i] += bytes[i];
            }
        }
        for (i, &n) in sizes.iter().enumerate() {
            println!(
                "segment rows {n}: {} bytes, {:.2}% of unsegmented",
                total[i],
                100.0 * total[i] as f64 / total[0] as f64
            );
        }
    }

    #[test]
    fn damaged_segment_directories_are_errors_not_panics() {
        let keys = run_keys();
        let (d3, _i3, _d2, _i2) = both_layouts(&keys);
        let seg = d3.path().join("spo.seg");
        let good = std::fs::read(&seg).unwrap();
        // truncated, a bad magic and an unsupported segment size refuse the open
        for bad in [
            good[..good.len() - 1].to_vec(),
            [b"SPKSEG09".as_slice(), &good[8..]].concat(),
            [&good[..8], &100u32.to_le_bytes()[..], &good[12..]].concat(),
        ] {
            std::fs::write(&seg, &bad).unwrap();
            assert!(PermIndex::open(d3.path(), Perm::Spo).is_err());
        }
        // a missing directory refuses a format 3 open
        std::fs::remove_file(&seg).unwrap();
        assert!(PermIndex::open_format(d3.path(), Perm::Spo, 3).is_err());
        // a frame length that runs past its column is a decode error
        let mut bad = good.clone();
        bad[SEG_HEADER + 32..SEG_HEADER + 36].copy_from_slice(&u32::MAX.to_le_bytes());
        std::fs::write(&seg, &bad).unwrap();
        let idx = PermIndex::open(d3.path(), Perm::Spo).unwrap();
        assert!(idx.decode_segment(0, 0, 0, false).is_err());
        assert!(idx.decode_col(0, 0).is_err());
        // frames that do not cover their column are a decode error
        let mut bad = good.clone();
        let len = u32::from_le_bytes(bad[SEG_HEADER + 32..SEG_HEADER + 36].try_into().unwrap());
        bad[SEG_HEADER + 32..SEG_HEADER + 36].copy_from_slice(&(len - 1).to_le_bytes());
        std::fs::write(&seg, &bad).unwrap();
        let idx = PermIndex::open(d3.path(), Perm::Spo).unwrap();
        assert!(idx.decode_col(0, 0).is_err());
    }

    #[test]
    fn read_ahead_covers_a_short_range_in_one_request_and_a_long_one_in_windows() {
        let dir = tempfile::tempdir().unwrap();
        let mut w = PermWriter::create(dir.path(), Perm::Spo).unwrap();
        for s in 0..(6 * BLOCK_ROWS as u64) {
            w.push([s, s % 7, s % 13, 0]).unwrap();
        }
        w.finish(dir.path(), Perm::Spo).unwrap();
        let idx = PermIndex::open(dir.path(), Perm::Spo).unwrap();
        let cache = BlockCache::new(1 << 26);
        assert_eq!(idx.blocks.len(), 6);
        let all = |_: usize| ALL_COLS;
        let enabled = io_hints() && read_window() != 0;
        let trace = idx.trace_hints();
        // a short range: the first request covers every block
        let mut ra = ReadAhead::default();
        ra.before(&idx, &cache, 0, 6, &all);
        if enabled {
            let total = idx
                .blocks
                .iter()
                .map(|m| m.col_len.iter().map(|&n| n as usize).sum::<usize>())
                .sum::<usize>();
            assert!(ra.requested > 0 && ra.requested <= 6);
            if total <= ReadAhead::FIRST.min(read_window()) {
                assert_eq!(ra.requested, 6);
                assert_eq!(ra.refill_at, usize::MAX);
            }
        } else {
            assert_eq!((ra.requested, ra.refill_at, ra.window), (0, 0, 0));
            assert!(trace.spans().is_empty());
        }
        // The same position cannot add another refill or grow the target.
        let before = (ra.requested, ra.window, trace.spans().len());
        ra.before(&idx, &cache, 0, 6, &all);
        assert_eq!((ra.requested, ra.window, trace.spans().len()), before);
        // the blocks are 100s of KB each, so a request of the 1 MiB first window stops short
        // of the sixth block, and the next one starts where it ended
        let mut ra = ReadAhead::default();
        let end = idx.blocks.len();
        ra.window = 1; // the first window is the smallest allowed: one block per request
        ra.before(&idx, &cache, 0, end, &all);
        if enabled {
            assert!(ra.requested >= 1 && ra.requested <= end);
        } else {
            assert_eq!(ra.requested, 0);
            assert!(trace.spans().is_empty());
        }
        let first = ra.requested;
        if enabled && first < end {
            assert!(ra.refill_at > 0 && ra.refill_at < end);
            ra.before(&idx, &cache, ra.refill_at, end, &all);
            assert!(ra.requested > first);
        }
        // scanning with read-ahead gives the rows of a scan without it
        let mut n = 0u64;
        idx.for_each_key_range_cols(&cache, &[0; 4], &[u64::MAX; 4], ALL_COLS, |blk, s, e| {
            let first = n;
            n += (e - s) as u64;
            assert_eq!(blk.cols[0][s], blk.key(s)[0]);
            for row in s..e {
                let value = first + (row - s) as u64;
                assert_eq!(blk.key(row), [value, value % 7, value % 13, 0]);
            }
            Ok(true)
        })
        .unwrap();
        assert_eq!(n, 6 * BLOCK_ROWS as u64);
    }

    #[test]
    fn warm_decodes_the_columns_in_parallel_and_keeps_them() {
        let dir = tempfile::tempdir().unwrap();
        let mut w = PermWriter::create(dir.path(), Perm::Spo).unwrap();
        for s in 0..(4 * BLOCK_ROWS as u64) {
            w.push([s, s % 7, s % 13, 0]).unwrap();
        }
        w.finish(dir.path(), Perm::Spo).unwrap();
        let idx = PermIndex::open(dir.path(), Perm::Spo).unwrap();
        let cache = BlockCache::new(1 << 26);
        let mask = ALL_COLS;
        let blocks: Vec<usize> = (0..idx.blocks.len()).collect();
        assert!(!cache.has_cols(&idx, 2, mask));
        // too few columns to be worth the hand-off: nothing is decoded
        cache.warm(&idx, &blocks[..2], mask, &|| Ok(())).unwrap();
        assert!(!cache.has_cols(&idx, 0, mask));
        cache.warm(&idx, &blocks, mask, &|| Ok(())).unwrap();
        for b in &blocks {
            assert!(cache.has_cols(&idx, *b, mask));
        }
        // the warmed columns equal a direct decode
        let blk = cache.get_cols(&idx, 3, mask).unwrap();
        for c in 0..4 {
            assert_eq!(&*blk.cols[c], &*idx.decode_col(3, c).unwrap());
        }
        // a read-through view keeps nothing
        let through = cache.read_through();
        let empty = BlockCache::new(1 << 26);
        empty
            .read_through()
            .warm(&idx, &blocks, mask, &|| Ok(()))
            .unwrap();
        assert!(!empty.has_cols(&idx, 0, mask));
        let _ = through;
    }

    fn warm_fixture(rows: usize) -> (tempfile::TempDir, PermIndex) {
        let dir = tempfile::tempdir().unwrap();
        let mut writer = PermWriter::create(dir.path(), Perm::Spo).unwrap();
        for i in 0..rows as u64 {
            writer.push([i, i % 7, i % 13, 0]).unwrap();
        }
        writer.finish(dir.path(), Perm::Spo).unwrap();
        let idx = PermIndex::open(dir.path(), Perm::Spo).unwrap();
        (dir, idx)
    }

    #[test]
    fn cache_construction_does_not_start_the_decode_pool() {
        let before = DECODE_POOL_CALLS.with(|n| n.get());
        let shared = SharedBlocks::new(128 << 20);
        let view = BlockCache::attach(&shared, 7);
        let _through = view.read_through();
        let _private = BlockCache::new(16 << 20);
        assert_eq!(DECODE_POOL_CALLS.with(|n| n.get()), before);
        // the pool starts when a request decodes ahead
        let (_dir, idx) = warm_fixture(8 * BLOCK_ROWS);
        view.warm(&idx, &(0..8).collect::<Vec<_>>(), ALL_COLS, &|| Ok(()))
            .unwrap();
        assert_eq!(DECODE_POOL_CALLS.with(|n| n.get()), before + 1);
        let pool = decode_pool().expect("a decode pool");
        assert!(pool.current_num_threads() >= 2);
        pool.install(|| {
            assert!(
                std::thread::current()
                    .name()
                    .unwrap()
                    .starts_with("sparkles-decode-")
            );
        });
    }

    #[test]
    fn shared_warm_peek_and_eviction_keep_namespace_ownership() {
        let (_dir, idx) = warm_fixture(4 * BLOCK_ROWS);
        let shared = SharedBlocks::new(64 << 20);
        let a = BlockCache::attach(&shared, 17);
        let b = BlockCache::attach(&shared, 23);
        let a2 = BlockCache::attach(&shared, 17);
        let blocks: Vec<_> = (0..idx.blocks.len()).collect();
        a.warm(&idx, &blocks, ALL_COLS, &|| Ok(())).unwrap();
        let mut same = a2.warm_columns(&idx);
        let mut other = b.warm_columns(&idx);
        for &block in &blocks {
            same.add(block, ALL_COLS);
            other.add(block, ALL_COLS);
        }
        assert!(same.work.is_empty(), "same namespace reuses warmed columns");
        assert_eq!(
            other.work.len(),
            16,
            "other namespace must decode its own columns"
        );
        other.finish(&|| Ok(())).unwrap();
        let per_namespace = (4 * BLOCK_ROWS * 4 * 8) as u64;
        let usage = shared.usage();
        assert_eq!(usage[&17].bytes, per_namespace);
        assert_eq!(usage[&23].bytes, per_namespace);
        assert_eq!(shared.bytes(), 2 * per_namespace);
        let held = b.get_cols(&idx, 0, 1 << 1).unwrap();
        b.evict_column(&idx, 0, 1);
        assert!(!b.has_cols(&idx, 0, 1 << 1));
        assert!(a.has_cols(&idx, 0, ALL_COLS));
        assert!(a2.has_cols(&idx, 0, ALL_COLS));
        assert_eq!(
            &*held.cols[1],
            &*idx.decode_col(0, 1).unwrap(),
            "Arc survives eviction"
        );
        let before = shared.bytes();
        b.read_through()
            .warm(&idx, &blocks, ALL_COLS, &|| Ok(()))
            .unwrap();
        assert_eq!(shared.bytes(), before);
        assert!(!b.has_cols(&idx, 0, 1 << 1));
        assert!(shared.bytes() <= shared.capacity());
    }

    #[test]
    fn oversized_shared_warm_keeps_existing_namespace_entries() {
        let (_dir, idx) = warm_fixture(4 * BLOCK_ROWS);
        let shared = SharedBlocks::new(1 << 20);
        let a = BlockCache::attach(&shared, 1);
        let b = BlockCache::attach(&shared, 2);
        let held = a.get_cols(&idx, 0, 1).unwrap();
        let before = shared.bytes();
        b.warm(&idx, &[0, 1, 2, 3], ALL_COLS, &|| Ok(())).unwrap();
        assert_eq!(
            b.misses(),
            0,
            "oversized optional work does not decode a prefix"
        );
        assert_eq!(shared.bytes(), before);
        assert!(a.has_cols(&idx, 0, 1));
        assert!(!b.has_cols(&idx, 0, 1));
        assert_eq!(&*held.cols[0], &*idx.decode_col(0, 0).unwrap());
    }

    fn warm_checked(
        cache: &BlockCache,
        idx: &PermIndex,
        blocks: &[usize],
        mask: ColMask,
        check: &(dyn Fn() -> Result<()> + Sync),
    ) -> Result<()> {
        cache.warm(idx, blocks, mask, check)
    }

    #[test]
    fn warm_cancellation_stops_after_the_inflight_pool_wave() {
        use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
        use std::sync::mpsc;
        use std::time::Duration;
        let (_dir, idx) = warm_fixture(8 * BLOCK_ROWS);
        let mut cache = BlockCache::new(64 << 20);
        let width = decode_pool().unwrap().current_num_threads();
        let entered = Arc::new(AtomicUsize::new(0));
        let cancelled = Arc::new(AtomicBool::new(false));
        let expired = Arc::new(AtomicBool::new(false));
        let release = Arc::new((parking_lot::Mutex::new(false), parking_lot::Condvar::new()));
        let (tx, rx) = mpsc::channel();
        cache.decode_hook = Some(Arc::new({
            let entered = entered.clone();
            let release = release.clone();
            let expired = expired.clone();
            move || {
                if entered.fetch_add(1, Ordering::SeqCst) < width {
                    let _ = tx.send(());
                    let mut go = release.0.lock();
                    while !*go {
                        if release
                            .1
                            .wait_for(&mut go, Duration::from_secs(5))
                            .timed_out()
                        {
                            expired.store(true, Ordering::SeqCst);
                            break;
                        }
                    }
                }
            }
        }));
        let cache = Arc::new(cache);
        let worker = std::thread::spawn({
            let cache = cache.clone();
            let cancelled = cancelled.clone();
            move || {
                warm_checked(&cache, &idx, &(0..8).collect::<Vec<_>>(), ALL_COLS, &|| {
                    if cancelled.load(Ordering::SeqCst) {
                        Err(Error::Cancelled)
                    } else {
                        Ok(())
                    }
                })
            }
        });
        let observed = (0..width).try_for_each(|_| rx.recv_timeout(Duration::from_secs(3)));
        cancelled.store(true, Ordering::SeqCst);
        *release.0.lock() = true;
        release.1.notify_all();
        let result = worker.join().unwrap();
        assert!(
            observed.is_ok(),
            "first pool wave did not enter: {observed:?}"
        );
        assert!(!expired.load(Ordering::SeqCst));
        eprintln!(
            "pool width={width}, decoded={}, result={result:?}",
            cache.misses()
        );
        assert!(
            cache.misses() <= width as u64,
            "optional decode continued after cancellation"
        );
        assert!(matches!(result, Err(Error::Cancelled)), "{result:?}");
    }

    #[test]
    fn warm_expired_deadline_decodes_nothing() {
        let (_dir, idx) = warm_fixture(4 * BLOCK_ROWS);
        let cache = BlockCache::new(64 << 20);
        let deadline = std::time::Instant::now() - std::time::Duration::from_secs(1);
        let result = warm_checked(&cache, &idx, &[0, 1, 2, 3], ALL_COLS, &|| {
            if std::time::Instant::now() > deadline {
                Err(Error::Timeout)
            } else {
                Ok(())
            }
        });
        eprintln!("decoded={}, result={result:?}", cache.misses());
        assert_eq!(cache.misses(), 0);
        assert!(matches!(result, Err(Error::Timeout)), "{result:?}");
    }

    #[test]
    fn warm_oversized_request_skips_optional_decodes() {
        let (_dir, idx) = warm_fixture(32 * BLOCK_ROWS);
        let cache = BlockCache::new(8 << 20);
        warm_checked(
            &cache,
            &idx,
            &(0..32).collect::<Vec<_>>(),
            ALL_COLS,
            &|| Ok(()),
        )
        .unwrap();
        let warmed = cache.misses();
        for b in 0..32 {
            let block = cache.get(&idx, b).unwrap();
            for row in 0..BLOCK_ROWS {
                let i = (b * BLOCK_ROWS + row) as u64;
                assert_eq!(block.key(row), [i, i % 7, i % 13, 0]);
            }
        }
        eprintln!("optional={warmed}, total decoded={}", cache.misses());
        assert_eq!(warmed, 0, "oversized optional warmup must be skipped");
        assert_eq!(cache.misses(), 128);
    }

    #[test]
    fn warm_admits_at_most_an_eighth_of_the_cache_per_request() {
        let (_dir, idx) = warm_fixture(8 * BLOCK_ROWS);
        // 8 blocks of 4 columns fill the cache, which one request may not
        let cache = BlockCache::new(8 * 4 * BLOCK_ROWS as u64 * 8);
        cache
            .warm(&idx, &(0..8).collect::<Vec<_>>(), ALL_COLS, &|| Ok(()))
            .unwrap();
        assert_eq!(cache.misses(), 0);
        // while a request of 16 columns in a cache of 128 columns is admitted
        let cache = BlockCache::new(8 * 16 * BLOCK_ROWS as u64 * 8);
        cache
            .warm(&idx, &[0, 1, 2, 3], ALL_COLS, &|| Ok(()))
            .unwrap();
        assert_eq!(cache.misses(), 16);
    }

    #[test]
    fn warm_checks_after_the_final_decode_and_preserves_advisory_errors() {
        let (_dir, mut idx) = warm_fixture(4 * BLOCK_ROWS);
        let cache = BlockCache::new(64 << 20);
        let result = cache.warm(&idx, &[0, 1, 2, 3], ALL_COLS, &|| {
            if cache.misses() == 16 {
                Err(Error::Cancelled)
            } else {
                Ok(())
            }
        });
        assert_eq!(cache.misses(), 16);
        assert!(matches!(result, Err(Error::Cancelled)), "{result:?}");
        // Missing mapped data takes the real decoder's Corrupt path. Optional warmup
        // still defers it to consumption rather than turning unused advice into failure.
        idx.data = None;
        let cache = BlockCache::new(64 << 20);
        cache
            .warm(&idx, &[0, 1, 2, 3], ALL_COLS, &|| Ok(()))
            .unwrap();
        assert_eq!(cache.misses(), 16);
        assert!(matches!(cache.get(&idx, 0), Err(Error::Corrupt(_))));
    }

    #[test]
    fn warm_budget_is_selective_and_deduplicates_missing_columns() {
        let (_dir, idx) = warm_fixture(8 * BLOCK_ROWS);
        let mask = (1 << 1) | (1 << 3);
        let cache = BlockCache::new(64 << 20);
        cache
            .warm(&idx, &(0..8).collect::<Vec<_>>(), mask, &|| Ok(()))
            .unwrap();
        assert_eq!(cache.misses(), 16);
        for b in 0..8 {
            assert!(cache.has_cols(&idx, b, mask));
            assert!(!cache.has_cols(&idx, b, 1));
            let got = cache.get_cols(&idx, b, mask).unwrap();
            for c in [1, 3] {
                assert_eq!(got.cols[c], idx.decode_col(b, c).unwrap());
            }
        }
        // a request may add an eighth of the cache
        let cache = BlockCache::new(4 * BLOCK_ROWS as u64 * 8 * 4 * 8);
        let duplicates = (0..4).cycle().take(64).collect::<Vec<_>>();
        cache.warm(&idx, &duplicates, ALL_COLS, &|| Ok(())).unwrap();
        assert_eq!(
            cache.misses(),
            16,
            "budget and work count distinct missing keys"
        );
    }

    #[test]
    fn warm_budget_is_an_eighth_with_a_floor_for_small_caches() {
        const MIB: u64 = 1 << 20;
        assert_eq!(warm_budget(1024 * MIB), 128 * MIB, "the default profile");
        assert_eq!(warm_budget(512 * MIB), 64 * MIB);
        assert_eq!(warm_budget(256 * MIB), 64 * MIB, "the small profile");
        assert_eq!(warm_budget(64 * MIB), 16 * MIB, "a quarter below the floor");
        assert_eq!(warm_budget(0), 0);
        // the floor admits a request that an eighth of a 256 MiB cache declines
        let (_dir, idx) = warm_fixture(48 * BLOCK_ROWS);
        let blocks = (0..48).collect::<Vec<_>>();
        // 48 blocks of 4 columns of 256 KiB: 48 MiB, over 32 and under 64
        let cache = BlockCache::new(256 * MIB);
        cache.warm(&idx, &blocks, ALL_COLS, &|| Ok(())).unwrap();
        assert_eq!(cache.misses(), 192);
        // and a quarter of a small cache still bounds it
        let cache = BlockCache::new(128 * MIB);
        cache.warm(&idx, &blocks, ALL_COLS, &|| Ok(())).unwrap();
        assert_eq!(cache.misses(), 0, "48 MiB is over a quarter of 128 MiB");
    }

    #[test]
    fn warm_budget_counts_partial_tails_and_only_missing_bytes() {
        let (_dir, idx) = warm_fixture(4 * BLOCK_ROWS + 17);
        let full = 4 * BLOCK_ROWS as u64 * 8 * 4;
        let cache = BlockCache::new((full + 17 * 8 * 4) * 8);
        cache
            .warm(&idx, &[0, 1, 2, 3, 4], ALL_COLS, &|| Ok(()))
            .unwrap();
        assert_eq!(
            cache.misses(),
            20,
            "partial tail fits the exact decoded byte budget"
        );
        let (_dir, idx) = warm_fixture(12 * BLOCK_ROWS);
        let mask = 3;
        let cache = BlockCache::new(16 * BLOCK_ROWS as u64 * 8 * 8);
        for b in 0..4 {
            cache.get_cols(&idx, b, mask).unwrap();
        }
        assert_eq!(cache.misses(), 8);
        cache
            .warm(&idx, &(0..12).collect::<Vec<_>>(), mask, &|| Ok(()))
            .unwrap();
        assert_eq!(
            cache.misses(),
            24,
            "cached columns consume no new warm budget"
        );
    }

    #[test]
    fn warm_tiny_read_through_and_disqualified_work_stay_bounded() {
        let (_dir, idx) = warm_fixture(4 * BLOCK_ROWS);
        for bytes in [0, 1, 64 << 10] {
            let cache = BlockCache::new(bytes);
            cache
                .warm(&idx, &[0, 1, 2, 3], ALL_COLS, &|| Ok(()))
                .unwrap();
            assert_eq!(cache.misses(), 0);
            let block = cache.get(&idx, 0).unwrap();
            assert_eq!(block.key(10), [10, 3, 10, 0]);
        }
        let cache = BlockCache::new(64 << 20).read_through();
        cache
            .warm(&idx, &[0, 1, 2, 3], ALL_COLS, &|| Ok(()))
            .unwrap();
        assert_eq!(cache.misses(), 0);
        assert_eq!(cache.get(&idx, 0).unwrap().key(10), [10, 3, 10, 0]);
        assert_eq!(cache.entries(), 0);
        let cache = BlockCache::new(1);
        let mut work = cache.warm_columns(&idx);
        for b in (0..4).cycle().take(1024) {
            work.add(b, ALL_COLS);
        }
        assert!(!work.eligible);
        assert!(work.work.is_empty() && work.seen.is_empty());
        assert_eq!(work.bytes, 0);
        assert_eq!(work.work.capacity(), 0);
        work.finish(&|| Ok(())).unwrap();
        assert_eq!(cache.misses(), 0);
    }

    #[test]
    fn advice_union_requires_complete_spans() {
        for (spans, expected) in [
            (vec![], false),
            (vec![(10, 10)], false),
            (vec![(10, 11)], false),
            (vec![(19, 20)], false),
            (vec![(0, 12), (13, 30)], false),
            (vec![(10, 20)], true),
            (vec![(0, 30)], true),
            (vec![(15, 20), (10, 15)], true),
            (vec![(16, 20), (10, 16), (10, 16), (12, 18)], true),
            (vec![(0, 5), (20, 30)], false),
        ] {
            assert_eq!(advice_covers_span(&spans, (10, 20)), expected, "{spans:?}");
        }
        assert!(advice_covers_span(&[], (10, 10)));
    }

    #[test]
    fn read_ahead_sorted_coverage_lookup_edges() {
        let mut ahead = ReadAhead::default();
        assert!(!ahead.covers(10, 20));
        ahead.covered.extend([(10, 20), (30, 40), (50, 60)]);
        for (from, to, expected) in [
            (0, 5, false),
            (20, 30, false),
            (25, 35, false),
            (10, 20, true),
            (12, 18, true),
            (30, 40, true),
            (12, 21, false),
            (30, 41, false),
            (55, 60, true),
            (60, 61, false),
            (65, 70, false),
        ] {
            assert_eq!(ahead.covers(from, to), expected, "[{from}, {to})");
        }
    }

    #[cfg(unix)]
    #[test]
    fn read_ahead_sorted_coverage_lookup_has_bounded_work() {
        let (_dir, idx) = read_ahead_fixture(128);
        let cache = BlockCache::new(64 << 20);
        for b in (0..idx.blocks.len()).step_by(2) {
            cache.get_cols(&idx, b, 1).unwrap();
        }
        for pair in idx.blocks.windows(2) {
            let gap = pair[1].offset - pair[0].offset - u64::from(pair[0].col_len[0]);
            assert!(gap > 64 << 10, "selective spans must remain separate");
        }
        let before = cache.misses();
        let trace = idx.trace_hints();
        let mut ahead = ReadAhead::default();
        let mut rows = 0;
        for b in 0..idx.blocks.len() {
            let hint = ahead.before(&idx, &cache, b, idx.blocks.len(), &|_| 1);
            if b == 0 && io_hints() && read_window() != 0 {
                assert_eq!(ahead.requested, 128);
                assert_eq!(ahead.covered.len(), 64, "many actual future intervals");
                assert_eq!(trace.spans().len(), 64);
            }
            let block = cache.get_cols_hinted(&idx, b, 1, hint).unwrap();
            for &value in block.cols[0].iter() {
                assert_eq!(value, rows);
                rows += 1;
            }
        }
        assert_eq!(rows, 128 * BLOCK_ROWS as u64);
        assert_eq!(cache.misses() - before, 64);
        let inspections = ahead.lookup_inspections;
        eprintln!("128 real blocks, 64 cached skips, actual containment comparisons={inspections}");
        assert!(
            inspections <= 4 * idx.blocks.len(),
            "coverage lookup exceeds linear pass bound"
        );
        if !io_hints() || read_window() == 0 {
            assert_eq!(inspections, 0);
        }
    }

    #[cfg(unix)]
    #[test]
    fn read_ahead_evicted_skipped_column_gets_fallback_advice() {
        let (_dir, idx) = read_ahead_fixture(6);
        let cache = BlockCache::new(64 << 20);
        cache.get_cols(&idx, 1, 2).unwrap();
        let before = cache.misses();
        let trace = idx.trace_hints();
        let target_start = idx.blocks[1].offset as usize + idx.blocks[1].col_len[0] as usize;
        let target = (
            target_start,
            target_start + idx.blocks[1].col_len[1] as usize,
        );
        assert!(target.1 - target.0 > 128 << 10);
        assert!(target.1 - target.0 > advice_page_size());
        assert!(idx.blocks[0].col_len[2] > 64 << 10);
        assert!(idx.blocks[1].col_len[2] > 64 << 10);
        let expected = read_ahead_reference(6 * BLOCK_ROWS);
        let mut n = 0;
        let mut previously_hinted = false;
        idx.for_each_key_range_cols(&cache, &[0; 4], &[u64::MAX; 4], 2, |block, start, end| {
            if n == 0 {
                previously_hinted = trace
                    .spans()
                    .iter()
                    .any(|&(a, z)| a < target.1 && z > target.0);
                cache.evict_column(&idx, 1, 1);
            }
            for row in start..end {
                assert_eq!(block.cols[1][row], expected[n as usize]);
                n += 1;
            }
            Ok(true)
        })
        .unwrap();
        assert!(
            !previously_hinted,
            "fixture must not coalesce across the skipped block"
        );
        assert_eq!(n, 6 * BLOCK_ROWS as u64);
        assert_eq!(
            cache.misses() - before,
            6,
            "evicted column must miss during consumption"
        );
        let spans = trace.spans();
        eprintln!(
            "eager target={target:?}, page={}, misses={}, rows={n}, target advice={:?}",
            advice_page_size(),
            cache.misses() - before,
            spans
                .iter()
                .filter(|&&(a, z)| a < target.1 && z > target.0)
                .collect::<Vec<_>>()
        );
        assert_eq!(
            spans.iter().any(|&(a, z)| a < target.1 && z > target.0),
            io_hints(),
            "an evicted column needs fallback advice when hints are enabled"
        );
        assert_eq!(
            advice_covers_span(&spans, target),
            io_hints(),
            "complete evicted column needs advice"
        );
        if !io_hints() {
            assert!(spans.is_empty());
        }
    }

    #[cfg(unix)]
    #[test]
    fn read_ahead_actual_outstanding_bytes_obey_the_window() {
        let (_dir, idx) = read_ahead_fixture(128);
        for mask in [2, 5] {
            let cache = BlockCache::new(1);
            let trace = idx.trace_hints();
            let mut ahead = ReadAhead::default();
            let mut max_outstanding = 0;
            let limit = read_window();
            let enabled = limit > 0 && io_hints();
            let mut violations = Vec::new();
            for b in 0..idx.blocks.len() {
                ahead.before(&idx, &cache, b, idx.blocks.len(), &|_| mask);
                if enabled {
                    assert!(
                        ahead.requested > b,
                        "every positive window must progress even for an oversized block"
                    );
                } else {
                    assert_eq!((ahead.requested, ahead.window), (0, 0));
                    assert!(trace.spans().is_empty());
                }
                assert!(
                    ahead
                        .covered
                        .front()
                        .is_none_or(|&(a, _)| a >= idx.blocks[b].offset as usize)
                );
                let once = trace.spans().len();
                ahead.before(&idx, &cache, b, idx.blocks.len(), &|_| mask);
                assert_eq!(
                    trace.spans().len(),
                    once,
                    "same position must not refill twice"
                );
                // Independent oracle from actual advice and on-disk block metadata, not
                // ReadAhead's counters. Merge the requested byte union beyond this block.
                let at = idx.blocks[b].offset as usize
                    + idx.blocks[b]
                        .col_len
                        .iter()
                        .map(|&n| n as usize)
                        .sum::<usize>();
                let mut spans = trace
                    .spans()
                    .into_iter()
                    .filter_map(|(a, z)| (z > at).then_some((a.max(at), z)))
                    .collect::<Vec<_>>();
                spans.sort_unstable();
                let mut merged: Vec<(usize, usize)> = Vec::new();
                for (a, z) in spans {
                    if let Some(last) = merged.last_mut()
                        && a <= last.1
                    {
                        last.1 = last.1.max(z);
                    } else {
                        merged.push((a, z));
                    }
                }
                let total = merged.iter().map(|&(a, z)| z - a).sum::<usize>();
                max_outstanding = max_outstanding.max(total);
                // One next block's requested compressed column span and its possible
                // 64KiB gap is the extension. Do not allow unrelated columns to inflate
                // this oracle's allowance for a selective mask.
                let extension = idx
                    .blocks
                    .iter()
                    .map(|m| {
                        let first = mask.trailing_zeros() as usize;
                        let last = (u8::BITS - 1 - mask.leading_zeros()) as usize;
                        m.col_len[first..=last]
                            .iter()
                            .map(|&n| n as usize)
                            .sum::<usize>()
                    })
                    .max()
                    .unwrap()
                    + (64 << 10);
                if total > limit + extension {
                    violations.push((b, total, limit, extension));
                }
            }
            eprintln!(
                "limit={limit}, max actual outstanding={max_outstanding}, violations={violations:?}"
            );
            assert!(
                violations.is_empty(),
                "outstanding advice exceeds target plus one block extension"
            );
        }
    }

    #[test]
    fn a_warm_pass_does_no_read_ahead_bookkeeping() {
        let (_dir, idx) = read_ahead_fixture(8);
        let cache = BlockCache::new(64 << 20);
        let (lo, hi) = ([0; 4], [u64::MAX; 4]);
        let mut rows = 0;
        let pass = |rows: &mut usize| {
            idx.for_each_key_range_cols(&cache, &lo, &hi, 0b11, |b, s, e| {
                *rows += e - s;
                assert_eq!(b.cols[0].len(), b.len());
                Ok(true)
            })
            .unwrap();
        };
        pass(&mut rows);
        let calls = READ_AHEAD_CALLS.with(|n| n.get());
        assert!(
            calls >= 8,
            "the cold pass asks the read-ahead for each block"
        );
        let misses = cache.misses();
        pass(&mut rows);
        assert_eq!(rows, 2 * 8 * BLOCK_ROWS);
        assert_eq!(
            READ_AHEAD_CALLS.with(|n| n.get()),
            calls,
            "the warm pass consulted the read-ahead"
        );
        assert_eq!(cache.misses(), misses);
    }

    /// The cold end block of a count is asked for even when the other end is cached.
    #[test]
    fn count_between_hints_a_cold_end_when_the_other_is_warm() {
        if !io_hints() {
            return;
        }
        let (_dir, idx) = read_ahead_fixture(8);
        let cache = BlockCache::new(64 << 20);
        let (lo, hi) = (
            [BLOCK_ROWS as u64 / 2, 0, 0, 0],
            [6 * BLOCK_ROWS as u64 + 5, u64::MAX, u64::MAX, u64::MAX],
        );
        let bounds = bound_cols(&lo, &hi);
        cache.get_cols(&idx, 0, bounds).unwrap();
        assert!(!cache.has_cols(&idx, 6, bounds));
        let m = &idx.blocks[6];
        let end = m.offset as usize + m.col_len.iter().map(|&l| l as usize).sum::<usize>();
        let trace = idx.trace_hints();
        idx.count_between(&cache, &lo, &hi).unwrap();
        let inside = |s: &&(usize, usize)| s.0 >= m.offset as usize && s.1 <= end;
        let asked = trace.spans();
        let asked = asked.iter().filter(inside).count();
        // one request from the prefetch, one from the decode of the block
        assert!(asked >= 2, "{asked} requests for the cold end block");
    }

    /// A warm count takes each end block from the cache once, without asking the cache
    /// whether it holds the block first.
    #[test]
    fn count_between_looks_a_warm_end_block_up_once() {
        let (_dir, idx) = read_ahead_fixture(8);
        let cache = BlockCache::new(64 << 20);
        let (lo, hi) = (
            [BLOCK_ROWS as u64 / 2, 0, 0, 0],
            [6 * BLOCK_ROWS as u64 + 5, u64::MAX, u64::MAX, u64::MAX],
        );
        let cold = idx.count_between(&cache, &lo, &hi).unwrap();
        let (hits, misses) = (cache.hits(), cache.misses());
        let calls = HAS_COLS_CALLS.with(|n| n.get());
        assert_eq!(idx.count_between(&cache, &lo, &hi).unwrap(), cold);
        assert_eq!(HAS_COLS_CALLS.with(|n| n.get()), calls);
        assert_eq!(cache.misses(), misses);
        let cols = u64::from(bound_cols(&lo, &hi).count_ones());
        assert_eq!(cache.hits() - hits, 2 * cols);
    }

    #[test]
    fn prefetch_reports_whether_the_cache_lacks_a_column() {
        let (_dir, idx) = read_ahead_fixture(2);
        let cache = BlockCache::new(64 << 20);
        assert!(idx.prefetch(&cache, 0, ALL_COLS));
        cache.get_cols(&idx, 0, ALL_COLS).unwrap();
        assert!(!idx.prefetch(&cache, 0, ALL_COLS));
        assert!(idx.prefetch(&cache, 1, ALL_COLS));
        assert!(!idx.prefetch(&cache, 1, 0));
        // a partly cached block still lacks a column
        cache.get_cols(&idx, 1, 0b1).unwrap();
        assert!(!idx.prefetch(&cache, 1, 0b1));
        assert!(idx.prefetch(&cache, 1, 0b11));
    }

    #[test]
    fn get_cols_cached_counts_hits_only_for_a_complete_block() {
        let (_dir, idx) = read_ahead_fixture(1);
        let cache = BlockCache::new(64 << 20);
        cache.get_cols(&idx, 0, 0b1).unwrap();
        let (hits, misses) = (cache.hits(), cache.misses());
        assert!(cache.get_cols_cached(&idx, 0, 0b11).is_none());
        assert_eq!((cache.hits(), cache.misses()), (hits, misses));
        let block = cache.get_cols_cached(&idx, 0, 0b1).unwrap();
        assert_eq!(block.cols[0].len(), block.len());
        assert_eq!((cache.hits(), cache.misses()), (hits + 1, misses));
    }

    #[test]
    fn read_ahead_cached_and_empty_masks_make_bounded_progress() {
        let (_dir, idx) = read_ahead_fixture(8);
        let enabled = io_hints() && read_window() > 0;
        let cache = BlockCache::new(64 << 20);
        for b in 0..8 {
            cache.get_cols(&idx, b, 1).unwrap();
        }
        let trace = idx.trace_hints();
        let mut cached = ReadAhead {
            window: 1,
            ..Default::default()
        };
        for b in 0..8 {
            let hint = cached.before(&idx, &cache, b, 8, &|_| 1);
            assert!(hint, "cache presence is not hint coverage");
            assert!(if enabled {
                cached.requested > b
            } else {
                cached.requested == 0
            });
            assert!(cached.covered.is_empty());
            cache.get_cols_hinted(&idx, b, 1, hint).unwrap();
        }
        assert_eq!(cached.requested, if enabled { 8 } else { 0 });
        assert!(trace.spans().is_empty(), "cache hits need no fallback I/O");
        let mut empty = ReadAhead {
            window: 1,
            ..Default::default()
        };
        empty.before(&idx, &cache, 0, 8, &|_| 0);
        assert_eq!(
            empty.requested,
            if enabled { 2 } else { 0 },
            "zero spans must consume traversal budget"
        );
        let first = empty.requested;
        empty.before(&idx, &cache, 0, 8, &|_| 0);
        assert_eq!(empty.requested, first);
        for b in 1..8 {
            empty.before(&idx, &cache, b, 8, &|_| 0);
        }
        assert_eq!(empty.requested, if enabled { 8 } else { 0 });
        assert!(empty.covered.is_empty() && trace.spans().is_empty());
        let mut mixed = ReadAhead {
            window: 1,
            ..Default::default()
        };
        for b in 0..8 {
            mixed.before(&idx, &cache, b, 8, &|next| {
                if next % 3 == 0 { 0 } else { 1 }
            });
            assert!(if enabled {
                mixed.requested > b
            } else {
                mixed.requested == 0
            });
        }
        assert_eq!(
            mixed.requested,
            if enabled { 8 } else { 0 },
            "zero-mask gaps cannot stall a cached pass"
        );
        assert!(mixed.covered.is_empty() && trace.spans().is_empty());
    }

    #[cfg(unix)]
    #[test]
    fn read_ahead_charges_coalesced_gaps_and_prunes_consumed_coverage() {
        let (_dir, idx) = warm_fixture(64 * BLOCK_ROWS);
        let cache = BlockCache::new(1);
        let trace = idx.trace_hints();
        let mut ahead = ReadAhead::default();
        // Only the first column is needed. The actual advised range bridges all
        // small intervening columns and gaps, so selected-column lengths alone lie.
        ahead.before(&idx, &cache, 0, 64, &|_| 1);
        let spans = trace.spans();
        let selected = (0..ahead.requested)
            .map(|b| idx.blocks[b].col_len[0] as usize)
            .sum::<usize>();
        let actual = spans.iter().map(|&(a, z)| z - a).sum::<usize>();
        if io_hints() && read_window() > 0 {
            assert!(actual > selected);
        } else {
            assert_eq!(actual, 0);
            assert_eq!(ahead.requested, 0);
        }
        assert_eq!(
            actual,
            ahead.covered.iter().map(|&(a, z)| z - a).sum::<usize>()
        );
        let once = spans.len();
        ahead.before(&idx, &cache, 0, 64, &|_| 1);
        assert_eq!(
            trace.spans().len(),
            once,
            "early stop requests nothing else"
        );
        for b in 1..64 {
            ahead.before(&idx, &cache, b, 64, &|_| if b % 3 == 0 { 0 } else { 1 });
            let at = idx.blocks[b].offset as usize;
            assert!(ahead.covered.front().is_none_or(|&(a, _)| a >= at));
        }
        assert!(
            ahead.covered.len() <= 1,
            "consumed history must not accumulate"
        );
    }

    #[test]
    fn columns_of_every_varint_width_round_trip() {
        // deltas of 1 to 10 varint bytes, in the fast part and in the last 10 bytes
        let mut col = Vec::new();
        let mut v = 0u64;
        for i in 0..2000u64 {
            v = v.wrapping_add(match i % 7 {
                0 => 1,
                1 => 300,
                2 => 1 << 20,
                3 => 1 << 40,
                4 => u64::MAX / 3,
                5 => 0,
                _ => (1 << 62) + i,
            });
            col.push(v);
        }
        col.extend([u64::MAX, 0, 1 << 63]);
        // runs of one-byte deltas, up and down, that the eight-at-a-time path reads
        for i in 0..1000u64 {
            v = v.wrapping_add((i * 37 % 63).wrapping_sub(31));
            col.push(v);
        }
        col.push(5);
        let mut scratch = Vec::new();
        for n in [0, 1, 2, 9, 10, 11, col.len()] {
            let enc = encode_column(&col[..n], &mut scratch);
            assert_eq!(&decode_column(&enc, n).unwrap()[..], &col[..n]);
            let tail = encode_column(&col[col.len() - n..], &mut scratch);
            assert_eq!(&decode_column(&tail, n).unwrap()[..], &col[col.len() - n..]);
        }
        // a varint that runs past 10 bytes is damage
        assert!(decode_varints_into(&[0xff; 11], &mut [0]).is_err());
        assert!(decode_varints_into(&[0xff; 20], &mut [0]).is_err());
    }

    #[test]
    fn damaged_blocks_are_errors_not_panics() {
        let col: Vec<u64> = (0..1000u64).map(|i| i * 7).collect();
        let mut scratch = Vec::new();
        let enc = encode_column(&col, &mut scratch);
        assert_eq!(&decode_column(&enc, 1000).unwrap()[..], &col[..]);
        // fewer values than rows, values left over, a size prefix that is too large
        assert!(decode_column(&enc, 1001).is_err());
        assert!(decode_column(&enc, 999).is_err());
        let mut big = enc.clone();
        big[3] = 0x7f;
        assert!(decode_column(&big, 1000).is_err());
        assert!(decode_column(&enc[..2], 1000).is_err());
        // a truncated data file
        let dir = tempfile::tempdir().unwrap();
        let mut w = PermWriter::create(dir.path(), Perm::Spo).unwrap();
        for i in 0..100u64 {
            w.push([i, 0, 0, 0]).unwrap();
        }
        w.finish(dir.path(), Perm::Spo).unwrap();
        let dat = dir.path().join("spo.dat");
        let len = std::fs::metadata(&dat).unwrap().len();
        std::fs::OpenOptions::new()
            .write(true)
            .open(&dat)
            .unwrap()
            .set_len(len - 3)
            .unwrap();
        let idx = PermIndex::open(dir.path(), Perm::Spo).unwrap();
        assert!(idx.decode_block(0).is_err());
    }
}
