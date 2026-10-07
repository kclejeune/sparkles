//! Term dictionaries.
//!
//! * [`Vocab`] — the immutable **base vocabulary** built by the bulk loader / compaction.
//!   Keys are sorted, so id order equals key order (QLever-style), which lets range and
//!   prefix filters work on ids. Stored front-coded (blocks of [`FC_BLOCK`] keys, each key
//!   stored as `shared-prefix-len, suffix`) and memory-mapped.
//! * [`DeltaVocab`] — append-only dictionary for terms introduced by SPARQL Update. Ids
//!   are in insertion order. Persisted as a length-prefixed log.
//! * [`LocalVocab`] — per-query dictionary for computed terms.

mod sync;
pub(crate) use sync::{LazySync, PendingSync};

use crate::error::Result;
use memmap2::Mmap;
use parking_lot::RwLock;
use rustc_hash::FxHashMap;
use std::fs::{File, OpenOptions};
use std::io::{BufWriter, Read, Write};
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering as AtomicOrdering};

pub const FC_BLOCK: usize = 16;

/// Front-coded blocks per entry of the sparse index (`vocab.idx`).
const IDX_BLOCKS: usize = 128;
const IDX_MAGIC: &[u8; 8] = b"SPKVIDX1";

/// The number of indexes `i` in `0..n` for which `pred(i)` holds, when it holds for a
/// prefix of them.
fn partition_point_by(n: usize, pred: impl Fn(usize) -> bool) -> usize {
    let (mut lo, mut hi) = (0, n);
    while lo < hi {
        let mid = lo + (hi - lo) / 2;
        if pred(mid) {
            lo = mid + 1;
        } else {
            hi = mid;
        }
    }
    lo
}

/// The sparse index of a vocabulary (`vocab.idx`): the first key and the data offset of
/// every [`IDX_BLOCKS`]-th front-coded block, held in memory. A lookup finds its group
/// of blocks here, and then reads only that group's offsets and data, one contiguous
/// range each. Without it, a lookup on a cold server binary-searches the whole
/// vocabulary on disk, one random read per step: 22 reads in 3 million terms, 28 in
/// 200 million.
///
/// The file is derived data, written next to the vocabulary when it is built. A
/// generation without it (built by an older version) looks terms up without it.
struct Sparse {
    /// the keys, one after another
    keys: Vec<u8>,
    /// where each key starts in `keys`, and its end
    starts: Vec<u32>,
    /// the data offset of each group's first block
    offsets: Vec<u64>,
    /// one bit per group: a lookup asked for the group to be read ahead
    hinted: Box<[AtomicU64]>,
}

impl Sparse {
    fn len(&self) -> usize {
        self.offsets.len()
    }

    /// Whether this is the first lookup in group `g`, which asks for the group's offsets
    /// and data to be read ahead. Those are two `madvise` calls, which take longer than
    /// the rest of a lookup when the pages are in memory: `AVG` over 20,000 groups
    /// interns 20,000 computed decimals, and with a hint on every lookup it ran about
    /// 15 ms slower at 10.5M triples. So each group asks once in the life of the
    /// vocabulary. The cold read the hint is for happens then, and later lookups find
    /// the pages in the page cache. If the kernel evicts them, a lookup reads them a
    /// page per fault, as it would without the hint.
    fn first_hint(&self, g: usize) -> bool {
        let (word, bit) = (&self.hinted[g / 64], 1u64 << (g % 64));
        // a plain load first, so that warm lookups do not write the shared word
        word.load(AtomicOrdering::Relaxed) & bit == 0
            && word.fetch_or(bit, AtomicOrdering::Relaxed) & bit == 0
    }
    fn key(&self, g: usize) -> &[u8] {
        let end = self
            .starts
            .get(g + 1)
            .map_or(self.keys.len(), |&e| e as usize);
        &self.keys[self.starts[g] as usize..end]
    }

    /// The index file's bytes for `entries` of (data offset, first key).
    fn encode(entries: &[(u64, Vec<u8>)]) -> Vec<u8> {
        let mut out =
            Vec::with_capacity(24 + entries.iter().map(|e| 12 + e.1.len()).sum::<usize>());
        out.extend_from_slice(IDX_MAGIC);
        out.extend_from_slice(&(IDX_BLOCKS as u32).to_le_bytes());
        out.extend_from_slice(&0u32.to_le_bytes());
        out.extend_from_slice(&(entries.len() as u64).to_le_bytes());
        for (off, key) in entries {
            out.extend_from_slice(&off.to_le_bytes());
            out.extend_from_slice(&(key.len() as u32).to_le_bytes());
            out.extend_from_slice(key);
        }
        out
    }

    /// Read `vocab.idx` for a vocabulary of `blocks` front-coded blocks and `data_len`
    /// data bytes. `None` when the file is missing, or does not describe this
    /// vocabulary (then lookups do without it).
    fn read(path: &Path, blocks: usize, data_len: u64) -> Option<Sparse> {
        let buf = std::fs::read(path).ok()?;
        let s = Self::decode(&buf, blocks, data_len);
        if s.is_none() {
            tracing::warn!(
                target: "sparkles::vocab",
                "{} does not match its vocabulary; terms are looked up without it",
                path.display()
            );
        }
        s
    }

    fn decode(buf: &[u8], blocks: usize, data_len: u64) -> Option<Sparse> {
        let u32_at = |p: usize| Some(u32::from_le_bytes(buf.get(p..p + 4)?.try_into().ok()?));
        let u64_at = |p: usize| Some(u64::from_le_bytes(buf.get(p..p + 8)?.try_into().ok()?));
        if buf.get(..8)? != IDX_MAGIC || u32_at(8)? as usize != IDX_BLOCKS {
            return None;
        }
        let n = u64_at(16)? as usize;
        if n != blocks.div_ceil(IDX_BLOCKS) {
            return None;
        }
        let mut s = Sparse {
            keys: Vec::new(),
            starts: Vec::with_capacity(n),
            offsets: Vec::with_capacity(n),
            hinted: (0..n.div_ceil(64)).map(|_| AtomicU64::new(0)).collect(),
        };
        let mut pos = 24;
        for _ in 0..n {
            let off = u64_at(pos)?;
            let len = u32_at(pos + 8)? as usize;
            let key = buf.get(pos + 12..pos + 12 + len)?;
            if off > data_len
                || s.offsets.last().is_some_and(|&o| o > off)
                || (!s.starts.is_empty() && s.key(s.len() - 1) >= key)
            {
                return None;
            }
            s.starts.push(u32::try_from(s.keys.len()).ok()?);
            s.keys.extend_from_slice(key);
            s.offsets.push(off);
            pos += 12 + len;
        }
        (pos == buf.len()).then_some(s)
    }
}

pub(crate) fn write_varint(out: &mut Vec<u8>, mut v: u64) {
    while v >= 0x80 {
        out.push((v as u8) | 0x80);
        v >>= 7;
    }
    out.push(v as u8);
}

#[inline]
pub(crate) fn read_varint(buf: &[u8], pos: &mut usize) -> u64 {
    let mut v = 0u64;
    let mut shift = 0;
    loop {
        let b = buf[*pos];
        *pos += 1;
        v |= ((b & 0x7F) as u64) << shift;
        if b < 0x80 {
            return v;
        }
        shift += 7;
    }
}

