//! Permutation indexes (QLever `CompressedRelation`-style).
//!
//! Every quad `(s, p, o, g)` is stored in several *permutations*, each a fully sorted
//! sequence of 4-column keys. A permutation file is a sequence of blocks of up to
//! [`BLOCK_ROWS`] rows. Each block stores its four columns separately
//! (delta + zig-zag varint, then LZ4). Block metadata (first/last key, row offset, byte
//! range) is kept in RAM, so a scan with a bound prefix touches only the blocks that can
//! contain it, and exact range counts need at most two block decodes.

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

/// A decoded block: four columns of equal length. Columns a reader did not ask for (see
/// [`BlockCache::get_cols`]) are empty and read as 0 through [`Block::key`].
#[derive(Clone)]
pub struct Block {
    pub cols: [Arc<[u64]>; 4],
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

/// Decode one compressed column of `rows` values. Damaged input is an error, never a
/// panic: the size prefix must fit `rows` varints, and the values must use exactly the
/// decompressed bytes.
pub(crate) fn decode_column(bytes: &[u8], rows: usize) -> Result<Arc<[u64]>> {
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
        decode_varints(raw, rows)
    })
}

/// The `rows` zigzag-delta varints that make up all of `raw`.
fn decode_varints(raw: &[u8], rows: usize) -> Result<Arc<[u64]>> {
    let bad = |m: &str| Error::Corrupt(format!("block column: {m}"));
    // Allocated as the shared slice the block cache keeps: decoding into a Vec and
    // converting it copied every column into a second fresh allocation.
    let mut col: Arc<[u64]> = std::iter::repeat_n(0, rows).collect();
    let out = Arc::get_mut(&mut col).expect("a new Arc is unique");
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
    Ok(col)
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
    meta: Vec<u8>,
    pos: u64,
    rows: u64,
    buf: [Vec<u64>; 4],
    last: Option<Key>,
    scratch: Vec<u8>,
}

