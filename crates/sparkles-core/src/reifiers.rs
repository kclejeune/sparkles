//! Reifier proposition-term index (roadmap item 766, spec C25 "Recall and Ask with claims").
//!
//! A claim occurrence in a proposal graph reifies its proposition with RDF 1.2:
//! `<claim> rdf:reifies <<( s p o )>>`. Finding all claims about a set of entities in a
//! proposal graph without an index requires scanning every quad of the graph and decoding
//! every triple term. This module maintains a derived index mapping
//! `(graph, term_hash(iri), reifier, triple_term)` for every `rdf:reifies` quad whose
//! triple term has an IRI subject or IRI object.
//!
//! # Base generations and deltas
//!
//! * For an immutable base [`crate::store::Generation`], [`build`] writes `reifiers.bin`
//!   alongside the permutations during bulk loads and compactions (and omits the file when
//!   the generation holds no indexed reifier entries, recording `entries: 0` in `meta.json`).
//! * For live, branched and historical [`crate::store::Delta`] overlays, `Delta::reifiers_ins`
//!   tracks inserted `rdf:reifies` quads in a persistent [`crate::store::keyset::KeySet`] using
//!   the same `[graph, term_hash, reifier, triple_term]` 4-column key layout, while deletions
//!   of base quads are filtered against `Delta::del[Perm::Spo]`.
//! * Lookups hash each requested entity IRI with SHA-256 (first 8 bytes, little-endian),
//!   binary-search `reifiers.bin` and range-scan `Delta::reifiers_ins`, and verify the exact
//!   triple-term subject or object against the requested IRI so a 64-bit hash collision can
//!   never produce a false positive.
//!
//! # File format, version 1
//!
//! All integers are little-endian:
//! * **Header (32 bytes):** magic `SPKREIF\0` (8), version `u32` (4), reserved `u32` = 0 (4),
//!   `entries: u64` (8), reserved `u64` = 0 (8).
//! * **Entries (`entries * 32` bytes):** strictly ascending, deduplicated `[u64; 4]` keys
//!   `[graph_id, term_hash, reifier_id, triple_term_id]`.
//! * **Footer (24 bytes):** `body_len: u64` (`32 + entries * 32`), 8-byte SHA-256 prefix over
//!   `0..body_len`, end magic `SPKREND\0` (8).

use crate::error::{Error, Result};
use crate::id::{Id, Tag};
use crate::index::{Key, Perm, PermIndex};
use crate::vocab::Vocab;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::cmp::Reverse;
use std::collections::BinaryHeap;
use std::fs::File;
use std::io::{BufReader, BufWriter, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// The file of a generation's reifier proposition-term index.
pub const FILE: &str = "reifiers.bin";
/// The version of `reifiers.bin` this build writes (see [`crate::formats::REIFIERS`]).
pub const VERSION: u32 = 1;
const MAGIC: &[u8; 8] = b"SPKREIF\0";
const END: &[u8; 8] = b"SPKREND\0";
const HEADER: usize = 32;
const ENTRY: usize = 32;
const FOOTER: usize = 24;

/// `rdf:reifies` IRI string.
pub const RDF_REIFIES: &str = "http://www.w3.org/1999/02/22-rdf-syntax-ns#reifies";
/// Vocabulary key of `rdf:reifies` (`'<' + RDF_REIFIES`).
pub const RDF_REIFIES_KEY: &[u8] = b"<http://www.w3.org/1999/02/22-rdf-syntax-ns#reifies";

/// Maximum entries sorted in memory before spilling sorted runs to disk (~32 MiB).
const SORT_CHUNK_ENTRIES: usize = 1 << 20;
/// Maximum sorted runs merged in a single k-way merge pass (bounds FDs and read buffers).
const MAX_MERGE_FANIN: usize = 64;
/// Chunk size for checkpointed SHA-256 hashing of long IRIs and sidecar verification.
const HASH_CHECK_CHUNK: usize = 64 << 10;

/// Metadata about a generation's reifier proposition-term index, recorded in `meta.json`.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReifierInfo {
    /// Version of `reifiers.bin`, 0 when absent.
    pub format: u32,
    /// Number of indexed `(graph, term_hash, reifier, triple_term)` entries.
    pub entries: u64,
    /// Size of `reifiers.bin` in bytes, 0 when no file was written.
    #[serde(default)]
    pub bytes: u64,
}

impl ReifierInfo {
    /// Metadata for a generation known to contain zero indexed reifier entries.
    pub fn empty() -> Self {
        Self {
            format: VERSION,
            entries: 0,
            bytes: 0,
        }
    }
}

/// Deterministic 64-bit hash of an IRI's UTF-8 bytes (without the leading `'<'` key tag).
#[inline]
pub fn term_hash(iri: &[u8]) -> u64 {
    let digest = Sha256::digest(iri);
    u64::from_le_bytes(digest[..8].try_into().unwrap())
}

/// Checkpointed variant of [`term_hash`] that polls `check` every 64 KiB chunk.
pub fn term_hash_controlled<E>(
    iri: &[u8],
    mut check: impl FnMut() -> std::result::Result<(), E>,
) -> std::result::Result<u64, E> {
    check()?;
    let mut hasher = Sha256::new();
    for chunk in iri.chunks(HASH_CHECK_CHUNK) {
        check()?;
        hasher.update(chunk);
    }
    check()?;
    let digest = hasher.finalize();
    Ok(u64::from_le_bytes(digest[..8].try_into().unwrap()))
}