enum Bytes {
    Map(Mmap),
    Vec(Vec<u8>),
}
impl Bytes {
    fn as_slice(&self) -> &[u8] {
        match self {
            Bytes::Map(m) => m,
            Bytes::Vec(v) => v,
        }
    }
    /// Map the file, for random access when `random` (see [`crate::index::io_hints`]).
    fn open(path: &Path, random: bool) -> Result<Bytes> {
        let f = File::open(path)?;
        if f.metadata()?.len() == 0 {
            return Ok(Bytes::Vec(Vec::new()));
        }
        if random {
            return Ok(Bytes::Map(crate::index::map_random(&f)?));
        }
        // SAFETY: index files are immutable once written; generations are never
        // modified in place.
        Ok(Bytes::Map(unsafe { Mmap::map(&f)? }))
    }

    /// Ask for bytes `[start, end)` to be read ahead (a mapped file only).
    fn will_need(&self, start: usize, end: usize) {
        if let Bytes::Map(m) = self {
            crate::index::will_need(m, start, end.saturating_sub(start));
        }
    }
}

/// Read-ahead in windows for a pass over the vocabulary data in id order, up to `end`
/// (`end` 0: off). Each window is asked for once, when the pass reaches the middle of
/// the one before it.
#[derive(Default)]
struct Ahead {
    /// the data before this offset has been asked for
    until: usize,
    end: usize,
    /// Offset pages must arrive before the data offsets are read from them.
    offsets_until: usize,
}

impl Ahead {
    const WINDOW: usize = 1 << 20;

    /// The pass reads `block` next.
    #[inline]
    fn at(&mut self, v: &Vocab, block: usize) {
        if self.end == 0 {
            return;
        }
        const OFFSETS_WINDOW: usize = 64 << 10;
        let offset = block * 8;
        if offset + OFFSETS_WINDOW / 2 >= self.offsets_until {
            let from = offset.max(self.offsets_until);
            let to = (offset + OFFSETS_WINDOW).min(v.offsets.as_slice().len());
            v.offsets.will_need(from, to);
            self.offsets_until = to;
        }
        let pos = v.block_offset(block);
        if self.until < self.end && pos + Self::WINDOW / 2 >= self.until {
            let from = pos.max(self.until);
            let to = (pos + Self::WINDOW).min(self.end);
            v.data.will_need(from, to);
            self.until = to;
        }
    }
}

/// Sorted, front-coded, memory-mapped vocabulary.
pub struct Vocab {
    data: Bytes,
    offsets: Bytes,
    len: u64,
    /// id of the first IRI key: literal keys (`"…`) sort before triple-term keys (`(…`),
    /// which sort before IRI keys (`<…`)
    first_iri: u64,
    first_triple: u64,
    /// the sparse index of the blocks' first keys, when the generation has one
    sparse: Option<Sparse>,
    /// Bulk decodes hint a block once per mapping, not on every warm export.
    /// Like sparse lookup hints, these are advisory. Evicted pages still fault in.
    prefetched: Box<[AtomicU64]>,
}

impl Vocab {
    pub fn empty() -> Vocab {
        Vocab {
            data: Bytes::Vec(Vec::new()),
            offsets: Bytes::Vec(Vec::new()),
            len: 0,
            first_iri: 0,
            first_triple: 0,
            sparse: None,
            prefetched: Box::new([]),
        }
    }

    pub fn open(dir: &Path) -> Result<Vocab> {
        // A lookup reads a few hundred bytes of the data, so the data is read a page at
        // a time (see `crate::index::io_hints`). With the sparse index, a lookup reads
        // one range of the offsets, which are mapped for random access too. Without it, a
        // binary search steps through the whole offsets file, which is small next to the
        // data and read in windows.
        let data = Bytes::open(&dir.join("vocab.dat"), true)?;
        let off_path = dir.join("vocab.off");
        let blocks = (std::fs::metadata(&off_path)?.len() as usize / 8).saturating_sub(1);
        let sparse = if crate::index::sparse_vocab() {
            Sparse::read(&dir.join("vocab.idx"), blocks, data.as_slice().len() as u64)
        } else {
            None
        };
        let offsets = Bytes::open(&off_path, sparse.is_some())?;
        let off = offsets.as_slice();
        let len = if off.len() >= 8 {
            u64::from_le_bytes(off[off.len() - 8..].try_into().unwrap())
        } else {
            0
        };
        let mut v = Vocab {
            data,
            offsets,
            len,
            first_iri: 0,
            first_triple: 0,
            sparse,
            prefetched: (0..blocks.div_ceil(64))
                .map(|_| AtomicU64::new(0))
                .collect(),
        };
        // a cheap test that the index belongs to these files: its first and last data
        // offsets (`sparkles check` compares every entry)
        if let Some(s) = &v.sparse
            && let Some(last) = s.len().checked_sub(1)
            && (s.offsets[0] != v.block_offset(0) as u64
                || s.offsets[last] != v.block_offset(last * IDX_BLOCKS) as u64)
        {
            tracing::warn!(
                target: "sparkles::vocab",
                "{} does not match its vocabulary; terms are looked up without it",
                dir.join("vocab.idx").display()
            );
            v.sparse = None;
        }
        v.first_iri = match v.find(b"<") {
            Ok(i) | Err(i) => i,
        };
        v.first_triple = match v.find(b"(") {
            Ok(i) | Err(i) => i,
        };
        Ok(v)
    }

