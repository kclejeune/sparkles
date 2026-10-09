//! The merge of the partial vocabularies, in parallel over ranges of keys.
//!
//! Each batch's partial vocabulary is a file of its sorted distinct keys in blocks (see
//! [`write_partial`]), with a sample of every [`SAMPLE_EVERY`]th key and the byte offset
//! of its block kept in memory. Keys taken from the samples split the key space into
//! ranges, and each range is merged by a task of its own: it seeks every batch file to
//! the range's first key, merges the keys of the range, keeps the distinct ones
//! front-coded, and writes each batch key's id, in rank order, to the map file (see
//! [`Maps`]). The ranges are appended to the vocabulary in key order as they complete.
//!
//! A range is about [`RANGE_BYTES`] of partial vocabulary, so that it can wait in
//! memory until the ranges before it are appended. The ranges wait in memory as long as
//! they hold at most [`RANGE_MEMORY`] together. A range that would go past that writes its
//! keys to a file instead.

use super::iostat::{Tmp, TmpBytes};
use crate::error::{Error, Result};
use crate::index::read_varint_checked;
use crate::vocab::{VocabWriter, write_varint};
use parking_lot::Mutex;
use std::cmp::Reverse;
use std::collections::{BTreeMap, BinaryHeap};
use std::fs::File;
use std::io::{BufReader, BufWriter, Read, Seek, SeekFrom, Write};
use std::os::unix::fs::FileExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};

/// Every this many keys of a partial vocabulary, a sample is kept.
pub(super) const SAMPLE_EVERY: u64 = 4096;
/// Keys per block of a partial vocabulary at most.
const BLOCK_KEYS: u64 = 256;
/// A block of a partial vocabulary ends once its keys take this many bytes front-coded.
/// This bounds the memory of a merge, which holds one decoded block per batch and range.
const BLOCK_BYTES: usize = 16 << 10;
/// Map entries buffered per batch before they are written.
const MAP_BUF: usize = 4096;
/// Bytes of map records a range collects before it appends them to the map file.
const MAP_FLUSH: usize = 1 << 20;
/// Bytes of partial vocabularies per range of the merge, about.
const RANGE_BYTES: u64 = 8 << 20;
/// The most ranges a merge splits the keys into.
const MAX_RANGES: usize = 4096;
/// Bytes of merged ranges held in memory at once at most.
const RANGE_MEMORY: u64 = 512 << 20;
/// A range takes memory from [`RANGE_MEMORY`] in steps of this many bytes.
const RANGE_STEP: u64 = 1 << 20;

/// A sampled key of a partial vocabulary: its rank, the byte offset of the block it
/// starts, and the key.
pub(super) type Sample = (u64, u64, Box<[u8]>);

/// One batch's partial vocabulary.
pub(super) struct Partial {
    pub voc: PathBuf,
    pub keys: u64,
    /// the size of the file
    pub bytes: u64,
    pub samples: Vec<Sample>,
}

/// Write the sorted distinct `keys` of a batch as a partial vocabulary. Returns its
/// samples and its size in bytes.
///
/// The file is a sequence of blocks of at most [`BLOCK_KEYS`] keys. A block holds its
/// keys front-coded (the length of the prefix shared with the previous key of the
/// block and the length of the rest, as varints, then the rest), compressed with LZ4,
/// and is written as its compressed length (`u32`) followed by the compressed bytes. A
/// block starts at every [`SAMPLE_EVERY`]th key, so that a merge can start reading at a
/// sample.
pub(super) fn write_partial<'a>(
    path: &Path,
    keys: impl Iterator<Item = &'a [u8]>,
) -> Result<(Vec<Sample>, u64)> {
    let mut w = BufWriter::with_capacity(1 << 20, File::create(path)?);
    let mut samples = Vec::new();
    let mut raw = Vec::new();
    let mut prev: &[u8] = &[];
    let mut in_block = 0u64;
    let mut offset = 0u64;
    for (rank, k) in keys.enumerate() {
        let rank = rank as u64;
        let sample = rank.is_multiple_of(SAMPLE_EVERY);
        if in_block > 0 && (sample || in_block == BLOCK_KEYS || raw.len() >= BLOCK_BYTES) {
            offset += write_block(&mut w, &raw)?;
            raw.clear();
            in_block = 0;
        }
        if sample {
            samples.push((rank, offset, k.into()));
        }
        let shared = if in_block == 0 {
            0
        } else {
            prev.iter().zip(k).take_while(|(a, b)| a == b).count()
        };
        write_varint(&mut raw, shared as u64);
        write_varint(&mut raw, (k.len() - shared) as u64);
        raw.extend_from_slice(&k[shared..]);
        prev = k;
        in_block += 1;
    }
    if in_block > 0 {
        offset += write_block(&mut w, &raw)?;
    }
    w.flush()?;
    Ok((samples, offset))
}