/// Extract the raw subject and object sub-keys from a vocabulary triple-term key
/// (`'(' varint(len_s) key_s varint(len_p) key_p varint(len_o) key_o`).
fn parse_triple_key_endpoints(key: &[u8]) -> Option<(&[u8], &[u8])> {
    if key.first() != Some(&b'(') {
        return None;
    }
    let mut pos = 1usize;
    let mut read_part = || -> Option<&[u8]> {
        let mut len = 0u64;
        let mut shift = 0u32;
        loop {
            let b = *key.get(pos)?;
            pos += 1;
            if shift == 63 && b > 1 {
                return None;
            }
            len |= u64::from(b & 0x7f).checked_shl(shift)?;
            if b < 0x80 {
                break;
            }
            shift += 7;
            if shift >= 64 {
                return None;
            }
        }
        let len = usize::try_from(len).ok()?;
        let end = pos.checked_add(len)?;
        let part = key.get(pos..end)?;
        pos = end;
        Some(part)
    };
    let s = read_part()?;
    let _p = read_part()?;
    let o = read_part()?;
    if pos != key.len() {
        return None;
    }
    Some((s, o))
}

/// Distinct 64-bit hashes of a triple term's IRI subject and IRI object.
pub type TermHashes = (Option<u64>, Option<u64>);

/// Extract the distinct 64-bit hashes of the IRI subject and IRI object of a raw
/// vocabulary triple-term key (`'(' varint(len_s) key_s varint(len_p) key_p varint(len_o) key_o`),
/// without heap allocation. Returns `None` if `key` is not a well-formed triple-term key.
pub fn triple_key_term_hashes(key: &[u8]) -> Option<TermHashes> {
    let (s, o) = parse_triple_key_endpoints(key)?;
    let hs = (s.first() == Some(&b'<')).then(|| term_hash(&s[1..]));
    let ho = (o.first() == Some(&b'<'))
        .then(|| term_hash(&o[1..]))
        .filter(|h| Some(*h) != hs);
    Some((hs, ho))
}

/// Checkpointed variant of [`triple_key_term_hashes`] for controlled replay and
/// permission preparation.
pub fn triple_key_term_hashes_controlled<E>(
    key: &[u8],
    mut check: impl FnMut() -> std::result::Result<(), E>,
) -> std::result::Result<Option<TermHashes>, E> {
    check()?;
    let Some((s, o)) = parse_triple_key_endpoints(key) else {
        return Ok(None);
    };
    let hs = if s.first() == Some(&b'<') {
        Some(term_hash_controlled(&s[1..], &mut check)?)
    } else {
        None
    };
    let ho = if o.first() == Some(&b'<') {
        let h = term_hash_controlled(&o[1..], &mut check)?;
        (Some(h) != hs).then_some(h)
    } else {
        None
    };
    Ok(Some((hs, ho)))
}

fn corrupt(what: impl std::fmt::Display) -> Error {
    Error::Corrupt(format!("{FILE}: {what}"))
}

// ------------------------------------------------------------------ building ------

/// Remove temporary spill files on drop.
struct RunGuard {
    paths: Vec<PathBuf>,
    next_id: usize,
}

impl RunGuard {
    fn new() -> Self {
        Self {
            paths: Vec::new(),
            next_id: 0,
        }
    }

    fn create_run(&mut self, dir: &Path) -> Result<(PathBuf, File)> {
        let id = self.next_id;
        self.next_id += 1;
        let path = dir.join(format!("{FILE}.run.{id}.tmp"));
        let f = File::create(&path)?;
        self.paths.push(path.clone());
        Ok((path, f))
    }
}

impl Drop for RunGuard {
    fn drop(&mut self) {
        for p in &self.paths {
            let _ = std::fs::remove_file(p);
        }
    }
}

/// Remove a temporary output file on drop unless disarmed after atomic rename.
struct TmpFileGuard(Option<PathBuf>);

impl TmpFileGuard {
    fn new(path: PathBuf) -> Self {
        Self(Some(path))
    }

    fn disarm(&mut self) {
        self.0 = None;
    }
}

impl Drop for TmpFileGuard {
    fn drop(&mut self) {
        if let Some(path) = self.0.take() {
            let _ = std::fs::remove_file(path);
        }
    }
}

fn flush_run(dir: &Path, runs: &mut RunGuard, buf: &mut Vec<Key>) -> Result<PathBuf> {
    buf.sort_unstable();
    buf.dedup();
    let (path, f) = runs.create_run(dir)?;
    let mut w = BufWriter::with_capacity(256 << 10, f);
    for k in buf.iter() {
        for word in k {
            w.write_all(&word.to_le_bytes())?;
        }
    }
    w.flush()?;
    buf.clear();
    Ok(path)
}

