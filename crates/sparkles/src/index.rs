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
use crate::vocab::{read_varint, write_varint};
use memmap2::Mmap;
use std::fs::File;
use std::io::{BufWriter, Write};
use std::path::Path;
use std::sync::Arc;

pub const BLOCK_ROWS: usize = 32 * 1024;

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

const META_BYTES: usize = 8 * 4 * 2 + 8 + 4 * 4 + 4 + 8;

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
    fn read(b: &[u8]) -> BlockMeta {
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

fn encode_column(col: &[u64], scratch: &mut Vec<u8>) -> Vec<u8> {
    scratch.clear();
    let mut prev = 0u64;
    for &v in col {
        let d = v.wrapping_sub(prev) as i64;
        write_varint(scratch, ((d << 1) ^ (d >> 63)) as u64);
        prev = v;
    }
    lz4_flex::compress_prepend_size(scratch)
}

fn decode_column(bytes: &[u8], rows: usize) -> Result<Vec<u64>> {
    let raw = lz4_flex::decompress_size_prepended(bytes)
        .map_err(|e| Error::Corrupt(format!("block decompression: {e}")))?;
    let mut out = Vec::with_capacity(rows);
    let mut pos = 0;
    let mut prev = 0u64;
    for _ in 0..rows {
        let z = read_varint(&raw, &mut pos);
        let d = ((z >> 1) as i64) ^ -((z & 1) as i64);
        prev = prev.wrapping_add(d as u64);
        out.push(prev);
    }
    Ok(out)
}

/// Streaming writer for one permutation. Keys must arrive sorted and deduplicated.
pub struct PermWriter {
    data: BufWriter<File>,
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
                File::create(dir.join(format!("{}.dat", perm.name())))?,
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

    pub fn finish(mut self, dir: &Path, perm: Perm) -> Result<u64> {
        self.flush_block()?;
        self.data.flush()?;
        self.data.get_ref().sync_all()?;
        let mut m = File::create(dir.join(format!("{}.meta", perm.name())))?;
        m.write_all(&self.meta)?;
        m.sync_all()?;
        Ok(self.rows)
    }
}

/// Read side of one permutation.
pub struct PermIndex {
    pub perm: Perm,
    data: Option<Mmap>,
    pub blocks: Vec<BlockMeta>,
    pub rows: u64,
    /// unique id of this permutation instance, used for cache keys
    pub uid: u64,
}

impl PermIndex {
    pub fn empty(perm: Perm) -> PermIndex {
        PermIndex {
            perm,
            data: None,
            blocks: Vec::new(),
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
            // SAFETY: generation files are immutable once written.
            Some(unsafe { Mmap::map(&f)? })
        } else {
            None
        };
        Ok(PermIndex {
            perm,
            data,
            blocks,
            rows,
            uid: next_uid(),
        })
    }

    pub fn disk_bytes(&self) -> u64 {
        self.data.as_ref().map_or(0, |d| d.len() as u64) + (self.blocks.len() * META_BYTES) as u64
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

    /// Decode one column of a block (columns are compressed separately).
    pub fn decode_col(&self, b: usize, c: usize) -> Result<Vec<u64>> {
        let m = &self.blocks[b];
        let data = self
            .data
            .as_ref()
            .ok_or_else(|| Error::Corrupt("no data".into()))?;
        let off = m.offset as usize + m.col_len[..c].iter().map(|&l| l as usize).sum::<usize>();
        decode_column(&data[off..off + m.col_len[c] as usize], m.rows as usize)
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

fn next_uid() -> u64 {
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
        mut f: impl FnMut(&Block, usize, usize) -> Result<bool>,
    ) -> Result<()> {
        let (lo, hi) = self.key_block_range(lo_key, hi_key);
        let bounds = bound_cols(lo_key, hi_key);
        for b in lo..hi {
            let m = &self.blocks[b];
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
}