fn write_block(w: &mut impl Write, raw: &[u8]) -> Result<u64> {
    let c = lz4_flex::compress_prepend_size(raw);
    w.write_all(&(c.len() as u32).to_le_bytes())?;
    w.write_all(&c)?;
    Ok(4 + c.len() as u64)
}

/// Where some map entries of a batch are: their range and their bytes in the map file.
struct MapRecord {
    range: usize,
    offset: u64,
    len: u32,
    count: u32,
}

/// The maps of all batches from the rank of a key in its batch to the key's global id.
///
/// While a range merges, it buffers the ids of each batch's keys in the range and
/// encodes them as records of up to [`MAP_BUF`] ids: the first id within the range as a
/// varint, then the difference of each id to the one before. A batch's ids increase with
/// its ranks, so most differences take a byte or two. The ranges append their records
/// to one map file, and the records of each batch are kept in memory in rank order.
pub(super) struct Maps {
    path: PathBuf,
    file: File,
    /// per batch, its records in rank order
    records: Vec<Vec<MapRecord>>,
    /// the first global id of each range
    starts: Vec<u64>,
}

impl Maps {
    /// The global ids of the keys of batch `part` (which has `keys` keys), in rank order.
    pub(super) fn read(&self, part: usize, keys: u64) -> Result<Vec<u64>> {
        let bad = || Error::Corrupt("vocabulary map record".into());
        let mut out = Vec::with_capacity(keys as usize);
        let mut buf = Vec::new();
        for r in self.records.get(part).ok_or_else(bad)? {
            buf.resize(r.len as usize, 0);
            self.file.read_exact_at(&mut buf, r.offset)?;
            let (mut pos, mut id) = (0, 0u64);
            for i in 0..r.count {
                let v = read_varint_checked(&buf, &mut pos).ok_or_else(bad)?;
                id = if i == 0 { v } else { id + v };
                out.push(self.starts[r.range] + id);
            }
            if pos != buf.len() {
                return Err(bad());
            }
        }
        if out.len() as u64 != keys {
            return Err(Error::Corrupt(format!(
                "vocabulary map of {} ids for {keys} keys",
                out.len()
            )));
        }
        Ok(out)
    }

    /// Delete the map file.
    pub(super) fn remove(self) -> Result<()> {
        drop(self.file);
        std::fs::remove_file(self.path)?;
        Ok(())
    }
}

/// The map file the ranges append to, and its length.
type MapFile = Mutex<(BufWriter<File>, u64)>;

/// The distinct keys of a range, front-coded.
enum RangeKeys {
    /// in memory, holding this many bytes of [`RANGE_MEMORY`]
    Memory(Vec<u8>, u64),
    File(PathBuf),
}

/// What a range of the merge produced.
struct RangeOut {
    terms: u64,
    keys: RangeKeys,
    /// the batch of each of its map records
    records: Vec<(usize, MapRecord)>,
}