fn write_sorted(
    dir: &Path,
    entries_count: u64,
    mut stream: impl FnMut(&mut dyn FnMut(Key) -> Result<()>) -> Result<()>,
) -> Result<ReifierInfo> {
    if entries_count == 0 {
        let _ = std::fs::remove_file(dir.join(FILE));
        return Ok(ReifierInfo::empty());
    }
    let tmp = dir.join(format!("{FILE}.tmp"));
    let file = File::create(&tmp)?;
    let mut guard = TmpFileGuard::new(tmp.clone());
    let mut w = BufWriter::with_capacity(256 << 10, file);
    let mut hasher = Sha256::new();
    let mut write_hashed = |bytes: &[u8]| -> Result<()> {
        hasher.update(bytes);
        w.write_all(bytes)?;
        Ok(())
    };
    let mut header = [0u8; HEADER];
    header[..8].copy_from_slice(MAGIC);
    header[8..12].copy_from_slice(&VERSION.to_le_bytes());
    // [12..16] reserved = 0
    header[16..24].copy_from_slice(&entries_count.to_le_bytes());
    // [24..32] reserved = 0
    write_hashed(&header)?;

    let mut written = 0u64;
    let mut prev: Option<Key> = None;
    let mut raw = [0u8; ENTRY];
    stream(&mut |k| {
        if let Some(p) = prev
            && k <= p
        {
            return Err(corrupt("writer received unordered or duplicate key"));
        }
        prev = Some(k);
        for (i, word) in k.iter().enumerate() {
            raw[i * 8..(i + 1) * 8].copy_from_slice(&word.to_le_bytes());
        }
        write_hashed(&raw)?;
        written += 1;
        Ok(())
    })?;
    if written != entries_count {
        return Err(corrupt(format!(
            "writer emitted {written} entries, expected {entries_count}"
        )));
    }
    let body_len = (HEADER as u64) + written * (ENTRY as u64);
    let digest = hasher.finalize();
    w.write_all(&body_len.to_le_bytes())?;
    w.write_all(&digest[..8])?;
    w.write_all(END)?;
    w.flush()?;
    w.into_inner()
        .map_err(|e| Error::Io(e.into_error()))?
        .sync_all()?;
    let final_path = dir.join(FILE);
    std::fs::rename(&tmp, &final_path)?;
    guard.disarm();
    let bytes = body_len + (FOOTER as u64);
    Ok(ReifierInfo {
        format: VERSION,
        entries: written,
        bytes,
    })
}

fn read_key(r: &mut BufReader<File>) -> Result<Option<Key>> {
    let mut raw = [0u8; ENTRY];
    let mut off = 0;
    while off < ENTRY {
        match r.read(&mut raw[off..])? {
            0 if off == 0 => return Ok(None),
            0 => return Err(corrupt("truncated temporary sort run")),
            n => off += n,
        }
    }
    Ok(Some([
        u64::from_le_bytes(raw[0..8].try_into().unwrap()),
        u64::from_le_bytes(raw[8..16].try_into().unwrap()),
        u64::from_le_bytes(raw[16..24].try_into().unwrap()),
        u64::from_le_bytes(raw[24..32].try_into().unwrap()),
    ]))
}

/// Merge `run_paths` (each sorted) into strictly ascending unique keys, invoking `emit`
/// for each distinct key.
fn merge_unique(
    run_paths: &[PathBuf],
    interrupt: &dyn Fn() -> Result<()>,
    mut emit: impl FnMut(Key) -> Result<()>,
) -> Result<()> {
    let mut readers: Vec<BufReader<File>> = run_paths
        .iter()
        .map(|p| Ok(BufReader::with_capacity(64 << 10, File::open(p)?)))
        .collect::<Result<_>>()?;
    let mut heap: BinaryHeap<Reverse<(Key, usize)>> = BinaryHeap::new();
    for (idx, r) in readers.iter_mut().enumerate() {
        if let Some(k) = read_key(r)? {
            heap.push(Reverse((k, idx)));
        }
    }
    let mut prev: Option<Key> = None;
    let mut steps = 0usize;
    while let Some(Reverse((k, idx))) = heap.pop() {
        if steps.is_multiple_of(65_536) {
            interrupt()?;
        }
        steps += 1;
        if prev != Some(k) {
            prev = Some(k);
            emit(k)?;
        }
        if let Some(next) = read_key(&mut readers[idx])? {
            heap.push(Reverse((next, idx)));
        }
    }
    Ok(())
}

/// Reduce `active` runs in multi-level batches of at most `max_fanin` until at most
/// `max_fanin` runs remain, removing consumed runs after each level to bound disk use.
fn reduce_runs(
    dir: &Path,
    runs: &mut RunGuard,
    mut active: Vec<PathBuf>,
    max_fanin: usize,
    interrupt: &dyn Fn() -> Result<()>,
) -> Result<Vec<PathBuf>> {
    let fanin = max_fanin.max(2);
    while active.len() > fanin {
        let mut next_level = Vec::with_capacity(active.len().div_ceil(fanin));
        for chunk in active.chunks(fanin) {
            if chunk.len() == 1 {
                next_level.push(chunk[0].clone());
                continue;
            }
            let (out_path, f) = runs.create_run(dir)?;
            let mut w = BufWriter::with_capacity(256 << 10, f);
            merge_unique(chunk, interrupt, |k| {
                for word in k {
                    w.write_all(&word.to_le_bytes())?;
                }
                Ok(())
            })?;
            w.flush()?;
            next_level.push(out_path);
        }
        for old in active {
            if !next_level.contains(&old) {
                let _ = std::fs::remove_file(&old);
            }
        }
        active = next_level;
    }
    Ok(active)
}

