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

mod cold;
pub mod delta;
pub mod fsst;
pub mod numeric;
mod sync;
pub(crate) use delta::DeltaFile;
pub use delta::parse_delta;
pub use delta::{DeltaVocab, VocabMark};
pub(crate) use sync::{LazySync, PendingSync};

use crate::budgeted::{BudgetControl, Control, Unlimited};
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

/// The room past a key that a checked decode reserves for an FSST suffix.
const FSST_ROOM: usize = 64;

/// Every key of a front-coded block, as a mask of [`Vocab::decode_block_with`].
const ALL_KEYS: u32 = (1 << FC_BLOCK) - 1;

/// How many bytes of key `i` of a front-coded block a decode of the keys in `wanted`
/// (a bit per key) needs, given the entry after it: all of a wanted key
/// (`usize::MAX`), and of another key the bytes that the next key takes from it, its
/// shared-prefix length. A key that gives the next no more than its own shared prefix
/// is not decoded at all, and another only up to what the next takes, so a lookup of
/// one key decodes little more than that key. This holds for plain and FSST suffixes
/// alike, so a long key in the block is not rebuilt for a short neighbour (item 948).
/// The need of a key after the last wanted one does not arise, since decoding stops
/// there.
#[inline(always)]
fn lookahead(wanted: u32, i: usize, next: Option<Entry>) -> usize {
    match next {
        Some(e) if wanted >> i & 1 == 0 => e.shared,
        _ => usize::MAX,
    }
}

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
    /// data bytes. Index format 5 requires the file, so a missing one, or one that does
    /// not describe this vocabulary, is [`crate::Error::Corrupt`].
    fn read_with<C: BudgetControl>(
        path: &Path,
        blocks: usize,
        data_len: u64,
        control: &mut C,
    ) -> crate::error::Result<crate::store::cold::Loaded<Sparse>> {
        let start = control.used();
        let bytes = match control.read_file(path) {
            Ok(bytes) => bytes,
            Err(crate::Error::Io(error)) => {
                control.release(control.used() - start);
                return Err(missing_index(path, &error));
            }
            Err(error) => return Err(error),
        };
        let parsed = match Self::decode_with(&bytes, blocks, data_len, control) {
            Ok(parsed) => parsed,
            Err(error) => {
                control.drop_vec(bytes);
                control.release(control.used() - start);
                return Err(error);
            }
        };
        control.drop_vec(bytes);
        match parsed {
            Some(value) => Ok(crate::store::cold::Loaded {
                value,
                charge: control.used() - start,
            }),
            None => {
                // The index that does not describe this vocabulary and all of its
                // partial storage dropped before the refusal.
                control.release(control.used() - start);
                Err(mismatched_index(path))
            }
        }
    }

    fn decode(buf: &[u8], blocks: usize, data_len: u64) -> Option<Sparse> {
        Self::decode_with(buf, blocks, data_len, &mut Unlimited)
            .ok()
            .flatten()
    }

    fn decode_with<C: BudgetControl>(
        buf: &[u8],
        blocks: usize,
        data_len: u64,
        control: &mut C,
    ) -> crate::error::Result<Option<Sparse>> {
        let mut failure = None;
        macro_rules! admit {
            ($expression:expr) => {
                match $expression {
                    Ok(value) => value,
                    Err(error) => {
                        failure = Some(error);
                        return None;
                    }
                }
            };
        }
        let value = (|| {
            let u32_at = |p: usize| Some(u32::from_le_bytes(buf.get(p..p + 4)?.try_into().ok()?));
            let u64_at = |p: usize| Some(u64::from_le_bytes(buf.get(p..p + 8)?.try_into().ok()?));
            if buf.get(..8)? != IDX_MAGIC || u32_at(8)? as usize != IDX_BLOCKS {
                return None;
            }
            let n = u64_at(16)? as usize;
            if n != blocks.div_ceil(IDX_BLOCKS) {
                return None;
            }
            let (mut starts, mut offsets) = (Vec::new(), Vec::new());
            admit!(control.reserve(&mut starts, n));
            admit!(control.reserve(&mut offsets, n));
            let hinted = admit!(control.filled(n.div_ceil(64)));
            let warm_keys = admit!(control.filled(WARM_KEY_SLOTS));
            let mut s = Sparse {
                keys: Vec::new(),
                starts,
                offsets,
                hinted,
                warm_keys,
            };
            let mut pos = 24;
            for _ in 0..n {
                admit!(control.check(0));
                let off = u64_at(pos)?;
                let len = u32_at(pos + 8)? as usize;
                let key = buf.get(pos + 12..pos + 12 + len)?;
                if off > data_len || s.offsets.last().is_some_and(|&o| o > off) {
                    return None;
                }
                let unordered = if s.starts.is_empty() {
                    false
                } else {
                    admit!(cold::compare_with(s.key(s.len() - 1), key, control))
                        != std::cmp::Ordering::Less
                };
                if unordered {
                    return None;
                }
                s.starts.push(u32::try_from(s.keys.len()).ok()?);
                if C::ACCOUNTS {
                    let need = s.keys.len().checked_add(key.len())?;
                    admit!(control.reserve(&mut s.keys, need));
                    for (index, chunk) in key.chunks(64 << 10).enumerate() {
                        admit!(control.key_step("sparse copy", index * (64 << 10)));
                        s.keys.extend_from_slice(chunk);
                    }
                    admit!(control.key_step("sparse copy", key.len()));
                } else {
                    s.keys.extend_from_slice(key);
                }
                s.offsets.push(off);
                pos += 12 + len;
            }
            (pos == buf.len()).then_some(s)
        })();
        match failure {
            Some(error) => Err(error),
            None => Ok(value),
        }
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

/// Compile-time policy governing scratch admission, exact growth, and cooperative
/// checkpoint polling for front-coded block decoding and binary search.
///
/// Under [`Unlimited`], `CHECKED` and `POLLS` are both false so every check, admission,
/// and error path compiles away.
trait BlockScratch<E> {
    const CHECKED: bool;
    const POLLS: bool;
    const EXACT_GROW: bool;
    const ROOM: bool;
    const SCRATCH_FLOOR: bool;
    const ADMIT_PREFETCH: bool;
    #[cfg(any(test, feature = "failpoints"))]
    const PROJECTION_TAG: Option<&'static str>;

    fn checkpoint(&mut self, _phase: &'static str) -> std::result::Result<(), E> {
        Ok(())
    }
    fn admit_growth(&mut self, _old_cap: usize, _new_cap: usize) -> std::result::Result<(), E> {
        Ok(())
    }
    fn settled(&mut self, _new_cap: usize) -> std::result::Result<(), E> {
        Ok(())
    }
    fn invalid_err(&self, _msg: &'static str) -> E {
        unreachable!("unchecked vocabulary scratch cannot fail")
    }
    fn alloc_err(&self, _err: std::collections::TryReserveError) -> E {
        self.invalid_err("vocabulary allocation failed")
    }
}

impl BlockScratch<std::convert::Infallible> for Unlimited {
    const CHECKED: bool = false;
    const POLLS: bool = false;
    const EXACT_GROW: bool = false;
    const ROOM: bool = false;
    const SCRATCH_FLOOR: bool = false;
    const ADMIT_PREFETCH: bool = false;
    #[cfg(any(test, feature = "failpoints"))]
    const PROJECTION_TAG: Option<&'static str> = None;
}

/// Checked scratch reservation with `scratch_capacity()` floor and optional `FSST_ROOM`
/// fast-path headroom (for cursor/projection lookups and sorted scans).
struct CheckedScratch<R, const ROOM: bool> {
    reserve: R,
}

impl<R, E, const ROOM: bool> BlockScratch<E> for CheckedScratch<R, ROOM>
where
    R: FnMut(usize) -> std::result::Result<(), E>,
{
    const CHECKED: bool = true;
    const POLLS: bool = false;
    const EXACT_GROW: bool = false;
    const ROOM: bool = ROOM;
    const SCRATCH_FLOOR: bool = true;
    const ADMIT_PREFETCH: bool = false;
    #[cfg(any(test, feature = "failpoints"))]
    const PROJECTION_TAG: Option<&'static str> = Some("checked-vocab-scratch-allocated");

    #[inline(always)]
    fn admit_growth(&mut self, _old_cap: usize, new_cap: usize) -> std::result::Result<(), E> {
        (self.reserve)(new_cap)
    }
}

/// Checked scratch reservation for `get_sorted_owned_with_ahead` that also admits
/// prefetch block-index scratch through the same callback.
struct OwnedCheckedScratch<R> {
    reserve: R,
}

impl<R> BlockScratch<crate::error::Error> for OwnedCheckedScratch<R>
where
    R: FnMut(usize) -> crate::error::Result<()>,
{
    const CHECKED: bool = true;
    const POLLS: bool = false;
    const EXACT_GROW: bool = false;
    const ROOM: bool = false;
    const SCRATCH_FLOOR: bool = true;
    const ADMIT_PREFETCH: bool = true;
    #[cfg(any(test, feature = "failpoints"))]
    const PROJECTION_TAG: Option<&'static str> = Some("checked-vocab-scratch-allocated");

    #[inline(always)]
    fn admit_growth(&mut self, _old_cap: usize, new_cap: usize) -> crate::error::Result<()> {
        (self.reserve)(new_cap)
    }

    fn invalid_err(&self, msg: &'static str) -> crate::error::Error {
        crate::error::Error::invalid(msg)
    }
}

/// Exact scratch admission using `try_reserve_exact` without `scratch_capacity()` floor
/// (for call-owned projection scratch in `get_with_fallible` and `find_with_checked`).
struct ExactScratch<R> {
    reserve: R,
}

impl<R> BlockScratch<crate::error::Error> for ExactScratch<R>
where
    R: FnMut(usize) -> crate::error::Result<()>,
{
    const CHECKED: bool = true;
    const POLLS: bool = false;
    const EXACT_GROW: bool = true;
    const ROOM: bool = false;
    const SCRATCH_FLOOR: bool = false;
    const ADMIT_PREFETCH: bool = false;
    #[cfg(any(test, feature = "failpoints"))]
    const PROJECTION_TAG: Option<&'static str> = Some("vocab-scratch-allocated");

    #[inline(always)]
    fn admit_growth(&mut self, _old_cap: usize, new_cap: usize) -> crate::error::Result<()> {
        (self.reserve)(new_cap)
    }

    fn invalid_err(&self, msg: &'static str) -> crate::error::Error {
        crate::error::Error::invalid(msg)
    }

    fn alloc_err(&self, err: std::collections::TryReserveError) -> crate::error::Error {
        crate::error::Error::invalid(format!("vocabulary allocation failed: {err}"))
    }
}

/// Fully controlled scratch accounting for coexisting old/new buffers, settled capacity,
/// and cooperative checkpoints during measurement, decoding, and byte comparison.
struct ControlledScratch<G, T, K> {
    grow: G,
    settled: T,
    checkpoint: K,
}

impl<G, T, K, E> BlockScratch<E> for ControlledScratch<G, T, K>
where
    E: From<crate::error::Error>,
    G: FnMut(usize, usize) -> std::result::Result<(), E>,
    T: FnMut(usize) -> std::result::Result<(), E>,
    K: FnMut(&'static str) -> std::result::Result<(), E>,
{
    const CHECKED: bool = true;
    const POLLS: bool = true;
    const EXACT_GROW: bool = true;
    const ROOM: bool = false;
    const SCRATCH_FLOOR: bool = false;
    const ADMIT_PREFETCH: bool = false;
    #[cfg(any(test, feature = "failpoints"))]
    const PROJECTION_TAG: Option<&'static str> = None;

    #[inline(always)]
    fn checkpoint(&mut self, phase: &'static str) -> std::result::Result<(), E> {
        (self.checkpoint)(phase)
    }

    #[inline(always)]
    fn admit_growth(&mut self, old_cap: usize, new_cap: usize) -> std::result::Result<(), E> {
        (self.grow)(old_cap, new_cap)
    }

    #[inline(always)]
    fn settled(&mut self, new_cap: usize) -> std::result::Result<(), E> {
        (self.settled)(new_cap)
    }

    fn invalid_err(&self, msg: &'static str) -> E {
        crate::error::Error::invalid(msg).into()
    }
}

/// Compile-time policy for admitting hint scratch vectors before read-ahead prefetching.
trait HintAdmit {
    type Error;
    const CHECKED: bool;
    fn admit(&mut self, _bytes: usize) -> std::result::Result<(), Self::Error> {
        Ok(())
    }
    fn invalid_err(&self, msg: &'static str) -> Self::Error;
}

impl HintAdmit for Unlimited {
    type Error = crate::error::Error;
    const CHECKED: bool = false;

    fn invalid_err(&self, msg: &'static str) -> crate::error::Error {
        crate::error::Error::invalid(msg)
    }
}

struct CheckedHint<F>(F);

impl<F: FnMut(usize) -> crate::error::Result<()>> HintAdmit for CheckedHint<F> {
    type Error = crate::error::Error;
    const CHECKED: bool = true;

    #[inline(always)]
    fn admit(&mut self, bytes: usize) -> crate::error::Result<()> {
        (self.0)(bytes)
    }

    fn invalid_err(&self, msg: &'static str) -> crate::error::Error {
        crate::error::Error::invalid(msg)
    }
}

struct ScratchHint<'a, S, E>(&'a mut S, std::marker::PhantomData<fn() -> E>);

impl<S: BlockScratch<E>, E> HintAdmit for ScratchHint<'_, S, E> {
    type Error = E;
    const CHECKED: bool = true;

    #[inline(always)]
    fn admit(&mut self, bytes: usize) -> std::result::Result<(), E> {
        self.0.admit_growth(0, bytes)
    }

    fn invalid_err(&self, msg: &'static str) -> E {
        self.0.invalid_err(msg)
    }
}

/// Compare two byte slices under `S`. Under non-polling controls, compiles directly to
/// `left.cmp(right)`; under polling controls, compares in 1024-byte chunks with checkpoints.
#[inline(always)]
fn compare_bytes_with<S: BlockScratch<E>, E>(
    left: &[u8],
    right: &[u8],
    scratch: &mut S,
) -> std::result::Result<std::cmp::Ordering, E> {
    if !S::POLLS {
        return Ok(left.cmp(right));
    }
    compare_prepared_bytes(left, right, &mut |phase| scratch.checkpoint(phase))
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
#[derive(Clone, Copy, Default)]
struct Entry {
    shared: usize,
    start: usize,
    len: usize,
    /// the bytes are FSST codes
    coded: bool,
}

impl Entry {
    /// The most bytes this entry's suffix can stand for: its length, or eight bytes per
    /// code. A partial decode reserves no more than this, so a damaged shared-prefix
    /// length in the next entry cannot ask for more room than the suffix could fill.
    #[inline(always)]
    fn most(&self) -> usize {
        if self.coded {
            self.len.saturating_mul(8)
        } else {
            self.len
        }
    }
}

impl Vocab {
    pub(crate) fn resident_bytes(&self) -> u64 {
        let sparse = self.sparse.as_ref().map_or(0, |s| {
            s.keys.capacity() as u64
                + s.starts.capacity() as u64 * 4
                + s.offsets.capacity() as u64 * 8
                + (s.hinted.len() + s.warm_keys.len()) as u64 * 8
        });
        sparse
            + self.prefetched.len() as u64 * 8
            + self
                .num
                .as_ref()
                .map_or(0, numeric::NumColumn::resident_bytes)
            + self
                .fsst
                .as_ref()
                .map_or(0, |_| std::mem::size_of::<fsst::Tables>() as u64)
    }
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
        Ok(Self::open_with(dir, fsst, &mut Unlimited)?.value)
    }

    pub(crate) fn open_with<C: BudgetControl>(
        dir: &Path,
        fsst: bool,
        control: &mut C,
    ) -> crate::error::Result<crate::store::cold::Loaded<Vocab>> {
        use crate::error::Error;
        control.check(0)?;
        let start = control.used();
        let tables = if fsst {
            if C::ACCOUNTS {
                let path = dir.join(fsst::FILE);
                let version = fsst::file_version(&path)?;
                control.check(0)?;
                crate::formats::VOCAB_FSST.check(fsst::FILE, version, None)?;
                // Two tables have at most 255 nine-byte symbols in the current codec.
                if std::fs::metadata(&path)?.len() > 32 + 2 * (1 + 255 * 9) {
                    return Err(Error::Corrupt("vocab.fsst: trailing bytes".into()));
                }
                let bytes = control.read_file(&path)?;
                control.charge(std::mem::size_of::<fsst::Tables>() as u64 + 64 + 4096)?;
                let tables = fsst::Tables::decode(&bytes)?;
                control.release(4096);
                control.drop_vec(bytes);
                Some(Box::new(tables))
            } else {
                Some(Box::new(fsst::Tables::read(dir)?))
            }
        } else {
            None
        };
        control.check(0)?;
        // A lookup reads a few hundred bytes of the data, so the data is read a page at
        // a time (see `crate::index::io_hints`). With the sparse index, a lookup reads
        // one range of the offsets, which are mapped for random access too. Without it, a
        // binary search steps through the whole offsets file, which is small next to the
        // data and read in windows.
        let data = Bytes::open(&dir.join("vocab.dat"), true)?;
        let off_path = dir.join("vocab.off");
        let (offsets, blocks, len, sparse) = if C::ACCOUNTS {
            let offsets = Bytes::open(&off_path, true)?;
            let off = offsets.as_slice();
            if !off.len().is_multiple_of(8) {
                return Err(Error::Corrupt("vocabulary offset length".into()));
            }
            let blocks = (off.len() / 8).saturating_sub(1);
            let len = if off.len() >= 8 {
                u64::from_le_bytes(off[off.len() - 8..].try_into().unwrap())
            } else {
                0
            };
            if usize::try_from(len).ok().map(|n| n.div_ceil(FC_BLOCK)) != Some(blocks) {
                return Err(Error::Corrupt(
                    "vocabulary offsets disagree with its term count".into(),
                ));
            }
            let sparse = if crate::index::sparse_vocab() {
                let before = control.used();
                match Sparse::read_with(
                    &dir.join("vocab.idx"),
                    blocks,
                    data.as_slice().len() as u64,
                    control,
                ) {
                    Ok(loaded) => Some(loaded.value),
                    Err(crate::Error::BudgetExceeded(_)) => {
                        // The index is required on disk but not in memory: a budgeted
                        // reader that cannot hold it looks terms up through the offsets,
                        // which remains exact. Its failed decoder owns no surviving storage.
                        control.release(control.used() - before);
                        None
                    }
                    Err(error) => return Err(error),
                }
            } else {
                require_index_file(dir)?;
                None
            };
            (offsets, blocks, len, sparse)
        } else {
            let blocks = (std::fs::metadata(&off_path)?.len() as usize / 8).saturating_sub(1);
            let sparse = if crate::index::sparse_vocab() {
                Some(
                    Sparse::read_with(
                        &dir.join("vocab.idx"),
                        blocks,
                        data.as_slice().len() as u64,
                        control,
                    )?
                    .value,
                )
            } else {
                require_index_file(dir)?;
                None
            };
            let offsets = Bytes::open(&off_path, sparse.is_some())?;
            let off = offsets.as_slice();
            let len = if off.len() >= 8 {
                u64::from_le_bytes(off[off.len() - 8..].try_into().unwrap())
            } else {
                0
            };
            (offsets, blocks, len, sparse)
        };
        let prefetched = control.filled(blocks.div_ceil(64))?;
        let mut vocab = Vocab {
            data,
            offsets,
            len,
            first_iri: 0,
            first_triple: 0,
            sparse,
            prefetched,
            num: None,
            fsst: tables,
        };
        // a cheap test that the index belongs to these files: its first and last data
        // offsets (`sparkles check` compares every entry)
        if let Some(sparse) = &vocab.sparse
            && let Some(last) = sparse.len().checked_sub(1)
            && (sparse.offsets[0] != vocab.block_offset(0) as u64
                || sparse.offsets[last] != vocab.block_offset(last * IDX_BLOCKS) as u64)
        {
            if C::ACCOUNTS {
                // The index and its owned storage drop with the refused vocabulary.
                let sparse = vocab.sparse.take().unwrap();
                let bytes = sparse.keys.capacity() as u64
                    + sparse.starts.capacity() as u64 * 4
                    + sparse.offsets.capacity() as u64 * 8
                    + (sparse.hinted.len() + sparse.warm_keys.len()) as u64 * 8;
                drop(sparse);
                control.release(bytes);
            }
            return Err(mismatched_index(&dir.join("vocab.idx")));
        }
        if C::ACCOUNTS {
            // Type boundaries need one byte, not a reconstructed giant literal.
            vocab.first_iri = vocab.boundary_with(b'<', control)?;
            vocab.first_triple = vocab.boundary_with(b'(', control)?;
            let before = control.used();
            vocab.num = match numeric::NumColumn::open_with(dir, len, vocab.first_triple, control) {
                Ok(loaded) => loaded.map(|loaded| loaded.value),
                Err(crate::Error::BudgetExceeded(_)) => {
                    // Numeric values can be decoded from keys when this optional
                    // column cannot fit. Do not invoke the old uncontrolled opener.
                    control.release(control.used() - before);
                    None
                }
                Err(error) => return Err(error),
            };
        } else {
            vocab.first_iri = match vocab.find(b"<") {
                Ok(i) | Err(i) => i,
            };
            vocab.first_triple = match vocab.find(b"(") {
                Ok(i) | Err(i) => i,
            };
            vocab.num = numeric::NumColumn::open(dir, vocab.len, vocab.first_triple);
        }
        control.check(0)?;
        Ok(crate::store::cold::Loaded {
            value: vocab,
            charge: control.used() - start,
        })
    }

    #[cfg(test)]
    pub(crate) fn open_controlled(
        dir: &Path,
        fsst: bool,
        budget: &mut crate::store::cold::ColdBudget<'_>,
    ) -> crate::error::Result<crate::store::cold::Loaded<Vocab>> {
        Self::open_with(dir, fsst, budget)
    }

    fn entry_controlled(&self, position: &mut usize, first: bool) -> crate::error::Result<Entry> {
        let bad = || crate::Error::Corrupt("invalid vocabulary entry".into());
        let data = self.data.as_slice();
        let shared =
            usize::try_from(crate::index::read_varint_checked(data, position).ok_or_else(bad)?)
                .map_err(|_| bad())?;
        let word =
            usize::try_from(crate::index::read_varint_checked(data, position).ok_or_else(bad)?)
                .map_err(|_| bad())?;
        let (len, coded) = if self.fsst.is_some() && !first {
            (word >> 1, word & 1 != 0)
        } else {
            (word, false)
        };
        let start = *position;
        let end = start.checked_add(len).ok_or_else(bad)?;
        data.get(start..end).ok_or_else(bad)?;
        *position = end;
        Ok(Entry {
            shared,
            start,
            len,
            coded,
        })
    }

    fn boundary_with<C: Control>(&self, threshold: u8, control: &C) -> crate::error::Result<u64> {
        let count = self.num_blocks();
        let first_byte = |block: usize| -> crate::error::Result<Option<u8>> {
            control.check(0)?;
            let mut position = usize::try_from(u64::from_le_bytes(
                self.offsets.as_slice()[block * 8..block * 8 + 8]
                    .try_into()
                    .unwrap(),
            ))
            .map_err(|_| crate::Error::Corrupt("vocabulary offset overflows".into()))?;
            let entry = self.entry_controlled(&mut position, true)?;
            Ok(self.data.as_slice()[entry.start..entry.start + entry.len]
                .first()
                .copied())
        };
        let (mut lo, mut hi) = (0usize, count);
        while lo < hi {
            let mid = lo + (hi - lo) / 2;
            if first_byte(mid)?.is_some_and(|byte| byte >= threshold) {
                hi = mid;
            } else {
                lo = mid + 1;
            }
        }
        if lo == 0 {
            control.check(0)?;
            return Ok(0);
        }
        let block = lo - 1;
        let mut position = usize::try_from(u64::from_le_bytes(
            self.offsets.as_slice()[block * 8..block * 8 + 8]
                .try_into()
                .unwrap(),
        ))
        .map_err(|_| crate::Error::Corrupt("vocabulary offset overflows".into()))?;
        let mut previous = None;
        for index in 0..FC_BLOCK.min(self.len as usize - block * FC_BLOCK) {
            control.check(0)?;
            let entry = self.entry_controlled(&mut position, index == 0)?;
            if entry.shared == 0 || previous.is_none() {
                let suffix = &self.data.as_slice()[entry.start..entry.start + entry.len];
                previous = match &self.fsst {
                    Some(tables) if entry.coded => tables
                        .table((block * FC_BLOCK + index) as u64)
                        .first_byte_with(suffix, control)?,
                    _ => suffix.first().copied(),
                };
            }
            if previous.is_some_and(|byte| byte >= threshold) {
                return Ok((block * FC_BLOCK + index) as u64);
            }
        }
        control.check(0)?;
        Ok(((block + 1) * FC_BLOCK).min(self.len as usize) as u64)
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

    /// Whether this generation already has a valid current-format numeric column loaded
    /// in memory or written to `dir`.
    pub(crate) fn has_current_numeric_column(&self, dir: &Path) -> bool {
        self.num.is_some() || numeric::NumColumn::is_current(dir, self.len, self.first_triple)
    }

    /// Upgrade a missing or unreadable numeric column under the generation's writer lock.
    pub(crate) fn ensure_numeric_column(&self, dir: &Path) -> Result<Option<u64>> {
        if self.has_current_numeric_column(dir) {
            return Ok(None);
        }
        write_numeric_column(dir, self).map(Some)
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

    /// The first capacity of a key buffer. An FSST suffix decodes without branches only
    /// with 64 bytes of room past the key (`fsst::Table::decode_at`), so a buffer of 64
    /// bytes would be reallocated on its second key, once per lookup of a single key.
    #[inline(always)]
    fn scratch_capacity(&self) -> usize {
        if self.fsst.is_some() { 192 } else { 64 }
    }

    /// Decode a whole block, invoking `f(index_in_block, key)`; stops when `f` returns false.
    fn scan_block(&self, b: usize, mut f: impl FnMut(usize, &[u8]) -> bool) {
        let mut key = Vec::with_capacity(self.scratch_capacity());
        let mut cb = |i, _, k: &mut Vec<u8>, _: &mut Unlimited| Ok(f(i, k));
        let _ = if ioacct::profiling() {
            self.decode_block_dispatch::<true, _, _>(b, ALL_KEYS, &mut key, &mut Unlimited, &mut cb)
        } else {
            self.decode_block_dispatch::<false, _, _>(
                b,
                ALL_KEYS,
                &mut key,
                &mut Unlimited,
                &mut cb,
            )
        };
    }

    #[inline(always)]
    fn decode_block_dispatch<const P: bool, S: BlockScratch<E>, E>(
        &self,
        b: usize,
        wanted: u32,
        key: &mut Vec<u8>,
        scratch: &mut S,
        each: &mut impl FnMut(usize, usize, &mut Vec<u8>, &mut S) -> std::result::Result<bool, E>,
    ) -> std::result::Result<usize, E> {
        if self.fsst.is_some() {
            self.decode_block_with::<P, true, S, E>(b, wanted, key, scratch, each)
        } else {
            self.decode_block_with::<P, false, S, E>(b, wanted, key, scratch, each)
        }
    }

    /// Unified front-coded block decoder for both plain (`F = false`) and FSST (`F = true`)
    /// vocabularies, generic over [`BlockScratch`]. Under [`Unlimited`], all admission and
    /// checkpoint branches compile away and the dense fast path is preserved unchanged.
    /// Returns the last decoded entry's shared-prefix length within the block.
    #[inline(always)]
    fn decode_block_with<const P: bool, const F: bool, S: BlockScratch<E>, E>(
        &self,
        b: usize,
        wanted: u32,
        key: &mut Vec<u8>,
        scratch: &mut S,
        each: &mut impl FnMut(usize, usize, &mut Vec<u8>, &mut S) -> std::result::Result<bool, E>,
    ) -> std::result::Result<usize, E> {
        key.clear();
        let data = self.data.as_slice();
        let mut pos = self.block_offset_at::<P>(b);
        let start = b * FC_BLOCK;
        let n = FC_BLOCK.min((self.len as usize) - start);
        if P {
            ioacct::vocab_blocks_scanned(1);
            self.touch_block(b);
        }
        let wanted = wanted & ((1u32 << n) - 1);
        if wanted == 0 {
            return Ok(0);
        }
        let last = 31 - wanted.leading_zeros() as usize;
        if !S::CHECKED && wanted == (2u32 << last) - 1 {
            // every key up to the last is wanted, so each is decoded whole
            let mut last_shared = 0;
            for i in 0..=last {
                let e = self.entry::<F>(data, &mut pos, i == 0);
                last_shared = e.shared;
                key.truncate(e.shared);
                self.push_suffix::<F>(data, e, (start + i) as u64, usize::MAX, key, false);
                if wanted >> i & 1 == 1 && !each(i, e.shared, key, scratch)? {
                    return Ok(last_shared);
                }
            }
            return Ok(last_shared);
        }
        // Under checked non-polling decoders, capacity changes only through our own
        // reservations below, so cache it in a register across entries.
        let mut cap = key.capacity();
        let mut e = self.entry::<F>(data, &mut pos, true);
        if !S::CHECKED {
            for i in 0..=last {
                let next = (i < last).then(|| self.entry::<F>(data, &mut pos, false));
                let need = lookahead(wanted, i, next);
                self.step::<F>(data, e, (start + i) as u64, need, key, false);
                if wanted >> i & 1 == 1 && !each(i, e.shared, key, scratch)? {
                    return Ok(e.shared);
                }
                if let Some(n) = next {
                    e = n;
                }
            }
            return Ok(e.shared);
        }
        if !F && !S::POLLS && !S::EXACT_GROW {
            // Checked plain-suffix fast path: keep the tight register-friendly loop.
            for at in 0..=last {
                let next = (at < last).then(|| self.entry::<F>(data, &mut pos, false));
                let nd = lookahead(wanted, at, next);
                if nd <= e.shared {
                    key.truncate(nd);
                } else {
                    key.truncate(e.shared);
                    let add = e.len.min(nd - e.shared);
                    let total = key.len() + add;
                    if total > cap {
                        let total = total.max(self.scratch_capacity());
                        scratch.admit_growth(cap, total)?;
                        key.reserve_exact(total - key.len());
                        cap = key.capacity();
                        #[cfg(any(test, feature = "failpoints"))]
                        if let Some(tag) = S::PROJECTION_TAG {
                            crate::algo::observe_projection(
                                crate::algo::ProjectionEvent::Materialized(tag, cap),
                            );
                        }
                    }
                    key.extend_from_slice(&data[e.start..e.start + add]);
                }
                if wanted >> at & 1 == 1 && !each(at, e.shared, key, scratch)? {
                    return Ok(e.shared);
                }
                if let Some(n) = next {
                    e = n;
                }
            }
            return Ok(e.shared);
        }
        for at in 0..=last {
            if S::POLLS {
                scratch.checkpoint("reconstructing vocabulary entry")?;
            }
            let id = (start + at) as u64;
            let next = (at < last).then(|| self.entry::<F>(data, &mut pos, false));
            let nd = lookahead(wanted, at, next);
            if S::POLLS {
                key.truncate(e.shared.min(nd));
            }
            if nd > e.shared {
                let prefix = key.len().min(e.shared);
                let suffix = &data[e.start..e.start + e.len];
                let coded_table = if F && e.coded {
                    self.fsst.as_ref().map(|t| t.table(id))
                } else {
                    None
                };
                let add = if nd == usize::MAX {
                    if let Some(table) = coded_table {
                        if S::POLLS {
                            struct PrefixMeasure<'a, S> {
                                scratch: &'a mut S,
                                prefix: usize,
                                cur_cap: usize,
                            }
                            impl<S: BlockScratch<E>, E> fsst::SuffixMeasure<E> for PrefixMeasure<'_, S> {
                                fn admit(&mut self, length: usize) -> std::result::Result<(), E> {
                                    let new = self.prefix.checked_add(length).ok_or_else(|| {
                                        self.scratch.invalid_err("vocabulary scratch overflows")
                                    })?;
                                    if new > self.cur_cap {
                                        self.scratch.admit_growth(self.cur_cap, new)?;
                                    }
                                    Ok(())
                                }
                                fn checkpoint(
                                    &mut self,
                                    phase: &'static str,
                                ) -> std::result::Result<(), E> {
                                    self.scratch.checkpoint(phase)
                                }
                                fn overflow(&self) -> E {
                                    self.scratch.invalid_err("vocabulary suffix overflows")
                                }
                            }
                            let mut measure = PrefixMeasure {
                                scratch,
                                prefix,
                                cur_cap: key.capacity(),
                            };
                            table.measure_with::<true, _, E>(suffix, &mut measure)?
                        } else {
                            table.decoded_len(suffix)
                        }
                    } else {
                        e.len
                    }
                } else {
                    (nd - e.shared).min(e.most())
                };
                let total = if S::EXACT_GROW {
                    prefix
                        .checked_add(add)
                        .ok_or_else(|| scratch.invalid_err("vocabulary scratch overflows"))?
                } else {
                    prefix.saturating_add(add)
                };
                if S::EXACT_GROW {
                    if total > cap {
                        scratch.admit_growth(cap, total)?;
                        key.try_reserve_exact(total - key.len())
                            .map_err(|err| scratch.alloc_err(err))?;
                        cap = key.capacity();
                        if cap != total {
                            return Err(
                                scratch.invalid_err("unsupported vocabulary scratch capacity")
                            );
                        }
                        if S::POLLS {
                            scratch.settled(cap)?;
                        }
                        #[cfg(any(test, feature = "failpoints"))]
                        if let Some(tag) = S::PROJECTION_TAG {
                            crate::algo::observe_projection(
                                crate::algo::ProjectionEvent::Materialized(tag, cap),
                            );
                        }
                    }
                } else {
                    let fast_room = F && e.coded && e.len <= 8;
                    let room = if S::ROOM && fast_room { FSST_ROOM } else { 0 };
                    if total.saturating_add(room) > cap {
                        // An FSST suffix of at most eight codes decodes fastest with room for
                        // eight symbols (`fsst::Table::decode_at`). That headroom is optional:
                        // if admitting it is refused, fall back to the required capacity (or
                        // keep the already-admitted buffer when `total <= cap`) so checked
                        // lookups never fail over optional headroom.
                        let min_cap = if S::SCRATCH_FLOOR {
                            total.max(self.scratch_capacity())
                        } else {
                            total
                        };
                        let target = if fast_room {
                            let preferred = if S::SCRATCH_FLOOR {
                                total.saturating_add(FSST_ROOM).max(self.scratch_capacity())
                            } else {
                                total.saturating_add(FSST_ROOM)
                            };
                            if scratch.admit_growth(cap, preferred).is_ok() {
                                Some(preferred)
                            } else if total > cap {
                                scratch.admit_growth(cap, min_cap)?;
                                Some(min_cap)
                            } else {
                                None
                            }
                        } else {
                            scratch.admit_growth(cap, min_cap)?;
                            Some(min_cap)
                        };
                        if let Some(target) = target {
                            key.reserve_exact(target - key.len());
                            cap = key.capacity();
                            #[cfg(any(test, feature = "failpoints"))]
                            if let Some(tag) = S::PROJECTION_TAG {
                                crate::algo::observe_projection(
                                    crate::algo::ProjectionEvent::Materialized(tag, cap),
                                );
                            }
                        }
                    }
                }
                if S::POLLS {
                    if let Some(table) = coded_table {
                        table.decode_controlled(suffix, add, key, &mut |phase| {
                            scratch.checkpoint(phase)
                        })?;
                    } else {
                        for chunk in suffix[..suffix.len().min(add)].chunks(1024) {
                            key.extend_from_slice(chunk);
                            scratch.checkpoint("copying vocabulary suffix")?;
                        }
                    }
                } else {
                    self.step::<F>(data, e, id, nd, key, true);
                }
            } else if !S::POLLS {
                key.truncate(nd);
            }
            if wanted >> at & 1 == 1 && !each(at, e.shared, key, scratch)? {
                break;
            }
            if let Some(n) = next {
                e = n;
            }
        }
        if S::POLLS {
            scratch.checkpoint("completed vocabulary reconstruction")?;
        }
        Ok(e.shared)
    }

    /// The entry at `pos` (advanced past it), the first of its block when `first`.
    /// `F` says whether the vocabulary is FSST-coded.
    #[inline(always)]
    fn entry<const F: bool>(&self, data: &[u8], pos: &mut usize, first: bool) -> Entry {
        let shared = read_varint(data, pos) as usize;
        let word = read_varint(data, pos) as usize;
        let (len, coded) = if F && !first {
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

    /// Append the first `max` bytes of the suffix of entry `e` (of key `id`) to `key`, or
    /// all of it when it is shorter. With `exact`, the caller has reserved the room and
    /// the buffer does not grow (see [`fsst::Table::decode_upto`]).
    #[inline(always)]
    fn push_suffix<const F: bool>(
        &self,
        data: &[u8],
        e: Entry,
        id: u64,
        max: usize,
        key: &mut Vec<u8>,
        exact: bool,
    ) {
        if F && e.coded
            && let Some(t) = &self.fsst
        {
            t.table(id)
                .decode_at(data, e.start, e.len, max, key, !exact)
        } else {
            key.extend_from_slice(&data[e.start..e.start + e.len.min(max)])
        }
    }

    /// Bring `key` from the bytes it holds of the key before entry `e` to the first
    /// `need` bytes of the key of `e` (of id `id`), or all of it when `need` is
    /// `usize::MAX`. `key` must hold at least `need.min(e.shared)` bytes, which
    /// [`lookahead`] ensures.
    #[inline(always)]
    fn step<const F: bool>(
        &self,
        data: &[u8],
        e: Entry,
        id: u64,
        need: usize,
        key: &mut Vec<u8>,
        exact: bool,
    ) {
        if need <= e.shared {
            key.truncate(need);
        } else {
            key.truncate(e.shared);
            self.push_suffix::<F>(data, e, id, need - e.shared, key, exact);
        }
    }

    pub fn get(&self, id: u64) -> Option<Vec<u8>> {
        self.get_with(id, |key| key.to_vec())
    }

    /// Decode a key while its block scratch is borrowed, avoiding an intermediate
    /// owned key when the caller immediately converts it to another owned value.
    pub(crate) fn get_with<T>(&self, id: u64, mut f: impl FnMut(&[u8]) -> T) -> Option<T> {
        self.get_with_shared_prefix(id, |_, key| f(key))
    }

    /// Decode a key with the length of its common prefix with the preceding key.
    /// Both vocabulary codecs store that length for keys after the first in a block.
    /// For the first key of a block, the prefix length is unknown.
    pub(crate) fn get_with_shared_prefix<T>(
        &self,
        id: u64,
        f: impl FnMut(Option<usize>, &[u8]) -> T,
    ) -> Option<T> {
        self.get_with_shared_prefix_in(id, &mut Vec::new(), f)
    }

    /// [`Vocab::get_with_shared_prefix`] reusing the caller's scratch buffer across calls.
    pub(crate) fn get_with_shared_prefix_in<T>(
        &self,
        id: u64,
        key: &mut Vec<u8>,
        f: impl FnMut(Option<usize>, &[u8]) -> T,
    ) -> Option<T> {
        if id >= self.len {
            return None;
        }
        // one check for a profile here, rather than one in each step of the decoding:
        // a result of a few hundred terms decodes each with its own call
        if ioacct::profiling() {
            let _t = ioacct::timer(Phase::VocabRead);
            self.get_with_at::<true, T>(id, key, f)
        } else {
            self.get_with_at::<false, T>(id, key, f)
        }
    }

    #[inline(always)]
    fn get_with_at<const P: bool, T>(
        &self,
        id: u64,
        key: &mut Vec<u8>,
        mut f: impl FnMut(Option<usize>, &[u8]) -> T,
    ) -> Option<T> {
        let b = (id as usize) / FC_BLOCK;
        let want = (id as usize) % FC_BLOCK;
        let mut out = None;
        let mut buf = std::mem::take(key);
        let cap = self.scratch_capacity();
        if buf.capacity() < cap {
            buf.clear();
            buf.reserve(cap);
        }
        let mut read = |_, shared, key: &mut Vec<u8>, _: &mut Unlimited| {
            out = Some(f((want != 0).then_some(shared), key));
            Ok(false)
        };
        let _ = self.decode_block_dispatch::<P, _, _>(
            b,
            1 << want,
            &mut buf,
            &mut Unlimited,
            &mut read,
        );
        *key = buf;
        out
    }

    /// If entry `id` is stored with an FSST-coded suffix, return its encoded code count
    /// (for regression tests verifying `fast_room` eligibility).
    #[cfg(test)]
    pub(crate) fn entry_coded_len(&self, id: u64) -> Option<usize> {
        if id >= self.len || self.fsst.is_none() {
            return None;
        }
        let b = (id as usize) / FC_BLOCK;
        let want = (id as usize) % FC_BLOCK;
        let data = self.data.as_slice();
        let mut pos = self.block_offset_at::<false>(b);
        let mut e = self.entry::<true>(data, &mut pos, true);
        for _ in 1..=want {
            e = self.entry::<true>(data, &mut pos, false);
        }
        e.coded.then_some(e.len)
    }

    /// [`Vocab::get_with_shared_prefix`] with scratch admitted before the walk and
    /// every subsequent growth admitted before allocation.
    #[cfg(test)]
    pub(crate) fn get_with_shared_prefix_checked<T, E>(
        &self,
        id: u64,
        reserve: impl FnMut(usize) -> std::result::Result<(), E>,
        f: impl FnOnce(Option<usize>, &[u8]) -> T,
    ) -> std::result::Result<Option<T>, E> {
        self.get_with_shared_prefix_checked_impl::<false, T, E>(id, &mut Vec::new(), reserve, f)
    }

    /// [`Vocab::get_with_shared_prefix`] reusing the caller's scratch buffer across calls,
    /// with initial scratch capacity and every subsequent growth admitted before allocation.
    /// Preserves the required capacity so optional `FSST_ROOM` headroom never fails a
    /// lookup that fits in the admitted buffer.
    pub(crate) fn get_with_shared_prefix_checked_in<T, E>(
        &self,
        id: u64,
        key: &mut Vec<u8>,
        reserve: impl FnMut(usize) -> std::result::Result<(), E>,
        f: impl FnOnce(Option<usize>, &[u8]) -> T,
    ) -> std::result::Result<Option<T>, E> {
        self.get_with_shared_prefix_checked_impl::<false, T, E>(id, key, reserve, f)
    }

    #[inline(always)]
    fn get_with_shared_prefix_checked_impl<const ROOM: bool, T, E>(
        &self,
        id: u64,
        key: &mut Vec<u8>,
        mut reserve: impl FnMut(usize) -> std::result::Result<(), E>,
        f: impl FnOnce(Option<usize>, &[u8]) -> T,
    ) -> std::result::Result<Option<T>, E> {
        if id >= self.len {
            return Ok(None);
        }
        let b = (id as usize) / FC_BLOCK;
        let want = (id as usize) % FC_BLOCK;
        let capacity = self.scratch_capacity();
        if key.capacity() < capacity {
            reserve(capacity)?;
            key.clear();
            key.reserve_exact(capacity);
            #[cfg(any(test, feature = "failpoints"))]
            crate::algo::observe_projection(crate::algo::ProjectionEvent::Materialized(
                "checked-vocab-scratch-allocated",
                key.capacity(),
            ));
        }
        let mut buf = std::mem::take(key);
        let mut scratch = CheckedScratch::<_, ROOM> { reserve };
        let mut ignore = |_, _, _: &mut Vec<u8>, _: &mut CheckedScratch<_, ROOM>| Ok(true);
        let mask = 1 << want;
        let res = if ioacct::profiling() {
            let _t = ioacct::timer(Phase::VocabRead);
            self.decode_block_dispatch::<true, _, E>(b, mask, &mut buf, &mut scratch, &mut ignore)
        } else {
            self.decode_block_dispatch::<false, _, E>(b, mask, &mut buf, &mut scratch, &mut ignore)
        };
        *key = buf;
        let shared = res?;
        Ok(Some(f((want != 0).then_some(shared), key)))
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
        reserve: impl FnMut(usize) -> std::result::Result<(), E>,
        f: impl FnMut(u64, &[u8]) -> std::result::Result<(), E>,
    ) -> std::result::Result<(), E> {
        let mut scratch = CheckedScratch::<_, false> { reserve };
        self.get_sorted_with(ids, &mut Vec::new(), &mut scratch, f, true)
    }

    /// The bulk output owner admits hint metadata and reconstruction independently
    /// of decoded payloads, retaining the same sorted/FSST/read-ahead policies.
    pub(crate) fn get_sorted_owned_with_ahead(
        &self,
        ids: &[u64],
        reserve: impl FnMut(usize) -> crate::error::Result<()>,
        f: impl FnMut(u64, &[u8]) -> crate::error::Result<()>,
        read_ahead: bool,
    ) -> crate::error::Result<()> {
        let mut scratch = OwnedCheckedScratch { reserve };
        self.get_sorted_with(ids, &mut Vec::new(), &mut scratch, f, read_ahead)
    }

    /// Unified sorted multi-key decoder, generic over [`BlockScratch`]. Under [`Unlimited`],
    /// admission and error propagation compile away.
    #[inline]
    fn get_sorted_with<S: BlockScratch<E>, E>(
        &self,
        ids: &[u64],
        key: &mut Vec<u8>,
        scratch: &mut S,
        f: impl FnMut(u64, &[u8]) -> std::result::Result<(), E>,
        read_ahead: bool,
    ) -> std::result::Result<(), E> {
        // One key needs no read-ahead or prefetch of its block.
        let ahead = if read_ahead && ids.len() > 1 {
            self.ahead_for(ids)
        } else {
            Ahead::default()
        };
        if read_ahead && ahead.end == 0 && ids.len() > 1 {
            // scattered ids: ask for their blocks at once, so that a cold vocabulary
            // reads them in parallel instead of faulting on one after another
            if S::ADMIT_PREFETCH {
                self.prefetch_sorted_with(
                    ids,
                    &mut ScratchHint(scratch, std::marker::PhantomData),
                )?;
            } else {
                self.prefetch_sorted(ids);
            }
        }
        // Rebuild in a local buffer, which the decode loops can keep in registers, and
        // return it, with its capacity, to the caller on every path.
        let mut buf = std::mem::take(key);
        if !S::CHECKED
            && buf.capacity() < self.scratch_capacity()
            && ids.first().is_some_and(|&id| id < self.len)
        {
            buf.clear();
            buf.reserve(self.scratch_capacity());
        }
        let done = if ioacct::profiling() {
            let _t = ioacct::timer(Phase::VocabRead);
            self.get_sorted_dispatch::<true, S, E>(ids, &mut buf, scratch, f, ahead)
        } else {
            self.get_sorted_dispatch::<false, S, E>(ids, &mut buf, scratch, f, ahead)
        };
        *key = buf;
        done
    }

    #[inline]
    fn get_sorted_dispatch<const P: bool, S: BlockScratch<E>, E>(
        &self,
        ids: &[u64],
        key: &mut Vec<u8>,
        scratch: &mut S,
        f: impl FnMut(u64, &[u8]) -> std::result::Result<(), E>,
        ahead: Ahead,
    ) -> std::result::Result<(), E> {
        if self.fsst.is_some() {
            self.get_sorted_as::<P, true, S, E>(ids, key, scratch, f, ahead)
        } else {
            self.get_sorted_as::<P, false, S, E>(ids, key, scratch, f, ahead)
        }
    }

    #[inline(always)]
    fn get_sorted_as<const P: bool, const F: bool, S: BlockScratch<E>, E>(
        &self,
        ids: &[u64],
        key: &mut Vec<u8>,
        scratch: &mut S,
        mut f: impl FnMut(u64, &[u8]) -> std::result::Result<(), E>,
        mut ahead: Ahead,
    ) -> std::result::Result<(), E> {
        if let [id] = ids {
            // One key needs no partition of its ids.
            key.clear();
            if *id < self.len {
                let b = *id as usize / FC_BLOCK;
                let mask = 1u32 << (*id as usize % FC_BLOCK);
                let mut emit = |at, _, k: &mut Vec<u8>, _: &mut S| {
                    f((b * FC_BLOCK + at) as u64, k)?;
                    Ok(true)
                };
                self.decode_block_with::<P, F, S, E>(b, mask, key, scratch, &mut emit)?;
            }
            return Ok(());
        }
        key.clear();
        let mut i = 0;
        while i < ids.len() && ids[i] < self.len {
            let b = ids[i] as usize / FC_BLOCK;
            ahead.at(self, b);
            let end = ((b + 1) * FC_BLOCK) as u64;
            let j = i + ids[i..].partition_point(|&x| x < end);
            let n = FC_BLOCK.min(self.len as usize - b * FC_BLOCK);
            let mut mask = 0u32;
            for &x in &ids[i..j] {
                mask |= 1 << (x as usize % FC_BLOCK);
            }
            mask &= (1u32 << n) - 1;
            let mut emit = |at, _, k: &mut Vec<u8>, _: &mut S| {
                f((b * FC_BLOCK + at) as u64, k)?;
                Ok(true)
            };
            self.decode_block_with::<P, F, S, E>(b, mask, key, scratch, &mut emit)?;
            i = j;
        }
        Ok(())
    }

    /// Decode one key after `reserve` admits reconstruction capacity, including
    /// intermediate front-coded keys. The callback borrows the reconstructed key.
    /// Callers may admit decoded output separately before allocating it.
    pub fn get_with_checked<T, E>(
        &self,
        id: u64,
        reserve: impl FnMut(usize) -> std::result::Result<(), E>,
        mut f: impl FnMut(&[u8]) -> T,
    ) -> std::result::Result<Option<T>, E> {
        let mut out = None;
        let mut scratch = CheckedScratch::<_, false> { reserve };
        self.get_sorted_with(
            &[id],
            &mut Vec::new(),
            &mut scratch,
            |_, key| {
                out = Some(f(key));
                Ok(())
            },
            true,
        )?;
        Ok(out)
    }

    /// The same checked reconstruction with the caller's physically paid buffer.
    /// The caller reserves every requested growth before it happens and retains
    /// the buffer's capacity credit until that allocation is actually freed.
    pub(crate) fn get_with_checked_in<T, E>(
        &self,
        id: u64,
        key: &mut Vec<u8>,
        reserve: impl FnMut(usize) -> std::result::Result<(), E>,
        mut f: impl FnMut(&[u8]) -> T,
    ) -> std::result::Result<Option<T>, E> {
        let mut out = None;
        let mut scratch = CheckedScratch::<_, true> { reserve };
        self.get_sorted_with(
            &[id],
            key,
            &mut scratch,
            |_, key| {
                out = Some(f(key));
                Ok(())
            },
            true,
        )?;
        Ok(out)
    }

    /// Request-controlled reconstruction. The callback borrows admitted scratch.
    pub(crate) fn get_with_controlled<T>(
        &self,
        id: u64,
        mut reserve: impl FnMut(usize) -> crate::error::Result<()>,
        checkpoint: impl FnMut(&'static str) -> crate::error::Result<()>,
        mut f: impl FnMut(&[u8]) -> T,
    ) -> crate::error::Result<Option<T>> {
        self.get_with_controlled_capacity_checked(
            id,
            |_, new| reserve(new),
            |_| Ok(()),
            checkpoint,
            |key| Ok(f(key)),
        )
    }

    /// Read a key while its owner accounts for old and new scratch during growth,
    /// then only the settled allocation. The callback runs while the scratch is live.
    pub fn get_with_controlled_capacity<T, E: From<crate::error::Error>>(
        &self,
        id: u64,
        grow: impl FnMut(usize, usize) -> Result<(), E>,
        settled: impl FnMut(usize) -> Result<(), E>,
        mut read: impl FnMut(&[u8]) -> Result<T, E>,
    ) -> Result<Option<T>, E> {
        if id >= self.len {
            return Ok(None);
        }
        let mut result = None;
        let mut scratch = ControlledScratch {
            grow,
            settled,
            checkpoint: |_| Ok(()),
        };
        let mut key = Vec::new();
        let _timer = ioacct::timer(Phase::VocabRead);
        self.decode_block_dispatch::<true, _, E>(
            id as usize / FC_BLOCK,
            1 << (id as usize % FC_BLOCK),
            &mut key,
            &mut scratch,
            &mut |_, _, k, _| {
                result = Some(read(k)?);
                Ok(false)
            },
        )?;
        Ok(result)
    }

    /// Read a key while its owner accounts for old and new scratch during growth,
    /// then only the settled allocation, polling `checkpoint` during block scanning
    /// and suffix decoding. The callback runs while the scratch is live.
    pub fn get_with_controlled_capacity_checked<T, E: From<crate::error::Error>>(
        &self,
        id: u64,
        grow: impl FnMut(usize, usize) -> Result<(), E>,
        settled: impl FnMut(usize) -> Result<(), E>,
        mut checkpoint: impl FnMut(&'static str) -> Result<(), E>,
        mut read: impl FnMut(&[u8]) -> Result<T, E>,
    ) -> Result<Option<T>, E> {
        checkpoint("starting vocabulary reconstruction")?;
        if id >= self.len {
            return Ok(None);
        }
        let mut result = None;
        let mut scratch = ControlledScratch {
            grow,
            settled,
            checkpoint,
        };
        let mut key = Vec::new();
        let _timer = ioacct::timer(Phase::VocabRead);
        self.decode_block_dispatch::<true, _, E>(
            id as usize / FC_BLOCK,
            1 << (id as usize % FC_BLOCK),
            &mut key,
            &mut scratch,
            &mut |_, _, k, _| {
                result = Some(read(k)?);
                Ok(false)
            },
        )?;
        Ok(result)
    }

    /// Reconstruct one key into admitted storage that the callback may retain.
    /// Growth covers both physical buffers; settlement follows old-buffer retirement.
    /// The callback's owner must keep that admission until its final allocation drops.
    pub(crate) fn get_owned_with_controlled<T>(
        &self,
        id: u64,
        grow: impl FnMut(usize, usize) -> crate::error::Result<()>,
        settled: impl FnMut(usize) -> crate::error::Result<()>,
        mut checkpoint: impl FnMut(&'static str) -> crate::error::Result<()>,
        f: impl FnOnce(Vec<u8>) -> crate::error::Result<T>,
    ) -> crate::error::Result<Option<T>> {
        checkpoint("starting owned vocabulary reconstruction")?;
        if id >= self.len {
            return Ok(None);
        }
        let mut f = Some(f);
        let mut result = None;
        let mut scratch = ControlledScratch {
            grow,
            settled,
            checkpoint,
        };
        let mut key = Vec::new();
        let _timer = ioacct::timer(Phase::VocabRead);
        self.decode_block_dispatch::<true, _, crate::error::Error>(
            id as usize / FC_BLOCK,
            1 << (id as usize % FC_BLOCK),
            &mut key,
            &mut scratch,
            &mut |_, _, k, _| {
                result = Some(f.take().expect("one selected vocabulary key")(
                    std::mem::take(k),
                )?);
                Ok(false)
            },
        )?;
        Ok(result)
    }

    pub(crate) fn find_with_controlled(
        &self,
        key: &[u8],
        mut reserve: impl FnMut(usize) -> crate::error::Result<()>,
        checkpoint: impl FnMut(&'static str) -> crate::error::Result<()>,
    ) -> crate::error::Result<std::result::Result<u64, u64>> {
        self.find_with_controlled_growth(key, |_, new| reserve(new), checkpoint)
    }

    pub(crate) fn find_with_controlled_growth(
        &self,
        key: &[u8],
        grow: impl FnMut(usize, usize) -> crate::error::Result<()>,
        checkpoint: impl FnMut(&'static str) -> crate::error::Result<()>,
    ) -> crate::error::Result<std::result::Result<u64, u64>> {
        self.find_with_controlled_capacity(key, grow, |_| Ok(()), checkpoint)
    }

    pub(crate) fn find_with_controlled_capacity<E: From<crate::error::Error>>(
        &self,
        key: &[u8],
        grow: impl FnMut(usize, usize) -> std::result::Result<(), E>,
        settled: impl FnMut(usize) -> std::result::Result<(), E>,
        checkpoint: impl FnMut(&'static str) -> std::result::Result<(), E>,
    ) -> std::result::Result<std::result::Result<u64, u64>, E> {
        let mut scratch = ControlledScratch {
            grow,
            settled,
            checkpoint,
        };
        let _timer = ioacct::timer(Phase::VocabRead);
        self.find_with::<true, _, E>(key, &mut scratch)
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
        let _ = self.get_sorted_with(
            ids,
            &mut Vec::new(),
            &mut Unlimited,
            |id, k| {
                f(id, k);
                Ok(())
            },
            read_ahead,
        );
    }

    /// Decode every key of block `b` into the reusable buffer `key`.
    #[cfg(test)]
    fn scan_block_with_scratch(
        &self,
        b: usize,
        key: &mut Vec<u8>,
        mut f: impl FnMut(usize, &[u8]) -> bool,
    ) {
        let mut cb = |i, _, k: &mut Vec<u8>, _: &mut Unlimited| Ok(f(i, k));
        let _ = self.decode_block_dispatch::<true, _, _>(b, ALL_KEYS, key, &mut Unlimited, &mut cb);
    }

    /// Ask the kernel to read ahead, asynchronously, the pages of the front-coded blocks
    /// that hold these ids (sorted ascending): first those of their offsets, then those
    /// of the blocks. Pages in memory are left as they are. A cold decode of scattered
    /// ids then finds its pages read by many requests at once, instead of one page fault
    /// at a time. Nearby pages are asked for in one range.
    pub fn prefetch_sorted(&self, ids: &[u64]) {
        let _ = self.prefetch_sorted_with(ids, &mut Unlimited);
    }

    pub(crate) fn prefetch_sorted_checked(
        &self,
        ids: &[u64],
        reserve: impl FnMut(usize) -> crate::error::Result<()>,
    ) -> crate::error::Result<()> {
        self.prefetch_sorted_with(ids, &mut CheckedHint(reserve))
    }

    fn prefetch_sorted_with<A: HintAdmit>(
        &self,
        ids: &[u64],
        admit: &mut A,
    ) -> std::result::Result<(), A::Error> {
        if !crate::index::io_hints()
            || !matches!((&self.data, &self.offsets), (Bytes::Map(_), Bytes::Map(_)))
        {
            return Ok(());
        }
        let blocks = if A::CHECKED {
            let count = ids.iter().take_while(|&&id| id < self.len).count();
            let bytes = count
                .checked_mul(std::mem::size_of::<usize>())
                .ok_or_else(|| admit.invalid_err("vocabulary hint size overflow"))?;
            admit.admit(bytes)?;
            let mut blocks = Vec::new();
            blocks
                .try_reserve_exact(count)
                .map_err(|_| admit.invalid_err("vocabulary hint allocation failed"))?;
            if blocks.capacity() != count {
                return Err(admit.invalid_err("vocabulary hint capacity exceeds admission"));
            }
            blocks.extend(
                ids.iter()
                    .take_while(|&&id| id < self.len)
                    .map(|&id| id as usize / FC_BLOCK),
            );
            blocks
        } else {
            ids.iter()
                .take_while(|&&id| id < self.len)
                .map(|&id| id as usize / FC_BLOCK)
                .collect()
        };
        self.prefetch_blocks(blocks);
        Ok(())
    }

    fn prefetch_blocks(&self, mut blocks: Vec<usize>) {
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
        let _ = self.prefetch_value_pages_with(ids, &mut Unlimited);
    }

    /// Whether [`Vocab::prefetch_value_pages_checked`] has any memory-mapped vocabulary
    /// block not yet hinted or any memory-mapped numeric-column segment covering `ids`.
    /// Callers that already checked [`crate::index::io_hints`] use this to skip scratch
    /// admission, allocation and sorting for in-memory vocabularies and warm blocks.
    pub(crate) fn has_unhinted_value_pages(&self, ids: impl Iterator<Item = u64>) -> bool {
        let vocab_mapped = matches!((&self.data, &self.offsets), (Bytes::Map(_), Bytes::Map(_)));
        let num = self.num.as_ref().filter(|n| n.is_mapped());
        if !vocab_mapped && num.is_none() {
            return false;
        }
        let mut last = usize::MAX;
        for id in ids {
            if id >= self.len {
                continue;
            }
            if num.is_some_and(|n| n.may_hold(id)) {
                return true;
            }
            if vocab_mapped {
                let b = id as usize / FC_BLOCK;
                if b != last {
                    last = b;
                    let asked = self
                        .prefetched
                        .get(b / 64)
                        .map_or(0, |w| w.load(AtomicOrdering::Relaxed));
                    if asked & (1u64 << (b % 64)) == 0 {
                        return true;
                    }
                }
            }
        }
        false
    }

    /// Checked version of [`Vocab::prefetch_value_pages`] that admits the sorted block-index
    /// vector through `reserve` before allocating it.
    pub(crate) fn prefetch_value_pages_checked(
        &self,
        ids: &[u64],
        reserve: impl FnMut(usize) -> crate::error::Result<()>,
    ) -> crate::error::Result<()> {
        self.prefetch_value_pages_with(ids, &mut CheckedHint(reserve))
    }

    fn prefetch_value_pages_with<A: HintAdmit<Error = crate::error::Error>>(
        &self,
        ids: &[u64],
        admit: &mut A,
    ) -> crate::error::Result<()> {
        self.prefetch_sorted_with(ids, admit)?;
        if let Some(num) = &self.num {
            num.advise_ids(ids);
        }
        Ok(())
    }

    /// Overlap the vocabulary reads of a medium-sized result decoded in row order.
    /// With a sparse index, use its in-memory offsets and hint each group once, as
    /// `find` does. Warm requests then avoid sorting their cells or repeating hints.
    #[cfg(test)]
    pub(crate) fn prefetch_terms(&self, ids: impl Iterator<Item = u64>) {
        let _ = self.prefetch_terms_with(ids, 0, &mut Unlimited);
    }

    /// An immutable result supplies a checked maximum count. Every actual vector
    /// is admitted before allocation and every hint bit changes only afterwards.
    pub(crate) fn prefetch_terms_checked(
        &self,
        ids: impl Iterator<Item = u64>,
        count: usize,
        reserve: impl FnMut(usize) -> crate::error::Result<()>,
    ) -> crate::error::Result<()> {
        self.prefetch_terms_with(ids, count, &mut CheckedHint(reserve))
    }

    fn prefetch_terms_with<A: HintAdmit<Error = crate::error::Error>>(
        &self,
        ids: impl Iterator<Item = u64>,
        count: usize,
        admit: &mut A,
    ) -> crate::error::Result<()> {
        if !crate::index::io_hints()
            || !matches!((&self.data, &self.offsets), (Bytes::Map(_), Bytes::Map(_)))
        {
            return Ok(());
        }
        let bytes = if A::CHECKED {
            let bytes = count
                .checked_mul(std::mem::size_of::<u64>())
                .ok_or_else(|| crate::error::Error::invalid("vocabulary hint size overflow"))?;
            admit.admit(bytes)?;
            bytes
        } else {
            0
        };
        let Some(sparse) = &self.sparse else {
            let mut all = if A::CHECKED {
                let mut all = Vec::new();
                all.try_reserve_exact(count).map_err(|_| {
                    crate::error::Error::invalid("vocabulary hint allocation failed")
                })?;
                if all.capacity() != count {
                    return Err(crate::error::Error::invalid(
                        "vocabulary hint capacity exceeds admission",
                    ));
                }
                for id in ids {
                    if all.len() == count {
                        return Err(crate::error::Error::invalid(
                            "vocabulary hint count exceeds admission",
                        ));
                    }
                    all.push(id);
                }
                all
            } else {
                ids.collect()
            };
            all.sort_unstable();
            all.dedup();
            return if A::CHECKED {
                self.prefetch_sorted_checked(&all, |need| {
                    admit.admit(bytes.checked_add(need).ok_or_else(|| {
                        crate::error::Error::invalid("vocabulary hint overlap overflow")
                    })?)
                })
            } else {
                self.prefetch_sorted(&all);
                Ok(())
            };
        };
        let mut groups = Vec::new();
        if A::CHECKED {
            groups
                .try_reserve_exact(count)
                .map_err(|_| crate::error::Error::invalid("vocabulary hint allocation failed"))?;
            if groups.capacity() != count {
                return Err(crate::error::Error::invalid(
                    "vocabulary hint capacity exceeds admission",
                ));
            }
        }
        let mut last = usize::MAX;
        for id in ids.filter(|&id| id < self.len) {
            let g = id as usize / (FC_BLOCK * IDX_BLOCKS);
            if g != last {
                // No unadmitted push can follow a global hint-state mutation.
                if A::CHECKED && groups.len() == count {
                    return Err(crate::error::Error::invalid(
                        "vocabulary hint count exceeds admission",
                    ));
                }
                if sparse.first_hint(g) {
                    groups.push(g);
                }
            }
            last = g;
        }
        groups.sort_unstable();
        self.prefetch_groups(&groups);
        Ok(())
    }

    fn prefetch_groups(&self, groups: &[usize]) {
        let s = self.sparse.as_ref().expect("sparse hint groups");
        for &g in groups {
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
            self.find_with::<true, _, _>(key, &mut Unlimited)
                .unwrap_or_else(|e| match e {})
        } else {
            self.find_with::<false, _, _>(key, &mut Unlimited)
                .unwrap_or_else(|e| match e {})
        }
    }

    /// Unified binary search over front-coded blocks, generic over [`BlockScratch`].
    /// Under [`Unlimited`], narrows with the sparse index and uses the zero-rebuild
    /// LCP in-block scan; under checked or polling controls, admits reconstruction
    /// scratch and compares cooperatively.
    #[inline(always)]
    fn find_with<const P: bool, S: BlockScratch<E>, E>(
        &self,
        key: &[u8],
        scratch: &mut S,
    ) -> std::result::Result<std::result::Result<u64, u64>, E> {
        let nb = self.num_blocks();
        if nb == 0 {
            if S::POLLS {
                scratch.checkpoint("completed vocabulary search")?;
            }
            return Ok(Err(0));
        }
        // last block whose first key <= key
        let (mut lo, mut hi) = (0usize, nb);
        if !S::POLLS
            && let Some(s) = &self.sparse
        {
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
            if compare_bytes_with::<S, E>(self.block_first_at::<P>(mid), key, scratch)?.is_le() {
                lo = mid + 1;
            } else {
                hi = mid;
            }
        }
        if lo == 0 {
            if S::POLLS {
                scratch.checkpoint("completed vocabulary search")?;
            }
            return Ok(Err(0));
        }
        let b = lo - 1;
        if !S::CHECKED {
            return Ok(if self.fsst.is_some() {
                self.find_in_block::<P, true>(b, key)
            } else {
                self.find_in_block::<P, false>(b, key)
            });
        }
        let mut result = Err(((b + 1) * FC_BLOCK).min(self.len as usize) as u64);
        let mut buf = Vec::new();
        self.decode_block_dispatch::<P, S, E>(
            b,
            ALL_KEYS,
            &mut buf,
            scratch,
            &mut |at, _, actual, scratch| {
                Ok(match compare_bytes_with::<S, E>(actual, key, scratch)? {
                    std::cmp::Ordering::Less => true,
                    std::cmp::Ordering::Equal => {
                        result = Ok((b * FC_BLOCK + at) as u64);
                        false
                    }
                    std::cmp::Ordering::Greater => {
                        result = Err((b * FC_BLOCK + at) as u64);
                        false
                    }
                })
            },
        )?;
        Ok(result)
    }

    /// Find `probe` in block `b`, whose first key is not above it: `Ok(id)` when it is
    /// there and otherwise `Err` of the id of the first key above it, or of the next
    /// block's first id.
    ///
    /// The keys are compared without being rebuilt. With `c` the length of the prefix
    /// that the probe shares with the key before, a key that shares fewer than `c` bytes
    /// with that key is above the probe, since its byte at the shared length is above the
    /// key's and so above the probe's, and a key that shares more is below it with the
    /// same `c`. Only a key that shares exactly `c` bytes has its suffix read and
    /// compared, and only that suffix is decoded. This relies on the writer storing the
    /// longest shared prefix, which [`VocabWriter`] does and `sparkles check` verifies.
    #[inline(always)]
    fn find_in_block<const P: bool, const F: bool>(
        &self,
        b: usize,
        probe: &[u8],
    ) -> std::result::Result<u64, u64> {
        let data = self.data.as_slice();
        let mut pos = self.block_offset_at::<P>(b);
        let start = b * FC_BLOCK;
        let n = FC_BLOCK.min((self.len as usize) - start);
        if P {
            ioacct::vocab_blocks_scanned(1);
            self.touch_block(b);
        }
        let lcp = |a: &[u8], b: &[u8]| a.iter().zip(b).take_while(|(x, y)| x == y).count();
        let e = self.entry::<F>(data, &mut pos, true);
        let first = &data[e.start..e.start + e.len];
        match first.cmp(probe) {
            std::cmp::Ordering::Less => {}
            std::cmp::Ordering::Equal => return Ok(start as u64),
            std::cmp::Ordering::Greater => return Err(start as u64),
        }
        let mut c = lcp(first, probe);
        let mut scratch = Vec::new();
        for i in 1..n {
            let id = (start + i) as u64;
            let e = self.entry::<F>(data, &mut pos, false);
            if e.shared < c {
                return Err(id);
            }
            if e.shared > c {
                continue;
            }
            let suffix: &[u8] = if F && e.coded {
                scratch.clear();
                self.push_suffix::<F>(data, e, id, usize::MAX, &mut scratch, false);
                &scratch
            } else {
                &data[e.start..e.start + e.len]
            };
            let rest = &probe[c..];
            match suffix.cmp(rest) {
                std::cmp::Ordering::Less => c += lcp(suffix, rest),
                std::cmp::Ordering::Equal => return Ok(id),
                std::cmp::Ordering::Greater => return Err(id),
            }
        }
        Err(((b + 1) * FC_BLOCK).min(self.len as usize) as u64)
    }

    pub(crate) fn get_with_fallible<T>(
        &self,
        id: u64,
        reserve: impl FnMut(usize) -> crate::error::Result<()>,
        mut f: impl FnMut(&[u8]) -> T,
    ) -> crate::error::Result<Option<T>> {
        if id >= self.len {
            return Ok(None);
        }
        let b = id as usize / FC_BLOCK;
        self.ahead_for(&[id]).at(self, b);
        let mut result = None;
        let mut scratch = ExactScratch { reserve };
        let mut key = Vec::new();
        self.decode_block_dispatch::<false, _, crate::error::Error>(
            b,
            1 << (id as usize % FC_BLOCK),
            &mut key,
            &mut scratch,
            &mut |_, _, k, _| {
                result = Some(f(k));
                Ok(false)
            },
        )?;
        Ok(result)
    }

    pub(crate) fn find_with_checked(
        &self,
        key: &[u8],
        reserve: impl FnMut(usize) -> crate::error::Result<()>,
    ) -> crate::error::Result<std::result::Result<u64, u64>> {
        let mut scratch = ExactScratch { reserve };
        self.find_with::<false, _, crate::error::Error>(key, &mut scratch)
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
/// Bytes of suffixes an FSST-coded vocabulary collects at most before it codes them, at
/// the end of a block, so that long keys do not hold [`FSST_BATCH`] of them.
const FSST_BATCH_BYTES: usize = 64 << 20;

/// The keys of an FSST-coded vocabulary not written yet (see [`VocabWriter::flush_fsst`]).
struct FsstWriter {
    enc: fsst::Encoder,
    /// the first id of the second kind of key, once pushed
    split: Option<u64>,
    /// the suffixes of the keys, one after another
    bytes: Vec<u8>,
    /// per key, its shared-prefix length and the end of its suffix in `bytes`
    ents: Vec<(u32, u32)>,
    /// the batch is coded once `bytes` holds this many at the end of a block
    batch_bytes: usize,
    /// the most bytes of suffixes held at once, for tests
    peak_bytes: usize,
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
            batch_bytes: FSST_BATCH_BYTES,
            peak_bytes: 0,
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
        f.peak_bytes = f.peak_bytes.max(f.bytes.len());
        let full = f.ents.len() == FSST_BATCH
            || (f.bytes.len() >= f.batch_bytes && f.ents.len().is_multiple_of(FC_BLOCK));
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
/// `offsets` and `data`. The number of its entries when it matches, and otherwise what
/// is wrong. Index format 5 requires the file, so a missing one, one that does not read
/// as an index of this vocabulary and one that names other offsets or keys are all
/// errors, and the open refuses the first two.
pub(crate) fn verify_sparse_index(
    dir: &Path,
    offsets: &[usize],
    data: &[u8],
) -> std::result::Result<usize, String> {
    let path = dir.join("vocab.idx");
    let buf = std::fs::read(&path).map_err(|e| missing_index(&path, &e).to_string())?;
    let Some(s) = Sparse::decode(&buf, offsets.len(), data.len() as u64) else {
        return Err("not a sparse index of this vocabulary".into());
    };
    for g in 0..s.len() {
        let b = g * IDX_BLOCKS;
        let mut pos = offsets[b];
        let first = crate::index::read_varint_checked(data, &mut pos)
            .and_then(|_| crate::index::read_varint_checked(data, &mut pos))
            .and_then(|len| data.get(pos..pos + len as usize));
        if s.offsets[g] != offsets[b] as u64 || first != Some(s.key(g)) {
            return Err(format!(
                "entry {g} does not name the offset and first key of block {b}"
            ));
        }
    }
    Ok(s.len())
}

/// Require `vocab.idx` of the vocabulary in `dir` to exist when a process opens the
/// vocabulary without using it: the switch leaves the index out of memory, not off the
/// disk.
fn require_index_file(dir: &Path) -> crate::error::Result<()> {
    let path = dir.join("vocab.idx");
    std::fs::metadata(&path)
        .map(|_| ())
        .map_err(|e| missing_index(&path, &e))
}

/// The refusal of a generation whose `vocab.idx` cannot be read: index format 5
/// requires the file.
fn missing_index(path: &Path, error: &std::io::Error) -> crate::Error {
    crate::Error::Corrupt(format!(
        "{}: {error}; index format {} requires the sparse vocabulary index",
        path.display(),
        crate::builder::FORMAT_VERSION
    ))
}

/// The refusal of a generation whose `vocab.idx` does not describe its vocabulary.
fn mismatched_index(path: &Path) -> crate::Error {
    crate::Error::Corrupt(format!(
        "{} is not the sparse index of this vocabulary",
        path.display()
    ))
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
    write_numeric_column(dir, &Vocab::open(dir)?)
}

fn write_numeric_column(dir: &Path, v: &Vocab) -> Result<u64> {
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
    if n == 0 {
        match std::fs::remove_file(dir.join(numeric::FILE)) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(e.into()),
        }
    }
    crate::store::sync_dir(dir)?;
    Ok(n)
}

/// An append-only dictionary; used for the persisted delta vocabulary and for per-query
/// local vocabularies.
#[derive(Clone, Default)]
pub struct AppendVocab {
    keys: Vec<Arc<[u8]>>,
    map: FxHashMap<Arc<[u8]>, u64>,
    /// Cold readers use a fixed-fingerprint index rather than hashing payloads
    /// during native HashMap growth. Ordinary live dictionaries keep `map`.
    cold: Option<cold::Index>,
    /// total key length
    key_bytes: usize,
}

/// The longest key a controlled delta-vocabulary lookup hashes in one probe.
pub(crate) const PREPARED_PROBE_BYTES: usize = 1 << 20;

fn compare_prepared_bytes<E>(
    left: &[u8],
    right: &[u8],
    checkpoint: &mut impl FnMut(&'static str) -> std::result::Result<(), E>,
) -> std::result::Result<std::cmp::Ordering, E> {
    let common = left.len().min(right.len());
    for (left, right) in left[..common]
        .chunks(1024)
        .zip(right[..common].chunks(1024))
    {
        checkpoint("comparing vocabulary bytes")?;
        let order = left.cmp(right);
        if !order.is_eq() {
            return Ok(order);
        }
    }
    checkpoint("completed vocabulary comparison")?;
    Ok(left.len().cmp(&right.len()))
}

impl AppendVocab {
    pub fn len(&self) -> u64 {
        self.keys.len() as u64
    }

    /// Bytes allocated by cloning a dictionary that has grown only by appending.
    /// Key bodies are shared by the cloned Arcs.
    pub(crate) fn append_only_clone_bytes(&self) -> u64 {
        let vector = self
            .len()
            .saturating_mul(std::mem::size_of::<Arc<[u8]>>() as u64);
        let capacity = self.map.capacity() as u64;
        let index = if let Some(cold) = &self.cold {
            cold.bytes()
        } else if capacity == 0 {
            0
        } else {
            // Without deletions, the pinned std HashMap capacity is at least
            // half the backing bucket count and strictly below it. Its clone
            // keeps that power-of-two bucket count. Allow one control byte per
            // bucket and slack for the alignment and trailing control group.
            let buckets = capacity
                .saturating_add(1)
                .checked_next_power_of_two()
                .unwrap_or(u64::MAX);
            let slot = std::mem::size_of::<(Arc<[u8]>, u64)>() as u64 + 1;
            buckets.saturating_mul(slot).saturating_add(64)
        };
        vector.saturating_add(index)
    }

    pub fn is_empty(&self) -> bool {
        self.keys.is_empty()
    }
    pub fn get(&self, id: u64) -> Option<&[u8]> {
        self.keys.get(id as usize).map(|k| &**k)
    }
    pub fn find(&self, key: &[u8]) -> Option<u64> {
        if let Some(index) = &self.cold {
            return index.find(&self.keys, key);
        }
        self.map.get(key).copied()
    }
    /// Controlled lookup of a key among the first `limit` entries. A key of up to
    /// [`PREPARED_PROBE_BYTES`] is one probe of the index, whose hashing and comparison
    /// take no more than a fraction of a millisecond. A longer key is compared with
    /// each entry in polled pieces instead, since hashing it could not be interrupted.
    pub(crate) fn find_prepared(
        &self,
        key: &[u8],
        limit: u64,
        checkpoint: impl FnMut(&'static str) -> crate::error::Result<()>,
    ) -> crate::error::Result<Option<u64>> {
        self.find_prepared_with(key, limit, checkpoint)
    }

    pub(crate) fn find_prepared_with<E>(
        &self,
        key: &[u8],
        limit: u64,
        mut checkpoint: impl FnMut(&'static str) -> std::result::Result<(), E>,
    ) -> std::result::Result<Option<u64>, E> {
        checkpoint("searching delta vocabulary")?;
        if key.len() <= PREPARED_PROBE_BYTES {
            let found = self.find(key).filter(|&id| id < limit);
            checkpoint("completed delta vocabulary search")?;
            return Ok(found);
        }
        for (id, candidate) in self
            .keys
            .iter()
            .take(limit.min(self.len()) as usize)
            .enumerate()
        {
            checkpoint("searching delta vocabulary")?;
            if candidate.len() == key.len()
                && compare_prepared_bytes(candidate, key, &mut checkpoint)?.is_eq()
            {
                return Ok(Some(id as u64));
            }
        }
        checkpoint("completed delta vocabulary search")?;
        Ok(None)
    }

    pub fn insert(&mut self, key: &[u8]) -> (u64, bool) {
        if let Some(index) = &mut self.cold {
            let hash = cold::fingerprint(key);
            if let Some(id) = index.find_hashed(&self.keys, key, hash) {
                return (id, false);
            }
            let owned: Arc<[u8]> = key.into();
            let id = self.keys.len() as u64;
            self.key_bytes += owned.len();
            index.insert(hash, id);
            self.keys.push(owned);
            return (id, true);
        }
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
        if let Some(index) = &mut self.cold {
            let hash = cold::fingerprint(key);
            let existing = index.find_hashed(&self.keys, key, hash);
            let owned: Arc<[u8]> = key.into();
            let id = self.keys.len() as u64;
            self.key_bytes += owned.len();
            if existing.is_none() {
                index.insert(hash, id);
            }
            self.keys.push(owned);
            return;
        }
        let k: Arc<[u8]> = key.into();
        let i = self.keys.len() as u64;
        self.key_bytes += k.len();
        self.keys.push(k.clone());
        self.map.entry(k).or_insert(i);
    }

    /// Insert into a dictionary under `control`. Under [`Unlimited`], inserts into the
    /// ordinary map directly; under an accounting carrier, constructs a freshly admitted
    /// cold dictionary. Positional layers keep duplicate entries, while ordinary
    /// dictionaries reuse the first key.
    pub(crate) fn push_with<C: BudgetControl>(
        &mut self,
        key: &[u8],
        positional: bool,
        control: &mut C,
    ) -> Result<(u64, bool)> {
        if !C::ACCOUNTS {
            return if positional {
                let id = self.keys.len() as u64;
                self.push_dup(key);
                Ok((id, true))
            } else {
                Ok(self.insert(key))
            };
        }
        control.check(0)?;
        if self.cold.is_none() {
            if !self.keys.is_empty()
                || !self.map.is_empty()
                || self.keys.capacity() != 0
                || self.map.capacity() != 0
            {
                return Err(crate::Error::invalid(
                    "controlled dictionary construction needs a fresh dictionary",
                ));
            }
            self.cold = Some(cold::Index::default());
        }
        let (hash, existing) = self
            .cold
            .as_ref()
            .unwrap()
            .find_with(&self.keys, key, control)?;
        if !positional && let Some(id) = existing {
            return Ok((id, false));
        }
        let need = self
            .keys
            .len()
            .checked_add(1)
            .ok_or_else(|| control.exhausted())?;
        control.reserve(&mut self.keys, need)?;
        let bytes = (key.len() as u64)
            .checked_add(64)
            .ok_or_else(|| control.exhausted())?;
        control.charge(bytes)?;
        let owned = cold::copy_with(key, control)?;
        let id = self.keys.len() as u64;
        if existing.is_none() {
            self.cold.as_mut().unwrap().insert_with(hash, id, control)?;
        }
        self.key_bytes += owned.len();
        self.keys.push(owned);
        control.check(0)?;
        Ok((id, true))
    }

    #[cfg(test)]
    pub(crate) fn push_controlled(
        &mut self,
        key: &[u8],
        positional: bool,
        budget: &mut crate::store::cold::ColdBudget<'_>,
    ) -> Result<(u64, bool)> {
        self.push_with(key, positional, budget)
    }

    pub(crate) fn cold_bytes(&self) -> u64 {
        (self.keys.capacity() as u64)
            .saturating_mul(std::mem::size_of::<Arc<[u8]>>() as u64)
            .saturating_add(self.cold.as_ref().map_or_else(
                || {
                    (self.map.capacity() as u64)
                        .saturating_mul(128)
                        .saturating_add(u64::from(!self.map.is_empty()) * 1024)
                },
                cold::Index::bytes,
            ))
            .saturating_add(self.key_bytes as u64)
            .saturating_add((self.keys.len() as u64).saturating_mul(64))
    }

    /// Keep the first `n` entries only.
    pub fn truncate(&mut self, n: u64) {
        while self.keys.len() as u64 > n {
            let k = self.keys.pop().expect("longer than n");
            self.key_bytes -= k.len();
            let i = self.keys.len() as u64;
            if self.cold.is_none() && self.map.get(&k) == Some(&i) {
                self.map.remove(&k);
            }
        }
        if let Some(index) = &mut self.cold {
            index.truncate(n);
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
        assert_eq!(vocab.block_first_at::<true>(1), keys[FC_BLOCK]);
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
        for fsst in [false, true] {
            let dir = tempfile::tempdir().unwrap();
            write_codec(dir.path(), &keys, fsst);
            let v = Vocab::open(dir.path()).unwrap();
            let mut called = false;
            let blocked = v.get_with_checked(
                0,
                |need| if need > 1024 { Err(need) } else { Ok(()) },
                |_| called = true,
            );
            assert!(blocked.unwrap_err() > 32 << 10);
            assert!(!called);
            // Either codec decodes only the byte of the long key that the short key
            // takes (items 108 and 948), so the long key costs nothing, and the buffer
            // takes the unchecked decoder's first size.
            let mut most = 0;
            let got = v.get_with_checked(
                1,
                |need| {
                    most = most.max(need);
                    if need > 1024 { Err(need) } else { Ok(()) }
                },
                |k| k.to_vec(),
            );
            assert_eq!(got, Ok(Some(keys[1].clone())), "fsst {fsst}");
            assert_eq!(most, v.scratch_capacity(), "fsst {fsst}");
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
    }

    /// A decode of a short key reserves about that key's length, not the length of a
    /// long key before it in its block, on every decoding path and for both codecs
    /// (item 948). The long keys share one byte with the short keys after them, so
    /// that byte is all a decode of a short key needs of them.
    #[test]
    fn short_key_decodes_reserve_their_own_length() {
        const LONG: usize = 64 << 10;
        const SMALL: usize = 256;
        let keys = [
            format!("\"a{}", "x".repeat(LONG)).into_bytes(),
            b"\"b".to_vec(),
            format!("\"c{}", "y".repeat(LONG)).into_bytes(),
            b"\"d".to_vec(),
        ];
        for fsst in [false, true] {
            let dir = tempfile::tempdir().unwrap();
            write_codec(dir.path(), &keys, fsst);
            let v = Vocab::open(dir.path()).unwrap();
            assert_eq!(v.fsst.is_some(), fsst);
            let most = std::cell::Cell::new(0usize);
            let small = |need: usize| {
                most.set(most.get().max(need));
                if need > SMALL { Err(need) } else { Ok(()) }
            };
            for id in [1u64, 3] {
                let want = Some(keys[id as usize].clone());
                assert_eq!(
                    v.get_with_checked(id, small, |k| k.to_vec()),
                    Ok(want.clone()),
                    "fsst {fsst}: checked {id}"
                );
                let mut key = Vec::new();
                assert_eq!(
                    v.get_with_checked_in(id, &mut key, small, |k| k.to_vec()),
                    Ok(want.clone()),
                    "fsst {fsst}: checked in {id}"
                );
                assert!(key.capacity() <= SMALL, "fsst {fsst}: {}", key.capacity());
                assert_eq!(
                    v.get_with_fallible(
                        id,
                        |n| small(n).map_err(|_| crate::Error::Cancelled),
                        |k| k.to_vec()
                    )
                    .unwrap(),
                    want.clone(),
                    "fsst {fsst}: fallible {id}"
                );
                assert_eq!(
                    v.get_with_controlled(
                        id,
                        |n| small(n).map_err(|_| crate::Error::Cancelled),
                        |_| Ok(()),
                        |k| k.to_vec()
                    )
                    .unwrap(),
                    want.clone(),
                    "fsst {fsst}: controlled {id}"
                );
                // the unchecked decoder grows its scratch only as far as the short key
                for profile in [false, true] {
                    let mut key = Vec::new();
                    let mut got = None;
                    let b = id as usize / FC_BLOCK;
                    let wanted = 1 << (id as usize % FC_BLOCK);
                    let mut f = |_, _: usize, k: &mut Vec<u8>, _: &mut Unlimited| {
                        got = Some(k.to_vec());
                        Ok::<bool, std::convert::Infallible>(false)
                    };
                    if profile {
                        let _ = v.decode_block_dispatch::<true, _, _>(
                            b,
                            wanted,
                            &mut key,
                            &mut Unlimited,
                            &mut f,
                        );
                    } else {
                        let _ = v.decode_block_dispatch::<false, _, _>(
                            b,
                            wanted,
                            &mut key,
                            &mut Unlimited,
                            &mut f,
                        );
                    }
                    assert_eq!(got, want, "fsst {fsst}: unchecked {id}");
                    assert!(
                        key.capacity() <= SMALL,
                        "fsst {fsst}: unchecked scratch {}",
                        key.capacity()
                    );
                }
            }
            // both short keys in one checked pass skip the long key between them
            let mut got = Vec::new();
            v.get_sorted_checked(&[1, 3], small, |id, k| {
                got.push((id, k.to_vec()));
                Ok(())
            })
            .unwrap();
            assert_eq!(got, vec![(1, keys[1].clone()), (3, keys[3].clone())]);
            assert!(most.get() <= SMALL, "fsst {fsst}: reserved {}", most.get());
            // a long key itself is still reserved whole
            assert!(v.get_with_checked(2, small, |_| ()).unwrap_err() > LONG);
        }
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

    /// The checked rebuild works in a buffer taken from its caller. The caller gets the
    /// same allocation back after a reservation error, a callback error or success, and
    /// can reuse it, for both codecs and with and without kept FSST room.
    #[test]
    fn checked_rebuild_returns_the_callers_buffer_on_every_path() {
        let mut keys: Vec<Vec<u8>> = (0..3 * FC_BLOCK)
            .map(|i| format!("<urn:buffer:{}:{i:04}", "long".repeat(i % 9)).into_bytes())
            .collect();
        keys.sort();
        for fsst in [false, true] {
            let d = tempfile::tempdir().unwrap();
            write_codec(d.path(), &keys, fsst);
            let v = Vocab::open(d.path()).unwrap();
            assert_eq!(v.is_fsst(), fsst);
            let id = FC_BLOCK as u64 + 20;
            let want = &keys[id as usize];
            for room in [false, true] {
                let run = |key: &mut Vec<u8>, fail: Option<&'static str>| {
                    let reserve = |_| {
                        if fail == Some("reserve") {
                            Err("reserve")
                        } else {
                            Ok(())
                        }
                    };
                    let f = |_, k: &[u8]| {
                        assert_eq!(k, want);
                        if fail == Some("callback") {
                            Err("callback")
                        } else {
                            Ok(())
                        }
                    };
                    if room {
                        let mut scratch = CheckedScratch::<_, true> { reserve };
                        v.get_sorted_with(&[id], key, &mut scratch, f, true)
                    } else {
                        let mut scratch = CheckedScratch::<_, false> { reserve };
                        v.get_sorted_with(&[id], key, &mut scratch, f, true)
                    }
                };
                // A buffer with room for every key needs no reservation, so a failing
                // reservation is never reached and the callback's error is returned.
                let mut key = Vec::with_capacity(4096);
                let (ptr, cap) = (key.as_ptr(), key.capacity());
                for fail in [None, Some("callback"), Some("reserve")] {
                    let expected = match fail {
                        Some("callback") => Err("callback"),
                        _ => Ok(()),
                    };
                    assert_eq!(run(&mut key, fail), expected);
                    assert_eq!((key.as_ptr(), key.capacity()), (ptr, cap));
                }
                // A small buffer must reserve. The refusal still returns the same
                // allocation, and the buffer then grows and rebuilds the key.
                let mut key = Vec::with_capacity(4);
                let (ptr, cap) = (key.as_ptr(), key.capacity());
                assert_eq!(run(&mut key, Some("reserve")), Err("reserve"));
                assert_eq!((key.as_ptr(), key.capacity()), (ptr, cap));
                assert_eq!(run(&mut key, Some("callback")), Err("callback"));
                let (ptr, cap) = (key.as_ptr(), key.capacity());
                assert!(cap >= want.len());
                assert_eq!(run(&mut key, None), Ok(()));
                assert_eq!((key.as_ptr(), key.capacity()), (ptr, cap));
            }
        }
    }

    /// A writer reuses one rebuild buffer for every cell. Each growth of it is admitted
    /// first, so its capacity never exceeds the largest reservation, and a step that
    /// fits the buffer reserves nothing. Keys of varied lengths and shared prefixes are
    /// decoded in several orders, for both codecs and with and without kept FSST room.
    #[test]
    fn reused_checked_buffer_grows_only_after_a_reservation() {
        let mut keys: Vec<Vec<u8>> = (0..6 * FC_BLOCK)
            .map(|i| {
                let tail = "y".repeat((i * 37) % 300);
                format!("<urn:reuse:{}:{i:04}{tail}", "seg/".repeat(i % 13)).into_bytes()
            })
            .collect();
        keys.sort();
        let n = keys.len() as u64;
        let orders: [Vec<u64>; 3] = [
            (0..n).collect(),
            (0..n).rev().collect(),
            (0..n).map(|i| (i * 7) % n).collect(),
        ];
        for fsst in [false, true] {
            let d = tempfile::tempdir().unwrap();
            write_codec(d.path(), &keys, fsst);
            let v = Vocab::open(d.path()).unwrap();
            assert_eq!(v.is_fsst(), fsst);
            for room in [false, true] {
                for order in &orders {
                    let mut key = Vec::new();
                    let mut admitted = 0;
                    for &id in order {
                        let held = key.capacity();
                        // A refused growth leaves the buffer as it was, so the
                        // reservation comes before the allocation.
                        let refused = if room {
                            let mut scratch = CheckedScratch::<_, true> {
                                reserve: |_| Err(()),
                            };
                            v.get_sorted_with(&[id], &mut key, &mut scratch, |_, _| Ok(()), true)
                        } else {
                            let mut scratch = CheckedScratch::<_, false> {
                                reserve: |_| Err(()),
                            };
                            v.get_sorted_with(&[id], &mut key, &mut scratch, |_, _| Ok(()), true)
                        };
                        assert_eq!(key.capacity(), held, "growth before admission at {id}");
                        let mut reserved = false;
                        // Each reservation grows the buffer to exactly what it admits, so
                        // a later one in the same decode must exceed it.
                        let mut fits = held;
                        let reserve = |need: usize| {
                            assert!(need > fits, "{need} fits the held {fits} bytes");
                            fits = need;
                            admitted = admitted.max(need);
                            reserved = true;
                            Ok::<_, ()>(())
                        };
                        let mut got = None;
                        let f = |_, k: &[u8]| {
                            got = Some(k.to_vec());
                            Ok(())
                        };
                        if room {
                            let mut scratch = CheckedScratch::<_, true> { reserve };
                            v.get_sorted_with(&[id], &mut key, &mut scratch, f, true)
                        } else {
                            let mut scratch = CheckedScratch::<_, false> { reserve };
                            v.get_sorted_with(&[id], &mut key, &mut scratch, f, true)
                        }
                        .unwrap();
                        assert_eq!(got.as_ref(), Some(&keys[id as usize]));
                        assert!(key.capacity() <= admitted, "unadmitted growth at {id}");
                        if !reserved {
                            assert_eq!(key.capacity(), held);
                        }
                        assert_eq!(refused.is_err(), reserved, "refusal at {id}");
                    }
                }
            }
        }
    }

    #[test]
    fn controlled_vocabulary_open_matches_both_codecs_and_type_boundaries() {
        use crate::{
            history::{HistoryOptions, ReadLimit},
            store::cold::ColdBudget,
        };
        let options = HistoryOptions::default();
        let mut keys = mixed_keys(5000);
        keys.extend([b"(".to_vec(), b"(urn:triple".to_vec()]);
        keys.sort();
        keys.dedup();
        for fsst in [false, true] {
            let directory = tempfile::tempdir().unwrap();
            write_codec(directory.path(), &keys, fsst);
            let normal = Vocab::open_codec(directory.path(), fsst).unwrap();
            let mut budget = ColdBudget::new(ReadLimit {
                options: &options,
                memory: Some(16 << 20),
                held: 0,
            })
            .unwrap();
            let loaded = Vocab::open_controlled(directory.path(), fsst, &mut budget).unwrap();
            assert_eq!(loaded.value.len(), normal.len());
            assert_eq!(loaded.value.first_iri(), normal.first_iri());
            assert_eq!(loaded.value.first_triple(), normal.first_triple());
            assert_eq!(budget.used(), loaded.charge);
            for (index, key) in keys.iter().enumerate().step_by(127) {
                assert_eq!(loaded.value.get(index as u64).as_ref(), Some(key));
                assert_eq!(loaded.value.find(key), normal.find(key));
            }
            drop(loaded.value);
            budget.release(loaded.charge);
            assert_eq!(budget.used(), 0);
        }
    }

    #[test]
    fn controlled_type_boundaries_do_not_reconstruct_giant_mapped_keys() {
        use crate::{
            history::{HistoryOptions, ReadLimit},
            store::cold::ColdBudget,
        };
        let options = HistoryOptions::default();
        let mut keys = vec![
            format!("{}{}", '"', "literal".repeat(200_000)).into_bytes(),
            b"(urn:triple".to_vec(),
            format!("<urn:{}", "iri".repeat(400_000)).into_bytes(),
        ];
        keys.sort();
        for fsst in [false, true] {
            let directory = tempfile::tempdir().unwrap();
            write_codec(directory.path(), &keys, fsst);
            let mut budget = ColdBudget::new(ReadLimit {
                options: &options,
                memory: Some(64 << 10),
                held: 0,
            })
            .unwrap();
            let loaded = Vocab::open_controlled(directory.path(), fsst, &mut budget).unwrap();
            assert_eq!(loaded.value.first_triple(), 1);
            assert_eq!(loaded.value.first_iri(), 2);
            assert!(loaded.value.sparse.is_none());
            assert!(loaded.charge < 64 << 10);
            assert_eq!(budget.used(), loaded.charge);
        }
    }

    #[test]
    fn controlled_sparse_keys_cancel_inside_copy_and_lexicographic_comparison() {
        use crate::{
            Error,
            history::{HistoryOptions, ReadLimit},
            store::cold::ColdBudget,
        };
        use std::{cell::Cell, sync::atomic::AtomicBool};
        const CHUNK: usize = 64 << 10;
        let mut first = vec![b'a'; CHUNK * 4 + 17];
        *first.last_mut().unwrap() = b'b';
        let mut second = first.clone();
        *second.last_mut().unwrap() = b'c';
        let wire = Sparse::encode(&[(0, first), (8, second)]);
        for stage in ["sparse copy", "sparse compare"] {
            let cancel = Arc::new(AtomicBool::new(false));
            let options = HistoryOptions {
                cancel: Some(cancel.clone()),
                deadline: None,
            };
            let reached = Cell::new(false);
            let observer = |current: &str, at: usize| {
                if current == stage && (CHUNK..CHUNK * 4).contains(&at) {
                    reached.set(true);
                    cancel.store(true, AtomicOrdering::Relaxed);
                }
            };
            let mut budget = ColdBudget::new(ReadLimit {
                options: &options,
                memory: Some(16 << 20),
                held: 0,
            })
            .unwrap();
            budget.observe_keys(&observer);
            let result = Sparse::decode_with(&wire, IDX_BLOCKS + 1, 16, &mut budget);
            assert!(matches!(result, Err(Error::Cancelled)), "stage={stage}");
            assert!(
                reached.get(),
                "cancellation happened before processing a sparse key"
            );
        }
    }

    /// Random subsets of a block decode to the keys asked for, whichever keys before
    /// them are skipped or decoded in part (item 108), with both codecs and every
    /// decoder, including the checked ones, whose buffer never outgrows what they
    /// reserved.
    #[test]
    fn subsets_of_blocks_decode_like_the_keys() {
        let mut keys = mixed_keys(400);
        // keys whose shared prefixes rise and fall within a block, and long ones
        for i in 0..300 {
            keys.push(format!("\"{}{}", "ab".repeat(i % 7), "z".repeat(i % 40)).into_bytes());
            keys.push(format!("<p/{}/{i}", "q".repeat(i % 300)).into_bytes());
        }
        keys.sort();
        keys.dedup();
        let mut x = 0x9E37_79B9_7F4A_7C15u64;
        let mut rand = move || {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            x
        };
        for fsst in [false, true] {
            let d = tempfile::tempdir().unwrap();
            write_codec(d.path(), &keys, fsst);
            let v = Vocab::open(d.path()).unwrap();
            assert_eq!(v.is_fsst(), fsst);
            for _ in 0..400 {
                let b = rand() as usize % keys.len().div_ceil(FC_BLOCK);
                let mask = rand() as u32;
                let ids: Vec<u64> = (0..FC_BLOCK)
                    .filter(|i| mask >> i & 1 == 1)
                    .map(|i| (b * FC_BLOCK + i) as u64)
                    .filter(|&id| id < keys.len() as u64)
                    .collect();
                let want: Vec<(u64, Vec<u8>)> =
                    ids.iter().map(|&i| (i, keys[i as usize].clone())).collect();
                let mut got = Vec::new();
                v.get_sorted(&ids, |i, k| got.push((i, k.to_vec())));
                assert_eq!(got, want, "fsst {fsst} block {b} mask {mask:x}");
                let mut got = Vec::new();
                let cap = std::cell::Cell::new(0usize);
                v.get_sorted_checked::<()>(
                    &ids,
                    |n| {
                        cap.set(cap.get().max(n));
                        Ok(())
                    },
                    |i, k| {
                        assert!(k.len() <= cap.get());
                        got.push((i, k.to_vec()));
                        Ok(())
                    },
                )
                .unwrap();
                assert_eq!(got, want, "checked, fsst {fsst} block {b} mask {mask:x}");
                for &(i, ref k) in &want {
                    assert_eq!(v.get(i).as_ref(), Some(k));
                    assert_eq!(
                        v.get_with_fallible(i, |_| Ok(()), |k| k.to_vec())
                            .unwrap()
                            .as_ref(),
                        Some(k)
                    );
                }
            }
        }
    }

    #[test]
    fn owned_controlled_keys_keep_real_capacity_for_both_codecs() {
        use crate::{access::Preparation, task::Control};
        struct Kept {
            key: Vec<u8>,
            _credit: crate::access::Reservation,
        }
        let mut keys = mixed_keys(90);
        keys.sort();
        keys.dedup();
        for fsst in [false, true] {
            let dir = tempfile::tempdir().unwrap();
            write_codec(dir.path(), &keys, fsst);
            let vocab = Vocab::open(dir.path()).unwrap();
            let preparation = Preparation::new(Control::none(), 4 << 20).unwrap();
            let base = preparation.inner_used();
            for (id, expected) in keys.iter().enumerate() {
                let credit = std::cell::RefCell::new(Some(preparation.reserve(0).unwrap()));
                let actual = vocab
                    .get_owned_with_controlled(
                        id as u64,
                        |old, new| {
                            let total = (old + new) as u64;
                            credit.borrow_mut().as_mut().unwrap().grow_to(total)?;
                            assert!(preparation.inner_used() >= base + total);
                            Ok(())
                        },
                        |capacity| {
                            credit
                                .borrow_mut()
                                .as_mut()
                                .unwrap()
                                .release_to(capacity as u64);
                            assert_eq!(preparation.inner_used(), base + capacity as u64);
                            Ok(())
                        },
                        |phase| preparation.checkpoint(phase),
                        |key| {
                            assert_eq!(preparation.inner_used(), base + key.capacity() as u64);
                            Ok(Kept {
                                key,
                                _credit: credit.borrow_mut().take().unwrap(),
                            })
                        },
                    )
                    .unwrap()
                    .unwrap();
                assert_eq!(&actual.key, expected);
                assert_eq!(
                    preparation.inner_used(),
                    base + actual.key.capacity() as u64
                );
                drop(actual);
                assert_eq!(preparation.inner_used(), base);
            }
        }
    }

    #[test]
    fn owned_key_growth_requires_old_and_new_physical_capacity_together() {
        use crate::{BudgetKind, Error, access::Preparation, task::Control};
        let mut second = b"\"a".to_vec();
        second.extend_from_slice(&vec![b'x'; 4096]);
        let keys = vec![b"\"a!".to_vec(), second];
        let dir = tempfile::tempdir().unwrap();
        write_codec(dir.path(), &keys, false);
        let vocab = Vocab::open(dir.path()).unwrap();
        let preparation = Preparation::new(Control::none(), 1024 + 4098).unwrap();
        let original = preparation.inner_used();
        // The existing constructor's actual header is smaller than a request root.
        // Keep a real previously admitted allocation live, leaving exactly enough
        // room for the new key alone, but not its simultaneous old buffer.
        struct Existing {
            _bytes: Vec<u8>,
            _credit: crate::access::Reservation,
        }
        let size = preparation.room().checked_sub(4098).unwrap() as usize;
        let held = preparation.reserve(size as u64).unwrap();
        let mut bytes = Vec::new();
        bytes.try_reserve_exact(size).unwrap();
        assert_eq!(bytes.capacity(), size);
        bytes.resize(size, 0);
        let existing = Existing {
            _bytes: bytes,
            _credit: held,
        };
        assert_eq!(preparation.room(), 4098);
        let base = preparation.inner_used();
        let credit = std::cell::RefCell::new(preparation.reserve(0).unwrap());
        let overlap = std::cell::Cell::new(false);
        let actual = vocab.get_owned_with_controlled(
            1,
            |old, new| {
                if old > 0 {
                    assert_eq!((old, new), (2, 4098));
                    overlap.set(true);
                }
                credit.borrow_mut().grow_to((old + new) as u64)
            },
            |capacity| {
                credit.borrow_mut().release_to(capacity as u64);
                Ok(())
            },
            |phase| preparation.checkpoint(phase),
            |_| -> crate::Result<()> { panic!("refusal precedes output publication") },
        );
        assert!(
            overlap.get(),
            "the selected real front-coded key must reallocate"
        );
        assert!(matches!(actual,Err(Error::BudgetExceeded(b)) if b.kind==BudgetKind::Memory));
        assert_eq!(preparation.inner_used(), base + 2);
        drop(credit);
        assert_eq!(preparation.inner_used(), base);
        drop(existing);
        assert_eq!(preparation.inner_used(), original);
    }

    #[test]
    fn controlled_fsst_reconstruction_observes_controls_inside_length_and_decode() {
        use crate::Error;
        use crate::access::Preparation;
        use crate::task::{Control, Progress};
        use std::sync::{
            Arc,
            atomic::{AtomicBool, Ordering},
        };
        use std::time::{Duration, Instant};
        let keys: Vec<Vec<u8>> = (0..8)
            .map(|i| format!("\"{i} {}", "payload words ".repeat(12_000)).into_bytes())
            .collect();
        let dir = tempfile::tempdir().unwrap();
        write_codec(dir.path(), &keys, true);
        let v = Vocab::open(dir.path()).unwrap();
        let mut position = v.block_offset(0);
        v.entry::<true>(v.data.as_slice(), &mut position, true);
        let entry = v.entry::<true>(v.data.as_slice(), &mut position, false);
        assert!(
            entry.coded && entry.len > 2048,
            "fixture must traverse actual long FSST suffix"
        );
        for phase in ["measuring vocabulary suffix", "decoding vocabulary suffix"] {
            for deadline_mode in [false, true] {
                let seen = Arc::new(AtomicBool::new(false));
                let observed = seen.clone();
                let control = Control::none();
                let cancel = control.cancel.clone();
                let deadline = deadline_mode.then(|| Instant::now() + Duration::from_millis(100));
                let preparation = Preparation::new(
                    Control {
                        deadline,
                        progress: Progress::new(move |_, message| {
                            if message == phase && !observed.swap(true, Ordering::SeqCst) {
                                if let Some(deadline) = deadline {
                                    while let Some(left) =
                                        deadline.checked_duration_since(Instant::now())
                                    {
                                        std::thread::park_timeout(left);
                                    }
                                } else {
                                    cancel.cancel();
                                }
                            }
                        }),
                        ..control
                    },
                    16 << 20,
                )
                .unwrap();
                let published = std::cell::Cell::new(false);
                let result = v.get_with_controlled(
                    1,
                    |_| Ok(()),
                    |phase| preparation.checkpoint(phase),
                    |_| published.set(true),
                );
                assert!(
                    seen.load(Ordering::SeqCst),
                    "missing checkpoint inside {phase}"
                );
                assert!(!published.get());
                assert!(if deadline_mode {
                    matches!(result, Err(Error::Timeout))
                } else {
                    matches!(result, Err(Error::Cancelled))
                });
            }
        }
    }

    #[test]
    fn controlled_vocabulary_matches_old_search_and_decoding_for_both_codecs() {
        let mut keys: Vec<Vec<u8>> = (0..90)
            .map(|i| {
                format!(
                    "\"{i:05} {}",
                    "payload words ".repeat(if i % 7 == 0 { 3000 } else { 1 })
                )
                .into_bytes()
            })
            .collect();
        keys.extend((0..90).map(|i| {
            format!(
                "<urn:{i:05}:{}",
                "iri ".repeat(if i % 11 == 0 { 2000 } else { 1 })
            )
            .into_bytes()
        }));
        keys.sort();
        for fsst in [false, true] {
            let dir = tempfile::tempdir().unwrap();
            write_codec(dir.path(), &keys, fsst);
            let vocab = Vocab::open(dir.path()).unwrap();
            for (id, key) in keys.iter().enumerate() {
                let capacity = std::cell::Cell::new(0);
                let actual = vocab
                    .get_with_controlled(
                        id as u64,
                        |bytes| {
                            capacity.set(capacity.get().max(bytes));
                            Ok(())
                        },
                        |_| Ok(()),
                        |bytes| {
                            assert!(bytes.len() <= capacity.get());
                            bytes.to_vec()
                        },
                    )
                    .unwrap();
                assert_eq!(actual.as_ref(), Some(key));
                assert_eq!(
                    vocab
                        .find_with_controlled(key, |_| Ok(()), |_| Ok(()))
                        .unwrap(),
                    vocab.find(key)
                );
                let mut absent = key.clone();
                absent.push(0);
                assert_eq!(
                    vocab
                        .find_with_controlled(&absent, |_| Ok(()), |_| Ok(()))
                        .unwrap(),
                    vocab.find(&absent)
                );
            }
            assert_eq!(
                vocab
                    .get_with_controlled(
                        u64::MAX,
                        |_| panic!("absent allocation"),
                        |_| Ok(()),
                        |_| ()
                    )
                    .unwrap(),
                None
            );
        }
    }

    #[test]
    fn controlled_delta_lookup_probes_the_index_instead_of_scanning() {
        let mut vocab = AppendVocab::default();
        for i in 0..10_000 {
            vocab.insert(format!("<urn:entry:{i}").as_bytes());
        }
        let mut checks = 0;
        let mut count = |_: &'static str| {
            checks += 1;
            Ok(())
        };
        let found = vocab
            .find_prepared(b"<urn:entry:9999", 10_000, &mut count)
            .unwrap();
        assert_eq!(found, Some(9999));
        assert!(checks <= 2, "the lookup visited {checks} entries");
        // the snapshot's limit hides later entries, and absent keys stay absent
        assert_eq!(
            vocab
                .find_prepared(b"<urn:entry:9999", 9999, |_| Ok(()))
                .unwrap(),
            None
        );
        assert_eq!(
            vocab
                .find_prepared(b"<urn:absent", 10_000, |_| Ok(()))
                .unwrap(),
            None
        );
        // a key too long to hash in one probe is still found, with polling
        let long = [b"<".as_slice(), &vec![b'x'; PREPARED_PROBE_BYTES + 1]].concat();
        vocab.insert(&long);
        let mut polls = 0;
        let found = vocab
            .find_prepared(&long, 10_001, |_| {
                polls += 1;
                Ok(())
            })
            .unwrap();
        assert_eq!(found, Some(10_000));
        assert!(polls > 10_000);
    }

    #[test]
    fn controlled_fsst_low_cap_refuses_during_length_before_decode_or_allocation() {
        use std::cell::Cell;
        let keys: Vec<Vec<u8>> = (0..8)
            .map(|i| format!("\"{i} {}", "payload words ".repeat(12_000)).into_bytes())
            .collect();
        let dir = tempfile::tempdir().unwrap();
        write_codec(dir.path(), &keys, true);
        let vocab = Vocab::open(dir.path()).unwrap();
        let largest = Cell::new(0);
        let decoded = Cell::new(false);
        let preparation =
            crate::access::Preparation::new(crate::task::Control::none(), 32 << 10).unwrap();
        let mut workspace = preparation.reserve(0).unwrap();
        let result = vocab.get_with_controlled(
            1,
            |bytes| {
                largest.set(largest.get().max(bytes));
                workspace.grow_to((bytes as u64).saturating_mul(2).saturating_add(256))
            },
            |phase| {
                if phase == "decoding vocabulary suffix" {
                    decoded.set(true);
                }
                Ok(())
            },
            |_| panic!("refused key published"),
        );
        assert!(matches!(result, Err(crate::Error::BudgetExceeded(_))));
        assert!(!decoded.get());
        assert!(
            largest.get() < keys[1].len() / 2,
            "whole suffix measured before admission refusal"
        );
    }

    /// A damaged shared-prefix length in the entry after a key does not make a checked
    /// decode of the later key reserve room for it (Codex review of item 108): the
    /// partial decode of the key before reserves at most what its suffix can fill.
    #[test]
    fn a_damaged_shared_prefix_reserves_no_more_than_the_suffix() {
        let keys = [b"\"a".to_vec(), b"\"ab".to_vec()];
        for fsst in [false, true] {
            let d = tempfile::tempdir().unwrap();
            write_codec(d.path(), &keys, fsst);
            let path = d.path().join("vocab.dat");
            let data = std::fs::read(&path).unwrap();
            // the second entry's shared length becomes 2^62
            let mut pos = 0;
            read_varint(&data, &mut pos);
            let len = read_varint(&data, &mut pos) as usize;
            let second = pos + len;
            let mut at = second;
            read_varint(&data, &mut at);
            let mut out = data[..second].to_vec();
            write_varint(&mut out, 1 << 62);
            out.extend_from_slice(&data[at..]);
            std::fs::write(&path, &out).unwrap();
            let v = Vocab::open(d.path()).unwrap();
            let most = std::cell::Cell::new(0usize);
            let got = v.get_with_checked(
                1,
                |need| {
                    most.set(most.get().max(need));
                    if need > 1 << 20 { Err(need) } else { Ok(()) }
                },
                |k| k.len(),
            );
            assert!(
                most.get() <= 1 << 20,
                "fsst {fsst}: asked for {}",
                most.get()
            );
            assert!(got.is_ok(), "fsst {fsst}: {got:?}");
            let got = v.get_with_fallible(1, |_| Ok(()), |k| k.len());
            assert!(got.is_ok(), "fsst {fsst}");
            // the unchecked decoders do not panic either
            v.get(1);
            v.get_sorted(&[0, 1], |_, _| {});
        }
    }

    /// Long keys are coded in batches bounded by their bytes, which end at a block.
    #[test]
    fn fsst_batches_are_bounded_by_bytes() {
        let keys: Vec<Vec<u8>> = (0..400)
            .map(|i| format!("\"{i:05} {}", "payload words ".repeat(300)).into_bytes())
            .chain((0..50).map(|i| format!("<http://e/{i:03}/{}", "x".repeat(9000)).into_bytes()))
            .collect();
        let d = tempfile::tempdir().unwrap();
        let sample: Vec<&[u8]> = keys.iter().map(|k| &k[..200]).collect();
        let mut w =
            VocabWriter::create_fsst(d.path(), fsst::Encoder::train([&sample, &sample])).unwrap();
        w.fsst.as_mut().unwrap().batch_bytes = 64 << 10;
        for k in &keys {
            w.push(k).unwrap();
        }
        let peak = w.fsst.as_ref().unwrap().peak_bytes;
        w.finish().unwrap();
        // one block of these keys past the budget at most
        assert!(peak < (64 << 10) + FC_BLOCK * 10_000, "{peak}");
        let v = Vocab::open(d.path()).unwrap();
        assert_eq!(v.len(), keys.len() as u64);
        for (i, k) in keys.iter().enumerate() {
            assert_eq!(v.get(i as u64).as_ref(), Some(k), "{i}");
            assert_eq!(v.find(k), Ok(i as u64));
        }
    }

    /// An FSST-coded vocabulary answers every read as the front-coded one of the same
    /// keys does (X11 A7), and is smaller.
    #[test]
    fn fsst_decodes_any_subset_of_a_block_like_front() {
        // keys whose shared prefixes rise and fall, so that a key the next one takes
        // few bytes of limits what every key before it must give
        let mut keys: Vec<Vec<u8>> = Vec::new();
        for i in 0..400u32 {
            let long = "x".repeat((i % 7 * 9) as usize);
            keys.push(format!("<http://example.org/{:03}/{long}{}>", i / 5, i % 5).into_bytes());
            keys.push(format!("\"{:03} {long} {}\"", i / 3, i).into_bytes());
        }
        keys.sort();
        keys.dedup();
        let (fd, sd) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
        write_codec(fd.path(), &keys, false);
        write_codec(sd.path(), &keys, true);
        let (f, s) = (
            Vocab::open(fd.path()).unwrap(),
            Vocab::open(sd.path()).unwrap(),
        );
        assert!(s.is_fsst());
        let mut x = 0x9E37_79B9_7F4A_7C15u64;
        for _ in 0..2000 {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            let b = (x >> 20) % (keys.len() as u64 / FC_BLOCK as u64);
            let mask = (x & 0xffff) | 1 << (x >> 40 & 15);
            let ids: Vec<u64> = (0..FC_BLOCK as u64)
                .filter(|i| mask >> i & 1 == 1)
                .map(|i| b * FC_BLOCK as u64 + i)
                .collect();
            let (mut a, mut c, mut d) = (Vec::new(), Vec::new(), Vec::new());
            f.get_sorted(&ids, |i, k| a.push((i, k.to_vec())));
            s.get_sorted(&ids, |i, k| c.push((i, k.to_vec())));
            s.get_sorted_checked::<()>(
                &ids,
                |_| Ok(()),
                |i, k| {
                    d.push((i, k.to_vec()));
                    Ok(())
                },
            )
            .unwrap();
            assert_eq!(a, c, "{ids:?}");
            assert_eq!(a, d, "{ids:?}");
            let last = *ids.last().unwrap();
            assert_eq!(s.get(last).as_ref(), Some(&keys[last as usize]));
            assert_eq!(
                s.get_with_fallible(last, |_| Ok(()), |k| k.to_vec())
                    .unwrap()
                    .as_ref(),
                Some(&keys[last as usize])
            );
        }
    }

    #[test]
    fn shared_prefix_matches_the_preceding_key_in_both_codecs() {
        let mut keys = Vec::new();
        for i in 0..300u32 {
            let long = "é".repeat(if i % 11 == 0 {
                120
            } else {
                (i % 5 * 7) as usize
            });
            keys.push(format!("\"s{i}{long}").into_bytes());
            keys.push(format!("<http://example.org/{:03}/{long}>", i / 3).into_bytes());
        }
        keys.sort();
        keys.dedup();
        for fsst in [false, true] {
            let dir = tempfile::tempdir().unwrap();
            write_codec(dir.path(), &keys, fsst);
            let v = Vocab::open(dir.path()).unwrap();
            assert_eq!(v.is_fsst(), fsst);
            for (i, expected) in keys.iter().enumerate() {
                let shared = (!i.is_multiple_of(FC_BLOCK)).then(|| {
                    keys[i - 1]
                        .iter()
                        .zip(expected)
                        .take_while(|(a, b)| a == b)
                        .count()
                });
                let check = |prefix: Option<usize>, key: &[u8]| {
                    assert_eq!(key, expected, "fsst {fsst} id {i}");
                    assert_eq!(prefix, shared, "fsst {fsst} id {i}");
                };
                assert_eq!(v.get_with_shared_prefix(i as u64, check), Some(()));
                assert_eq!(
                    v.get_with_shared_prefix_checked(i as u64, |_| Ok::<_, ()>(()), check),
                    Ok(Some(()))
                );
            }
            for id in [v.len(), v.len() + 1] {
                let unchecked: Option<()> =
                    v.get_with_shared_prefix(id, |_, _| panic!("out-of-bounds lookup"));
                assert_eq!(unchecked, None);
                let checked: Result<Option<()>, ()> = v.get_with_shared_prefix_checked(
                    id,
                    |_| panic!("out-of-bounds scratch admission"),
                    |_, _| panic!("out-of-bounds checked lookup"),
                );
                assert_eq!(checked, Ok(None));
            }
        }
    }

    #[test]
    fn checked_shared_prefix_lookup_admits_scratch_without_reading_a_long_suffix() {
        let keys = [
            format!("<aa{}>", "é".repeat(4096)).into_bytes(),
            b"<ab>".to_vec(),
        ];
        for fsst in [false, true] {
            let dir = tempfile::tempdir().unwrap();
            write_codec(dir.path(), &keys, fsst);
            let v = Vocab::open(dir.path()).unwrap();
            let cap = v.scratch_capacity();
            let mut admissions = Vec::new();
            let result = v.get_with_shared_prefix_checked(
                1,
                |need| {
                    admissions.push(need);
                    if need <= cap { Ok(()) } else { Err(need) }
                },
                |shared, key| {
                    assert_eq!(key, keys[1]);
                    assert_eq!(shared, Some(2));
                },
            );
            assert_eq!(result, Ok(Some(())));
            assert_eq!(admissions, [cap]);
            use std::sync::Arc;
            use std::sync::atomic::{AtomicUsize, Ordering};
            let observed = Arc::new(AtomicUsize::new(0));
            let seen = observed.clone();
            crate::algo::set_projection_observer(Some(Arc::new(move |event| {
                if let crate::algo::ProjectionEvent::Materialized(
                    "checked-vocab-scratch-allocated",
                    _,
                ) = event
                {
                    seen.fetch_add(1, Ordering::Relaxed);
                }
            })));
            let refused_initial: Result<Option<()>, _> =
                v.get_with_shared_prefix_checked(1, Err, |_, _| panic!("initial scratch refused"));
            let initial_allocations = observed.load(Ordering::Relaxed);
            let refused_growth: Result<Option<()>, _> = v.get_with_shared_prefix_checked(
                0,
                |need| if need <= cap { Ok(()) } else { Err(need) },
                |_, _| panic!("scratch growth refused"),
            );
            crate::algo::set_projection_observer(None);
            assert_eq!(refused_initial, Err(cap));
            assert_eq!(initial_allocations, 0);
            assert_eq!(refused_growth, Err(keys[0].len()));
            assert_eq!(observed.load(Ordering::Relaxed), 1);
        }
    }

    #[test]
    fn reused_shared_prefix_lookup_avoids_reallocations_and_enforces_admission() {
        let mut keys = Vec::new();
        for i in 0..40 {
            keys.push(format!("\"k{i:03}\"\x7f").into_bytes());
        }
        keys.push(format!("\"z{}\"\x7f", "x".repeat(4096)).into_bytes());
        keys.sort();
        keys.dedup();
        let long_idx = keys.iter().position(|k| k.len() > 4000).unwrap() as u64;
        for fsst in [false, true] {
            let dir = tempfile::tempdir().unwrap();
            write_codec(dir.path(), &keys, fsst);
            let v = Vocab::open(dir.path()).unwrap();
            let cap = v.scratch_capacity();

            // Unchecked reuse matches single-shot across block boundaries.
            let mut unchecked_buf = Vec::new();
            for (i, expected) in keys.iter().enumerate() {
                let mut single = None;
                v.get_with_shared_prefix(i as u64, |p, k| single = Some((p, k.to_vec())));
                let mut reused = None;
                v.get_with_shared_prefix_in(i as u64, &mut unchecked_buf, |p, k| {
                    reused = Some((p, k.to_vec()));
                });
                assert_eq!(reused, single, "fsst {fsst} id {i}");
                assert_eq!(reused.unwrap().1, *expected);
            }

            // Checked reuse admits initial scratch once and does not reallocate for short keys.
            use std::sync::Arc;
            use std::sync::atomic::{AtomicUsize, Ordering};
            let observed = Arc::new(AtomicUsize::new(0));
            let seen = observed.clone();
            crate::algo::set_projection_observer(Some(Arc::new(move |event| {
                if let crate::algo::ProjectionEvent::Materialized(
                    "checked-vocab-scratch-allocated",
                    _,
                ) = event
                {
                    seen.fetch_add(1, Ordering::Relaxed);
                }
            })));
            let mut checked_buf = Vec::new();
            let mut admissions = Vec::new();
            for i in 0..keys.len() as u64 {
                if i == long_idx {
                    continue;
                }
                let got = v
                    .get_with_shared_prefix_checked_in(
                        i,
                        &mut checked_buf,
                        |need| {
                            admissions.push(need);
                            if need <= cap { Ok(()) } else { Err(need) }
                        },
                        |shared, key| (shared, key.to_vec()),
                    )
                    .unwrap()
                    .unwrap();
                assert_eq!(got.1, keys[i as usize], "fsst {fsst} id {i}");
                let expected_shared = (!(i as usize).is_multiple_of(FC_BLOCK)).then(|| {
                    keys[i as usize - 1]
                        .iter()
                        .zip(&keys[i as usize])
                        .take_while(|(a, b)| a == b)
                        .count()
                });
                assert_eq!(got.0, expected_shared, "fsst {fsst} id {i}");
            }
            assert_eq!(admissions, [cap], "fsst {fsst}: single initial admission");
            assert_eq!(
                observed.load(Ordering::Relaxed),
                1,
                "fsst {fsst}: single scratch allocation across all short keys"
            );

            // Refusing growth for the long key fails before allocating more capacity.
            let refused: Result<Option<()>, _> = v.get_with_shared_prefix_checked_in(
                long_idx,
                &mut checked_buf,
                |need| if need <= cap { Ok(()) } else { Err(need) },
                |_, _| panic!("growth refused"),
            );
            assert!(refused.is_err(), "fsst {fsst}: growth must refuse");
            assert_eq!(
                observed.load(Ordering::Relaxed),
                1,
                "fsst {fsst}: no growth allocation on refusal"
            );

            // Refusing initial scratch on a fresh buffer allocates nothing.
            let mut empty_buf = Vec::new();
            let refused_init: Result<Option<()>, _> =
                v.get_with_shared_prefix_checked_in(0, &mut empty_buf, Err, |_, _| {
                    panic!("initial scratch refused")
                });
            crate::algo::set_projection_observer(None);
            assert_eq!(refused_init, Err(cap));
            assert_eq!(empty_buf.capacity(), 0);
        }
    }

    #[test]
    fn checked_fsst_lookup_succeeds_when_optional_room_is_refused() {
        let mut keys = Vec::new();
        let mid_pad = "alpha_beta_gamma_delta_".repeat(5);
        let long_pad = "alpha_beta_gamma_delta_".repeat(8);
        for i in 0..160u32 {
            keys.push(format!("\"m-{mid_pad}delta_tail_{i:04}_alpha_beta\"\x7f").into_bytes());
        }
        for i in 0..32u32 {
            keys.push(format!("\"z-{long_pad}delta_tail_{i:02}_alpha_beta\"\x7f").into_bytes());
        }
        keys.sort();
        keys.dedup();

        let dir = tempfile::tempdir().unwrap();
        write_codec(dir.path(), &keys, true);
        let v = Vocab::open(dir.path()).unwrap();
        assert!(v.is_fsst());
        let cap = v.scratch_capacity();
        assert_eq!(cap, 192);

        // (a) A verified FSST-coded entry with at most 8 codes (`fast_room`) whose key fits
        // in `cap` (192) while `len + FSST_ROOM` exceeds `cap`: refusing the preferred
        // headroom reservation must fall back to the already-admitted buffer.
        let fit_id = (0..keys.len())
            .find(|&i| {
                let len = keys[i].len();
                len > cap - FSST_ROOM
                    && len <= cap
                    && !i.is_multiple_of(FC_BLOCK)
                    && v.entry_coded_len(i as u64).is_some_and(|codes| codes <= 8)
            })
            .expect(
                "fixture must contain a verified FSST-coded entry in (cap - FSST_ROOM, cap] with <= 8 codes",
            ) as u64;
        let fit_len = keys[fit_id as usize].len();
        let expected_shared = (!(fit_id as usize).is_multiple_of(FC_BLOCK)).then(|| {
            keys[fit_id as usize - 1]
                .iter()
                .zip(&keys[fit_id as usize])
                .take_while(|(a, b)| a == b)
                .count()
        });

        let mut shared_buf = Vec::new();
        let mut shared_refused = Vec::new();
        let got_shared = v.get_with_shared_prefix_checked_in(
            fit_id,
            &mut shared_buf,
            |need| {
                if need <= cap {
                    Ok(())
                } else {
                    shared_refused.push(need);
                    Err(need)
                }
            },
            |shared, key| (shared, key.to_vec()),
        );
        assert_eq!(
            got_shared,
            Ok(Some((expected_shared, keys[fit_id as usize].clone()))),
            "reused checked shared-prefix walk must succeed within admitted required capacity"
        );
        assert!(
            shared_refused.is_empty(),
            "ROOM = false shared-prefix walk must not request optional headroom: {shared_refused:?}"
        );

        let mut reused_buf = Vec::new();
        let mut fit_refused = Vec::new();
        let got_reused = v.get_with_checked_in(
            fit_id,
            &mut reused_buf,
            |need| {
                if need <= cap {
                    Ok(())
                } else {
                    fit_refused.push(need);
                    Err(need)
                }
            },
            |key| key.to_vec(),
        );
        assert_eq!(
            got_reused,
            Ok(Some(keys[fit_id as usize].clone())),
            "reused checked walk must succeed within admitted required capacity"
        );
        assert!(
            fit_refused.contains(&(fit_len + FSST_ROOM)),
            "fast_room must attempt and observe refusal of preferred headroom {}: {fit_refused:?}",
            fit_len + FSST_ROOM
        );
        assert_eq!(
            reused_buf.capacity(),
            cap,
            "falling back when total <= cap must keep the already-admitted buffer"
        );

        let mut fresh_refused = Vec::new();
        let got_fresh = v.get_with_checked(
            fit_id,
            |need| {
                if need <= cap {
                    Ok(())
                } else {
                    fresh_refused.push(need);
                    Err(need)
                }
            },
            |key| key.to_vec(),
        );
        assert_eq!(
            got_fresh,
            Ok(Some(keys[fit_id as usize].clone())),
            "fresh checked walk must succeed within admitted required capacity"
        );
        assert!(
            fresh_refused.is_empty(),
            "ROOM = false fresh walk must not request optional headroom: {fresh_refused:?}"
        );

        // (b) A verified FSST-coded entry longer than `cap` with at most 8 codes (`fast_room`):
        // refusing `len + FSST_ROOM` must fall back to admitting `len` and growing the buffer.
        let grown_id = (0..keys.len())
            .find(|&i| {
                keys[i].len() > cap
                    && !i.is_multiple_of(FC_BLOCK)
                    && v.entry_coded_len(i as u64).is_some_and(|codes| codes <= 8)
            })
            .expect(
                "fixture must contain a verified FSST-coded entry longer than cap with <= 8 codes",
            ) as u64;
        let need_len = keys[grown_id as usize].len();

        let mut grown_shared = Vec::new();
        let mut grown_shared_admitted = Vec::new();
        let mut grown_shared_refused = Vec::new();
        assert_eq!(
            v.get_with_shared_prefix_checked_in(
                grown_id,
                &mut grown_shared,
                |need| {
                    if need <= need_len {
                        grown_shared_admitted.push(need);
                        Ok(())
                    } else {
                        grown_shared_refused.push(need);
                        Err(need)
                    }
                },
                |_, k| k.to_vec()
            ),
            Ok(Some(keys[grown_id as usize].clone()))
        );
        assert!(
            grown_shared_refused.contains(&(need_len + FSST_ROOM)),
            "fast_room growth must attempt and observe refusal of preferred headroom {}: {grown_shared_refused:?}",
            need_len + FSST_ROOM
        );
        assert!(
            grown_shared_admitted.contains(&need_len),
            "after preferred headroom refusal, shared-prefix walk must fall back to required size {need_len}: {grown_shared_admitted:?}"
        );

        let mut grown_reused = Vec::new();
        let mut grown_admitted = Vec::new();
        let mut grown_refused = Vec::new();
        assert_eq!(
            v.get_with_checked_in(
                grown_id,
                &mut grown_reused,
                |need| {
                    if need <= need_len {
                        grown_admitted.push(need);
                        Ok(())
                    } else {
                        grown_refused.push(need);
                        Err(need)
                    }
                },
                |k| k.to_vec()
            ),
            Ok(Some(keys[grown_id as usize].clone()))
        );
        assert!(
            grown_refused.contains(&(need_len + FSST_ROOM)),
            "fast_room must attempt and observe refusal of preferred headroom {}: {grown_refused:?}",
            need_len + FSST_ROOM
        );
        assert!(
            grown_admitted.contains(&need_len),
            "after preferred headroom refusal, walk must fall back to admitting required size {need_len}: {grown_admitted:?}"
        );
        assert!(
            grown_reused.capacity() >= need_len,
            "buffer must grow to at least required length {need_len}"
        );
    }

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
    fn a_missing_sparse_index_is_corrupt() {
        let dir = tempfile::tempdir().unwrap();
        write_keys(dir.path(), 3 * FC_BLOCK * IDX_BLOCKS + 77);
        assert!(Vocab::open(dir.path()).unwrap().sparse.is_some());
        std::fs::remove_file(dir.path().join("vocab.idx")).unwrap();
        let error = Vocab::open(dir.path())
            .err()
            .expect("the index is required");
        assert!(matches!(error, crate::Error::Corrupt(_)), "{error}");
        assert!(
            error
                .to_string()
                .contains("requires the sparse vocabulary index")
        );
        // a process that does not use the index still needs the file on disk
        crate::index::set_sparse_vocab(false);
        let opened = Vocab::open(dir.path());
        crate::index::set_sparse_vocab(true);
        assert!(matches!(opened, Err(crate::Error::Corrupt(_))));
    }

    #[test]
    fn a_sparse_index_of_another_vocabulary_is_corrupt() {
        let a = tempfile::tempdir().unwrap();
        let b = tempfile::tempdir().unwrap();
        write_keys(a.path(), 2 * FC_BLOCK * IDX_BLOCKS);
        write_keys(b.path(), 5 * FC_BLOCK * IDX_BLOCKS);
        std::fs::copy(a.path().join("vocab.idx"), b.path().join("vocab.idx")).unwrap();
        let error = Vocab::open(b.path())
            .err()
            .expect("another vocabulary's index");
        assert!(matches!(error, crate::Error::Corrupt(_)), "{error}");
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
        assert_eq!(verify(), Ok(4));
        // the last key's final digit, lowered: still in order, but not block 384's key
        let path = dir.path().join("vocab.idx");
        let mut idx = std::fs::read(&path).unwrap();
        let n = idx.len();
        idx[n - 1] -= 1;
        std::fs::write(&path, &idx).unwrap();
        assert!(verify().unwrap_err().contains("block 384"));
        std::fs::write(&path, b"junk").unwrap();
        assert!(verify().unwrap_err().contains("not a sparse index"));
        std::fs::remove_file(&path).unwrap();
        assert!(
            verify()
                .unwrap_err()
                .contains("requires the sparse vocabulary index")
        );
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

    #[test]
    fn control_trait_vocab_open_and_scanners_parity_and_limits() {
        use crate::{
            Error,
            history::{HistoryOptions, ReadLimit},
            store::cold::ColdBudget,
        };
        use std::sync::atomic::AtomicBool;
        use std::time::{Duration, Instant};

        let keys = mixed_keys(400);
        for fsst in [false, true] {
            let dir = tempfile::tempdir().unwrap();
            write_codec(dir.path(), &keys, fsst);

            let plain = Vocab::open_with(dir.path(), fsst, &mut Unlimited).unwrap();
            assert_eq!(plain.charge, 0);
            let options = HistoryOptions::default();
            let mut budget = ColdBudget::new(ReadLimit {
                options: &options,
                memory: Some(16 << 20),
                held: 0,
            })
            .unwrap();
            let controlled = Vocab::open_with(dir.path(), fsst, &mut budget).unwrap();
            assert_eq!(plain.value.len(), controlled.value.len());
            assert_eq!(plain.value.first_iri(), controlled.value.first_iri());
            assert_eq!(plain.value.first_triple(), controlled.value.first_triple());
            assert!(controlled.charge > 0);
            drop(controlled.value);
            budget.release(controlled.charge);
            assert_eq!(budget.used(), 0);

            // Uncapped ReadLimit still honours cancellation and deadline on open_with.
            let cancel = Arc::new(AtomicBool::new(false));
            let cancelled_opts = HistoryOptions {
                cancel: Some(cancel.clone()),
                deadline: None,
            };
            let mut uncapped_cancel = ColdBudget::new(ReadLimit {
                options: &cancelled_opts,
                memory: None,
                held: 0,
            })
            .unwrap();
            cancel.store(true, AtomicOrdering::Relaxed);
            assert!(matches!(
                Vocab::open_with(dir.path(), fsst, &mut uncapped_cancel),
                Err(Error::Cancelled)
            ));
            assert_eq!(uncapped_cancel.used(), 0);

            let deadline = Instant::now() + Duration::from_millis(20);
            let expired_opts = HistoryOptions {
                cancel: None,
                deadline: Some(deadline),
            };
            let mut uncapped_timeout = ColdBudget::new(ReadLimit {
                options: &expired_opts,
                memory: None,
                held: 0,
            })
            .unwrap();
            while Instant::now() < deadline {
                std::thread::sleep(Duration::from_millis(1));
            }
            assert!(matches!(
                Vocab::open_with(dir.path(), fsst, &mut uncapped_timeout),
                Err(Error::Timeout)
            ));
            assert_eq!(uncapped_timeout.used(), 0);

            // Tight budget refuses open_with cleanly.
            let mut tiny = ColdBudget::new(ReadLimit {
                options: &options,
                memory: Some(1),
                held: 0,
            })
            .unwrap();
            assert!(matches!(
                Vocab::open_with(dir.path(), fsst, &mut tiny),
                Err(Error::BudgetExceeded(_))
            ));

            // Parity across Unlimited, CheckedScratch, ExactScratch, and ControlledScratch.
            let v = &plain.value;
            let sample_ids: Vec<u64> = (0..keys.len() as u64).step_by(7).collect();
            let mut unlimited_out = Vec::new();
            v.get_sorted(&sample_ids, |id, k| unlimited_out.push((id, k.to_vec())));
            let mut checked_out = Vec::new();
            v.get_sorted_checked(
                &sample_ids,
                |_| Ok::<_, Error>(()),
                |id, k| {
                    checked_out.push((id, k.to_vec()));
                    Ok(())
                },
            )
            .unwrap();
            let mut owned_out = Vec::new();
            v.get_sorted_owned_with_ahead(
                &sample_ids,
                |_| Ok(()),
                |id, k| {
                    owned_out.push((id, k.to_vec()));
                    Ok(())
                },
                true,
            )
            .unwrap();
            assert_eq!(checked_out, unlimited_out);
            assert_eq!(owned_out, unlimited_out);

            // Tight budget refuses get_sorted_checked and find_with_checked cleanly.
            assert!(matches!(
                v.get_sorted_checked(&sample_ids, |_| Err(Error::Cancelled), |_, _| Ok(())),
                Err(Error::Cancelled)
            ));
            assert!(matches!(
                v.find_with_checked(&keys[17], |_| Err(Error::Timeout)),
                Err(Error::Timeout)
            ));
            assert!(matches!(
                v.find_with_controlled(&keys[17], |_| Ok(()), |_| Err(Error::Cancelled)),
                Err(Error::Cancelled)
            ));
            for &id in &sample_ids {
                let k = &keys[id as usize];
                assert_eq!(v.find(k), Ok(id));
                assert_eq!(v.find_with_checked(k, |_| Ok(())).unwrap(), Ok(id));
                assert_eq!(
                    v.find_with_controlled(k, |_| Ok(()), |_| Ok(())).unwrap(),
                    Ok(id)
                );
                let fallible = v.get_with_fallible(id, |_| Ok(()), |b| b.to_vec()).unwrap();
                assert_eq!(fallible.as_deref(), Some(k.as_slice()));
            }
        }
    }
}