/// Merge the partial vocabularies into the vocabulary of `dir` with `threads` tasks.
/// Returns the number of terms and the maps of the batches to global ids.
pub(super) fn merge(
    dir: &Path,
    tmp: &Path,
    parts: &[Partial],
    threads: usize,
    tmp_bytes: &TmpBytes,
    interrupted: &(dyn Fn() -> Result<()> + Sync),
) -> Result<(u64, Maps)> {
    // the range boundaries: quantiles of the sampled keys
    let mut all: Vec<&[u8]> = parts
        .iter()
        .flat_map(|p| p.samples.iter().map(|s| &*s.2))
        .collect();
    all.sort_unstable();
    all.dedup();
    let bytes: u64 = parts.iter().map(|p| p.bytes).sum();
    let ranges = (threads.max(1) * 4)
        .max(bytes.div_ceil(RANGE_BYTES) as usize)
        .min(MAX_RANGES)
        .min(all.len() + 1);
    let mut bounds: Vec<&[u8]> = (1..ranges).map(|i| all[i * all.len() / ranges]).collect();
    bounds.dedup();
    let ranges = bounds.len() + 1;
    let next = AtomicUsize::new(0);
    let held = AtomicU64::new(0);
    let map_path = tmp.join("vocab-map");
    let map_file: MapFile = Mutex::new((
        BufWriter::with_capacity(1 << 20, File::create(&map_path)?),
        0,
    ));
    let (tx, rx) = std::sync::mpsc::channel::<(usize, Result<RangeOut>)>();
    let mut w = VocabWriter::create(dir)?;
    let mut records: Vec<Vec<MapRecord>> = (0..parts.len()).map(|_| Vec::new()).collect();
    let mut starts = Vec::with_capacity(ranges);
    let written: Result<()> = std::thread::scope(|s| {
        for _ in 0..threads.max(1).min(ranges) {
            let tx = tx.clone();
            let (next, bounds, held, map_file) = (&next, &bounds, &held, &map_file);
            s.spawn(move || {
                loop {
                    let r = next.fetch_add(1, Ordering::Relaxed);
                    if r >= ranges {
                        return;
                    }
                    let lo = r.checked_sub(1).map(|i| bounds[i]);
                    let hi = bounds.get(r).copied();
                    let out = interrupted()
                        .and_then(|()| merge_range(tmp, parts, r, lo, hi, held, map_file));
                    let failed = out.is_err();
                    if tx.send((r, out)).is_err() || failed {
                        return;
                    }
                }
            });
        }
        drop(tx);
        // append the ranges in order as they complete
        let mut done: BTreeMap<usize, RangeOut> = BTreeMap::new();
        let mut key = Vec::new();
        for (r, out) in rx.iter() {
            let out = out.inspect_err(|_| {
                // the other threads take no further range
                next.store(ranges, Ordering::Relaxed);
            })?;
            done.insert(r, out);
            while let Some(out) = done.remove(&starts.len()) {
                let range = starts.len();
                starts.push(w.len());
                key.clear();
                match out.keys {
                    RangeKeys::Memory(bytes, reserved) => {
                        let mut r = &bytes[..];
                        for _ in 0..out.terms {
                            read_front_coded(&mut r, &mut key)?;
                            w.push(&key)?;
                        }
                        drop(bytes);
                        held.fetch_sub(reserved, Ordering::Relaxed);
                    }
                    RangeKeys::File(path) => {
                        let mut f = BufReader::with_capacity(1 << 20, File::open(&path)?);
                        tmp_bytes.add(Tmp::VocabRanges, f.get_ref().metadata()?.len());
                        for _ in 0..out.terms {
                            read_front_coded(&mut f, &mut key)?;
                            w.push(&key)?;
                        }
                        drop(f);
                        std::fs::remove_file(&path)?;
                    }
                }
                for (part, rec) in out.records {
                    debug_assert_eq!(rec.range, range);
                    records[part].push(rec);
                }
            }
        }
        if starts.len() != ranges {
            return Err(Error::Corrupt("a vocabulary range was not merged".into()));
        }
        Ok(())
    });
    let (mut mw, map_len) = map_file.into_inner();
    let flushed = mw.flush();
    drop(mw);
    let maps = File::open(&map_path).map(|file| Maps {
        path: map_path.clone(),
        file,
        records,
        starts,
    });
    if let Err(e) = written.and(flushed.map_err(Error::from)) {
        let _ = std::fs::remove_file(&map_path);
        return Err(e);
    }
    tmp_bytes.add(Tmp::Maps, map_len);
    Ok((w.finish()?, maps?))
}

/// The read position of one partial vocabulary within a range.
struct Cursor {
    r: BufReader<File>,
    /// the current block, decoded, and the read position in it
    block: Vec<u8>,
    pos: usize,
    comp: Vec<u8>,
    /// the key read last
    key: Vec<u8>,
    /// rank of the next key
    rank: u64,
    keys: u64,
    /// the batch, and its ids not written yet
    part: usize,
    map: Vec<u64>,
}

impl Cursor {
    /// Read the next key into `self.key`. False at the end of the partial vocabulary.
    fn advance(&mut self) -> Result<bool> {
        if self.rank == self.keys {
            return Ok(false);
        }
        let bad = || Error::Corrupt("partial vocabulary".into());
        if self.pos == self.block.len() {
            let mut len = [0u8; 4];
            self.r.read_exact(&mut len)?;
            self.comp.resize(u32::from_le_bytes(len) as usize, 0);
            self.r.read_exact(&mut self.comp)?;
            self.block = lz4_flex::decompress_size_prepended(&self.comp)
                .map_err(|e| Error::Corrupt(format!("partial vocabulary block: {e}")))?;
            self.pos = 0;
            self.key.clear();
        }
        let shared = read_varint_checked(&self.block, &mut self.pos).ok_or_else(bad)? as usize;
        let len = read_varint_checked(&self.block, &mut self.pos).ok_or_else(bad)? as usize;
        let rest = self
            .block
            .get(self.pos..self.pos.saturating_add(len))
            .ok_or_else(bad)?;
        if shared > self.key.len() {
            return Err(bad());
        }
        self.key.truncate(shared);
        self.key.extend_from_slice(rest);
        self.pos += len;
        self.rank += 1;
        Ok(true)
    }