/// Collect `(graph, term_hash, reifier, triple_term)` keys for `reifies` from `pos`,
/// spilling sorted runs of at most `chunk_entries` into `spill_dir` when needed.
fn collect_pos_runs(
    spill_dir: &mut impl FnMut() -> Result<PathBuf>,
    vocab: &Vocab,
    pos: &PermIndex,
    reifies: Id,
    chunk_entries: usize,
    interrupt: &dyn Fn() -> Result<()>,
) -> Result<(Vec<Key>, RunGuard, Vec<PathBuf>)> {
    let chunk_entries = chunk_entries.max(1);
    let (lo, hi) = pos.block_range(&[reifies.0]);
    let mut runs = RunGuard::new();
    let mut active: Vec<PathBuf> = Vec::new();
    let mut buf: Vec<Key> = Vec::new();
    if lo == hi {
        return Ok((buf, runs, active));
    }
    let mut last_o: Option<Id> = None;
    let mut last_hashes: (Option<u64>, Option<u64>) = (None, None);

    for b in lo..hi {
        interrupt()?;
        let block = pos.decode_block(b)?;
        for i in 0..block.len() {
            let k = block.key(i);
            if k[0] < reifies.0 {
                continue;
            }
            if k[0] > reifies.0 {
                break;
            }
            let o = Id(k[1]);
            let s = Id(k[2]);
            let g = Id(k[3]);
            if o.tag() != Tag::Vocab || !vocab.is_triple(o.payload()) {
                continue;
            }
            let hashes = if last_o == Some(o) {
                last_hashes
            } else {
                let h = vocab
                    .get_with_checked(o.payload(), |_| Ok::<(), ()>(()), triple_key_term_hashes)
                    .ok()
                    .flatten()
                    .flatten()
                    .unwrap_or((None, None));
                last_o = Some(o);
                last_hashes = h;
                h
            };
            for h in [hashes.0, hashes.1].into_iter().flatten() {
                if buf.len() >= chunk_entries {
                    let dir = spill_dir()?;
                    let path = flush_run(&dir, &mut runs, &mut buf)?;
                    active.push(path);
                }
                buf.push([g.0, h, s.0, o.0]);
            }
        }
    }
    Ok((buf, runs, active))
}

/// Build `reifiers.bin` in `dir` from the generation's `vocab` and `Perm::Pos` index.
/// When `reifies` is `None` or the generation contains no `rdf:reifies` triple-term quads
/// with IRI subjects or objects, no file is written and [`ReifierInfo::empty`] is returned.
pub fn build(
    dir: &Path,
    vocab: &Vocab,
    pos: &PermIndex,
    reifies: Option<Id>,
    interrupt: &dyn Fn() -> Result<()>,
) -> Result<ReifierInfo> {
    build_with_limits(
        dir,
        vocab,
        pos,
        reifies,
        SORT_CHUNK_ENTRIES,
        MAX_MERGE_FANIN,
        interrupt,
    )
}

fn build_with_limits(
    dir: &Path,
    vocab: &Vocab,
    pos: &PermIndex,
    reifies: Option<Id>,
    chunk_entries: usize,
    max_fanin: usize,
    interrupt: &dyn Fn() -> Result<()>,
) -> Result<ReifierInfo> {
    let Some(reifies) = reifies else {
        let _ = std::fs::remove_file(dir.join(FILE));
        return Ok(ReifierInfo::empty());
    };
    let (mut buf, mut runs, mut active) = collect_pos_runs(
        &mut || Ok(dir.to_path_buf()),
        vocab,
        pos,
        reifies,
        chunk_entries,
        interrupt,
    )?;

    if active.is_empty() {
        buf.sort_unstable();
        buf.dedup();
        let count = buf.len() as u64;
        return write_sorted(dir, count, |emit| {
            for (i, &k) in buf.iter().enumerate() {
                if i % 65_536 == 0 {
                    interrupt()?;
                }
                emit(k)?;
            }
            Ok(())
        });
    }

    if !buf.is_empty() {
        let path = flush_run(dir, &mut runs, &mut buf)?;
        active.push(path);
    }
    // Free the in-memory sort buffer before allocating merge readers.
    drop(buf);

    let active = reduce_runs(dir, &mut runs, active, max_fanin, interrupt)?;
    let total = if active.len() == 1 {
        let len = std::fs::metadata(&active[0])?.len();
        if !len.is_multiple_of(ENTRY as u64) {
            return Err(corrupt("truncated temporary sort run"));
        }
        len / (ENTRY as u64)
    } else {
        let mut count = 0u64;
        merge_unique(&active, interrupt, |_| {
            count += 1;
            Ok(())
        })?;
        count
    };
    write_sorted(dir, total, |emit| merge_unique(&active, interrupt, emit))
}

// ------------------------------------------------------------------ reading ------

/// An opened, memory-mapped `reifiers.bin`.
pub struct ReifierFile {
    map: memmap2::Mmap,
    entries: u64,
}

impl std::fmt::Debug for ReifierFile {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ReifierFile")
            .field("entries", &self.entries)
            .field("bytes", &self.map.len())
            .finish()
    }
}