    #[inline]
    pub fn len(&self) -> u64 {
        self.len
    }
    /// Is base id `id` an IRI (as opposed to a literal)? O(1) thanks to the sort order.
    #[inline]
    pub fn is_iri(&self, id: u64) -> bool {
        id >= self.first_iri
    }
    /// Is base id `id` an RDF 1.2 triple term?
    #[inline]
    pub fn is_triple(&self, id: u64) -> bool {
        id >= self.first_triple && id < self.first_iri
    }
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }
    pub fn disk_bytes(&self) -> u64 {
        (self.data.as_slice().len() + self.offsets.as_slice().len()) as u64
    }

    #[inline]
    fn num_blocks(&self) -> usize {
        // last 8 bytes store the term count
        (self.offsets.as_slice().len() / 8).saturating_sub(1)
    }
    #[inline]
    fn block_offset(&self, b: usize) -> usize {
        let o = self.offsets.as_slice();
        u64::from_le_bytes(o[b * 8..b * 8 + 8].try_into().unwrap()) as usize
    }

    /// First key of block `b` (stored uncompressed).
    fn block_first(&self, b: usize) -> &[u8] {
        let data = self.data.as_slice();
        let mut pos = self.block_offset(b);
        let _shared = read_varint(data, &mut pos);
        let len = read_varint(data, &mut pos) as usize;
        &data[pos..pos + len]
    }

    /// Decode a whole block, invoking `f(index_in_block, key)`; stops when `f` returns false.
    fn scan_block(&self, b: usize, mut f: impl FnMut(usize, &[u8]) -> bool) {
        let data = self.data.as_slice();
        let mut pos = self.block_offset(b);
        let start = b * FC_BLOCK;
        let n = FC_BLOCK.min((self.len as usize) - start);
        let mut key: Vec<u8> = Vec::with_capacity(64);
        for i in 0..n {
            let shared = read_varint(data, &mut pos) as usize;
            let len = read_varint(data, &mut pos) as usize;
            key.truncate(shared);
            key.extend_from_slice(&data[pos..pos + len]);
            pos += len;
            if !f(i, &key) {
                return;
            }
        }
    }

    pub fn get(&self, id: u64) -> Option<Vec<u8>> {
        if id >= self.len {
            return None;
        }
        let b = (id as usize) / FC_BLOCK;
        let want = (id as usize) % FC_BLOCK;
        let mut out = None;
        self.scan_block(b, |i, k| {
            if i == want {
                out = Some(k.to_vec());
                false
            } else {
                true
            }
        });
        out
    }

    /// Decode many ids (sorted ascending, deduplicated), touching each front-coded
    /// block once: `f(id, key)`.
    pub fn get_sorted(&self, ids: &[u64], f: impl FnMut(u64, &[u8])) {
        self.get_sorted_with_ahead(ids, f, true);
    }

    /// A serializer that already prefetched these blocks does not need dense hints
    /// or a per-block bitmap check on its decode's hot path.
    #[inline]
    pub(crate) fn get_sorted_with_ahead(
        &self,
        ids: &[u64],
        mut f: impl FnMut(u64, &[u8]),
        read_ahead: bool,
    ) {
        let mut ahead = if read_ahead {
            self.ahead_for(ids)
        } else {
            Ahead::default()
        };
        let mut i = 0;
        while i < ids.len() {
            let id = ids[i];
            if id >= self.len {
                break;
            }
            let b = (id as usize) / FC_BLOCK;
            ahead.at(self, b);
            let end = ((b + 1) * FC_BLOCK) as u64;
            let j = i + ids[i..].partition_point(|&x| x < end);
            let wanted = &ids[i..j];
            let last = (wanted[wanted.len() - 1] as usize) % FC_BLOCK;
            let mut w = 0;
            self.scan_block(b, |pos, k| {
                if w < wanted.len() && (wanted[w] as usize) % FC_BLOCK == pos {
                    f(wanted[w], k);
                    w += 1;
                }
                pos < last
            });
            i = j;
        }
    }

    /// Ask the kernel to read ahead, asynchronously, the pages of the front-coded blocks
    /// that hold these ids (sorted ascending): first those of their offsets, then those
    /// of the blocks. Pages in memory are left as they are. A cold decode of scattered
    /// ids then finds its pages read by many requests at once, instead of one page fault
    /// at a time. Nearby pages are asked for in one range.
    pub fn prefetch_sorted(&self, ids: &[u64]) {
        if !crate::index::io_hints() {
            return;
        }
        const GAP: usize = 64 << 10;
        fn advise(m: &Mmap, ranges: impl Iterator<Item = (usize, usize)>) {
            let mut cur: Option<(usize, usize)> = None;
            for (s, e) in ranges {
                let e = e.min(m.len());
                if s >= e {
                    continue;
                }
                cur = match cur {
                    Some((cs, ce)) if s <= ce + GAP => Some((cs, ce.max(e))),
                    Some((cs, ce)) => {
                        crate::index::will_need(m, cs, ce - cs);
                        Some((s, e))
                    }
                    None => Some((s, e)),
                };
            }
            if let Some((cs, ce)) = cur {
                crate::index::will_need(m, cs, ce - cs);
            }
        }
        let (Bytes::Map(data), Bytes::Map(offsets)) = (&self.data, &self.offsets) else {
            return;
        };
        let mut blocks: Vec<usize> = ids
            .iter()
            .take_while(|&&id| id < self.len)
            .map(|&id| id as usize / FC_BLOCK)
            .collect();
        blocks.dedup();
        let mut word_at = usize::MAX;
        let mut seen = 0;
        blocks.retain(|&b| {
            let at = b / 64;
            let word = &self.prefetched[at];
            if word_at != at {
                word_at = at;
                seen = word.load(AtomicOrdering::Relaxed);
            }
            let bit = 1u64 << (b % 64);
            if seen & bit != 0 {
                return false;
            }
            seen |= bit;
            word.fetch_or(bit, AtomicOrdering::Relaxed) & bit == 0
        });
        advise(offsets, blocks.iter().map(|&b| (b * 8, b * 8 + 16)));
        advise(
            data,
            blocks.iter().map(|&b| {
                let end = if b + 1 < self.num_blocks() {
                    self.block_offset(b + 1)
                } else {
                    data.len()
                };
                (self.block_offset(b), end)
            }),
        );
    }

    /// Overlap the vocabulary reads of a medium-sized result decoded in row order.
    /// With a sparse index, use its in-memory offsets and hint each group once, as
    /// `find` does. Warm requests then avoid sorting their cells or repeating hints.
    pub(crate) fn prefetch_terms(&self, ids: impl Iterator<Item = u64>) {
        if !crate::index::io_hints()
            || !matches!((&self.data, &self.offsets), (Bytes::Map(_), Bytes::Map(_)))
        {
            return;
        }
        let Some(s) = &self.sparse else {
            let mut ids: Vec<u64> = ids.collect();
            ids.sort_unstable();
            ids.dedup();
            self.prefetch_sorted(&ids);
            return;
        };
        let mut groups = Vec::new();
        let mut last = usize::MAX;
        for id in ids.filter(|&id| id < self.len) {
            let g = id as usize / (FC_BLOCK * IDX_BLOCKS);
            // Row-ordered columns often stay in one group for hundreds of cells.
            // Even a warm bitmap load need not be repeated for adjacent cells.
            if g != last && s.first_hint(g) {
                groups.push(g);
            }
            last = g;
        }
        groups.sort_unstable();
        for g in groups {
            let lo = g * IDX_BLOCKS;
            let hi = ((g + 1) * IDX_BLOCKS).min(self.num_blocks());
            self.offsets.will_need(lo * 8, (hi + 1) * 8);
            let end = s
                .offsets
                .get(g + 1)
                .map_or(self.data.as_slice().len(), |&o| o as usize);
            self.data.will_need(s.offsets[g] as usize, end);
        }
    }

    /// Binary search: `Ok(id)` if present, `Err(insertion point)` otherwise.
    pub fn find(&self, key: &[u8]) -> std::result::Result<u64, u64> {
        let nb = self.num_blocks();
        if nb == 0 {
            return Err(0);
        }
        // last block whose first key <= key
        let (mut lo, mut hi) = (0usize, nb);
        if let Some(s) = &self.sparse {
            // the group of blocks that holds the key, from memory; then only its offsets
            // and data are read, each asked for in one request
            let g = partition_point_by(s.len(), |g| s.key(g) <= key);
            if g == 0 {
                return Err(0);
            }
            let g = g - 1;
            lo = g * IDX_BLOCKS;
            hi = ((g + 1) * IDX_BLOCKS).min(nb);
            if crate::index::io_hints() && s.first_hint(g) {
                if let Bytes::Map(m) = &self.offsets {
                    crate::index::will_need(m, lo * 8, (hi - lo) * 8);
                }
                let end = s
                    .offsets
                    .get(g + 1)
                    .map_or(self.data.as_slice().len(), |&o| o as usize);
                self.data.will_need(s.offsets[g] as usize, end);
            }
            // block `lo` starts with the group's key, which is not above `key`
            lo += 1;
        }
        while lo < hi {
            let mid = (lo + hi) / 2;
            if self.block_first(mid) <= key {
                lo = mid + 1;
            } else {
                hi = mid;
            }
        }
        if lo == 0 {
            return Err(0);
        }
        let b = lo - 1;
        let mut result = Err(((b + 1) * FC_BLOCK).min(self.len as usize) as u64);
        self.scan_block(b, |i, k| match k.cmp(key) {
            std::cmp::Ordering::Less => true,
            std::cmp::Ordering::Equal => {
                result = Ok((b * FC_BLOCK + i) as u64);
                false
            }
            std::cmp::Ordering::Greater => {
                result = Err((b * FC_BLOCK + i) as u64);
                false
            }
        });
        result
    }

    /// Id range `[lo, hi)` of keys with the given byte prefix.
    pub fn prefix_range(&self, prefix: &[u8]) -> (u64, u64) {
        let lo = match self.find(prefix) {
            Ok(i) | Err(i) => i,
        };
        let mut upper = prefix.to_vec();
        // increment the prefix to get the exclusive upper bound
        while let Some(last) = upper.pop() {
            if last < 0xFF {
                upper.push(last + 1);
                let hi = match self.find(&upper) {
                    Ok(i) | Err(i) => i,
                };
                return (lo, hi);
            }
        }
        (lo, self.len)
    }

    /// The read-ahead for a pass over sorted `ids`: on when they are dense, at least one
    /// id per 4 KiB of the data they span. The data is mapped for random access, so a
    /// dense pass would otherwise read it a page per fault.
    fn ahead_for(&self, ids: &[u64]) -> Ahead {
        const DENSE: usize = 4096;
        let off = Ahead::default();
        let (Some(&first), Some(&last)) = (ids.first(), ids.last()) else {
            return off;
        };
        if ids.len() < 16 || first >= self.len {
            return off;
        }
        let start = self.block_offset(first as usize / FC_BLOCK);
        let end_block = (last.min(self.len - 1) as usize) / FC_BLOCK + 1;
        let end = if end_block < self.num_blocks() {
            self.block_offset(end_block)
        } else {
            self.data.as_slice().len()
        };
        if end.saturating_sub(start) > ids.len() * DENSE {
            return off;
        }
        Ahead {
            until: start,
            end,
            offsets_until: 0,
        }
    }

    /// Iterate all keys in order.
    pub fn for_each(&self, mut f: impl FnMut(u64, &[u8])) {
        let mut ahead = Ahead {
            until: 0,
            end: self.data.as_slice().len(),
            offsets_until: 0,
        };
        for b in 0..self.num_blocks() {
            ahead.at(self, b);
            self.scan_block(b, |i, k| {
                f((b * FC_BLOCK + i) as u64, k);
                true
            });
        }
    }
}