    /// The next key, or `None` at the end of the partial vocabulary or at a key not below
    /// `hi`.
    fn next(&mut self, hi: Option<&[u8]>) -> Result<Option<Vec<u8>>> {
        if !self.advance()? || hi.is_some_and(|h| *self.key >= *h) {
            return Ok(None);
        }
        Ok(Some(self.key.clone()))
    }

    fn put(&mut self, out: &mut MapOut, id: u64) -> Result<()> {
        self.map.push(id);
        if self.map.len() == MAP_BUF {
            self.flush(out)?;
        }
        Ok(())
    }

    fn flush(&mut self, out: &mut MapOut) -> Result<()> {
        if self.map.is_empty() {
            return Ok(());
        }
        let at = out.buf.len();
        let mut prev = 0;
        for (i, &v) in self.map.iter().enumerate() {
            write_varint(&mut out.buf, if i == 0 { v } else { v - prev });
            prev = v;
        }
        out.pending.push((
            self.part,
            MapRecord {
                range: out.range,
                offset: at as u64,
                len: (out.buf.len() - at) as u32,
                count: self.map.len() as u32,
            },
        ));
        self.map.clear();
        if out.buf.len() >= MAP_FLUSH {
            out.append()?;
        }
        Ok(())
    }
}

/// The map records of a range, collected and appended to the map file in groups.
struct MapOut<'a> {
    range: usize,
    file: &'a MapFile,
    /// records not appended yet, with offsets into `buf`
    buf: Vec<u8>,
    pending: Vec<(usize, MapRecord)>,
    /// records appended, with offsets into the file
    records: Vec<(usize, MapRecord)>,
}

impl MapOut<'_> {
    fn append(&mut self) -> Result<()> {
        if self.buf.is_empty() {
            return Ok(());
        }
        let base = {
            let mut f = self.file.lock();
            f.0.write_all(&self.buf)?;
            let base = f.1;
            f.1 += self.buf.len() as u64;
            base
        };
        self.records
            .extend(self.pending.drain(..).map(|(part, mut rec)| {
                rec.offset += base;
                (part, rec)
            }));
        self.buf.clear();
        Ok(())
    }
}

/// Where a range writes its distinct keys: memory while [`RANGE_MEMORY`] allows it, and
/// a file after that.
struct KeysOut<'a> {
    mem: Vec<u8>,
    /// bytes of [`RANGE_MEMORY`] this range holds
    reserved: u64,
    held: &'a AtomicU64,
    file: Option<(PathBuf, BufWriter<File>)>,
    path: PathBuf,
}

impl KeysOut<'_> {
    fn write(&mut self, bytes: &[u8]) -> Result<()> {
        if let Some((_, f)) = &mut self.file {
            f.write_all(bytes)?;
            return Ok(());
        }
        let need = (self.mem.len() + bytes.len()) as u64;
        while need > self.reserved {
            if self.held.fetch_add(RANGE_STEP, Ordering::Relaxed) + RANGE_STEP > RANGE_MEMORY {
                self.held.fetch_sub(RANGE_STEP, Ordering::Relaxed);
                return self.spill(bytes);
            }
            self.reserved += RANGE_STEP;
        }
        self.mem.extend_from_slice(bytes);
        Ok(())
    }

    /// Move the keys to a file, and write `bytes` after them.
    fn spill(&mut self, bytes: &[u8]) -> Result<()> {
        let mut f = BufWriter::with_capacity(1 << 20, File::create(&self.path)?);
        f.write_all(&self.mem)?;
        f.write_all(bytes)?;
        self.mem = Vec::new();
        self.release();
        self.file = Some((self.path.clone(), f));
        Ok(())
    }

    fn release(&mut self) {
        self.held.fetch_sub(self.reserved, Ordering::Relaxed);
        self.reserved = 0;
    }

    fn finish(mut self) -> Result<RangeKeys> {
        match self.file.take() {
            Some((path, mut f)) => {
                f.flush()?;
                Ok(RangeKeys::File(path))
            }
            None => {
                let reserved = std::mem::take(&mut self.reserved);
                Ok(RangeKeys::Memory(std::mem::take(&mut self.mem), reserved))
            }
        }
    }
}

