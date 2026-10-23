//! Term dictionaries.
//!
//! * [`Vocab`] — the immutable **base vocabulary** built by the bulk loader / compaction.
//!   Keys are sorted, so id order equals key order (QLever-style), which lets range and
//!   prefix filters work on ids. Stored front-coded (blocks of [`FC_BLOCK`] keys, each key
//!   stored as `shared-prefix-len, suffix`) and memory-mapped.
//! * [`DeltaVocab`] — append-only dictionary for terms introduced by SPARQL Update. Ids
//!   are in insertion order. Persisted in `delta.vocab` as checksummed chunks (see
//!   [`delta`]).
//! * [`LocalVocab`] — per-query dictionary for computed terms.

pub mod delta;
pub mod fsst;
pub mod numeric;
mod sync;
pub(crate) use delta::DeltaFile;
pub use delta::parse_delta;
pub use delta::{DeltaVocab, VocabMark};
pub(crate) use sync::{LazySync, PendingSync};

use crate::disk::WritebackFile;
use crate::error::Result;
use crate::ioacct::{self, IoFile, Phase};
use memmap2::Mmap;
use rustc_hash::FxHashMap;
use std::fs::File;
use std::io::{BufWriter, Write};
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
    /// the hashes of keys whose group has been asked for, one per slot and 0 for none (see
    /// [`KeyPrefetch`])
    warm_keys: Box<[AtomicU64]>,
}

/// The slots of [`Sparse::warm_keys`], 8 KiB per vocabulary with a sparse index.
const WARM_KEY_SLOTS: usize = 1024;

/// The hash of a key in [`Sparse::warm_keys`], never 0.
fn warm_key_hash(key: &[u8]) -> u64 {
    use std::hash::Hasher;
    let mut h = rustc_hash::FxHasher::default();
    h.write(key);
    h.finish().max(1)
}

impl Sparse {
    /// Whether a key of this hash was seen by [`KeyPrefetch`] after its group was asked
    /// for. Groups are asked for once in the life of the vocabulary, so the answer stays
    /// true. Two keys of one hash would leave the second key's group unasked, which costs
    /// a slower cold read and changes no answer.
    fn key_warm(&self, h: u64) -> bool {
        self.warm_keys[h as usize % WARM_KEY_SLOTS].load(AtomicOrdering::Relaxed) == h
    }

    fn set_key_warm(&self, h: u64) {
        self.warm_keys[h as usize % WARM_KEY_SLOTS].store(h, AtomicOrdering::Relaxed);
    }

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
        let first = word.load(AtomicOrdering::Relaxed) & bit == 0
            && word.fetch_or(bit, AtomicOrdering::Relaxed) & bit == 0;
        if first {
            ioacct::vocab_group_hinted();
        }
        first
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
            warm_keys: (0..WARM_KEY_SLOTS).map(|_| AtomicU64::new(0)).collect(),
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
    fn will_need(&self, file: IoFile, start: usize, end: usize) {
        if let Bytes::Map(m) = self {
            crate::index::will_need(file, m, start, end.saturating_sub(start));
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
            v.offsets.will_need(IoFile::VocabOff, from, to);
            self.offsets_until = to;
        }
        let pos = v.block_offset(block);
        let window = match crate::index::read_window() {
            0 => 1 << 20,
            w => w,
        };
        if self.until < self.end && pos + window / 2 >= self.until {
            let from = pos.max(self.until);
            let to = (pos + window).min(self.end);
            v.data.will_need(IoFile::VocabData, from, to);
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
    /// the values of the numeric literals (`vocab.num`), when the generation has them
    num: Option<numeric::NumColumn>,
    /// the FSST tables of the suffixes, when the vocabulary is FSST-coded (see [`fsst`])
    fsst: Option<Box<fsst::Tables>>,
}

/// One entry of a front-coded block: the length of the prefix it shares with the key
/// before it, and where its stored bytes are.
#[derive(Clone, Copy)]
struct Entry {
    shared: usize,
    start: usize,
    len: usize,
    /// the bytes are FSST codes
    coded: bool,
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
            num: None,
            fsst: None,
        }
    }

    /// Open the vocabulary in `dir`, FSST-coded when `vocab.fsst` is there. A generation
    /// opens its vocabulary with [`Vocab::open_codec`] and the codec its `meta.json`
    /// names instead.
    pub fn open(dir: &Path) -> Result<Vocab> {
        Self::open_codec(dir, dir.join(fsst::FILE).exists())
    }

    /// Whether the suffixes are FSST-coded.
    pub fn is_fsst(&self) -> bool {
        self.fsst.is_some()
    }

