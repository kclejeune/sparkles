//! The merge of the partial vocabularies, in parallel over ranges of keys.
//!
//! Each batch's partial vocabulary is a file of its sorted distinct keys in blocks (see
//! [`write_partial`]), with a sample of every [`SAMPLE_EVERY`]th key and the byte offset
//! of its block kept in memory. Keys taken from the samples split the key space into
//! ranges, and each range is merged by a task of its own: it seeks every batch file to
//! the range's first key, merges the keys of the range, writes the distinct ones
//! front-coded to a file of the range, and writes each batch key's id, in rank order, to
//! a map file of the range (see [`Maps`]). The range files are appended to the
//! vocabulary in key order as they complete.

use super::iostat::{Tmp, TmpBytes};
use crate::error::{Error, Result};
use crate::index::read_varint_checked;
use crate::vocab::{VocabWriter, read_varint, write_varint};
use std::cmp::Reverse;
use std::collections::{BTreeMap, BinaryHeap};
use std::fs::File;
use std::io::{BufReader, BufWriter, Read, Seek, SeekFrom, Write};
use std::os::unix::fs::FileExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};

/// Every this many keys of a partial vocabulary, a sample is kept.
pub(super) const SAMPLE_EVERY: u64 = 4096;
/// Keys per block of a partial vocabulary at most.
const BLOCK_KEYS: u64 = 256;
/// A block of a partial vocabulary ends once its keys take this many bytes front-coded.
/// This bounds the memory of a merge, which holds one decoded block per batch and range.
const BLOCK_BYTES: usize = 16 << 10;
/// Map entries buffered per batch before they are written.
const MAP_BUF: usize = 4096;

/// A sampled key of a partial vocabulary: its rank, the byte offset of the block it
/// starts, and the key.
pub(super) type Sample = (u64, u64, Box<[u8]>);

/// One batch's partial vocabulary.
pub(super) struct Partial {
    pub voc: PathBuf,
    pub keys: u64,
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

/// Where some map entries of a batch are: a range and a byte range of its map file.
struct MapRecord {
    range: usize,
    offset: u64,
    len: u32,
    count: u32,
}

/// The maps of all batches from the rank of a key in its batch to the key's global id.
///
/// Each range of the merge writes a map file of its own. While it merges, it buffers
/// the ids of each batch's keys in the range and writes them as records of up to
/// [`MAP_BUF`] ids: the first id within the range as a varint, then the difference of
/// each id to the one before. A batch's ids increase with its ranks, so most
/// differences take a byte or two. The records of each batch are kept in memory in
/// rank order.
pub(super) struct Maps {
    files: Vec<(PathBuf, File)>,
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
            self.files[r.range].1.read_exact_at(&mut buf, r.offset)?;
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