impl Drop for KeysOut<'_> {
    fn drop(&mut self) {
        // a range that failed gives its memory back
        self.release();
    }
}

/// Open partial vocabulary `part` at the first key not below `lo`.
fn seek(parts: &[Partial], part: usize, lo: Option<&[u8]>) -> Result<(Cursor, Option<Vec<u8>>)> {
    let p = &parts[part];
    let f = File::open(&p.voc)?;
    let (mut rank, mut offset) = (0, 0);
    if let Some(lo) = lo {
        // the last sample below `lo`
        let i = p.samples.partition_point(|s| *s.2 < *lo);
        if i > 0 {
            (rank, offset) = (p.samples[i - 1].0, p.samples[i - 1].1);
        }
    }
    let mut r = BufReader::with_capacity(16 << 10, f);
    r.seek(SeekFrom::Start(offset))?;
    let mut c = Cursor {
        r,
        block: Vec::new(),
        pos: 0,
        comp: Vec::new(),
        key: Vec::new(),
        rank,
        keys: p.keys,
        part,
        map: Vec::new(),
    };
    while c.advance()? {
        if lo.is_none_or(|lo| *c.key >= *lo) {
            let first = c.key.clone();
            return Ok((c, Some(first)));
        }
    }
    Ok((c, None))
}

/// Merge the keys in `[lo, hi)` of every partial vocabulary, keep the distinct ones
/// front-coded, and append the ids of the batches' keys in the range to the map file.
fn merge_range(
    tmp: &Path,
    parts: &[Partial],
    r: usize,
    lo: Option<&[u8]>,
    hi: Option<&[u8]>,
    held: &AtomicU64,
    map_file: &MapFile,
) -> Result<RangeOut> {
    let mut out = KeysOut {
        mem: Vec::new(),
        reserved: 0,
        held,
        file: None,
        path: tmp.join(format!("vocab-range-{r}")),
    };
    let mut maps = MapOut {
        range: r,
        file: map_file,
        buf: Vec::new(),
        pending: Vec::new(),
        records: Vec::new(),
    };
    let mut cursors = Vec::with_capacity(parts.len());
    let mut heap = BinaryHeap::new();
    for (i, p) in parts.iter().enumerate() {
        if p.keys == 0 {
            cursors.push(None);
            continue;
        }
        let (c, first) = seek(parts, i, lo)?;
        match first {
            Some(k) if hi.is_none_or(|h| *k < *h) => {
                heap.push(Reverse((k, i)));
                cursors.push(Some(c));
            }
            // the range holds none of its keys
            _ => cursors.push(None),
        }
    }
    let mut n = 0u64;
    let mut last: Vec<u8> = Vec::new();
    let mut buf = Vec::new();
    while let Some(Reverse((k, i))) = heap.pop() {
        if n == 0 || k != last {
            front_code(&last, &k, n == 0, &mut buf);
            out.write(&buf)?;
            last = k;
            n += 1;
        }
        let c = cursors[i].as_mut().unwrap();
        c.put(&mut maps, n - 1)?;
        if let Some(k) = c.next(hi)? {
            heap.push(Reverse((k, i)));
        }
    }
    for c in cursors.iter_mut().flatten() {
        c.flush(&mut maps)?;
    }
    maps.append()?;
    Ok(RangeOut {
        terms: n,
        keys: out.finish()?,
        records: maps.records,
    })
}

/// `key` front-coded after `prev` into `buf`.
fn front_code(prev: &[u8], key: &[u8], first: bool, buf: &mut Vec<u8>) {
    let shared = if first {
        0
    } else {
        prev.iter().zip(key).take_while(|(a, b)| a == b).count()
    };
    buf.clear();
    write_varint(buf, shared as u64);
    write_varint(buf, (key.len() - shared) as u64);
    buf.extend_from_slice(&key[shared..]);
}