impl ReifierFile {
    /// Open `reifiers.bin` in `dir`: `None` when absent. A version this build does not
    /// read returns [`Error::Unsupported`] with [`crate::formats::REIFIERS`]'s message,
    /// and a damaged file returns [`Error::Corrupt`].
    pub fn open(dir: &Path) -> Result<Option<ReifierFile>> {
        let path = dir.join(FILE);
        let f = match File::open(&path) {
            Ok(f) => f,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(e.into()),
        };
        if f.metadata()?.len() < (HEADER + FOOTER) as u64 {
            return Err(corrupt("shorter than its header"));
        }
        // SAFETY: the file is immutable once its generation is published, and a
        // generation's files are removed only after no reader holds the generation.
        let map = unsafe { memmap2::Mmap::map(&f)? };
        let len = map.len();
        if len < HEADER + FOOTER {
            return Err(corrupt("shorter than its header"));
        }
        if &map[..8] != MAGIC {
            return Err(corrupt("not a reifier index file (bad magic)"));
        }
        let version = u32::from_le_bytes(map[8..12].try_into().unwrap());
        let kind = &crate::formats::REIFIERS;
        if !kind.reads(version) {
            return Err(Error::Unsupported(format!(
                "{} has reifiers version {version}; Sparkles {} reads version {}. The \
                 reifier index is not used: claim lookups fall back to scanning, and the next \
                 compaction writes it again",
                path.display(),
                crate::formats::SPARKLES_VERSION,
                kind.readable()
            )));
        }
        let reserved32 = u32::from_le_bytes(map[12..16].try_into().unwrap());
        let entries = u64::from_le_bytes(map[16..24].try_into().unwrap());
        let reserved64 = u64::from_le_bytes(map[24..32].try_into().unwrap());
        if reserved32 != 0 || reserved64 != 0 {
            return Err(corrupt("non-zero reserved header bytes"));
        }
        let data_bytes = entries
            .checked_mul(ENTRY as u64)
            .ok_or_else(|| corrupt("entry count overflow"))?;
        let body = (HEADER as u64)
            .checked_add(data_bytes)
            .ok_or_else(|| corrupt("body length overflow"))?;
        let expected_len = body
            .checked_add(FOOTER as u64)
            .ok_or_else(|| corrupt("file length overflow"))?;
        if expected_len != len as u64 {
            return Err(corrupt("its length does not match its header"));
        }
        let foot = body as usize;
        let stored_body = u64::from_le_bytes(map[foot..foot + 8].try_into().unwrap());
        if stored_body != body || &map[foot + 16..foot + 24] != END {
            return Err(corrupt("incomplete or invalid footer"));
        }
        Ok(Some(ReifierFile { map, entries }))
    }

    pub fn entries(&self) -> u64 {
        self.entries
    }

    pub fn bytes(&self) -> u64 {
        self.map.len() as u64
    }

    /// Verify the file's SHA-256 footer checksum, polling `check` every 64 KiB chunk.
    pub fn verify_checksum_with<E: From<Error>>(
        &self,
        mut check: impl FnMut() -> std::result::Result<(), E>,
    ) -> std::result::Result<(), E> {
        let foot = self.map.len() - FOOTER;
        let mut hasher = Sha256::new();
        for chunk in self.map[..foot].chunks(HASH_CHECK_CHUNK) {
            check()?;
            hasher.update(chunk);
        }
        check()?;
        let digest = hasher.finalize();
        if digest[..8] != self.map[foot + 8..foot + 16] {
            return Err(corrupt("checksum mismatch").into());
        }
        Ok(())
    }

    /// Verify the file's SHA-256 footer checksum.
    pub fn verify_checksum(&self) -> Result<()> {
        self.verify_checksum_with::<Error>(|| Ok(()))
    }

    #[inline]
    pub fn entry(&self, index: u64) -> Key {
        let off = HEADER + (index as usize) * ENTRY;
        let slice = &self.map[off..off + ENTRY];
        [
            u64::from_le_bytes(slice[0..8].try_into().unwrap()),
            u64::from_le_bytes(slice[8..16].try_into().unwrap()),
            u64::from_le_bytes(slice[16..24].try_into().unwrap()),
            u64::from_le_bytes(slice[24..32].try_into().unwrap()),
        ]
    }

    /// Iterate all `(reifier_id, triple_term_id)` pairs in `graph` matching `hash`.
    pub fn lookup(&self, graph: Id, hash: u64) -> LookupIter<'_> {
        let target = (graph.0, hash);
        let (mut lo, mut hi) = (0u64, self.entries);
        while lo < hi {
            let mid = lo + (hi - lo) / 2;
            let e = self.entry(mid);
            if (e[0], e[1]) < target {
                lo = mid + 1;
            } else {
                hi = mid;
            }
        }
        LookupIter {
            file: self,
            graph: graph.0,
            hash,
            pos: lo,
        }
    }
}

/// Compare an opened `reifiers.bin` against its `meta.json` `reifiers` record.
/// Returns a diagnostic message on any format, entry-count, or byte-size mismatch.
pub(crate) fn metadata_mismatch(
    dir: &Path,
    file: &ReifierFile,
    recorded: Option<&ReifierInfo>,
) -> Option<String> {
    let info = recorded?;
    if info.format != VERSION {
        return Some(format!(
            "{}: {FILE} is version {VERSION}, meta.json records format {}",
            dir.display(),
            info.format
        ));
    }
    if info.entries != file.entries() {
        return Some(format!(
            "{}: {FILE} holds {} entries, meta.json records {}",
            dir.display(),
            file.entries(),
            info.entries
        ));
    }
    if info.bytes != file.bytes() {
        return Some(format!(
            "{}: {FILE} is {} bytes, meta.json records {} bytes",
            dir.display(),
            file.bytes(),
            info.bytes
        ));
    }
    None
}

/// Zero-allocation iterator over matching entries of a [`ReifierFile`].
pub struct LookupIter<'a> {
    file: &'a ReifierFile,
    graph: u64,
    hash: u64,
    pos: u64,
}

impl Iterator for LookupIter<'_> {
    type Item = (Id, Id);

    #[inline]
    fn next(&mut self) -> Option<Self::Item> {
        if self.pos >= self.file.entries {
            return None;
        }
        let e = self.file.entry(self.pos);
        if e[0] != self.graph || e[1] != self.hash {
            self.pos = self.file.entries;
            return None;
        }
        self.pos += 1;
        Some((Id(e[2]), Id(e[3])))
    }
}