/// Streaming writer for a sorted vocabulary.
pub struct VocabWriter {
    data: BufWriter<File>,
    offsets: BufWriter<File>,
    pos: u64,
    count: u64,
    prev: Vec<u8>,
    buf: Vec<u8>,
    /// the entries of the sparse index: the data offset and first key of every
    /// [`IDX_BLOCKS`]-th block
    sparse: Vec<(u64, Vec<u8>)>,
    dir: std::path::PathBuf,
}

impl VocabWriter {
    pub fn create(dir: &Path) -> Result<VocabWriter> {
        Ok(VocabWriter {
            data: BufWriter::with_capacity(1 << 20, File::create(dir.join("vocab.dat"))?),
            offsets: BufWriter::new(File::create(dir.join("vocab.off"))?),
            pos: 0,
            count: 0,
            prev: Vec::new(),
            buf: Vec::with_capacity(256),
            sparse: Vec::new(),
            dir: dir.to_path_buf(),
        })
    }

    /// Append the next key; keys must be pushed in strictly increasing order.
    /// Returns the assigned id.
    pub fn push(&mut self, key: &[u8]) -> Result<u64> {
        debug_assert!(self.count == 0 || key > self.prev.as_slice());
        self.buf.clear();
        let shared = if (self.count as usize).is_multiple_of(FC_BLOCK) {
            self.offsets.write_all(&self.pos.to_le_bytes())?;
            if (self.count as usize).is_multiple_of(FC_BLOCK * IDX_BLOCKS) {
                self.sparse.push((self.pos, key.to_vec()));
            }
            0
        } else {
            self.prev
                .iter()
                .zip(key)
                .take_while(|(a, b)| a == b)
                .count()
        };
        write_varint(&mut self.buf, shared as u64);
        write_varint(&mut self.buf, (key.len() - shared) as u64);
        self.buf.extend_from_slice(&key[shared..]);
        self.data.write_all(&self.buf)?;
        self.pos += self.buf.len() as u64;
        self.prev.clear();
        self.prev.extend_from_slice(key);
        let id = self.count;
        self.count += 1;
        Ok(id)
    }

    /// Keys pushed so far.
    pub fn len(&self) -> u64 {
        self.count
    }

    pub fn is_empty(&self) -> bool {
        self.count == 0
    }

    pub fn finish(mut self) -> Result<u64> {
        self.offsets.write_all(&self.count.to_le_bytes())?;
        self.data.flush()?;
        self.offsets.flush()?;
        self.data.get_ref().sync_all()?;
        self.offsets.get_ref().sync_all()?;
        write_sparse_index(&self.dir, &self.sparse)?;
        Ok(self.count)
    }
}

/// Write `vocab.idx` with the given entries (see [`Sparse`]).
fn write_sparse_index(dir: &Path, entries: &[(u64, Vec<u8>)]) -> Result<()> {
    let mut f = File::create(dir.join("vocab.idx"))?;
    f.write_all(&Sparse::encode(entries))?;
    f.sync_all()?;
    Ok(())
}

/// For `sparkles check`: compare the sparse index in `dir` with the vocabulary's block
/// `offsets` and `data`. `None` without an index, the number of its entries when it
/// matches, and otherwise whether the mismatch is an error and what it is. An index
/// that does not read as one for this vocabulary is ignored by the server (a warning).
/// One that reads well but names other offsets or keys would be used (an error).
pub(crate) fn verify_sparse_index(
    dir: &Path,
    offsets: &[usize],
    data: &[u8],
) -> Option<std::result::Result<usize, (bool, String)>> {
    let buf = std::fs::read(dir.join("vocab.idx")).ok()?;
    let Some(s) = Sparse::decode(&buf, offsets.len(), data.len() as u64) else {
        return Some(Err((
            false,
            "not a sparse index of this vocabulary; lookups do without it".into(),
        )));
    };
    for g in 0..s.len() {
        let b = g * IDX_BLOCKS;
        let mut pos = offsets[b];
        let first = crate::index::read_varint_checked(data, &mut pos)
            .and_then(|_| crate::index::read_varint_checked(data, &mut pos))
            .and_then(|len| data.get(pos..pos + len as usize));
        if s.offsets[g] != offsets[b] as u64 || first != Some(s.key(g)) {
            return Some(Err((
                true,
                format!("entry {g} does not name the offset and first key of block {b}"),
            )));
        }
    }
    Some(Ok(s.len()))
}