/// Read the next front-coded key into `key`, which holds the previous one.
fn read_front_coded(r: &mut impl Read, key: &mut Vec<u8>) -> Result<()> {
    let mut varint = || -> Result<u64> {
        let (mut v, mut shift) = (0u64, 0);
        loop {
            let mut b = [0u8];
            r.read_exact(&mut b)?;
            if shift > 63 {
                return Err(Error::Corrupt("vocabulary range".into()));
            }
            v |= ((b[0] & 0x7F) as u64) << shift;
            if b[0] < 0x80 {
                return Ok(v);
            }
            shift += 7;
        }
    };
    let shared = varint()? as usize;
    let len = varint()? as usize;
    if shared > key.len() {
        return Err(Error::Corrupt("vocabulary range".into()));
    }
    key.truncate(shared);
    let at = key.len();
    key.resize(at + len, 0);
    r.read_exact(&mut key[at..])?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::vocab::Vocab;

    fn key_sets() -> Vec<Vec<Vec<u8>>> {
        // overlapping sorted key sets with several samples each, an empty one, and one
        // with keys long enough to end blocks by their size
        [
            (0u64, 30_000u64, 3u64, 0usize),
            (5, 20_000, 7, 0),
            (0, 0, 1, 0),
            (1, 9000, 2, 0),
            (2, 3000, 11, 900),
        ]
        .iter()
        .map(|&(start, n, step, pad)| {
            let mut v: Vec<Vec<u8>> = (0..n)
                .map(|i| {
                    let mut k = format!("<http://e/{:07}", start + i * step).into_bytes();
                    k.extend(std::iter::repeat_n(b'x', pad * (i as usize % 3)));
                    k
                })
                .collect();
            v.sort();
            v.dedup();
            v
        })
        .collect()
    }

    #[test]
    fn ranges_merge_like_one_merge() {
        let dir = tempfile::tempdir().unwrap();
        let tmp = dir.path().join("tmp");
        std::fs::create_dir_all(&tmp).unwrap();
        let sets = key_sets();
        let parts: Vec<Partial> = sets
            .iter()
            .enumerate()
            .map(|(i, keys)| {
                let voc = tmp.join(format!("b{i}.voc"));
                let (samples, bytes) = write_partial(&voc, keys.iter().map(|k| &k[..])).unwrap();
                Partial {
                    voc,
                    keys: keys.len() as u64,
                    bytes,
                    samples,
                }
            })
            .collect();
        let (terms, maps) =
            merge(dir.path(), &tmp, &parts, 3, &Default::default(), &|| Ok(())).unwrap();
        let mut all: Vec<&Vec<u8>> = sets.iter().flatten().collect();
        all.sort();
        all.dedup();
        assert_eq!(terms, all.len() as u64);
        assert!(maps.starts.len() > 2);
        let v = Vocab::open(dir.path()).unwrap();
        for (i, keys) in sets.iter().enumerate() {
            let ids = maps.read(i, keys.len() as u64).unwrap();
            assert_eq!(ids.len(), keys.len());
            for (k, &id) in keys.iter().zip(&ids) {
                assert_eq!(&v.get(id).unwrap(), k);
            }
        }
        // the ranges left no files: the batch files and the map file are left
        assert_eq!(std::fs::read_dir(&tmp).unwrap().count(), parts.len() + 1);
        maps.remove().unwrap();
        assert_eq!(std::fs::read_dir(&tmp).unwrap().count(), parts.len());
    }

    #[test]
    fn a_range_past_the_memory_budget_goes_to_a_file() {
        let dir = tempfile::tempdir().unwrap();
        let held = AtomicU64::new(RANGE_MEMORY - RANGE_STEP);
        let mut out = KeysOut {
            mem: Vec::new(),
            reserved: 0,
            held: &held,
            file: None,
            path: dir.path().join("r"),
        };
        let mut want = Vec::new();
        for i in 0..100_000u32 {
            let k = format!("key {i:08} ").into_bytes();
            out.write(&k).unwrap();
            want.extend_from_slice(&k);
        }
        // one step fitted the budget, the rest went to the file
        assert!(out.file.is_some());
        assert_eq!(held.load(Ordering::Relaxed), RANGE_MEMORY - RANGE_STEP);
        let RangeKeys::File(p) = out.finish().unwrap() else {
            panic!("a range past the budget is a file");
        };
        assert_eq!(std::fs::read(p).unwrap(), want);
        // a range in memory gives its memory back when it is dropped
        let mut small = KeysOut {
            mem: Vec::new(),
            reserved: 0,
            held: &held,
            file: None,
            path: dir.path().join("s"),
        };
        small.write(b"abc").unwrap();
        assert_eq!(held.load(Ordering::Relaxed), RANGE_MEMORY);
        drop(small);
        assert_eq!(held.load(Ordering::Relaxed), RANGE_MEMORY - RANGE_STEP);
    }
}