impl PermWriter {
    pub fn create(dir: &Path, perm: Perm) -> Result<PermWriter> {
        Ok(PermWriter {
            data: BufWriter::with_capacity(
                1 << 20,
                crate::disk::WritebackFile::new(File::create(
                    dir.join(format!("{}.dat", perm.name())),
                )?),
            ),
            meta: Vec::new(),
            pos: 0,
            rows: 0,
            buf: Default::default(),
            last: None,
            scratch: Vec::new(),
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
        for (col, len) in self.buf.iter_mut().zip(col_len.iter_mut()) {
            let enc = encode_column(col, &mut self.scratch);
            self.data.write_all(&enc)?;
            *len = enc.len() as u32;
            self.pos += enc.len() as u64;
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

    /// Append block `m` as it is encoded (`bytes`, its four columns) after the current
    /// block, which is ended first. Its keys must follow the keys pushed before.
    pub(crate) fn push_raw(&mut self, m: &BlockMeta, bytes: &[u8]) -> Result<()> {
        self.flush_block()?;
        debug_assert!(self.last.is_none_or(|l| l < m.first), "blocks out of order");
        debug_assert_eq!(
            bytes.len() as u64,
            m.col_len.iter().map(|&l| l as u64).sum::<u64>()
        );
        self.data.write_all(bytes)?;
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
        let mut m = File::create(dir.join(format!("{}.meta", perm.name())))?;
        m.write_all(&self.meta)?;
        m.sync_all()?;
        Ok(self.rows)
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
}

impl PermIndex {
    pub fn empty(perm: Perm) -> PermIndex {
        PermIndex {
            perm,
            data: None,
            blocks: Arc::new(Vec::new()),
            rows: 0,
            uid: next_uid(),
        }
    }

    pub fn open(dir: &Path, perm: Perm) -> Result<PermIndex> {
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
        Ok(PermIndex {
            perm,
            data,
            blocks: Arc::new(blocks),
            rows,
            uid: next_uid(),
        })
    }

    pub fn disk_bytes(&self) -> u64 {
        self.data.as_ref().map_or(0, |d| d.len() as u64) + (self.blocks.len() * META_BYTES) as u64
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
        let mut cols: [Arc<[u64]>; 4] = std::array::from_fn(|_| empty_col());
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
    pub fn decode_col(&self, b: usize, c: usize) -> Result<Arc<[u64]>> {
        self.decode_col_hinted(b, c, true)
    }

    /// [`decode_col`](Self::decode_col), asking the kernel for the column's bytes first
    /// only when `hint`: a pass that has read ahead (see [`ReadAhead`]) has asked already.
    pub fn decode_col_hinted(&self, b: usize, c: usize, hint: bool) -> Result<Arc<[u64]>> {
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
        decode_column(bytes, m.rows as usize)
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

/// Cache of decoded block columns, keyed by (permutation instance, block, column and
/// namespace): readers decode and keep only the columns they use.
///
/// The third key field holds the column in its low 8 bits and the namespace of the
/// store that decoded it above them. A permutation instance (`uid`) is unique in the
/// process, so the namespace only attributes an entry to a dataset. It never separates
/// entries that would otherwise collide.
type ColumnCache = quick_cache::sync::Cache<(u64, u32, u32), Arc<[u64]>, BlockWeighter>;

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

#[derive(Clone)]
struct BlockWeighter;
impl quick_cache::Weighter<(u64, u32, u32), Arc<[u64]>> for BlockWeighter {
    fn weight(&self, _k: &(u64, u32, u32), v: &Arc<[u64]>) -> u64 {
        (v.len() * 8).max(1) as u64
    }
}

fn empty_col() -> Arc<[u64]> {
    static EMPTY: std::sync::OnceLock<Arc<[u64]>> = std::sync::OnceLock::new();
    EMPTY.get_or_init(|| Arc::from(Vec::new())).clone()
}

impl BlockCache {
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
        let mut cols: [Arc<[u64]>; 4] = std::array::from_fn(|_| empty_col());
        for (c, col) in cols.iter_mut().enumerate() {
            if mask & (1 << c) == 0 {
                continue;
            }
            let key = (idx.uid, b as u32, self.ns << 8 | c as u32);
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
        let mut cols: [Arc<[u64]>; 4] = std::array::from_fn(|_| empty_col());
        let mut found = 0u64;
        for (c, col) in cols.iter_mut().enumerate() {
            if mask & (1 << c) == 0 {
                continue;
            }
            *col = self
                .cache
                .get(&(idx.uid, b as u32, self.ns << 8 | c as u32))?;
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
            budget: self.cache.capacity() / 8,
            eligible: self.fill,
        }
    }

    #[cfg(test)]
    pub(crate) fn evict_column(&self, idx: &PermIndex, b: usize, c: u8) {
        assert!(
            self.cache
                .remove(&(idx.uid, b as u32, self.ns << 8 | u32::from(c)))
                .is_some()
        );
    }

    /// Whether the columns in `mask` of block `b` are decoded in the cache, without
    /// decoding them or counting a hit or miss.
    pub fn has_cols(&self, idx: &PermIndex, b: usize, mask: ColMask) -> bool {
        (0..4).all(|c| {
            mask & (1 << c) == 0
                || self
                    .cache
                    .peek(&(idx.uid, b as u32, self.ns << 8 | c as u32))
                    .is_some()
        })
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
    work: Vec<(usize, u8)>,
    seen: rustc_hash::FxHashSet<(usize, u8)>,
    bytes: u64,
    /// the most decoded bytes one request may add, an eighth of the cache so that one
    /// request cannot push out what the other queries hold
    budget: u64,
    eligible: bool,
}

impl WarmColumns<'_> {
    pub(crate) fn add(&mut self, b: usize, mask: ColMask) {
        if !self.eligible {
            return;
        }
        for c in 0..4u8 {
            let key = (b, c);
            if mask & (1 << c) == 0
                || self.seen.contains(&key)
                || self
                    .cache
                    .cache
                    .peek(&(self.idx.uid, b as u32, self.cache.ns << 8 | u32::from(c)))
                    .is_some()
            {
                continue;
            }
            // Match BlockWeighter exactly, including partial tails and empty columns.
            // Overflow declines the entire optional request before any decode occurs.
            let next = u64::from(self.idx.blocks[b].rows)
                .checked_mul(8)
                .and_then(|weight| self.bytes.checked_add(weight.max(1)));
            let Some(bytes) = next.filter(|&n| n <= self.budget) else {
                self.eligible = false;
                self.work = Vec::new();
                self.seen = Default::default();
                self.bytes = 0;
                return;
            };
            self.bytes = bytes;
            self.seen.insert(key);
            self.work.push(key);
        }
    }

    pub(crate) fn finish(self, check: &(dyn Fn() -> Result<()> + Sync)) -> Result<()> {
        use rayon::prelude::*;
        check()?;
        // Below this the hand-off costs about what decoding saves. Keep the existing
        // threshold; capacity admission never warms a partial prefix of an oversized set.
        const MIN_COLUMNS: usize = 16;
        if self.eligible && self.work.len() >= MIN_COLUMNS {
            let decode = |&(b, c): &(usize, u8)| -> Result<()> {
                check()?;
                // Only check errors propagate: an advisory decode error remains
                // for the consuming reader to encounter if it needs this column.
                let _ = self.cache.get_cols_hinted(self.idx, b, 1 << c, false);
                Ok(())
            };
            match decode_pool() {
                Some(pool) => pool.install(|| self.work.par_iter().try_for_each(decode))?,
                None => self.work.iter().try_for_each(decode)?,
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
        // each end block that bounds the range, which costs one lookup when it is cached
        let ends = if hi - 1 == lo { 1 } else { 2 };
        for e in [lo, hi - 1].into_iter().take(ends) {
            if partial(e) {
                self.prefetch(cache, e, bc);
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
            } else {
                let blk = cache.get_cols(self, b, bound_cols(&lo_key, &hi_key))?;
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

    /// Point lookup of a full key.
    pub fn contains(&self, cache: &BlockCache, key: &Key) -> Result<bool> {
        let b = self.blocks.partition_point(|m| m.last < *key);
        if b >= self.blocks.len() || self.blocks[b].first > *key {
            return Ok(false);
        }
        let blk = cache.get(self, b)?;
        let (s, e) = blk.prefix_range(key);
        Ok(s < e)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
        assert!(decode_varints(&[0xff; 11], 1).is_err());
        assert!(decode_varints(&[0xff; 20], 1).is_err());
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