/// Write the sparse index (`vocab.idx`) of the vocabulary in `dir` that lacks one,
/// built by a version from before the index. It reads the first key of every
/// [`IDX_BLOCKS`]-th block. Returns the number of entries.
pub fn add_sparse_index(dir: &Path) -> Result<usize> {
    let v = Vocab::open(dir)?;
    let entries: Vec<(u64, Vec<u8>)> = (0..v.num_blocks())
        .step_by(IDX_BLOCKS)
        .map(|b| (v.block_offset(b) as u64, v.block_first(b).to_vec()))
        .collect();
    let tmp = dir.join("vocab.idx.tmp");
    {
        let mut f = File::create(&tmp)?;
        f.write_all(&Sparse::encode(&entries))?;
        f.sync_all()?;
    }
    std::fs::rename(&tmp, dir.join("vocab.idx"))?;
    crate::store::sync_dir(dir)?;
    Ok(entries.len())
}

/// An append-only dictionary; used for the persisted delta vocabulary and for per-query
/// local vocabularies.
#[derive(Default)]
pub struct AppendVocab {
    keys: Vec<Arc<[u8]>>,
    map: FxHashMap<Arc<[u8]>, u64>,
    /// total key length
    key_bytes: usize,
}

impl AppendVocab {
    pub fn len(&self) -> u64 {
        self.keys.len() as u64
    }
    pub fn is_empty(&self) -> bool {
        self.keys.is_empty()
    }
    pub fn get(&self, id: u64) -> Option<&[u8]> {
        self.keys.get(id as usize).map(|k| &**k)
    }
    pub fn find(&self, key: &[u8]) -> Option<u64> {
        self.map.get(key).copied()
    }
    pub fn insert(&mut self, key: &[u8]) -> (u64, bool) {
        if let Some(&i) = self.map.get(key) {
            return (i, false);
        }
        let k: Arc<[u8]> = key.into();
        let i = self.keys.len() as u64;
        self.key_bytes += k.len();
        self.keys.push(k.clone());
        self.map.insert(k, i);
        (i, true)
    }
    /// Append `key` at the next position even if an earlier position holds it (the
    /// layers of a linked delta vocabulary are positional). A lookup by key finds the
    /// first position.
    pub(crate) fn push_dup(&mut self, key: &[u8]) {
        let k: Arc<[u8]> = key.into();
        let i = self.keys.len() as u64;
        self.key_bytes += k.len();
        self.keys.push(k.clone());
        self.map.entry(k).or_insert(i);
    }

    /// Keep the first `n` entries only.
    pub fn truncate(&mut self, n: u64) {
        while self.keys.len() as u64 > n {
            let k = self.keys.pop().expect("longer than n");
            self.key_bytes -= k.len();
            let i = self.keys.len() as u64;
            if self.map.get(&k) == Some(&i) {
                self.map.remove(&k);
            }
        }
    }
    pub fn iter(&self) -> impl Iterator<Item = (u64, &[u8])> {
        self.keys.iter().enumerate().map(|(i, k)| (i as u64, &**k))
    }
    pub fn bytes(&self) -> usize {
        self.key_bytes + self.keys.len() * 48
    }
}

/// The complete entries of a delta vocabulary file (`u32` length, key), and where they
/// end. An entry cut short ends them, as does a tail of zero bytes: no key is empty, and
/// a crash can leave zeros where the file's length reached the disk before its data
/// (on file systems that do not order the two). Either is a torn tail.
pub(crate) fn delta_entries(buf: &[u8]) -> (Vec<&[u8]>, usize) {
    let mut pos = 0;
    let mut keys = Vec::new();
    while pos + 4 <= buf.len() {
        let len = u32::from_le_bytes(buf[pos..pos + 4].try_into().unwrap()) as usize;
        if pos + 4 + len > buf.len() || (len == 0 && buf[pos..].iter().all(|&b| b == 0)) {
            break;
        }
        keys.push(&buf[pos + 4..pos + 4 + len]);
        pos += 4 + len;
    }
    (keys, pos)
}

/// The end of a delta vocabulary at some moment (see [`DeltaVocab::mark`]).
#[derive(Clone, Copy, Debug)]
pub struct VocabMark {
    entries: u64,
    bytes: u64,
    unsynced: bool,
    dirty_version: u64,
}

/// The persisted, append-only delta vocabulary (terms introduced by updates).
///
/// Readers and the single writer share it through an `RwLock`; ids only ever grow, so a
/// reader's snapshot remains valid while the writer appends.
pub struct DeltaVocab {
    inner: RwLock<AppendVocab>,
    file: Option<Arc<parking_lot::Mutex<DeltaFile>>>,
}

/// The append handle of a delta vocabulary file.
struct DeltaFile {
    w: BufWriter<File>,
    /// the file's length with the entries still in `w`'s buffer
    len: u64,
    /// entries were appended since the last [`DeltaVocab::sync`]
    unsynced: bool,
    /// Monotonic mutation version, including rollback; never restored from a mark.
    dirty_version: u64,
    version_exhausted: bool,
    /// Serializes fences and destructive rollback, not append/mark/flush.
    sync_gate: Arc<parking_lot::Mutex<()>>,
    #[cfg(test)]
    sync_hook: Option<Arc<dyn Fn() + Send + Sync>>,
    #[cfg(test)]
    fail_next_sync: bool,
}

impl DeltaFile {
    fn dirty(&mut self) -> Result<()> {
        self.unsynced = true;
        match self.dirty_version.checked_add(1) {
            Some(version) if !self.version_exhausted => self.dirty_version = version,
            _ => {
                self.version_exhausted = true;
                return Err(
                    std::io::Error::other("delta vocabulary dirty version exhausted").into(),
                );
            }
        }
        Ok(())
    }

    fn sync(file: &Arc<parking_lot::Mutex<Self>>) -> Result<()> {
        // Never acquire this gate while retaining the append mutex. Rollback uses
        // the same order, so a captured prefix cannot be truncated under its fence.
        let gate = file.lock().sync_gate.clone();
        let _fence = gate.lock();
        let (fd, version, end) = {
            let mut f = file.lock();
            if f.version_exhausted {
                return Err(
                    std::io::Error::other("delta vocabulary dirty version exhausted").into(),
                );
            }
            if !f.unsynced {
                return Ok(());
            }
            f.w.flush()?;
            (f.w.get_ref().try_clone()?, f.dirty_version, f.len)
        };
        #[cfg(test)]
        {
            let (hook, failed) = {
                let mut f = file.lock();
                (f.sync_hook.clone(), std::mem::take(&mut f.fail_next_sync))
            };
            if let Some(hook) = hook {
                hook();
            }
            if failed {
                return Err(std::io::Error::other("injected sync failure").into());
            }
        }
        // The exact descriptor keeps this generation alive. Subsequent appenders
        // can proceed; syncing extra suffix bytes never publishes that suffix.
        fd.sync_data()?;
        let mut f = file.lock();
        debug_assert!(f.len >= end, "rollback must serialize with prefix sync");
        if f.dirty_version == version && !f.version_exhausted {
            f.unsynced = false;
        }
        Ok(())
    }
}