/// The state of a generation's reifier proposition-term index.
#[derive(Clone, Debug)]
pub struct Loaded {
    /// Opened `reifiers.bin` when the generation holds indexed reifier entries.
    pub file: Option<Arc<ReifierFile>>,
    /// Whether this generation is fully covered by the index (either via `file`, or
    /// because it is known to hold zero reifiers). `false` for legacy pre-index
    /// generations or when an unreadable file was left unused.
    pub indexed: bool,
    /// Diagnostic when an expected `reifiers.bin` could not be opened.
    pub missing: Option<String>,
}

impl Default for Loaded {
    fn default() -> Self {
        Self::empty()
    }
}

impl Loaded {
    /// An empty generation with zero quads and zero reifiers.
    pub fn empty() -> Self {
        Self {
            file: None,
            indexed: true,
            missing: None,
        }
    }

    /// Open a generation's reifier index in `dir` given `meta.reifiers` and `meta.quads`,
    /// verifying metadata consistency and the file checksum before enabling indexed reads.
    pub fn open(dir: &Path, recorded: Option<&ReifierInfo>, quads: u64) -> Self {
        Self::open_controlled(dir, recorded, quads, || Ok(()))
            .expect("uncontrolled reifier open never cancels")
    }

    /// Controlled open for cold historical generations: polls `check` during checksum
    /// verification and propagates cancellation, timeout, or budget errors while routing
    /// file corruption or unsupported versions through the derived-index fallback.
    pub(crate) fn open_controlled(
        dir: &Path,
        recorded: Option<&ReifierInfo>,
        quads: u64,
        mut check: impl FnMut() -> Result<()>,
    ) -> Result<Self> {
        check()?;
        match ReifierFile::open(dir) {
            Ok(Some(f)) => {
                if let Some(msg) = metadata_mismatch(dir, &f, recorded) {
                    tracing::warn!(target: "sparkles::reifiers", "{msg}");
                    return Ok(Self {
                        file: None,
                        indexed: false,
                        missing: Some(msg),
                    });
                }
                match f.verify_checksum_with(&mut check) {
                    Ok(()) => {}
                    Err(e @ (Error::Cancelled | Error::Timeout | Error::BudgetExceeded(_))) => {
                        return Err(e);
                    }
                    Err(e) => {
                        let msg = e.to_string();
                        tracing::warn!(target: "sparkles::reifiers", "{msg}");
                        return Ok(Self {
                            file: None,
                            indexed: false,
                            missing: Some(msg),
                        });
                    }
                }
                Ok(Self {
                    file: Some(Arc::new(f)),
                    indexed: true,
                    missing: None,
                })
            }
            Ok(None) => {
                if quads == 0
                    || recorded
                        .is_some_and(|r| r.format == VERSION && r.entries == 0 && r.bytes == 0)
                {
                    return Ok(Self::empty());
                }
                if let Some(info) = recorded
                    && info.format > 0
                    && (info.entries > 0 || info.bytes > 0)
                {
                    let msg = format!(
                        "{}: meta.json records {} reifier index entries ({} bytes) but {FILE} is missing",
                        dir.display(),
                        info.entries,
                        info.bytes
                    );
                    tracing::warn!(target: "sparkles::reifiers", "{msg}");
                    return Ok(Self {
                        file: None,
                        indexed: false,
                        missing: Some(msg),
                    });
                }
                // Pre-index generation built before item 766: fall back to scan until compacted.
                Ok(Self {
                    file: None,
                    indexed: false,
                    missing: None,
                })
            }
            Err(e) => {
                let msg = e.to_string();
                tracing::warn!(target: "sparkles::reifiers", "{msg}");
                Ok(Self {
                    file: None,
                    indexed: false,
                    missing: Some(msg),
                })
            }
        }
    }

    /// Whether lookups on this generation can use the reifier index without scanning.
    #[inline]
    pub fn is_indexed(&self) -> bool {
        self.indexed
    }

    /// Bytes on disk for `reifiers.bin`, 0 when absent.
    pub fn disk_bytes(&self) -> u64 {
        self.file.as_ref().map_or(0, |f| f.bytes())
    }
}

// ------------------------------------------------------------------ check --------

/// Verify `reifiers.bin` in `dir` against `vocab` and `Perm::Pos` (for `sparkles check`),
/// spilling to a temporary directory outside `dir` when the index exceeds the in-memory
/// sort chunk so memory stays bounded and read-only database directories remain verifiable.
pub fn verify(dir: &Path, fsst: bool, max: usize) -> Result<(Vec<String>, String)> {
    verify_with_limits(dir, fsst, max, SORT_CHUNK_ENTRIES, MAX_MERGE_FANIN)
}