    /// Open the vocabulary in `dir`, whose suffixes are FSST-coded when `fsst` (and then
    /// `vocab.fsst` must be there).
    pub fn open_codec(dir: &Path, fsst: bool) -> Result<Vocab> {
        let tables = if fsst {
            Some(Box::new(fsst::Tables::read(dir)?))
        } else {
            None
        };
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
            num: None,
            fsst: tables,
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
        if crate::index::numeric_column() {
            v.num = numeric::NumColumn::open(dir, v.len, v.first_triple);
        }
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
    /// The first base id of an RDF 1.2 triple term: the literals come before it.
    #[inline]
    pub fn first_triple(&self) -> u64 {
        self.first_triple
    }
    /// The first base id of an IRI: the triple terms come before it.
    #[inline]
    pub fn first_iri(&self) -> u64 {
        self.first_iri
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

    /// The numeric column (`vocab.num`), when the generation has one and it is in use.
    #[inline]
    pub fn numeric(&self) -> Option<&numeric::NumColumn> {
        self.num.as_ref()
    }

    #[inline]
    fn num_blocks(&self) -> usize {
        // last 8 bytes store the term count
        (self.offsets.as_slice().len() / 8).saturating_sub(1)
    }
    #[inline]
    fn block_offset(&self, b: usize) -> usize {
        self.block_offset_at::<true>(b)
    }

    /// [`Vocab::block_offset`], recording the read under a profile when `P` (the hot
    /// lookups check for a profile once and pass `false` without one).
    #[inline(always)]
    fn block_offset_at<const P: bool>(&self, b: usize) -> usize {
        // Observe residency before the first mapped load, including offsets read
        // while preparing hints or binary-searching block starts.
        if P && ioacct::profiling() {
            self.touch_offset(b);
        }
        let o = self.offsets.as_slice();
        u64::from_le_bytes(o[b * 8..b * 8 + 8].try_into().unwrap()) as usize
    }

    /// First key of block `b` (stored uncompressed).
    fn block_first(&self, b: usize) -> &[u8] {
        self.block_first_at::<true>(b)
    }

    #[inline(always)]
    fn block_first_at<const P: bool>(&self, b: usize) -> &[u8] {
        if P {
            self.touch_block(b);
        }
        let data = self.data.as_slice();
        let mut pos = self.block_offset_at::<P>(b);
        let _shared = read_varint(data, &mut pos);
        let len = read_varint(data, &mut pos) as usize;
        &data[pos..pos + len]
    }

    /// Record the page of block `b`'s offset as read (profile only, out of line so that
    /// [`Vocab::block_offset`] stays small).
    #[cold]
    #[inline(never)]
    fn touch_offset(&self, b: usize) {
        if let Bytes::Map(o) = &self.offsets {
            ioacct::touch(IoFile::VocabOff, o.as_ptr(), b * 8, 8);
        }
    }

    /// Record the pages of front-coded block `b` as read (profile only). The hot paths
    /// that call it on every block carry the check alone.
    #[inline(always)]
    fn touch_block(&self, b: usize) {
        if ioacct::profiling() {
            self.touch_block_profiled(b);
        }
    }

    #[cold]
    #[inline(never)]
    fn touch_block_profiled(&self, b: usize) {
        if let Bytes::Map(m) = &self.data {
            let start = self.block_offset(b);
            let end = if b + 1 < self.num_blocks() {
                self.block_offset(b + 1)
            } else {
                m.len()
            };
            ioacct::touch(
                IoFile::VocabData,
                m.as_ptr(),
                start,
                end.saturating_sub(start),
            );
        }
    }

    /// Decode a whole block, invoking `f(index_in_block, key)`; stops when `f` returns false.
    fn scan_block(&self, b: usize, f: impl FnMut(usize, &[u8]) -> bool) {
        if ioacct::profiling() {
            self.scan_block_at::<true>(b, f)
        } else {
            self.scan_block_at::<false>(b, f)
        }
    }

    /// [`Vocab::scan_block`], counting and recording the block under a profile when `P`.
    #[inline(always)]
    fn scan_block_at<const P: bool>(&self, b: usize, mut f: impl FnMut(usize, &[u8]) -> bool) {
        let data = self.data.as_slice();
        let mut pos = self.block_offset_at::<P>(b);
        let start = b * FC_BLOCK;
        let n = FC_BLOCK.min((self.len as usize) - start);
        if P {
            ioacct::vocab_blocks_scanned(1);
            self.touch_block(b);
        }
        // Single-key callers own this fresh buffer throughout their scan.
        let mut key = Vec::with_capacity(64);
        for i in 0..n {
            self.reconstruct_key(data, &mut pos, (start + i) as u64, i == 0, &mut key);
            if !f(i, &key) {
                return;
            }
        }
    }

    /// The entry at `pos` (advanced past it), the first of its block when `first`.
    #[inline(always)]
    fn entry(&self, data: &[u8], pos: &mut usize, first: bool) -> Entry {
        let shared = read_varint(data, pos) as usize;
        let word = read_varint(data, pos) as usize;
        let (len, coded) = if self.fsst.is_some() && !first {
            (word >> 1, word & 1 == 1)
        } else {
            (word, false)
        };
        let start = *pos;
        *pos += len;
        Entry {
            shared,
            start,
            len,
            coded,
        }
    }

    /// The length of the suffix `e` (of key `id`) stands for.
    #[inline(always)]
    fn suffix_len(&self, data: &[u8], e: Entry, id: u64) -> usize {
        match &self.fsst {
            Some(t) if e.coded => t.table(id).decoded_len(&data[e.start..e.start + e.len]),
            _ => e.len,
        }
    }

    /// Append the suffix of entry `e` (of key `id`) to `key`. With `exact`, the buffer
    /// grows by no more than the suffix (see [`fsst::Table::decode_exact`]).
    #[inline(always)]
    fn push_suffix(&self, data: &[u8], e: Entry, id: u64, key: &mut Vec<u8>, exact: bool) {
        let bytes = &data[e.start..e.start + e.len];
        match &self.fsst {
            Some(t) if e.coded => {
                if exact {
                    t.table(id).decode_exact(bytes, key)
                } else {
                    t.table(id).decode_into(bytes, key)
                }
            }
            _ => key.extend_from_slice(bytes),
        }
    }

    /// One front-coded key, `id`, the first of its block when `first`; shared by the
    /// fresh-buffer and reusable-buffer loops.
    #[inline(always)]
    fn reconstruct_key(
        &self,
        data: &[u8],
        pos: &mut usize,
        id: u64,
        first: bool,
        key: &mut Vec<u8>,
    ) {
        let e = self.entry(data, pos, first);
        key.truncate(e.shared);
        self.push_suffix(data, e, id, key, false);
    }

    /// Each block starts with an empty reconstruction, while capacity can be reused
    /// by a sorted decode across blocks. Single-key callers retain a local buffer.
    #[inline(always)]
    fn scan_block_with_scratch(
        &self,
        b: usize,
        key: &mut Vec<u8>,
        mut f: impl FnMut(usize, &[u8]) -> bool,
    ) {
        key.clear();
        let data = self.data.as_slice();
        let mut pos = self.block_offset(b);
        ioacct::vocab_blocks_scanned(1);
        self.touch_block(b);
        let start = b * FC_BLOCK;
        let n = FC_BLOCK.min((self.len as usize) - start);
        for i in 0..n {
            self.reconstruct_key(data, &mut pos, (start + i) as u64, i == 0, key);
            if !f(i, key) {
                return;
            }
        }
    }

    pub fn get(&self, id: u64) -> Option<Vec<u8>> {
        self.get_with(id, |key| key.to_vec())
    }

    /// Decode a key while its block scratch is borrowed, avoiding an intermediate
    /// owned key when the caller immediately converts it to another owned value.
    pub(crate) fn get_with<T>(&self, id: u64, f: impl FnMut(&[u8]) -> T) -> Option<T> {
        if id >= self.len {
            return None;
        }
        // one check for a profile here, rather than one in each step of the decoding:
        // a result of a few hundred terms decodes each with its own call
        if ioacct::profiling() {
            let _t = ioacct::timer(Phase::VocabRead);
            self.get_with_at::<true, T>(id, f)
        } else {
            self.get_with_at::<false, T>(id, f)
        }
    }

    #[inline(always)]
    fn get_with_at<const P: bool, T>(&self, id: u64, mut f: impl FnMut(&[u8]) -> T) -> Option<T> {
        let b = (id as usize) / FC_BLOCK;
        let want = (id as usize) % FC_BLOCK;
        let mut out = None;
        self.scan_block_at::<P>(b, |i, k| {
            if i == want {
                out = Some(f(k));
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

    /// Fallible cursor decoding: reserve reconstruction storage before growing it,
    /// including intermediate keys needed to reach a selected front-coded key.
    pub(crate) fn get_sorted_checked<E>(
        &self,
        ids: &[u64],
        mut reserve: impl FnMut(usize) -> std::result::Result<(), E>,
        mut f: impl FnMut(u64, &[u8]) -> std::result::Result<(), E>,
    ) -> std::result::Result<(), E> {
        let _t = ioacct::timer(Phase::VocabRead);
        let mut ahead = self.ahead_for(ids);
        if ahead.end == 0 && ids.len() > 1 {
            // scattered ids: ask for their blocks at once, so that a cold vocabulary
            // reads them in parallel instead of faulting on one after another
            self.prefetch_sorted(ids);
        }
        let mut key = Vec::new();
        let data = self.data.as_slice();
        let mut i = 0;
        while i < ids.len() && ids[i] < self.len {
            let b = ids[i] as usize / FC_BLOCK;
            ahead.at(self, b);
            let end = ((b + 1) * FC_BLOCK) as u64;
            let j = i + ids[i..].partition_point(|&x| x < end);
            let wanted = &ids[i..j];
            let last = (wanted[wanted.len() - 1] as usize % FC_BLOCK)
                .min(FC_BLOCK.min(self.len as usize - b * FC_BLOCK) - 1);
            let mut w = 0;
            let mut pos = self.block_offset(b);
            ioacct::vocab_blocks_scanned(1);
            self.touch_block(b);
            key.clear();
            for at in 0..=last {
                let id = (b * FC_BLOCK + at) as u64;
                let e = self.entry(data, &mut pos, at == 0);
                let need = key
                    .len()
                    .min(e.shared)
                    .saturating_add(self.suffix_len(data, e, id));
                if need > key.capacity() {
                    reserve(need)?;
                    key.reserve_exact(need - key.len());
                }
                key.truncate(e.shared);
                self.push_suffix(data, e, id, &mut key, true);
                if w < wanted.len() && wanted[w] as usize % FC_BLOCK == at {
                    f(wanted[w], &key)?;
                    w += 1;
                }
            }
            i = j;
        }
        Ok(())
    }

    pub(crate) fn get_with_checked<T, E>(
        &self,
        id: u64,
        reserve: impl FnMut(usize) -> std::result::Result<(), E>,
        mut f: impl FnMut(&[u8]) -> T,
    ) -> std::result::Result<Option<T>, E> {
        let mut out = None;
        self.get_sorted_checked(&[id], reserve, |_, key| {
            out = Some(f(key));
            Ok(())
        })?;
        Ok(out)
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
        let _t = ioacct::timer(Phase::VocabRead);
        let mut ahead = if read_ahead {
            self.ahead_for(ids)
        } else {
            Ahead::default()
        };
        if read_ahead && ahead.end == 0 && ids.len() > 1 {
            // scattered ids, as in `get_sorted_checked`
            self.prefetch_sorted(ids);
        }
        // Empty and entirely out-of-range passes need no reconstruction storage.
        let mut key = if ids.first().is_some_and(|&id| id < self.len) {
            Vec::with_capacity(64)
        } else {
            Vec::new()
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
            self.scan_block_with_scratch(b, &mut key, |pos, k| {
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
        fn advise(file: IoFile, m: &Mmap, ranges: impl Iterator<Item = (usize, usize)>) {
            let mut cur: Option<(usize, usize)> = None;
            for (s, e) in ranges {
                let e = e.min(m.len());
                if s >= e {
                    continue;
                }
                cur = match cur {
                    Some((cs, ce)) if s <= ce + GAP => Some((cs, ce.max(e))),
                    Some((cs, ce)) => {
                        crate::index::will_need(file, m, cs, ce - cs);
                        Some((s, e))
                    }
                    None => Some((s, e)),
                };
            }
            if let Some((cs, ce)) = cur {
                crate::index::will_need(file, m, cs, ce - cs);
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
        advise(
            IoFile::VocabOff,
            offsets,
            blocks.iter().map(|&b| (b * 8, b * 8 + 16)),
        );
        advise(
            IoFile::VocabData,
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

    /// Whether [`Vocab::prefetch_sorted`] would ask for nothing for these ids (in any
    /// order, with repeats): the hints are off, the vocabulary is not mapped, or every
    /// block that holds one of them was asked for before. A warm serializer checks this
    /// first and then skips collecting and sorting its ids. It asks for nothing itself.
    pub(crate) fn prefetch_would_skip(&self, ids: impl Iterator<Item = u64>) -> bool {
        if !crate::index::io_hints()
            || !matches!((&self.data, &self.offsets), (Bytes::Map(_), Bytes::Map(_)))
        {
            return true;
        }
        let mut last = usize::MAX;
        for id in ids {
            if id >= self.len {
                continue;
            }
            let b = id as usize / FC_BLOCK;
            if b == last {
                continue;
            }
            last = b;
            let asked = self
                .prefetched
                .get(b / 64)
                .map_or(0, |w| w.load(AtomicOrdering::Relaxed));
            if asked & (1u64 << (b % 64)) == 0 {
                return false;
            }
        }
        true
    }

    /// Read ahead what evaluating these ids' values reads (ids sorted ascending), for a
    /// sort of a few thousand rows: the front-coded blocks of every id, as
    /// [`Vocab::prefetch_sorted`] does, and with a numeric column the column's pages of
    /// the numbers, read now in two rounds (see [`numeric::NumColumn::get_many`]). The
    /// sort compares the numbers from the column, and the keys arrive meanwhile for the
    /// rows that the results print.
    pub fn prefetch_values(&self, ids: &[u64]) {
        self.prefetch_sorted(ids);
        if let Some(num) = &self.num
            && crate::index::io_hints()
        {
            let maybe: Vec<u64> = ids.iter().copied().filter(|&id| num.may_hold(id)).collect();
            num.get_many(&maybe);
        }
    }

    /// Ask for the pages that evaluating these ids' values reads (ids sorted ascending), for
    /// a FILTER over a few rows: the front-coded blocks of every id and the numeric
    /// column's pages (see [`numeric::NumColumn::advise_ids`]). It reads no value, so a
    /// warm call costs the block lookups alone.
    pub fn prefetch_value_pages(&self, ids: &[u64]) {
        self.prefetch_sorted(ids);
        if let Some(num) = &self.num {
            num.advise_ids(ids);
        }
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
            self.offsets
                .will_need(IoFile::VocabOff, lo * 8, (hi + 1) * 8);
            let end = s
                .offsets
                .get(g + 1)
                .map_or(self.data.as_slice().len(), |&o| o as usize);
            self.data
                .will_need(IoFile::VocabData, s.offsets[g] as usize, end);
        }
    }

    /// Whether the vocabulary has a sparse index, which [`Vocab::prefetch_keys`] needs.
    pub fn has_sparse(&self) -> bool {
        self.sparse.is_some()
    }

    /// Ask for the sparse-index groups that hold these keys, all at once, before the keys
    /// are looked up one after the other with [`Vocab::find`] (which would wait for each
    /// group before looking for the next key). Groups read before are left alone. Does
    /// nothing without a sparse index or with the read hints off.
    pub fn prefetch_keys<K: AsRef<[u8]>>(&self, keys: &[K]) {
        if let Some(mut p) = self.key_prefetch() {
            keys.iter().for_each(|k| p.add(k.as_ref()));
            p.finish();
        }
    }

    /// [`Vocab::prefetch_keys`] for keys given one at a time, or `None` when it would do
    /// nothing (no sparse index, the read hints off or an empty vocabulary).
    pub fn key_prefetch(&self) -> Option<KeyPrefetch<'_>> {
        let sparse = self.sparse.as_ref()?;
        if !crate::index::io_hints() || self.num_blocks() == 0 {
            return None;
        }
        Some(KeyPrefetch {
            vocab: self,
            sparse,
            cold: Vec::new(),
        })
    }

    /// Binary search: `Ok(id)` if present, `Err(insertion point)` otherwise.
    pub fn find(&self, key: &[u8]) -> std::result::Result<u64, u64> {
        // one check for a profile, rather than one in each step of the search
        if ioacct::profiling() {
            ioacct::vocab_find();
            let _t = ioacct::timer(Phase::VocabFind);
            self.find_at::<true>(key)
        } else {
            self.find_at::<false>(key)
        }
    }

    #[inline(always)]
    fn find_at<const P: bool>(&self, key: &[u8]) -> std::result::Result<u64, u64> {
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
                    crate::index::will_need(IoFile::VocabOff, m, lo * 8, (hi - lo) * 8);
                }
                let end = s
                    .offsets
                    .get(g + 1)
                    .map_or(self.data.as_slice().len(), |&o| o as usize);
                self.data
                    .will_need(IoFile::VocabData, s.offsets[g] as usize, end);
            }
            // block `lo` starts with the group's key, which is not above `key`
            lo += 1;
        }
        while lo < hi {
            let mid = (lo + hi) / 2;
            if self.block_first_at::<P>(mid) <= key {
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
        self.scan_block_at::<P>(b, |i, k| match k.cmp(key) {
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

    /// Fallible checked reconstruction for call-owned projection scratch. Public
    /// dictionary readers retain their existing allocation and access paths.
    fn scan_block_checked(
        &self,
        b: usize,
        reserve: &mut impl FnMut(usize) -> crate::error::Result<()>,
        mut f: impl FnMut(usize, &[u8]) -> crate::error::Result<bool>,
    ) -> crate::error::Result<()> {
        let data = self.data.as_slice();
        let mut pos = self.block_offset(b);
        let n = FC_BLOCK.min(self.len as usize - b * FC_BLOCK);
        let mut key = Vec::new();
        for i in 0..n {
            let id = (b * FC_BLOCK + i) as u64;
            let e = self.entry(data, &mut pos, i == 0);
            let need = key
                .len()
                .min(e.shared)
                .checked_add(self.suffix_len(data, e, id))
                .ok_or_else(|| crate::error::Error::invalid("vocabulary scratch overflows"))?;
            if need > key.capacity() {
                reserve(need)?;
                key.try_reserve_exact(need - key.len()).map_err(|e| {
                    crate::error::Error::invalid(format!("vocabulary allocation failed: {e}"))
                })?;
                if key.capacity() != need {
                    return Err(crate::error::Error::invalid(
                        "unsupported vocabulary scratch capacity",
                    ));
                }
                #[cfg(any(test, feature = "failpoints"))]
                crate::algo::observe_projection(crate::algo::ProjectionEvent::Materialized(
                    "vocab-scratch-allocated",
                    key.capacity(),
                ));
            }
            key.truncate(e.shared);
            self.push_suffix(data, e, id, &mut key, true);
            if !f(i, &key)? {
                break;
            }
        }
        Ok(())
    }

    pub(crate) fn get_with_fallible<T>(
        &self,
        id: u64,
        mut reserve: impl FnMut(usize) -> crate::error::Result<()>,
        mut f: impl FnMut(&[u8]) -> T,
    ) -> crate::error::Result<Option<T>> {
        if id >= self.len {
            return Ok(None);
        }
        let b = id as usize / FC_BLOCK;
        self.ahead_for(&[id]).at(self, b);
        let wanted = id as usize % FC_BLOCK;
        let mut result = None;
        self.scan_block_checked(b, &mut reserve, |i, key| {
            if i == wanted {
                result = Some(f(key));
                Ok(false)
            } else {
                Ok(true)
            }
        })?;
        Ok(result)
    }

    pub(crate) fn find_with_checked(
        &self,
        key: &[u8],
        mut reserve: impl FnMut(usize) -> crate::error::Result<()>,
    ) -> crate::error::Result<std::result::Result<u64, u64>> {
        let nb = self.num_blocks();
        if nb == 0 {
            return Ok(Err(0));
        }
        // last block whose first key <= key
        let (mut lo, mut hi) = (0usize, nb);
        if let Some(s) = &self.sparse {
            // the group of blocks that holds the key, from memory; then only its offsets
            // and data are read, each asked for in one request
            let g = partition_point_by(s.len(), |g| s.key(g) <= key);
            if g == 0 {
                return Ok(Err(0));
            }
            let g = g - 1;
            lo = g * IDX_BLOCKS;
            hi = ((g + 1) * IDX_BLOCKS).min(nb);
            if crate::index::io_hints() && s.first_hint(g) {
                if let Bytes::Map(m) = &self.offsets {
                    crate::index::will_need(IoFile::VocabOff, m, lo * 8, (hi - lo) * 8);
                }
                let end = s
                    .offsets
                    .get(g + 1)
                    .map_or(self.data.as_slice().len(), |&o| o as usize);
                self.data
                    .will_need(IoFile::VocabData, s.offsets[g] as usize, end);
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
            return Ok(Err(0));
        }
        let b = lo - 1;
        let mut result = Err(((b + 1) * FC_BLOCK).min(self.len as usize) as u64);
        self.scan_block_checked(b, &mut reserve, |i, k| {
            Ok(match k.cmp(key) {
                std::cmp::Ordering::Less => true,
                std::cmp::Ordering::Equal => {
                    result = Ok((b * FC_BLOCK + i) as u64);
                    false
                }
                std::cmp::Ordering::Greater => {
                    result = Err((b * FC_BLOCK + i) as u64);
                    false
                }
            })
        })?;
        Ok(result)
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
    data: BufWriter<crate::disk::WritebackFile>,
    offsets: BufWriter<crate::disk::WritebackFile>,
    pos: u64,
    count: u64,
    prev: Vec<u8>,
    buf: Vec<u8>,
    /// the entries of the sparse index: the data offset and first key of every
    /// [`IDX_BLOCKS`]-th block
    sparse: Vec<(u64, Vec<u8>)>,
    /// the numeric column, built along
    num: numeric::NumWriter,
    dir: std::path::PathBuf,
    /// the FSST coding of the suffixes, for an FSST-coded vocabulary
    fsst: Option<Box<FsstWriter>>,
}

/// Keys an FSST-coded vocabulary collects before it codes them, in parallel by block.
const FSST_BATCH: usize = FC_BLOCK * 4096;

/// The keys of an FSST-coded vocabulary not written yet (see [`VocabWriter::flush_fsst`]).
struct FsstWriter {
    enc: fsst::Encoder,
    /// the first id of the second kind of key, once pushed
    split: Option<u64>,
    /// the suffixes of the keys, one after another
    bytes: Vec<u8>,
    /// per key, its shared-prefix length and the end of its suffix in `bytes`
    ents: Vec<(u32, u32)>,
}

impl VocabWriter {
    /// A writer of a front-coded vocabulary whose suffixes are FSST-coded with the
    /// tables of `enc` (see [`fsst`]).
    pub fn create_fsst(dir: &Path, enc: fsst::Encoder) -> Result<VocabWriter> {
        let mut w = Self::create(dir)?;
        w.fsst = Some(Box::new(FsstWriter {
            enc,
            split: None,
            bytes: Vec::new(),
            ents: Vec::with_capacity(FSST_BATCH),
        }));
        Ok(w)
    }

    pub fn create(dir: &Path) -> Result<VocabWriter> {
        Ok(VocabWriter {
            data: BufWriter::with_capacity(
                1 << 20,
                WritebackFile::new(File::create(dir.join("vocab.dat"))?),
            ),
            offsets: BufWriter::new(WritebackFile::new(File::create(dir.join("vocab.off"))?)),
            pos: 0,
            count: 0,
            prev: Vec::new(),
            buf: Vec::with_capacity(256),
            sparse: Vec::new(),
            num: numeric::NumWriter::create(dir)?,
            dir: dir.to_path_buf(),
            fsst: None,
        })
    }

    /// Append the next key; keys must be pushed in strictly increasing order.
    /// Returns the assigned id.
    pub fn push(&mut self, key: &[u8]) -> Result<u64> {
        debug_assert!(self.count == 0 || key > self.prev.as_slice());
        if self.fsst.is_some() {
            return self.push_fsst(key);
        }
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
        self.num.push(key)?;
        let id = self.count;
        self.count += 1;
        Ok(id)
    }

    /// [`VocabWriter::push`] of an FSST-coded vocabulary: the key waits until its batch
    /// is coded.
    fn push_fsst(&mut self, key: &[u8]) -> Result<u64> {
        let id = self.count;
        let shared = if (id as usize).is_multiple_of(FC_BLOCK) {
            0
        } else {
            self.prev
                .iter()
                .zip(key)
                .take_while(|(a, b)| a == b)
                .count()
        };
        let f = self.fsst.as_mut().unwrap();
        if f.split.is_none() && !fsst::is_first_kind(key) {
            f.split = Some(id);
        }
        let shared32 = u32::try_from(shared)
            .map_err(|_| crate::error::Error::invalid("a vocabulary key is over 4 GiB"))?;
        f.bytes.extend_from_slice(&key[shared..]);
        let end = u32::try_from(f.bytes.len())
            .map_err(|_| crate::error::Error::invalid("a vocabulary batch is over 4 GiB"))?;
        f.ents.push((shared32, end));
        let full = f.ents.len() == FSST_BATCH;
        self.prev.clear();
        self.prev.extend_from_slice(key);
        self.num.push(key)?;
        self.count += 1;
        if full {
            self.flush_fsst()?;
        }
        Ok(id)
    }

    /// Code and write the keys an FSST-coded vocabulary collected. They start a block,
    /// so the blocks are coded in parallel. A block's first key is written plain, and
    /// another key's suffix is written as its codes when they are shorter (see
    /// [`fsst`]).
    fn flush_fsst(&mut self) -> Result<()> {
        use rayon::prelude::*;
        let f = self.fsst.as_mut().unwrap();
        if f.ents.is_empty() {
            return Ok(());
        }
        let first = self.count - f.ents.len() as u64;
        debug_assert!((first as usize).is_multiple_of(FC_BLOCK));
        let split = f.split.unwrap_or(u64::MAX);
        let (enc, bytes, ents) = (&f.enc, &f.bytes, &f.ents);
        let blocks: Vec<Vec<u8>> = ents
            .par_chunks(FC_BLOCK)
            .enumerate()
            .map(|(bi, chunk)| {
                let base = bi * FC_BLOCK;
                let mut out = Vec::with_capacity(1024);
                let mut codes = Vec::new();
                for (j, &(shared, end)) in chunk.iter().enumerate() {
                    let i = base + j;
                    let start = if i == 0 { 0 } else { ents[i - 1].1 as usize };
                    let suffix = &bytes[start..end as usize];
                    write_varint(&mut out, shared as u64);
                    let id = first + i as u64;
                    if j == 0 {
                        write_varint(&mut out, suffix.len() as u64);
                        out.extend_from_slice(suffix);
                    } else if enc.encode(usize::from(id >= split), suffix, &mut codes) {
                        write_varint(&mut out, ((codes.len() as u64) << 1) | 1);
                        out.extend_from_slice(&codes);
                    } else {
                        write_varint(&mut out, (suffix.len() as u64) << 1);
                        out.extend_from_slice(suffix);
                    }
                }
                out
            })
            .collect();
        let first_block = first as usize / FC_BLOCK;
        for (bi, block) in blocks.iter().enumerate() {
            let b = first_block + bi;
            self.offsets.write_all(&self.pos.to_le_bytes())?;
            if b.is_multiple_of(IDX_BLOCKS) {
                // the first key is plain: shared 0, its length, the key
                let mut p = 0;
                let _ = read_varint(block, &mut p);
                let len = read_varint(block, &mut p) as usize;
                self.sparse.push((self.pos, block[p..p + len].to_vec()));
            }
            self.data.write_all(block)?;
            self.pos += block.len() as u64;
        }
        f.bytes.clear();
        f.ents.clear();
        Ok(())
    }

    /// Keys pushed so far.
    pub fn len(&self) -> u64 {
        self.count
    }

    pub fn is_empty(&self) -> bool {
        self.count == 0
    }

    pub fn finish(mut self) -> Result<u64> {
        if self.fsst.is_some() {
            self.flush_fsst()?;
        }
        self.offsets.write_all(&self.count.to_le_bytes())?;
        self.data.flush()?;
        self.offsets.flush()?;
        self.data.get_ref().get_ref().sync_all()?;
        self.offsets.get_ref().get_ref().sync_all()?;
        write_sparse_index(&self.dir, &self.sparse)?;
        self.num.finish(&self.dir, self.count)?;
        if let Some(f) = &self.fsst {
            f.enc
                .tables(f.split.unwrap_or(self.count))
                .write(&self.dir)?;
        }
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

/// For `sparkles check`: compare the numeric column in `dir` with its vocabulary.
/// `None` without a column, the number of values when it matches, and otherwise whether
/// the mismatch is an error and what it is. A column that does not read as one for this
/// vocabulary is ignored by the server (a warning). With `full`, the column is rebuilt
/// from every key and compared byte for byte, since one that reads well but holds other
/// values would be used (an error).
pub(crate) fn verify_numeric_column(
    dir: &Path,
    full: bool,
) -> Option<std::result::Result<u64, (bool, String)>> {
    if !dir.join(numeric::FILE).exists() {
        return None;
    }
    let v = match Vocab::open(dir) {
        Ok(v) => v,
        Err(e) => return Some(Err((true, e.to_string()))),
    };
    let Some(col) = numeric::NumColumn::open(dir, v.len, v.first_triple) else {
        return Some(Err((
            false,
            "not a numeric column of this vocabulary; numbers are decoded from their keys".into(),
        )));
    };
    if full {
        let rebuilt = (|| -> Result<Vec<u8>> {
            let tmp = tempfile::tempdir()?;
            let mut w = numeric::NumWriter::create(tmp.path())?;
            let mut failed = None;
            v.for_each(|_, k| {
                if failed.is_none()
                    && let Err(e) = w.push(k)
                {
                    failed = Some(e);
                }
            });
            if let Some(e) = failed {
                return Err(e);
            }
            w.finish(tmp.path(), v.len)?;
            Ok(std::fs::read(tmp.path().join(numeric::FILE)).unwrap_or_default())
        })();
        match rebuilt {
            Ok(b) if std::fs::read(dir.join(numeric::FILE)).ok().as_ref() == Some(&b) => {}
            Ok(_) => {
                return Some(Err((
                    true,
                    "does not hold the values of the vocabulary's numeric literals".into(),
                )));
            }
            Err(e) => return Some(Err((true, e.to_string()))),
        }
    }
    Some(Ok(col.count()))
}

/// Write the numeric column (`vocab.num`) of the vocabulary in `dir` that lacks one,
/// built by a version from before the column. It reads every literal key once. Returns
/// the numbers in it; with none, no file is written.
pub fn add_numeric_column(dir: &Path) -> Result<u64> {
    let v = Vocab::open(dir)?;
    let mut w = numeric::NumWriter::create(dir)?;
    let mut failed = None;
    v.for_each(|_, k| {
        if failed.is_none()
            && let Err(e) = w.push(k)
        {
            failed = Some(e);
        }
    });
    if let Some(e) = failed {
        return Err(e);
    }
    let n = w.finish(dir, v.len())?;
    crate::store::sync_dir(dir)?;
    Ok(n)
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

#[cfg(test)]
thread_local! {
    /// the keys [`KeyPrefetch::add`] searched the sparse index for on this thread
    static KEY_SEARCHES: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// The sparse-index groups of keys given one at a time, asked for at once by
/// [`KeyPrefetch::finish`] (see [`Vocab::prefetch_keys`]). A key seen before after its group
/// was asked for costs a hash and a load, so a warm query with constants does not
/// binary-search the sparse index twice for each of them, once here and once in
/// [`Vocab::find`].
pub struct KeyPrefetch<'v> {
    vocab: &'v Vocab,
    sparse: &'v Sparse,
    /// the group and hash of each key not known to be warm
    cold: Vec<(usize, u64)>,
}

impl KeyPrefetch<'_> {
    pub fn add(&mut self, key: &[u8]) {
        let h = warm_key_hash(key);
        if self.sparse.key_warm(h) {
            return;
        }
        let s = self.sparse;
        #[cfg(test)]
        KEY_SEARCHES.with(|n| n.set(n.get() + 1));
        // a key before the first group's has no group to ask for
        if let Some(g) = partition_point_by(s.len(), |g| s.key(g) <= key).checked_sub(1) {
            self.cold.push((g, h));
        }
    }

    /// Ask for the groups of the keys added that no lookup has asked for before.
    pub fn finish(mut self) {
        let (v, s) = (self.vocab, self.sparse);
        self.cold.sort_unstable();
        let nb = v.num_blocks();
        let mut last = None;
        for &(g, h) in &self.cold {
            if last != Some(g) {
                last = Some(g);
                if s.first_hint(g) {
                    let (lo, hi) = (g * IDX_BLOCKS, ((g + 1) * IDX_BLOCKS).min(nb));
                    v.offsets.will_need(IoFile::VocabOff, lo * 8, (hi + 1) * 8);
                    let end = s
                        .offsets
                        .get(g + 1)
                        .map_or(v.data.as_slice().len(), |&o| o as usize);
                    v.data
                        .will_need(IoFile::VocabData, s.offsets[g] as usize, end);
                }
            }
            s.set_key_warm(h);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Profiles are process-wide. Run each exact counter case without other tests.
    fn isolated_vocab_profile(test: &str) -> bool {
        if std::env::var_os("SPARKLES_VOCAB_PROFILE_CHILD").is_some() {
            return false;
        }
        let modes: &[&str] = if test.contains("sorted_reads") {
            &["off", "no-ahead", "pre-hint"]
        } else {
            &["off"]
        };
        for mode in modes {
            let output = std::process::Command::new(std::env::current_exe().unwrap())
                .args(["--exact", &format!("vocab::tests::{test}"), "--nocapture"])
                .env("SPARKLES_VOCAB_PROFILE_CHILD", "1")
                .env("SPARKLES_VOCAB_PROFILE_MODE", mode)
                .env(
                    "SPARKLES_IO_HINTS",
                    if *mode == "off" { "off" } else { "on" },
                )
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "{mode}: {}\n{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
        }
        true
    }

    fn profile_keys(dir: &Path, n: usize) -> Vec<Vec<u8>> {
        let keys: Vec<_> = (0..n)
            .map(|i| format!("<urn:profile:{i:08}>").into_bytes())
            .collect();
        let mut writer = VocabWriter::create(dir).unwrap();
        for key in &keys {
            writer.push(key).unwrap();
        }
        writer.finish().unwrap();
        keys
    }

    fn assert_vocab_profile(report: &ioacct::IoReport, blocks: u64) {
        eprintln!("vocab profile: {report:?}");
        assert_eq!(report.vocab.blocks_scanned, blocks, "scanned blocks");
        let pages = report.pages.as_ref().expect("actual mapped reads recorded");
        for file in ["vocab.off", "vocab.dat"] {
            assert!(pages[file].touched > 0, "{file}: {report:?}");
        }
    }

    fn bulk_profile(checked: bool) {
        let dir = tempfile::tempdir().unwrap();
        let keys = profile_keys(dir.path(), 2 * FC_BLOCK + 3);
        let vocab = Vocab::open(dir.path()).unwrap();
        let mode = std::env::var("SPARKLES_VOCAB_PROFILE_MODE").unwrap_or_else(|_| "off".into());
        let id = keys.len() as u64 - 1;
        if mode == "pre-hint" {
            assert!(crate::index::io_hints());
            let before = ioacct::IoSnapshot::now();
            vocab.prefetch_sorted(&[id]);
            assert!(ioacct::IoSnapshot::now().since(&before).readahead_bytes() > 0);
        }
        let profile = ioacct::Profile::start();
        let mut got = Vec::new();
        if checked {
            // A one-id checked pass has no automatic dense or scattered look-ahead.
            assert_eq!(vocab.ahead_for(&[id]).end, 0);
            vocab
                .get_sorted_checked(
                    &[id],
                    |_| Ok::<_, ()>(()),
                    |id, key| {
                        got.push((id, key.to_vec()));
                        Ok(())
                    },
                )
                .unwrap();
        } else {
            vocab.get_sorted_with_ahead(
                &[id],
                |id, key| got.push((id, key.to_vec())),
                mode != "no-ahead",
            );
        }
        assert_eq!(got, vec![(id, keys[id as usize].clone())]);
        let report = profile.report();
        assert!(
            report.readahead.is_empty(),
            "no advice during the single-id profile"
        );
        assert_vocab_profile(&report, 1);
    }

    #[test]
    fn sorted_reads_record_actual_pages() {
        if isolated_vocab_profile("sorted_reads_record_actual_pages") {
            return;
        }
        bulk_profile(false);
    }

    #[test]
    fn checked_sorted_reads_record_actual_pages() {
        if isolated_vocab_profile("checked_sorted_reads_record_actual_pages") {
            return;
        }
        bulk_profile(true);
    }

    #[test]
    fn profiled_bulk_boundaries_and_errors() {
        if isolated_vocab_profile("profiled_bulk_boundaries_and_errors") {
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let keys = profile_keys(dir.path(), 2 * FC_BLOCK + 3);
        let vocab = Vocab::open(dir.path()).unwrap();
        let last = vocab.len() - 1;
        for checked in [false, true] {
            let profile = ioacct::Profile::start();
            let ids = [0, 15, 16, last, vocab.len(), u64::MAX];
            let mut got = Vec::new();
            if checked {
                vocab
                    .get_sorted_checked(
                        &ids,
                        |_| Ok::<_, ()>(()),
                        |id, key| {
                            got.push((id, key.to_vec()));
                            Ok(())
                        },
                    )
                    .unwrap();
            } else {
                vocab.get_sorted(&ids, |id, key| got.push((id, key.to_vec())));
            }
            let expected: Vec<_> = ids[..4]
                .iter()
                .map(|&id| (id, keys[id as usize].clone()))
                .collect();
            assert_eq!(got, expected);
            assert_vocab_profile(&profile.report(), 3);
        }
        for ids in [&[][..], &[vocab.len(), u64::MAX][..]] {
            let profile = ioacct::Profile::start();
            vocab.get_sorted(ids, |_, _| panic!("invalid id decoded"));
            vocab
                .get_sorted_checked(
                    ids,
                    |_| panic!("invalid id reserved"),
                    |_, _| -> Result<()> { panic!("invalid id decoded") },
                )
                .unwrap();
            let report = profile.report();
            assert_eq!(report.vocab.blocks_scanned, 0);
            assert!(report.pages.is_none());
            assert!(report.readahead.is_empty());
        }
        // Callback and reservation errors stop at the block actually entered. No
        // successful later scan is invented, and reconstruction still reserves first.
        for reserve_error in [false, true] {
            let profile = ioacct::Profile::start();
            let mut callbacks = 0;
            let result = vocab.get_sorted_checked(
                &[0, 16, last],
                |_| {
                    if reserve_error {
                        Err("reserve")
                    } else {
                        Ok(())
                    }
                },
                |id, key| {
                    callbacks += 1;
                    assert_eq!(key, keys[id as usize]);
                    if id == 16 { Err("callback") } else { Ok(()) }
                },
            );
            assert_eq!(
                result,
                Err(if reserve_error { "reserve" } else { "callback" })
            );
            assert_eq!(callbacks, if reserve_error { 0 } else { 2 });
            assert_vocab_profile(&profile.report(), if reserve_error { 1 } else { 2 });
        }
        // Binary-search probes read a block's first key, but do not traverse a
        // decoder. They still observe its offset and data pages before reading.
        let profile = ioacct::Profile::start();
        assert_eq!(vocab.block_first(1), keys[FC_BLOCK]);
        assert_vocab_profile(&profile.report(), 0);
        drop(profile);
        let profile = ioacct::Profile::start();
        let ids: Vec<_> = (0..16).collect();
        let _ahead = vocab.ahead_for(&ids);
        let report = profile.report();
        assert_eq!(report.vocab.blocks_scanned, 0);
        let pages = report.pages.unwrap();
        assert!(pages["vocab.off"].touched > 0);
        assert!(
            !pages.contains_key("vocab.dat"),
            "offset preparation is not a data read"
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn cold_offset_is_observed_before_lookup() {
        if isolated_vocab_profile("cold_offset_is_observed_before_lookup") {
            return;
        }
        use std::os::fd::AsRawFd;
        let dir = tempfile::tempdir().unwrap();
        let keys = profile_keys(dir.path(), 65536);
        let vocab = Vocab::open(dir.path()).unwrap();
        let Bytes::Map(offsets) = &vocab.offsets else {
            panic!("mapped offsets");
        };
        // SAFETY: sysconf has no pointer arguments or other preconditions.
        let page = usize::try_from(unsafe { libc::sysconf(libc::_SC_PAGESIZE) }).unwrap();
        let id = 48000;
        let at = (id as usize / FC_BLOCK * 8) / page * page;
        assert!(at >= page && at + page < offsets.len());
        let file = File::open(dir.path().join("vocab.off")).unwrap();
        file.sync_all().unwrap();
        // SAFETY: the whole mapping is an owned immutable test file. DONTNEED drops
        // only its PTEs; fadvise then requests eviction only of this file's pages.
        unsafe {
            assert_eq!(
                libc::madvise(
                    offsets.as_ptr().cast_mut().cast(),
                    offsets.len(),
                    libc::MADV_RANDOM
                ),
                0
            );
            assert_eq!(
                libc::madvise(
                    offsets.as_ptr().cast_mut().cast(),
                    offsets.len(),
                    libc::MADV_DONTNEED
                ),
                0
            );
            assert_eq!(
                libc::posix_fadvise(file.as_raw_fd(), 0, 0, libc::POSIX_FADV_DONTNEED),
                0
            );
        }
        let resident = || {
            let mut state = 0u8;
            // SAFETY: at is a page-aligned offset inside the live mapping and state
            // has the one output byte required for this one-page query.
            assert_eq!(
                unsafe {
                    libc::mincore(offsets.as_ptr().add(at).cast_mut().cast(), page, &mut state)
                },
                0
            );
            state & 1
        };
        let before = resident();
        eprintln!("independent target offset page={at}, resident before lookup={before}");
        if before != 0 {
            // some file systems keep the page after fadvise: the case proves nothing
            eprintln!("skipped: the private offset page was not evicted");
            return;
        }
        let profile = ioacct::Profile::start();
        assert_eq!(vocab.get(id), Some(keys[id as usize].clone()));
        let after = resident();
        let report = profile.report();
        eprintln!("independent resident after lookup={after}; {report:?}");
        assert_eq!(after, 1);
        let offset_io = &report.pages.as_ref().unwrap()["vocab.off"];
        assert_eq!(offset_io.touched, 1);
        assert_eq!(offset_io.resident_after, 1);
        assert_eq!(
            offset_io.resident_before, 0,
            "offset was observed after its first load"
        );
        assert_eq!(offset_io.brought_in, 1);
    }

    #[test]
    fn projection_find_matches_public_search_and_rejects_before_growth() {
        let dir = tempfile::tempdir().unwrap();
        let keys = write_keys(dir.path(), 3 * FC_BLOCK * IDX_BLOCKS + 77);
        let v = Vocab::open(dir.path()).unwrap();
        for key in keys
            .iter()
            .step_by(127)
            .chain([b"".to_vec(), b"zzzz".to_vec()].iter())
        {
            assert_eq!(v.find_with_checked(key, |_| Ok(())).unwrap(), v.find(key));
            let mut absent = key.clone();
            absent.push(0);
            assert_eq!(
                v.find_with_checked(&absent, |_| Ok(())).unwrap(),
                v.find(&absent)
            );
        }
        let dir = tempfile::tempdir().unwrap();
        let mut writer = VocabWriter::create(dir.path()).unwrap();
        let keys = [
            b"<a".to_vec(),
            format!("<b{}", "x".repeat(4096)).into_bytes(),
            b"<c".to_vec(),
        ];
        for key in &keys {
            writer.push(key).unwrap();
        }
        writer.finish().unwrap();
        let v = Vocab::open(dir.path()).unwrap();
        for allowed in [0, 1] {
            let observed = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
            let seen = observed.clone();
            crate::algo::set_projection_observer(Some(std::sync::Arc::new(move |event| {
                if let crate::algo::ProjectionEvent::Materialized("vocab-scratch-allocated", _) =
                    event
                {
                    seen.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                }
            })));
            let mut calls = 0;
            let result = v.find_with_checked(&keys[2], |_| {
                calls += 1;
                if calls > allowed {
                    Err(crate::Error::Cancelled)
                } else {
                    Ok(())
                }
            });
            crate::algo::set_projection_observer(None);
            assert!(matches!(result, Err(crate::Error::Cancelled)));
            assert_eq!(observed.load(std::sync::atomic::Ordering::Relaxed), allowed);
        }
        assert_eq!(
            v.get_with_fallible(2, |_| Ok(()), |key| key.to_vec())
                .unwrap(),
            Some(keys[2].clone())
        );
        assert_eq!(v.get_with_fallible(3, |_| Ok(()), |_| ()).unwrap(), None);
    }
    #[test]
    fn checked_decode_reserves_intermediate_keys_before_growth() {
        let keys = [
            format!("\"a{}", "x".repeat(32 << 10)).into_bytes(),
            b"\"b".to_vec(),
        ];
        let dir = tempfile::tempdir().unwrap();
        let mut w = VocabWriter::create(dir.path()).unwrap();
        for key in &keys {
            w.push(key).unwrap();
        }
        w.finish().unwrap();
        let v = Vocab::open(dir.path()).unwrap();
        let mut called = false;
        let blocked = v.get_with_checked(
            1,
            |need| if need > 1024 { Err(need) } else { Ok(()) },
            |_| called = true,
        );
        assert!(blocked.unwrap_err() > 32 << 10);
        assert!(!called);
        assert_eq!(
            v.get_with_checked(1, |_| Ok::<_, ()>(()), |k| k.to_vec()),
            Ok(Some(keys[1].clone()))
        );
        let mut got = Vec::new();
        v.get_sorted_checked(
            &[0, 1, 2, 15, u64::MAX],
            |_| Ok::<_, ()>(()),
            |id, k| {
                got.push((id, k.to_vec()));
                Ok(())
            },
        )
        .unwrap();
        assert_eq!(got, vec![(0, keys[0].clone()), (1, keys[1].clone())]);
        assert_eq!(
            v.get_with_checked(u64::MAX, |_| Err::<(), _>(()), |_| panic!("invalid key")),
            Ok(None)
        );
    }

    #[test]
    fn borrowed_key_decode_matches_owned_for_term_kinds() {
        use oxrdf::{BaseDirection, BlankNode, Literal, NamedNode, Term, Triple};
        let terms = [
            Term::NamedNode(NamedNode::new_unchecked("urn:iri:é")),
            Term::BlankNode(BlankNode::new_unchecked("b2a")),
            Term::Literal(Literal::new_simple_literal("quoted \" é\n")),
            Term::Literal(Literal::new_language_tagged_literal_unchecked("chat", "fr")),
            Term::Literal(Literal::new_directional_language_tagged_literal_unchecked(
                "مرحبا",
                "ar",
                BaseDirection::Rtl,
            )),
            Term::Literal(Literal::new_typed_literal(
                "custom",
                NamedNode::new_unchecked("urn:datatype"),
            )),
            Term::Triple(Box::new(Triple::new(
                NamedNode::new_unchecked("urn:s"),
                NamedNode::new_unchecked("urn:p"),
                Literal::new_simple_literal("nested"),
            ))),
        ];
        let mut keys: Vec<_> = terms.iter().map(crate::id::term_key).collect();
        keys.sort();
        let dir = tempfile::tempdir().unwrap();
        let mut w = VocabWriter::create(dir.path()).unwrap();
        for key in &keys {
            w.push(key).unwrap();
        }
        w.finish().unwrap();
        let v = Vocab::open(dir.path()).unwrap();
        for (i, key) in keys.iter().enumerate() {
            assert_eq!(
                v.get_with_checked(i as u64, |_| Ok::<_, ()>(()), crate::id::key_to_term),
                Ok(Some(crate::id::key_to_term(key)))
            );
            assert_eq!(
                v.get_with(i as u64, crate::id::key_to_term),
                Some(crate::id::key_to_term(key))
            );
            assert_eq!(v.get(i as u64).as_ref(), Some(key));
        }
        for term in terms {
            let id = v.find(&crate::id::term_key(&term)).unwrap();
            assert_eq!(v.get_with(id, crate::id::key_to_term), Some(term));
        }
    }

    #[test]
    fn borrowed_key_decode_same_block_partial_tail_and_invalid_bytes() {
        let mut keys: Vec<Vec<u8>> = (0..(FC_BLOCK * 2 + 3))
            .map(|i| format!("<urn:{i:04}:{}", "x".repeat(i % 91)).into_bytes())
            .collect();
        keys.extend([b"<urn:invalid:\xff".to_vec(), b"opaque:\xfe".to_vec()]);
        keys.sort();
        let dir = tempfile::tempdir().unwrap();
        let mut w = VocabWriter::create(dir.path()).unwrap();
        for key in &keys {
            w.push(key).unwrap();
        }
        w.finish().unwrap();
        let v = Vocab::open(dir.path()).unwrap();
        // Repeated nonmonotonic lookups reconstruct the full key independently;
        // variable common prefixes, spill-sized keys and invalid UTF-8 stay opaque.
        for i in (0..keys.len()).rev().chain(0..keys.len()) {
            assert_eq!(v.get_with(i as u64, |k| k.to_vec()), Some(keys[i].clone()));
            assert_eq!(
                v.get_with(i as u64, crate::id::key_to_term),
                v.get(i as u64).as_deref().map(crate::id::key_to_term)
            );
        }
        for id in [v.len(), v.len() + 1, u64::MAX] {
            assert_eq!(
                v.get_with(id, |_| panic!("out-of-range callback")),
                None::<Vec<u8>>
            );
            assert_eq!(v.get(id), None);
        }
    }

    #[test]
    fn sorted_scratch_preserves_dense_sparse_partial_and_invalid_suffix() {
        let keys: Vec<Vec<u8>> = (0..FC_BLOCK * 3 + 7)
            .map(|i| {
                let mut key = format!("<urn:{i:05}:é:").into_bytes();
                key.extend_from_slice(&vec![b'x'; if i < FC_BLOCK { 300 } else { i % 29 }]);
                if i % 17 == 0 {
                    key.push(0xff);
                }
                key
            })
            .collect();
        let dir = tempfile::tempdir().unwrap();
        let mut w = VocabWriter::create(dir.path()).unwrap();
        for k in &keys {
            w.push(k).unwrap();
        }
        w.finish().unwrap();
        let v = Vocab::open(dir.path()).unwrap();
        let cases = [
            (0..v.len()).collect::<Vec<_>>(),
            vec![
                0,
                1,
                (FC_BLOCK - 1) as u64,
                FC_BLOCK as u64,
                v.len() - 1,
                v.len(),
                u64::MAX,
            ],
            (0..v.len()).step_by(19).collect(),
            vec![],
            vec![v.len(), u64::MAX],
        ];
        for ahead in [false, true] {
            for ids in &cases {
                let mut got = Vec::new();
                v.get_sorted_with_ahead(ids, |id, key| got.push((id, key.to_vec())), ahead);
                let expected: Vec<_> = ids
                    .iter()
                    .copied()
                    .take_while(|&id| id < v.len())
                    .map(|id| (id, keys[id as usize].clone()))
                    .collect();
                assert_eq!(got, expected);
                for (id, key) in got {
                    assert_eq!(v.get(id), Some(key));
                }
            }
        }
        let empty = Vocab::empty();
        empty.get_sorted(&[0, u64::MAX], |_, _| panic!("empty vocabulary callback"));
    }

    #[test]
    fn sorted_scratch_reuses_capacity_resets_blocks_and_stops_callbacks() {
        let dir = tempfile::tempdir().unwrap();
        let mut w = VocabWriter::create(dir.path()).unwrap();
        let keys: Vec<Vec<u8>> = (0..FC_BLOCK * 2 + 5)
            .map(|i| {
                format!(
                    "<urn:{i:05}:{}",
                    "x".repeat(if i < FC_BLOCK { 300 } else { 1 })
                )
                .into_bytes()
            })
            .collect();
        for key in &keys {
            w.push(key).unwrap();
        }
        w.finish().unwrap();
        let mut v = Vocab::open(dir.path()).unwrap();
        let mut scratch = Vec::with_capacity(512);
        let pointer = scratch.as_ptr();
        let capacity = scratch.capacity();
        for b in 0..v.num_blocks() {
            let mut seen = 0;
            v.scan_block_with_scratch(b, &mut scratch, |i, key| {
                assert_eq!(i, 0);
                assert_eq!(key, keys[b * FC_BLOCK]);
                seen += 1;
                false
            });
            assert_eq!(seen, 1);
            assert_eq!(scratch.as_ptr(), pointer);
            assert_eq!(scratch.capacity(), capacity);
        }
        let panic = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            v.scan_block_with_scratch(0, &mut scratch, |_, _| panic!("callback"));
        }));
        assert!(panic.is_err());
        v.scan_block_with_scratch(1, &mut scratch, |i, key| {
            assert_eq!(i, 0);
            assert_eq!(key, keys[FC_BLOCK]);
            false
        });
        // Even a nonzero first shared length retains the old fresh-buffer behavior.
        // This checks reset semantics, without claiming malformed vocab validation.
        let mut data = v.data.as_slice().to_vec();
        let at = v.block_offset(1);
        assert_eq!(data[at], 0);
        data[at] = 3;
        v.data = Bytes::Vec(data);
        scratch.extend_from_slice(b"previous block content");
        let mut single = Vec::new();
        v.scan_block(1, |_, key| {
            single.extend_from_slice(key);
            false
        });
        v.scan_block_with_scratch(1, &mut scratch, |_, key| {
            assert_eq!(key, single);
            false
        });
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

    /// A warm answer asks [`Vocab::prefetch_sorted`] for nothing, and the serializer
    /// learns that without sorting its ids: [`Vocab::prefetch_would_skip`] is true once
    /// every block of the ids was asked for, and asking it changes nothing.
    #[test]
    fn prefetch_would_skip_only_blocks_asked_for_before() {
        let dir = tempfile::tempdir().unwrap();
        write_keys(dir.path(), 4 * FC_BLOCK + 3);
        let v = Vocab::open(dir.path()).unwrap();
        let (a, b, far) = (1, FC_BLOCK as u64 + 2, 3 * FC_BLOCK as u64);
        if !crate::index::io_hints() {
            assert!(v.prefetch_would_skip([a, b, far].into_iter()));
            return;
        }
        assert!(!v.prefetch_would_skip([b, a].into_iter()));
        // asking leaves the blocks to be prefetched
        assert!(!v.prefetch_would_skip([a].into_iter()));
        v.prefetch_sorted(&[a, b]);
        assert!(v.prefetch_would_skip([b, a, a + 1].into_iter()));
        assert!(!v.prefetch_would_skip([a, far].into_iter()));
        // ids past the vocabulary have no block to ask for
        assert!(v.prefetch_would_skip([a, v.len(), u64::MAX].into_iter()));
        assert!(v.prefetch_would_skip(std::iter::empty()));
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

    /// Mixed keys of every kind, in order: literals with long shared and unshared parts,
    /// triple terms, IRIs of a few shapes, and adversarial bytes.
    fn mixed_keys(n: usize) -> Vec<Vec<u8>> {
        let mut keys: Vec<Vec<u8>> = (0..n)
            .flat_map(|i| {
                [
                    format!("<http://example.org/resource/{i:06}").into_bytes(),
                    format!("<http://dbpedia.org/resource/Thing_{}", i * 7919 % 100_003)
                        .into_bytes(),
                    format!("\"A description of item {i}, which is a thing\"@en").into_bytes(),
                    format!("\"{}\"^^<http://www.w3.org/2001/XMLSchema#integer", i * 31)
                        .into_bytes(),
                    format!("(<s{i}> <p> \"o\")").into_bytes(),
                ]
            })
            .collect();
        keys.push(b"\"".to_vec());
        keys.push([b"\"".as_slice(), &[255u8; 64]].concat());
        keys.push([b"<".as_slice(), &(1..=255u8).collect::<Vec<_>>()].concat());
        keys.push(format!("<x{}", "y".repeat(5000)).into_bytes());
        keys.sort();
        keys.dedup();
        keys
    }

    fn write_codec(dir: &Path, keys: &[Vec<u8>], fsst: bool) {
        let mut w = if fsst {
            let sample: Vec<&[u8]> = keys.iter().step_by(3).map(|k| &k[..]).collect();
            let (a, b): (Vec<&[u8]>, Vec<&[u8]>) =
                sample.iter().partition(|k| fsst::is_first_kind(k));
            VocabWriter::create_fsst(dir, fsst::Encoder::train([&a, &b])).unwrap()
        } else {
            VocabWriter::create(dir).unwrap()
        };
        for k in keys {
            w.push(k).unwrap();
        }
        w.finish().unwrap();
    }

    /// An FSST-coded vocabulary answers every read as the front-coded one of the same
    /// keys does (X11 A7), and is smaller.
    #[test]
    fn fsst_reads_like_front() {
        let keys = mixed_keys(3000);
        let (fd, sd) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
        write_codec(fd.path(), &keys, false);
        write_codec(sd.path(), &keys, true);
        let (f, s) = (
            Vocab::open(fd.path()).unwrap(),
            Vocab::open(sd.path()).unwrap(),
        );
        assert!(!f.is_fsst() && s.is_fsst());
        assert_eq!(f.len(), s.len());
        assert_eq!(
            (f.first_triple(), f.first_iri()),
            (s.first_triple(), s.first_iri())
        );
        let fsize = std::fs::metadata(fd.path().join("vocab.dat"))
            .unwrap()
            .len();
        let ssize = std::fs::metadata(sd.path().join("vocab.dat"))
            .unwrap()
            .len();
        assert!(ssize < fsize * 3 / 4, "{ssize} against {fsize}");
        for (i, k) in keys.iter().enumerate() {
            let id = i as u64;
            assert_eq!(s.get(id).as_ref(), Some(k), "{i}");
            assert_eq!(s.find(k), Ok(id));
            assert_eq!(s.find_with_checked(k, |_| Ok(())).unwrap(), Ok(id));
            let mut absent = k.clone();
            absent.push(0);
            assert_eq!(s.find(&absent), f.find(&absent));
            assert_eq!(
                s.get_with_fallible(id, |_| Ok(()), |k| k.to_vec())
                    .unwrap()
                    .as_ref(),
                Some(k)
            );
        }
        let ids: Vec<u64> = (0..keys.len() as u64).step_by(5).collect();
        let (mut a, mut b, mut c) = (Vec::new(), Vec::new(), Vec::new());
        f.get_sorted(&ids, |i, k| a.push((i, k.to_vec())));
        s.get_sorted(&ids, |i, k| b.push((i, k.to_vec())));
        s.get_sorted_checked::<()>(
            &ids,
            |_| Ok(()),
            |i, k| {
                c.push((i, k.to_vec()));
                Ok(())
            },
        )
        .unwrap();
        assert_eq!(a, b);
        assert_eq!(a, c);
        let mut all = Vec::new();
        s.for_each(|i, k| all.push((i, k.to_vec())));
        assert_eq!(all.len(), keys.len());
        assert!(all.iter().zip(&keys).all(|((_, a), b)| a == b));
        for p in [
            &b"\"A description of item 1"[..],
            b"<http://example.org/resource/0012",
            b"(",
        ] {
            assert_eq!(s.prefix_range(p), f.prefix_range(p));
        }
        // the checked decode grows its buffer only by what it reserved
        let reserved = std::cell::Cell::new(0);
        for id in (0..keys.len() as u64).step_by(97) {
            s.get_sorted_checked::<()>(
                &[id],
                |n| {
                    reserved.set(n);
                    Ok(())
                },
                |_, k| {
                    assert!(k.len() <= reserved.get());
                    Ok(())
                },
            )
            .unwrap();
        }
    }

    /// A generation opens its vocabulary by the codec its meta.json names: one marked
    /// FSST-coded needs its tables.
    #[test]
    fn an_fsst_vocabulary_without_its_tables_is_refused() {
        let keys = mixed_keys(200);
        let d = tempfile::tempdir().unwrap();
        write_codec(d.path(), &keys, true);
        std::fs::remove_file(d.path().join(fsst::FILE)).unwrap();
        let e = Vocab::open_codec(d.path(), true).err().unwrap().to_string();
        assert!(e.contains("vocab.fsst") && e.contains("missing"), "{e}");
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
    fn prefetch_keys_asks_for_every_group_before_the_lookups() {
        // the hint counter is process-wide, so the case runs alone in a child process
        const CHILD: &str = "SPARKLES_PREFETCH_KEYS_CHILD";
        if std::env::var_os(CHILD).is_none() {
            let output = std::process::Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "vocab::tests::prefetch_keys_asks_for_every_group_before_the_lookups",
                    "--nocapture",
                ])
                .env(CHILD, "1")
                .output()
                .unwrap();
            assert!(
                output.status.success()
                    && String::from_utf8_lossy(&output.stdout).contains("1 passed"),
                "{}\n{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let keys = write_keys(dir.path(), 64 * FC_BLOCK * IDX_BLOCKS);
        let v = Vocab::open(dir.path()).unwrap();
        let s = v.sparse.as_ref().unwrap();
        let group = |i: usize| i / (FC_BLOCK * IDX_BLOCKS);
        let picks = [
            5 * FC_BLOCK * IDX_BLOCKS + 3,
            9 * FC_BLOCK * IDX_BLOCKS,
            70_000,
        ];
        let hinted = || crate::ioacct::IoSnapshot::now().vocab_groups_hinted;
        let start = hinted();
        v.prefetch_keys(&picks.iter().map(|&i| keys[i].clone()).collect::<Vec<_>>());
        if crate::index::io_hints() {
            // each group has been asked for once, so a second batch and the lookups ask for none
            let groups: std::collections::HashSet<usize> =
                picks.iter().map(|&i| group(i)).collect();
            assert_eq!(hinted() - start, groups.len() as u64);
            for &i in &picks {
                assert!(!s.first_hint(group(i)), "group of key {i}");
            }
            v.prefetch_keys(&[keys[picks[0]].clone()]);
            assert_eq!(hinted() - start, groups.len() as u64);
            // the keys are now known to be warm, so a later batch skips their search
            for &i in &picks {
                assert!(s.key_warm(warm_key_hash(&keys[i])), "key {i} warm");
            }
            // a key whose group a lookup asked for is not warm until a batch has seen it,
            // and that batch asks for nothing more
            let other = 20 * FC_BLOCK * IDX_BLOCKS + 1;
            assert_eq!(v.find(&keys[other]), Ok(other as u64));
            assert!(!s.key_warm(warm_key_hash(&keys[other])));
            v.prefetch_keys(&[keys[other].clone()]);
            assert!(s.key_warm(warm_key_hash(&keys[other])));
            assert_eq!(hinted() - start, groups.len() as u64 + 1);
        }
        for &i in &picks {
            assert_eq!(v.find(&keys[i]), Ok(i as u64));
        }
        // keys outside the vocabulary, before its first key or in no group, do no harm
        v.prefetch_keys(&[b"".to_vec(), b"\xff\xff".to_vec()]);
    }

    /// Whether group `g` has been asked for.
    fn group_hinted(s: &Sparse, g: usize) -> bool {
        s.hinted[g / 64].load(AtomicOrdering::Relaxed) & (1 << (g % 64)) != 0
    }

    #[test]
    fn a_warm_constant_costs_no_search() {
        if !crate::index::io_hints() {
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let keys = write_keys(dir.path(), 16 * FC_BLOCK * IDX_BLOCKS);
        let v = Vocab::open(dir.path()).unwrap();
        let searches = || KEY_SEARCHES.with(|n| n.get());
        let picks = [keys[100].clone(), keys[5 * FC_BLOCK * IDX_BLOCKS].clone()];
        let start = searches();
        v.prefetch_keys(&picks);
        assert_eq!(searches() - start, 2);
        // the second batch of the same constants searches nothing
        v.prefetch_keys(&picks);
        v.prefetch_keys(&picks);
        assert_eq!(searches() - start, 2);
    }

    #[test]
    fn a_hash_collision_only_skips_a_hint() {
        if !crate::index::io_hints() {
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let keys = write_keys(dir.path(), 16 * FC_BLOCK * IDX_BLOCKS);
        let v = Vocab::open(dir.path()).unwrap();
        let s = v.sparse.as_ref().unwrap();
        let group = |i: usize| i / (FC_BLOCK * IDX_BLOCKS);
        // a key whose hash another key took: it reads as warm, so its group is not asked
        // for by the batch, and the lookup still finds it and asks for the group itself
        let i = 9 * FC_BLOCK * IDX_BLOCKS + 7;
        s.set_key_warm(warm_key_hash(&keys[i]));
        let start = KEY_SEARCHES.with(|n| n.get());
        v.prefetch_keys(&[keys[i].clone()]);
        assert_eq!(KEY_SEARCHES.with(|n| n.get()), start);
        assert!(!group_hinted(s, group(i)));
        assert_eq!(v.find(&keys[i]), Ok(i as u64));
        assert!(group_hinted(s, group(i)));
        // two keys of one slot and different hashes take the slot in turn, and each is
        // searched again after the other took it
        let a = 3 * FC_BLOCK * IDX_BLOCKS;
        let slot = |k: &[u8]| warm_key_hash(k) as usize % WARM_KEY_SLOTS;
        let b = (a + 1..keys.len())
            .find(|&b| slot(&keys[b]) == slot(&keys[a]))
            .unwrap();
        assert_ne!(warm_key_hash(&keys[a]), warm_key_hash(&keys[b]));
        let start = KEY_SEARCHES.with(|n| n.get());
        for k in [a, b, a] {
            v.prefetch_keys(&[keys[k].clone()]);
        }
        assert_eq!(KEY_SEARCHES.with(|n| n.get()) - start, 3);
        assert_eq!(v.find(&keys[a]), Ok(a as u64));
        assert_eq!(v.find(&keys[b]), Ok(b as u64));
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