impl DeltaVocab {
    pub(crate) fn fork_memory(&self, len: u64) -> DeltaVocab {
        let mut copy = AppendVocab::default();
        self.with(|v| {
            for n in 0..len {
                if let Some(k) = v.get(n) {
                    copy.push_dup(k);
                }
            }
        });
        DeltaVocab {
            inner: RwLock::new(copy),
            file: None,
        }
    }

    pub fn in_memory() -> DeltaVocab {
        DeltaVocab {
            inner: RwLock::new(AppendVocab::default()),
            file: None,
        }
    }

    /// The delta vocabulary of a sealed generation, for reading: no append handle, and a
    /// torn tail is ignored rather than truncated.
    pub fn open_read_only(path: &Path) -> Result<DeltaVocab> {
        let mut v = AppendVocab::default();
        if path.exists() {
            let mut buf = Vec::new();
            File::open(path)?.read_to_end(&mut buf)?;
            for key in delta_entries(&buf).0 {
                v.insert(key);
            }
        }
        Ok(DeltaVocab {
            inner: RwLock::new(v),
            file: None,
        })
    }

    pub fn open(path: &Path) -> Result<DeltaVocab> {
        let mut v = AppendVocab::default();
        if path.exists() {
            let mut buf = Vec::new();
            File::open(path)?.read_to_end(&mut buf)?;
            let (keys, pos) = delta_entries(&buf);
            for key in keys {
                v.insert(key);
            }
            // truncate a torn tail so future appends are well-formed
            if pos != buf.len() {
                OpenOptions::new()
                    .write(true)
                    .open(path)?
                    .set_len(pos as u64)?;
            }
        }
        let f = OpenOptions::new().create(true).append(true).open(path)?;
        let len = f.metadata()?.len();
        Ok(DeltaVocab {
            inner: RwLock::new(v),
            file: Some(Arc::new(parking_lot::Mutex::new(DeltaFile {
                w: BufWriter::new(f),
                len,
                unsynced: false,
                dirty_version: 0,
                version_exhausted: false,
                sync_gate: Arc::new(parking_lot::Mutex::new(())),
                #[cfg(test)]
                sync_hook: None,
                #[cfg(test)]
                fail_next_sync: false,
            }))),
        })
    }

    /// The delta vocabulary of a linked generation (a branch that reads an upstream
    /// generation's files). Its first entries are those of the upstream files `prefix`,
    /// each `(path, end)` pair contributing the entries up to id `end` (a file's
    /// entries continue the ids of the pairs before it). Its own entries follow, from
    /// `own`, which takes the appends unless the generation is opened `read_only`.
    pub(crate) fn open_layered(
        prefix: &[(std::path::PathBuf, u64)],
        own: &Path,
        read_only: bool,
    ) -> Result<DeltaVocab> {
        let mut v = AppendVocab::default();
        for (path, end) in prefix {
            if v.len() >= *end {
                continue;
            }
            let mut buf = Vec::new();
            File::open(path)?.read_to_end(&mut buf)?;
            for key in delta_entries(&buf).0 {
                if v.len() >= *end {
                    break;
                }
                // ids are positions: a key seen before still takes its position here
                v.push_dup(key);
            }
            if v.len() < *end {
                return Err(crate::error::Error::Corrupt(format!(
                    "{} has {} delta terms, fewer than the {end} a branch's link names",
                    path.display(),
                    v.len()
                )));
            }
        }
        let own_v = if read_only {
            DeltaVocab::open_read_only(own)?
        } else {
            DeltaVocab::open(own)?
        };
        let own_inner = own_v.inner.into_inner();
        for (_, key) in own_inner.iter() {
            v.push_dup(key);
        }
        Ok(DeltaVocab {
            inner: RwLock::new(v),
            file: own_v.file,
        })
    }

    pub fn len(&self) -> u64 {
        self.inner.read().len()
    }
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
    pub fn get(&self, id: u64) -> Option<Vec<u8>> {
        self.inner.read().get(id).map(|k| k.to_vec())
    }
    pub fn find(&self, key: &[u8]) -> Option<u64> {
        self.inner.read().find(key)
    }
    pub fn with<R>(&self, f: impl FnOnce(&AppendVocab) -> R) -> R {
        f(&self.inner.read())
    }

    /// Insert (writer only). Newly added keys are buffered for the next [`sync`](Self::sync).
    pub fn insert(&self, key: &[u8]) -> Result<u64> {
        let (id, new) = self.inner.write().insert(key);
        if new && let Some(f) = &self.file {
            let mut f = f.lock();
            f.dirty()?;
            f.w.write_all(&(key.len() as u32).to_le_bytes())?;
            f.w.write_all(key)?;
            f.len += 4 + key.len() as u64;
        }
        Ok(id)
    }

    /// Where the vocabulary ends now, for a later [`rollback`](Self::rollback).
    pub fn mark(&self) -> VocabMark {
        let entries = self.len();
        match &self.file {
            Some(f) => {
                let f = f.lock();
                VocabMark {
                    entries,
                    bytes: f.len,
                    unsynced: f.unsynced,
                    dirty_version: f.dirty_version,
                }
            }
            None => VocabMark {
                entries,
                bytes: 0,
                unsynced: false,
                dirty_version: 0,
            },
        }
    }

    /// Remove every entry inserted since `m` (writer only, with no snapshot reaching
    /// those ids). Entries still in the write buffer are dropped unwritten; when the
    /// buffer already spilled past the mark, the file is truncated back to it.
    pub fn rollback(&self, m: &VocabMark) -> Result<()> {
        // Only destructive rollback waits for an in-flight fence. Normal writers
        // can mark and append while it runs; marks never restore the version.
        let gate = self.file.as_ref().map(|f| f.lock().sync_gate.clone());
        let _fence = gate.as_ref().map(|g| g.lock());
        let mut inner = self.inner.write();
        if inner.len() <= m.entries {
            return Ok(());
        }
        inner.truncate(m.entries);
        let Some(f) = &self.file else {
            return Ok(());
        };
        let mut guard = f.lock();
        let f = &mut *guard;
        debug_assert!(f.dirty_version >= m.dirty_version);
        f.dirty()?;
        // take the buffered bytes out without writing them (`into_parts` does not flush)
        let dup = f.w.get_ref().try_clone()?;
        let old = std::mem::replace(&mut f.w, BufWriter::new(dup));
        let (_, buf) = old.into_parts();
        let buf = buf.unwrap_or_default();
        let on_disk = f.len - buf.len() as u64;
        let w = &mut f.w;
        if on_disk <= m.bytes {
            // the entries since the mark are all still buffered
            w.write_all(&buf[..(m.bytes - on_disk) as usize])?;
            f.unsynced = m.unsynced;
        } else {
            // the buffer spilled past the mark: cut the file back
            w.get_ref().set_len(m.bytes)?;
            f.unsynced = true;
        }
        f.len = m.bytes;
        Ok(())
    }

    /// Make every inserted entry durable. Without new entries since the last sync this
    /// does nothing: an `fdatasync` of a file with nothing to write still costs a device
    /// cache flush on some file systems, and every commit calls this.
    pub fn sync(&self) -> Result<()> {
        match &self.file {
            Some(file) => DeltaFile::sync(file),
            None => Ok(()),
        }
    }

