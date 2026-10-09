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

/// Ask the kernel to start reading `len` bytes of `m` at `off` (see [`io_hints`]).
#[inline]
pub(crate) fn will_need(m: &Mmap, off: usize, len: usize) {
    #[cfg(unix)]
    if len > 0 && off <= m.len() && len <= m.len() - off && io_hints() {
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
    }
    #[cfg(not(unix))]
    let _ = (m, off, len);
}

pub type Key = [u64; 4];

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
pub(crate) fn decode_column(bytes: &[u8], rows: usize) -> Result<Vec<u64>> {
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
fn decode_varints(raw: &[u8], rows: usize) -> Result<Vec<u64>> {
    let bad = |m: &str| Error::Corrupt(format!("block column: {m}"));
    let mut out = Vec::with_capacity(rows);
    let mut pos = 0;
    let mut prev = 0u64;
    for _ in 0..rows {
        // a varint takes at most 10 bytes: while 10 remain, read them without checking
        // for the end of the buffer at each byte
        let z = match raw.get(pos..pos + 10) {
            Some(s) => read_varint10(s.try_into().unwrap(), &mut pos),
            None => read_varint_checked(raw, &mut pos),
        }
        .ok_or_else(|| bad(&format!("holds fewer than {rows} values")))?;
        let d = ((z >> 1) as i64) ^ -((z & 1) as i64);
        prev = prev.wrapping_add(d as u64);
        out.push(prev);
    }
    if pos != raw.len() {
        return Err(bad(&format!(
            "{} bytes left after {rows} values",
            raw.len() - pos
        )));
    }
    Ok(out)
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
            *col = self.decode_col(b, c)?.into();
        }
        Ok(Block {
            cols,
            rows: m.rows as usize,
        })
    }

    /// Ask the kernel to start reading the columns in `mask` of block `b` (and those
    /// between them), unless the cache holds them decoded. A reader that knows the
    /// blocks it will visit asks for all of them first, so that a cold server reads them
    /// in parallel instead of one after another (see [`io_hints`]).
    pub fn prefetch(&self, cache: &BlockCache, b: usize, mask: ColMask) {
        let mask = mask & ALL_COLS;
        if mask == 0 || !io_hints() || cache.has_cols(self, b, mask) {
            return;
        }
        let (Some(data), Some(m)) = (self.data.as_ref(), self.blocks.get(b)) else {
            return;
        };
        let first = mask.trailing_zeros() as usize;
        let last = 7 - mask.leading_zeros() as usize;
        let at = |c: usize| {
            m.offset as usize + m.col_len[..c].iter().map(|&l| l as usize).sum::<usize>()
        };
        let (from, to) = (at(first), at(last + 1));
        will_need(data, from, to - from);
    }

    /// Decode one column of a block (columns are compressed separately).
    pub fn decode_col(&self, b: usize, c: usize) -> Result<Vec<u64>> {
        let m = &self.blocks[b];
        let data = self
            .data
            .as_ref()
            .ok_or_else(|| Error::Corrupt("no data".into()))?;
        let off = m.offset as usize + m.col_len[..c].iter().map(|&l| l as usize).sum::<usize>();
        // this column and the block's later ones, which a scan usually decodes next: one
        // read on a cold file instead of a page fault per page
        will_need(
            data,
            off,
            m.col_len[c..].iter().map(|&l| l as usize).sum::<usize>(),
        );
        let bytes = data.get(off..off + m.col_len[c] as usize).ok_or_else(|| {
            Error::Corrupt(format!(
                "{}.dat: block {b} ends past the end of the file",
                self.perm.name()
            ))
        })?;
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

/// Process-wide cache of decoded blocks, weighted by bytes.
/// Cache of decoded block columns, keyed by (permutation instance, block, column):
/// readers decode and keep only the columns they use.
/// Decoded columns by (permutation instance, block, column).
type ColumnCache = quick_cache::sync::Cache<(u64, u32, u8), Arc<[u64]>, BlockWeighter>;

pub struct BlockCache {
    cache: Arc<ColumnCache>,
    hits: std::sync::atomic::AtomicU64,
    misses: std::sync::atomic::AtomicU64,
    /// decoded blocks are added (false: a read-through view, see [`BlockCache::read_through`])
    fill: bool,
}

#[derive(Clone)]
struct BlockWeighter;
impl quick_cache::Weighter<(u64, u32, u8), Arc<[u64]>> for BlockWeighter {
    fn weight(&self, _k: &(u64, u32, u8), v: &Arc<[u64]>) -> u64 {
        (v.len() * 8).max(1) as u64
    }
}

fn empty_col() -> Arc<[u64]> {
    static EMPTY: std::sync::OnceLock<Arc<[u64]>> = std::sync::OnceLock::new();
    EMPTY.get_or_init(|| Arc::from(Vec::new())).clone()
}

impl BlockCache {
    pub fn new(bytes: u64) -> BlockCache {
        BlockCache {
            cache: Arc::new(quick_cache::sync::Cache::with_weighter(
                (bytes / (BLOCK_ROWS as u64 * 8)).max(64) as usize,
                bytes,
                BlockWeighter,
            )),
            hits: Default::default(),
            misses: Default::default(),
            fill: true,
        }
    }

    /// A view of the same cache that uses its blocks but never adds any: for one-off
    /// full scans (exports, backups) that would otherwise evict the blocks queries use.
    /// Its hit and miss counts are its own.
    pub fn read_through(&self) -> BlockCache {
        BlockCache {
            cache: self.cache.clone(),
            hits: Default::default(),
            misses: Default::default(),
            fill: false,
        }
    }

    /// A block with all four columns.
    pub fn get(&self, idx: &PermIndex, b: usize) -> Result<Block> {
        self.get_cols(idx, b, ALL_COLS)
    }

    /// A block with the columns in `mask` (the others are empty).
    pub fn get_cols(&self, idx: &PermIndex, b: usize, mask: ColMask) -> Result<Block> {
        let mut cols: [Arc<[u64]>; 4] = std::array::from_fn(|_| empty_col());
        for (c, col) in cols.iter_mut().enumerate() {
            if mask & (1 << c) == 0 {
                continue;
            }
            let key = (idx.uid, b as u32, c as u8);
            *col = match self.cache.get(&key) {
                Some(v) => {
                    self.hits.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                    v
                }
                None => {
                    self.misses
                        .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                    let v: Arc<[u64]> = idx.decode_col(b, c)?.into();
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

    /// Whether the columns in `mask` of block `b` are decoded in the cache, without
    /// decoding them or counting a hit or miss.
    pub fn has_cols(&self, idx: &PermIndex, b: usize, mask: ColMask) -> bool {
        (0..4).all(|c| {
            mask & (1 << c) == 0 || self.cache.peek(&(idx.uid, b as u32, c as u8)).is_some()
        })
    }

    pub fn bytes(&self) -> u64 {
        self.cache.weight()
    }
    pub fn entries(&self) -> usize {
        self.cache.len()
    }
    pub fn hits(&self) -> u64 {
        self.hits.load(std::sync::atomic::Ordering::Relaxed)
    }
    pub fn misses(&self) -> u64 {
        self.misses.load(std::sync::atomic::Ordering::Relaxed)
    }
}

impl PermIndex {
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
        for b in lo..hi {
            let m = &self.blocks[b];
            let mask = mask_of(b);
            let whole = m.first >= *lo_key && m.last <= *hi_key;
            let blk = cache.get_cols(self, b, if whole { mask } else { mask | bounds })?;
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
        let mut scratch = Vec::new();
        for n in [0, 1, 2, 9, 10, 11, col.len()] {
            let enc = encode_column(&col[..n], &mut scratch);
            assert_eq!(decode_column(&enc, n).unwrap(), &col[..n]);
            let tail = encode_column(&col[col.len() - n..], &mut scratch);
            assert_eq!(decode_column(&tail, n).unwrap(), &col[col.len() - n..]);
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
        assert_eq!(decode_column(&enc, 1000).unwrap(), col);
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