    /// Delete the map files.
    pub(super) fn remove(self) -> Result<()> {
        for (p, f) in self.files {
            drop(f);
            std::fs::remove_file(p)?;
        }
        Ok(())
    }
}

/// What a range of the merge produced.
struct RangeOut {
    /// distinct keys, and the file with them front-coded
    terms: u64,
    vocab: PathBuf,
    /// the map file, and the batch and place of each of its records
    map: PathBuf,
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
    let ranges = (threads.max(1) * 4).min(all.len() + 1);
    let mut bounds: Vec<&[u8]> = (1..ranges).map(|i| all[i * all.len() / ranges]).collect();
    bounds.dedup();
    let ranges = bounds.len() + 1;
    let next = AtomicUsize::new(0);
    let (tx, rx) = std::sync::mpsc::channel::<(usize, Result<RangeOut>)>();
    let mut w = VocabWriter::create(dir)?;
    let mut maps = Maps {
        files: Vec::with_capacity(ranges),
        records: (0..parts.len()).map(|_| Vec::new()).collect(),
        starts: Vec::with_capacity(ranges),
    };
    let written: Result<()> = std::thread::scope(|s| {
        for _ in 0..threads.max(1).min(ranges) {
            let tx = tx.clone();
            let (next, bounds) = (&next, &bounds);
            s.spawn(move || {
                loop {
                    let r = next.fetch_add(1, Ordering::Relaxed);
                    if r >= ranges {
                        return;
                    }
                    let lo = r.checked_sub(1).map(|i| bounds[i]);
                    let hi = bounds.get(r).copied();
                    let out = interrupted().and_then(|()| merge_range(tmp, parts, r, lo, hi));
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
        let mut buf = Vec::new();
        let mut key = Vec::new();
        for (r, out) in rx.iter() {
            let out = out.inspect_err(|_| {
                // the other threads take no further range
                next.store(ranges, Ordering::Relaxed);
            })?;
            done.insert(r, out);
            while let Some(out) = done.remove(&maps.starts.len()) {
                maps.starts.push(w.len());
                let mut f = BufReader::with_capacity(1 << 20, File::open(&out.vocab)?);
                tmp_bytes.add(Tmp::VocabRanges, f.get_ref().metadata()?.len());
                key.clear();
                for _ in 0..out.terms {
                    read_front_coded(&mut f, &mut key, &mut buf)?;
                    w.push(&key)?;
                }
                drop(f);
                std::fs::remove_file(&out.vocab)?;
                let mf = File::open(&out.map)?;
                tmp_bytes.add(Tmp::Maps, mf.metadata()?.len());
                maps.files.push((out.map, mf));
                for (part, rec) in out.records {
                    maps.records[part].push(rec);
                }
            }
        }
        if maps.starts.len() != ranges {
            return Err(Error::Corrupt("a vocabulary range was not merged".into()));
        }
        Ok(())
    });
    if let Err(e) = written {
        let _ = maps.remove();
        return Err(e);
    }
    Ok((w.finish()?, maps))
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
    /// The next key, or `None` at the end of the partial vocabulary or at a key not below
    /// `hi`.
    fn next(&mut self, hi: Option<&[u8]>) -> Result<Option<Vec<u8>>> {
        if self.rank == self.keys {
            return Ok(None);
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
        if hi.is_some_and(|h| *self.key >= *h) {
            return Ok(None);
        }
        self.rank += 1;
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
        out.buf.clear();
        let mut prev = 0;
        for (i, &v) in self.map.iter().enumerate() {
            write_varint(&mut out.buf, if i == 0 { v } else { v - prev });
            prev = v;
        }
        out.w.write_all(&out.buf)?;
        out.records.push((
            self.part,
            MapRecord {
                range: out.range,
                offset: out.pos,
                len: out.buf.len() as u32,
                count: self.map.len() as u32,
            },
        ));
        out.pos += out.buf.len() as u64;
        self.map.clear();
        Ok(())
    }
}

/// The map file a range writes.
struct MapOut {
    range: usize,
    w: BufWriter<File>,
    pos: u64,
    buf: Vec<u8>,
    records: Vec<(usize, MapRecord)>,
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
    loop {
        match c.next(None)? {
            Some(k) if lo.is_some_and(|lo| *k < *lo) => {}
            first => return Ok((c, first)),
        }
    }
}

/// Merge the keys in `[lo, hi)` of every partial vocabulary into a front-coded file,
/// and write the ids of the batches' keys in the range to a map file.
fn merge_range(
    tmp: &Path,
    parts: &[Partial],
    r: usize,
    lo: Option<&[u8]>,
    hi: Option<&[u8]>,
) -> Result<RangeOut> {
    let path = tmp.join(format!("vocab-range-{r}"));
    let map = tmp.join(format!("map-{r}"));
    let mut out = BufWriter::with_capacity(1 << 20, File::create(&path)?);
    let mut maps = MapOut {
        range: r,
        w: BufWriter::with_capacity(1 << 20, File::create(&map)?),
        pos: 0,
        buf: Vec::new(),
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
            write_front_coded(&mut out, &last, &k, n == 0, &mut buf)?;
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
    out.flush()?;
    maps.w.flush()?;
    Ok(RangeOut {
        terms: n,
        vocab: path,
        map,
        records: maps.records,
    })
}

fn write_front_coded(
    w: &mut impl Write,
    prev: &[u8],
    key: &[u8],
    first: bool,
    buf: &mut Vec<u8>,
) -> Result<()> {
    let shared = if first {
        0
    } else {
        prev.iter().zip(key).take_while(|(a, b)| a == b).count()
    };
    buf.clear();
    write_varint(buf, shared as u64);
    write_varint(buf, (key.len() - shared) as u64);
    buf.extend_from_slice(&key[shared..]);
    w.write_all(buf)?;
    Ok(())
}

/// Read the next front-coded key into `key`, which holds the previous one.
fn read_front_coded(r: &mut BufReader<File>, key: &mut Vec<u8>, buf: &mut Vec<u8>) -> Result<()> {
    let mut varint = || -> Result<u64> {
        buf.clear();
        loop {
            let mut b = [0u8];
            r.read_exact(&mut b)?;
            buf.push(b[0]);
            if b[0] < 0x80 {
                let mut pos = 0;
                return Ok(read_varint(buf, &mut pos));
            }
        }
    };
    let shared = varint()? as usize;
    let len = varint()? as usize;
    if shared > key.len() {
        return Err(Error::Corrupt("vocabulary range file".into()));
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

    #[test]
    fn ranges_merge_like_one_merge() {
        let dir = tempfile::tempdir().unwrap();
        let tmp = dir.path().join("tmp");
        std::fs::create_dir_all(&tmp).unwrap();
        // overlapping sorted key sets with several samples each, an empty one, and one
        // with keys long enough to end blocks by their size
        let sets: Vec<Vec<Vec<u8>>> = [
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
        .collect();
        let parts: Vec<Partial> = sets
            .iter()
            .enumerate()
            .map(|(i, keys)| {
                let voc = tmp.join(format!("b{i}.voc"));
                let (samples, _) = write_partial(&voc, keys.iter().map(|k| &k[..])).unwrap();
                Partial {
                    voc,
                    keys: keys.len() as u64,
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
        // the range files are gone: the batch files and the map files are left
        let ranges = maps.starts.len();
        assert_eq!(
            std::fs::read_dir(&tmp).unwrap().count(),
            parts.len() + ranges
        );
        maps.remove().unwrap();
        assert_eq!(std::fs::read_dir(&tmp).unwrap().count(), parts.len());
    }
}