    /// Schedule this exact generation's append file on a reusable fence worker.
    /// Publication still waits for completion; grouped writers may append a suffix.
    pub(crate) fn sync_on(&self, worker: &mut LazySync) -> Option<PendingSync> {
        self.file
            .as_ref()
            .and_then(|file| worker.start(file.clone()))
    }

    /// Whether entries were inserted since the last [`sync`](Self::sync), that is,
    /// whether the next one writes and syncs anything.
    pub fn needs_sync(&self) -> bool {
        self.file.as_ref().is_some_and(|f| f.lock().unsynced)
    }

    #[cfg(test)]
    pub(crate) fn set_sync_hook(&self, hook: Arc<dyn Fn() + Send + Sync>) {
        if let Some(f) = &self.file {
            f.lock().sync_hook = Some(hook);
        }
    }

    /// Make the next [`sync`](Self::sync) with new entries fail after writing them to
    /// the file, as a failed `fdatasync` would.
    #[cfg(test)]
    pub(crate) fn fail_next_sync(&self) {
        if let Some(f) = &self.file {
            f.lock().fail_next_sync = true;
        }
    }

    /// Write buffered entries to the file (without `fsync`) and return its length in
    /// bytes: every entry inserted so far lies within it. 0 for an in-memory vocabulary.
    /// For backups, which read the file up to this length through their own handle.
    pub fn flush(&self) -> Result<u64> {
        match &self.file {
            Some(f) => {
                let mut f = f.lock();
                f.w.flush()?;
                Ok(f.w.get_ref().metadata()?.len())
            }
            None => Ok(0),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn delta_sync_only_when_entries_were_added() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("dvocab.bin");
        let unsynced = |v: &DeltaVocab| v.file.as_ref().unwrap().lock().unsynced;
        let v = DeltaVocab::open(&path).unwrap();
        assert!(!unsynced(&v));
        assert_eq!(v.insert(b"<http://x/a>").unwrap(), 0);
        assert!(unsynced(&v));
        v.sync().unwrap();
        assert!(!unsynced(&v));
        // a key it already has appends nothing
        assert_eq!(v.insert(b"<http://x/a>").unwrap(), 0);
        assert!(!unsynced(&v));
        assert_eq!(v.insert(b"<http://x/b>").unwrap(), 1);
        v.sync().unwrap();
        drop(v);
        let v = DeltaVocab::open(&path).unwrap();
        assert_eq!(v.len(), 2);
        assert_eq!(v.get(1).unwrap(), b"<http://x/b>");
    }

    #[test]
    fn a_tail_of_zeros_is_torn_but_an_empty_entry_before_data_is_kept() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("dvocab.bin");
        let entry = |k: &[u8]| [&(k.len() as u32).to_le_bytes()[..], k].concat();
        let good = [entry(b"<http://x/a>"), entry(b"<http://x/b>")].concat();
        // the length of a crash's appends reached the disk, their data did not
        std::fs::write(&path, [&good[..], &[0u8; 23][..]].concat()).unwrap();
        let v = DeltaVocab::open(&path).unwrap();
        assert_eq!(v.len(), 2);
        assert_eq!(std::fs::read(&path).unwrap(), good);
        assert_eq!(v.insert(b"<http://x/c>").unwrap(), 2);
        v.sync().unwrap();
        drop(v);
        assert_eq!(DeltaVocab::open(&path).unwrap().len(), 3);
        // zeros followed by data are entries, as they always were read
        let odd = [&good[..], &entry(b"")[..], &entry(b"<http://x/d>")[..]].concat();
        std::fs::write(&path, &odd).unwrap();
        assert_eq!(DeltaVocab::open_read_only(&path).unwrap().len(), 4);
        assert_eq!(DeltaVocab::open(&path).unwrap().len(), 4);
        assert_eq!(std::fs::read(&path).unwrap(), odd);
    }

    #[test]
    fn get_sorted_matches_get() {
        let dir = tempfile::tempdir().unwrap();
        let mut w = VocabWriter::create(dir.path()).unwrap();
        for i in 0..1000 {
            w.push(format!("<http://x/{i:05}").as_bytes()).unwrap();
        }
        w.finish().unwrap();
        let v = Vocab::open(dir.path()).unwrap();
        let ids: Vec<u64> = (0..1000).filter(|i| i % 7 == 0 || i % 16 == 15).collect();
        let mut got = Vec::new();
        v.get_sorted(&ids, |i, k| got.push((i, k.to_vec())));
        assert_eq!(got.len(), ids.len());
        for (i, k) in got {
            assert_eq!(v.get(i).unwrap(), k);
        }
    }

    #[test]
    fn prefetch_preserves_terms_with_partial_last_block_and_without_sparse_index() {
        let dir = tempfile::tempdir().unwrap();
        let keys = write_keys(dir.path(), 3 * FC_BLOCK * IDX_BLOCKS + 77);
        for sparse in [true, false] {
            let mut v = Vocab::open(dir.path()).unwrap();
            if !sparse {
                v.sparse = None;
            }
            let last = v.len() - 1;
            // Unsorted, repeated, out-of-range and final-block ids are all possible
            // in a row-oriented answer. Hinting must not affect its decoded terms.
            let ids = [last, 0, 2048, last, v.len(), u64::MAX];
            v.prefetch_terms(ids.into_iter());
            v.prefetch_terms(ids.into_iter());
            let mut sorted: Vec<u64> = (0..v.len()).step_by(7).chain(ids).collect();
            sorted.sort_unstable();
            v.prefetch_sorted(&sorted);
            sorted.dedup();
            let expected: Vec<_> = sorted
                .iter()
                .copied()
                .filter(|&id| id < v.len())
                .map(|id| (id, keys[id as usize].clone()))
                .collect();
            for read_ahead in [true, false] {
                let mut got = Vec::new();
                v.get_sorted_with_ahead(
                    &sorted,
                    |id, key| got.push((id, key.to_vec())),
                    read_ahead,
                );
                assert_eq!(got, expected);
            }
        }
    }