fn verify_with_limits(
    dir: &Path,
    fsst: bool,
    max: usize,
    chunk_entries: usize,
    max_fanin: usize,
) -> Result<(Vec<String>, String)> {
    let Some(file) = ReifierFile::open(dir)? else {
        return Ok((Vec::new(), "no reifier index".into()));
    };
    let mut problems: Vec<String> = Vec::new();
    let mut note = |s: String| {
        if problems.len() < max {
            problems.push(s);
        }
    };
    if let Err(e) = file.verify_checksum() {
        note(e.to_string());
        return Ok((problems, "damaged".into()));
    }
    let mut prev: Option<Key> = None;
    for i in 0..file.entries() {
        let e = file.entry(i);
        if let Some(p) = prev
            && e <= p
        {
            note(format!(
                "entry {i} is not strictly greater than entry {}",
                i - 1
            ));
            break;
        }
        prev = Some(e);
    }
    let vocab = Vocab::open_codec(dir, fsst)?;
    let pos = PermIndex::open(dir, Perm::Pos)?;
    let reifies = vocab.find(RDF_REIFIES_KEY).ok().map(Id::vocab);
    let mut expected_count = 0u64;
    let mut diff_noted = false;
    let mut compare_key = |want: Key| -> Result<()> {
        let idx = expected_count;
        expected_count += 1;
        if !diff_noted && idx < file.entries() && file.entry(idx) != want {
            diff_noted = true;
            note(format!("entry {idx} differs from POS"));
        }
        Ok(())
    };
    if let Some(reifies) = reifies {
        let mut scratch_dir: Option<tempfile::TempDir> = None;
        let mut get_spill_dir = || -> Result<PathBuf> {
            if scratch_dir.is_none() {
                scratch_dir = Some(tempfile::tempdir()?);
            }
            Ok(scratch_dir.as_ref().unwrap().path().to_path_buf())
        };
        let (mut buf, mut runs, mut active) = collect_pos_runs(
            &mut get_spill_dir,
            &vocab,
            &pos,
            reifies,
            chunk_entries,
            &|| Ok(()),
        )?;
        if active.is_empty() {
            buf.sort_unstable();
            buf.dedup();
            for k in buf {
                compare_key(k)?;
            }
        } else {
            let spill_path = get_spill_dir()?;
            if !buf.is_empty() {
                let path = flush_run(&spill_path, &mut runs, &mut buf)?;
                active.push(path);
            }
            drop(buf);
            let active = reduce_runs(&spill_path, &mut runs, active, max_fanin, &|| Ok(()))?;
            merge_unique(&active, &|| Ok(()), compare_key)?;
        }
    }
    if expected_count != file.entries() {
        note(format!(
            "file holds {} entries, POS has {expected_count}",
            file.entries()
        ));
    }
    let summary = format!("{} entries, {} bytes", file.entries(), file.bytes());
    Ok((problems, summary))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::id::write_term_key;
    use oxrdf::{BlankNode, Literal, NamedNode, Term, Triple};

    #[test]
    fn triple_key_hashes_match_iri_subject_and_object_only() {
        let a = NamedNode::new_unchecked("urn:a");
        let p = NamedNode::new_unchecked("urn:p");
        let b = NamedNode::new_unchecked("urn:b");

        let mut key = Vec::new();
        write_term_key(
            &Term::Triple(Box::new(Triple::new(a.clone(), p.clone(), b.clone()))),
            &mut key,
        );
        assert_eq!(
            triple_key_term_hashes(&key),
            Some((Some(term_hash(b"urn:a")), Some(term_hash(b"urn:b"))))
        );

        // same subject and object deduplicates
        key.clear();
        write_term_key(
            &Term::Triple(Box::new(Triple::new(a.clone(), p.clone(), a.clone()))),
            &mut key,
        );
        assert_eq!(
            triple_key_term_hashes(&key),
            Some((Some(term_hash(b"urn:a")), None))
        );

        // literal object indexes only subject
        key.clear();
        write_term_key(
            &Term::Triple(Box::new(Triple::new(
                a.clone(),
                p.clone(),
                Literal::new_simple_literal("v"),
            ))),
            &mut key,
        );
        assert_eq!(
            triple_key_term_hashes(&key),
            Some((Some(term_hash(b"urn:a")), None))
        );

        // blank-node subject indexes only IRI object
        key.clear();
        write_term_key(
            &Term::Triple(Box::new(Triple::new(BlankNode::new_unchecked("b0"), p, b))),
            &mut key,
        );
        assert_eq!(
            triple_key_term_hashes(&key),
            Some((None, Some(term_hash(b"urn:b"))))
        );

        // non-triple key returns None
        assert_eq!(triple_key_term_hashes(b"<urn:a"), None);
    }

    #[test]
    fn file_roundtrip_and_unknown_version_refusal() {
        let dir = tempfile::tempdir().unwrap();
        let rows = vec![
            [1, 10, 100, 1000],
            [1, 10, 101, 1001],
            [1, 20, 102, 1002],
            [2, 10, 103, 1003],
        ];
        let info = write_sorted(dir.path(), rows.len() as u64, |emit| {
            for &r in &rows {
                emit(r)?;
            }
            Ok(())
        })
        .unwrap();
        assert_eq!(info.format, VERSION);
        assert_eq!(info.entries, 4);
        assert!(info.bytes > 0);

        let f = ReifierFile::open(dir.path()).unwrap().unwrap();
        f.verify_checksum().unwrap();
        let got: Vec<(Id, Id)> = f.lookup(Id(1), 10).collect();
        assert_eq!(got, vec![(Id(100), Id(1000)), (Id(101), Id(1001))]);
        assert_eq!(
            f.lookup(Id(1), 20).collect::<Vec<_>>(),
            vec![(Id(102), Id(1002))]
        );
        assert_eq!(
            f.lookup(Id(2), 10).collect::<Vec<_>>(),
            vec![(Id(103), Id(1003))]
        );
        assert!(f.lookup(Id(1), 99).next().is_none());

        // unknown future version is refused cleanly
        let path = dir.path().join(FILE);
        let mut bytes = std::fs::read(&path).unwrap();
        bytes[8..12].copy_from_slice(&99u32.to_le_bytes());
        std::fs::write(&path, &bytes).unwrap();
        let err = ReifierFile::open(dir.path()).unwrap_err();
        assert!(matches!(err, Error::Unsupported(_)), "{err:?}");
        assert!(err.to_string().contains("version 99"), "{err}");
        assert!(err.to_string().contains("next compaction"), "{err}");
    }

    #[test]
    fn multi_level_spill_merge_and_bounded_verify_match_in_memory_build() {
        use crate::builder::{BuildOptions, Builder};
        use crate::io::{RdfFormat, Source};

        let gen_dir = tempfile::tempdir().unwrap();
        let mut nq = String::new();
        for i in 0..45u32 {
            use std::fmt::Write;
            // Include duplicate endpoints (same subject/object) and distinct endpoints across graphs.
            let g = if i % 2 == 0 { "urn:g1" } else { "urn:g2" };
            let obj = if i % 5 == 0 {
                format!("<urn:s{i}>")
            } else {
                format!("<urn:o{}>", i % 13)
            };
            writeln!(
                nq,
                "<urn:c{i}> <{RDF_REIFIES}> <<( <urn:s{i}> <urn:p> {obj} )>> <{g}> ."
            )
            .unwrap();
        }
        let builder = Builder::new(
            gen_dir.path(),
            BuildOptions {
                threads: 1,
                ..Default::default()
            },
        )
        .unwrap();
        builder
            .add_source(&Source::from_bytes(
                nq.into_bytes(),
                RdfFormat::NQuads,
                None,
            ))
            .unwrap();
        let meta = builder.finish().unwrap();
        let vocab = Vocab::open_codec(
            gen_dir.path(),
            meta.has_feature(crate::builder::FEATURE_FSST_VOCAB),
        )
        .unwrap();
        let pos = PermIndex::open(gen_dir.path(), Perm::Pos).unwrap();
        let reifies = Some(Id::vocab(vocab.find(RDF_REIFIES_KEY).unwrap()));

        // Build in memory into dir_mem and with multi-level spill (chunk=7, fanin=2) into dir_spill.
        let dir_mem = tempfile::tempdir().unwrap();
        let dir_spill = tempfile::tempdir().unwrap();
        let info_mem = build_with_limits(
            dir_mem.path(),
            &vocab,
            &pos,
            reifies,
            10_000,
            64,
            &|| Ok(()),
        )
        .unwrap();
        let info_spill =
            build_with_limits(dir_spill.path(), &vocab, &pos, reifies, 7, 2, &|| Ok(())).unwrap();
        assert_eq!(info_spill, info_mem);
        assert!(info_spill.entries > 45);
        assert_eq!(
            std::fs::read(dir_spill.path().join(FILE)).unwrap(),
            std::fs::read(dir_mem.path().join(FILE)).unwrap()
        );
        // No leftover .tmp spill runs in dir_spill.
        let leftovers: Vec<_> = std::fs::read_dir(dir_spill.path())
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .filter(|n| n.ends_with(".tmp"))
            .collect();
        assert!(leftovers.is_empty(), "leftover tmp files: {leftovers:?}");

        // Bounded streaming verify_with_limits with multi-level spill agrees with POS.
        let (problems, _) = verify_with_limits(
            gen_dir.path(),
            meta.has_feature(crate::builder::FEATURE_FSST_VOCAB),
            10,
            7,
            2,
        )
        .unwrap();
        assert!(problems.is_empty(), "{problems:?}");
    }

    #[test]
    fn loaded_open_rejects_metadata_mismatches_and_checksum_damage() {
        let dir = tempfile::tempdir().unwrap();
        let rows = vec![[1, 10, 100, 1000], [1, 20, 101, 1001]];
        let info = write_sorted(dir.path(), rows.len() as u64, |emit| {
            for &r in &rows {
                emit(r)?;
            }
            Ok(())
        })
        .unwrap();

        // Matching metadata succeeds.
        let loaded = Loaded::open(dir.path(), Some(&info), 2);
        assert!(loaded.file.is_some() && loaded.is_indexed() && loaded.missing.is_none());

        // Format mismatch rejected even when file is valid v1.
        let bad_fmt = ReifierInfo {
            format: 0,
            ..info.clone()
        };
        let loaded = Loaded::open(dir.path(), Some(&bad_fmt), 2);
        assert!(loaded.file.is_none() && !loaded.is_indexed() && loaded.missing.is_some());

        // Entry count mismatch rejected.
        let bad_entries = ReifierInfo {
            entries: 99,
            ..info.clone()
        };
        let loaded = Loaded::open(dir.path(), Some(&bad_entries), 2);
        assert!(loaded.file.is_none() && !loaded.is_indexed() && loaded.missing.is_some());

        // Byte length mismatch rejected.
        let bad_bytes = ReifierInfo {
            bytes: info.bytes + 1,
            ..info.clone()
        };
        let loaded = Loaded::open(dir.path(), Some(&bad_bytes), 2);
        assert!(loaded.file.is_none() && !loaded.is_indexed() && loaded.missing.is_some());

        // Controlled open propagates cancellation during checksum verification.
        let err = Loaded::open_controlled(dir.path(), Some(&info), 2, || Err(Error::Cancelled))
            .unwrap_err();
        assert!(matches!(err, Error::Cancelled));

        // Flipping one byte inside an entry payload keeps framing valid but fails checksum.
        let path = dir.path().join(FILE);
        let mut bytes = std::fs::read(&path).unwrap();
        bytes[HEADER + 16] ^= 1;
        std::fs::write(&path, &bytes).unwrap();
        let loaded = Loaded::open(dir.path(), Some(&info), 2);
        assert!(loaded.file.is_none() && !loaded.is_indexed());
        assert!(loaded.missing.as_deref().unwrap().contains("checksum"));
    }
}