    #[test]
    fn front_coded_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let mut keys: Vec<Vec<u8>> = (0..1000)
            .map(|i| format!("<http://example.org/resource/{i:05}").into_bytes())
            .collect();
        keys.push(b"\"zzz\xff".to_vec());
        keys.sort();
        let mut w = VocabWriter::create(dir.path()).unwrap();
        for k in &keys {
            w.push(k).unwrap();
        }
        w.finish().unwrap();
        let v = Vocab::open(dir.path()).unwrap();
        assert_eq!(v.len(), keys.len() as u64);
        for (i, k) in keys.iter().enumerate() {
            assert_eq!(v.get(i as u64).unwrap(), *k);
            assert_eq!(v.find(k), Ok(i as u64));
        }
        assert!(v.find(b"<http://example.org/resource/00010x").is_err());
        let (lo, hi) = v.prefix_range(b"<http://example.org/resource/001");
        assert_eq!(hi - lo, 100);
    }

    /// A vocabulary of `n` keys spread over several sparse-index groups, in `dir`.
    fn write_keys(dir: &Path, n: usize) -> Vec<Vec<u8>> {
        let mut keys: Vec<Vec<u8>> = (0..n)
            .map(|i| format!("<http://example.org/r/{:07}", i * 3).into_bytes())
            .chain((0..n / 4).map(|i| format!("\"lit {i:06}").into_bytes()))
            .collect();
        keys.sort();
        let mut w = VocabWriter::create(dir).unwrap();
        for k in &keys {
            w.push(k).unwrap();
        }
        w.finish().unwrap();
        keys
    }

    #[test]
    fn sparse_index_finds_what_the_full_search_finds() {
        let dir = tempfile::tempdir().unwrap();
        // 5 groups and a partial one
        let keys = write_keys(dir.path(), 4 * FC_BLOCK * IDX_BLOCKS + 1234);
        let v = Vocab::open(dir.path()).unwrap();
        let s = v.sparse.as_ref().expect("the writer wrote vocab.idx");
        assert_eq!(s.len(), v.num_blocks().div_ceil(IDX_BLOCKS));
        let mut plain = Vocab::open(dir.path()).unwrap();
        plain.sparse = None;
        assert_eq!(
            (v.first_iri, v.first_triple),
            (plain.first_iri, plain.first_triple)
        );
        let mut probes: Vec<Vec<u8>> = keys.clone();
        for k in keys.iter().step_by(97) {
            // between keys, before the first and after the last
            let mut a = k.clone();
            a.push(b'0');
            probes.push(a);
            probes.push(k[..k.len() - 1].to_vec());
        }
        probes.extend([b"".to_vec(), b"!".to_vec(), b"\xff\xff".to_vec()]);
        for (g, _) in s.offsets.iter().enumerate() {
            probes.push(s.key(g).to_vec());
        }
        for p in &probes {
            assert_eq!(v.find(p), plain.find(p), "{}", String::from_utf8_lossy(p));
        }
        for (i, k) in keys.iter().enumerate() {
            assert_eq!(v.find(k), Ok(i as u64));
        }
        assert_eq!(
            v.prefix_range(b"<http://example.org/r/00001"),
            plain.prefix_range(b"<http://example.org/r/00001")
        );
    }

    #[test]
    fn each_group_asks_for_read_ahead_once() {
        let dir = tempfile::tempdir().unwrap();
        // 64 groups of IRIs and 16 of literals: two words of the bitmap
        let keys = write_keys(dir.path(), 64 * FC_BLOCK * IDX_BLOCKS);
        let v = Vocab::open(dir.path()).unwrap();
        let s = v.sparse.as_ref().unwrap();
        assert_eq!(s.len(), 80);
        for g in [0, 63, 64, 79] {
            assert!(s.first_hint(g));
            assert!(!s.first_hint(g));
        }
        assert!(s.first_hint(1));
        // a lookup marks its group, and finds the key with or without the hint
        let i = 5 * FC_BLOCK * IDX_BLOCKS + 3;
        assert_eq!(v.find(&keys[i]), Ok(i as u64));
        assert!(!s.first_hint(5) || !crate::index::io_hints());
        assert_eq!(v.find(&keys[i]), Ok(i as u64));
    }

    #[test]
    fn sparse_index_added_later_matches_the_written_one() {
        let dir = tempfile::tempdir().unwrap();
        write_keys(dir.path(), 3 * FC_BLOCK * IDX_BLOCKS + 77);
        let written = std::fs::read(dir.path().join("vocab.idx")).unwrap();
        std::fs::remove_file(dir.path().join("vocab.idx")).unwrap();
        assert!(Vocab::open(dir.path()).unwrap().sparse.is_none());
        let n = add_sparse_index(dir.path()).unwrap();
        assert_eq!(n, 4);
        assert_eq!(
            std::fs::read(dir.path().join("vocab.idx")).unwrap(),
            written
        );
        assert!(Vocab::open(dir.path()).unwrap().sparse.is_some());
    }

    #[test]
    fn a_sparse_index_of_another_vocabulary_is_ignored() {
        let a = tempfile::tempdir().unwrap();
        let b = tempfile::tempdir().unwrap();
        write_keys(a.path(), 2 * FC_BLOCK * IDX_BLOCKS);
        let keys = write_keys(b.path(), 5 * FC_BLOCK * IDX_BLOCKS);
        std::fs::copy(a.path().join("vocab.idx"), b.path().join("vocab.idx")).unwrap();
        let v = Vocab::open(b.path()).unwrap();
        assert!(v.sparse.is_none());
        assert_eq!(v.find(&keys[4000]), Ok(4000));
        // a cut file and a wrong magic number
        let idx = std::fs::read(a.path().join("vocab.idx")).unwrap();
        let blocks = (2 * IDX_BLOCKS * FC_BLOCK + 2 * IDX_BLOCKS * FC_BLOCK / 4).div_ceil(FC_BLOCK);
        assert!(Sparse::decode(&idx, blocks, u64::MAX).is_some());
        assert!(Sparse::decode(&idx[..idx.len() - 1], blocks, u64::MAX).is_none());
        let mut bad = idx.clone();
        bad[0] = b'X';
        assert!(Sparse::decode(&bad, blocks, u64::MAX).is_none());
    }

    #[test]
    fn check_compares_the_sparse_index_with_the_blocks() {
        let dir = tempfile::tempdir().unwrap();
        write_keys(dir.path(), 3 * FC_BLOCK * IDX_BLOCKS);
        let off = std::fs::read(dir.path().join("vocab.off")).unwrap();
        let data = std::fs::read(dir.path().join("vocab.dat")).unwrap();
        let offsets: Vec<usize> = off.as_chunks::<8>().0[..off.len() / 8 - 1]
            .iter()
            .map(|c| u64::from_le_bytes(*c) as usize)
            .collect();
        let verify = || verify_sparse_index(dir.path(), &offsets, &data);
        assert_eq!(verify(), Some(Ok(4)));
        // the last key's final digit, lowered: still in order, but not block 384's key
        let path = dir.path().join("vocab.idx");
        let mut idx = std::fs::read(&path).unwrap();
        let n = idx.len();
        idx[n - 1] -= 1;
        std::fs::write(&path, &idx).unwrap();
        assert!(matches!(verify(), Some(Err((true, _)))));
        std::fs::write(&path, b"junk").unwrap();
        assert!(matches!(verify(), Some(Err((false, _)))));
        std::fs::remove_file(&path).unwrap();
        assert_eq!(verify(), None);
    }

    #[test]
    fn get_sorted_reads_ahead_and_matches_get() {
        let dir = tempfile::tempdir().unwrap();
        let keys = write_keys(dir.path(), 2 * FC_BLOCK * IDX_BLOCKS);
        let v = Vocab::open(dir.path()).unwrap();
        for ids in [
            (0..keys.len() as u64).collect::<Vec<_>>(),
            (0..keys.len() as u64).step_by(1000).collect(),
        ] {
            let mut got = Vec::new();
            v.get_sorted(&ids, |id, k| got.push((id, k.to_vec())));
            let want: Vec<_> = ids.iter().map(|&i| (i, keys[i as usize].clone())).collect();
            assert_eq!(got, want);
        }
        let mut all = Vec::new();
        v.for_each(|_, k| all.push(k.to_vec()));
        assert_eq!(all, keys);
    }
}
