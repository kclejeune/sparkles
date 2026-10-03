//! The merge of the partial vocabularies, in parallel over ranges of keys.
//!
//! Each batch's partial vocabulary is a file of its sorted distinct keys (`u32` length,
//! then the bytes), with a sample of every [`SAMPLE_EVERY`]th key and its byte offset kept
//! in memory. Keys taken from the samples split the key space into ranges, and each range
//! is merged by a task of its own: it seeks every batch file to the range's first key,
//! merges the keys of the range, writes the distinct ones front-coded to a file of the
//! range, and writes each batch key's id into the batch's map at the key's rank. A range
//! does not know how many distinct keys the ranges before it hold, so the map holds the
//! range in the high bits and the id within the range in the low ones (see [`global`]).
//! The range files are appended to the vocabulary in key order as they complete.

use crate::error::{Error, Result};
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
/// Bits of a map entry that hold the id within its range.
const LOCAL_BITS: u32 = 40;
/// Map entries buffered per batch before they are written.
const MAP_BUF: usize = 4096;

/// A sampled key of a partial vocabulary: its rank, its byte offset and the key.
pub(super) type Sample = (u64, u64, Box<[u8]>);

/// One batch's partial vocabulary.
pub(super) struct Partial {
    pub voc: PathBuf,
    pub map: PathBuf,
    pub keys: u64,
    pub samples: Vec<Sample>,
}

/// The global id of a map entry, given the first id of each range.
#[inline]
pub(super) fn global(starts: &[u64], entry: u64) -> u64 {
    starts[(entry >> LOCAL_BITS) as usize] + (entry & ((1 << LOCAL_BITS) - 1))
}

/// Merge the partial vocabularies into the vocabulary of `dir` with `threads` tasks.
/// Returns the number of terms and the first id of each range.
pub(super) fn merge(
    dir: &Path,
    tmp: &Path,
    parts: &[Partial],
    threads: usize,
    interrupted: &(dyn Fn() -> Result<()> + Sync),
) -> Result<(u64, Vec<u64>)> {
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
    let maps: Vec<File> = parts
        .iter()
        .map(|p| -> Result<File> {
            let f = File::create(&p.map)?;
            f.set_len(p.keys * 8)?;
            Ok(f)
        })
        .collect::<Result<_>>()?;
    let next = AtomicUsize::new(0);
    let (tx, rx) = std::sync::mpsc::channel::<(usize, Result<(u64, PathBuf)>)>();
    let mut w = VocabWriter::create(dir)?;
    let mut starts = Vec::with_capacity(ranges);
    let written: Result<()> = std::thread::scope(|s| {
        for _ in 0..threads.max(1).min(ranges) {
            let tx = tx.clone();
            let (next, maps, bounds) = (&next, &maps, &bounds);
            s.spawn(move || {
                loop {
                    let r = next.fetch_add(1, Ordering::Relaxed);
                    if r >= ranges {
                        return;
                    }
                    let lo = r.checked_sub(1).map(|i| bounds[i]);
                    let hi = bounds.get(r).copied();
                    let out = interrupted().and_then(|()| merge_range(tmp, parts, maps, r, lo, hi));
                    let failed = out.is_err();
                    if tx.send((r, out)).is_err() || failed {
                        return;
                    }
                }
            });
        }
        drop(tx);
        // append the ranges in order as they complete
        let mut done: BTreeMap<usize, (u64, PathBuf)> = BTreeMap::new();
        let mut buf = Vec::new();
        let mut key = Vec::new();
        for (r, out) in rx.iter() {
            done.insert(r, out?);
            while let Some((n, path)) = done.remove(&starts.len()) {
                starts.push(w.len());
                let mut f = BufReader::with_capacity(1 << 20, File::open(&path)?);
                key.clear();
                for _ in 0..n {
                    read_front_coded(&mut f, &mut key, &mut buf)?;
                    w.push(&key)?;
                }
                drop(f);
                std::fs::remove_file(&path)?;
            }
        }
        if starts.len() != ranges {
            return Err(Error::Corrupt("a vocabulary range was not merged".into()));
        }
        Ok(())
    });
    written?;
    for m in &maps {
        m.sync_data()?;
    }
    Ok((w.finish()?, starts))
}

/// The read position of one partial vocabulary within a range.
struct Cursor {
    r: BufReader<File>,
    /// rank of the next key
    rank: u64,
    keys: u64,
    /// map entries from rank `map_at` on, not written yet
    map_at: u64,
    map: Vec<u64>,
}

impl Cursor {
    fn next(&mut self, hi: Option<&[u8]>) -> Result<Option<Vec<u8>>> {
        if self.rank == self.keys {
            return Ok(None);
        }
        let mut len = [0u8; 4];
        self.r.read_exact(&mut len)?;
        let mut k = vec![0u8; u32::from_le_bytes(len) as usize];
        self.r.read_exact(&mut k)?;
        if hi.is_some_and(|h| *k >= *h) {
            return Ok(None);
        }
        self.rank += 1;
        Ok(Some(k))
    }

    fn put(&mut self, map: &File, entry: u64) -> Result<()> {
        self.map.push(entry);
        if self.map.len() == MAP_BUF {
            self.flush(map)?;
        }
        Ok(())
    }

    fn flush(&mut self, map: &File) -> Result<()> {
        let bytes: Vec<u8> = self.map.iter().flat_map(|v| v.to_le_bytes()).collect();
        map.write_all_at(&bytes, self.map_at * 8)?;
        self.map_at += self.map.len() as u64;
        self.map.clear();
        Ok(())
    }
}

/// Open partial vocabulary `p` at the first key not below `lo`.
fn seek(p: &Partial, lo: Option<&[u8]>) -> Result<(Cursor, Option<Vec<u8>>)> {
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
        rank,
        keys: p.keys,
        map_at: rank,
        map: Vec::new(),
    };
    loop {
        let at = c.rank;
        match c.next(None)? {
            Some(k) if lo.is_some_and(|lo| *k < *lo) => {}
            Some(k) => {
                c.map_at = at;
                return Ok((c, Some(k)));
            }
            None => {
                c.map_at = c.rank;
                return Ok((c, None));
            }
        }
    }
}

/// Merge the keys in `[lo, hi)` of every partial vocabulary into a front-coded file.
/// Returns the number of distinct keys and the file.
fn merge_range(
    tmp: &Path,
    parts: &[Partial],
    maps: &[File],
    r: usize,
    lo: Option<&[u8]>,
    hi: Option<&[u8]>,
) -> Result<(u64, PathBuf)> {
    let path = tmp.join(format!("vocab-range-{r}"));
    let mut out = BufWriter::with_capacity(1 << 20, File::create(&path)?);
    let mut cursors = Vec::with_capacity(parts.len());
    let mut heap = BinaryHeap::new();
    for (i, p) in parts.iter().enumerate() {
        if p.keys == 0 {
            cursors.push(None);
            continue;
        }
        let (mut c, first) = seek(p, lo)?;
        match first {
            Some(k) if hi.is_none_or(|h| *k < *h) => {
                heap.push(Reverse((k, i)));
                cursors.push(Some(c));
            }
            _ => {
                // the range holds none of its keys
                c.map_at = c.rank;
                cursors.push(None);
            }
        }
    }
    let tag = (r as u64) << LOCAL_BITS;
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
        c.put(&maps[i], tag | (n - 1))?;
        if let Some(k) = c.next(hi)? {
            heap.push(Reverse((k, i)));
        }
    }
    for (c, m) in cursors.iter_mut().zip(maps) {
        if let Some(c) = c {
            c.flush(m)?;
        }
    }
    out.flush()?;
    Ok((n, path))
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

/// The samples of a partial vocabulary with these sorted keys: every [`SAMPLE_EVERY`]th
/// key, with its rank and byte offset.
pub(super) fn samples<'a>(keys: impl Iterator<Item = &'a [u8]>) -> Vec<Sample> {
    let mut out = Vec::new();
    let mut offset = 0u64;
    for (rank, k) in keys.enumerate() {
        if rank as u64 % SAMPLE_EVERY == 0 {
            out.push((rank as u64, offset, k.into()));
        }
        offset += 4 + k.len() as u64;
    }
    out
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
        // overlapping sorted key sets with several samples each, and an empty one
        let sets: Vec<Vec<Vec<u8>>> = [
            (0u64, 30_000u64, 3u64),
            (5, 20_000, 7),
            (0, 0, 1),
            (1, 9000, 2),
        ]
        .iter()
        .map(|&(start, n, step)| {
            let mut v: Vec<Vec<u8>> = (0..n)
                .map(|i| format!("<http://e/{:07}", start + i * step).into_bytes())
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
                let mut w = BufWriter::new(File::create(&voc).unwrap());
                for k in keys {
                    w.write_all(&(k.len() as u32).to_le_bytes()).unwrap();
                    w.write_all(k).unwrap();
                }
                w.flush().unwrap();
                Partial {
                    voc,
                    map: tmp.join(format!("b{i}.map")),
                    keys: keys.len() as u64,
                    samples: samples(keys.iter().map(|k| &k[..])),
                }
            })
            .collect();
        let (terms, starts) = merge(dir.path(), &tmp, &parts, 3, &|| Ok(())).unwrap();
        let mut all: Vec<&Vec<u8>> = sets.iter().flatten().collect();
        all.sort();
        all.dedup();
        assert_eq!(terms, all.len() as u64);
        assert!(starts.len() > 2);
        let v = Vocab::open(dir.path()).unwrap();
        for (p, keys) in parts.iter().zip(&sets) {
            let map = std::fs::read(&p.map).unwrap();
            assert_eq!(map.len(), keys.len() * 8);
            for (k, e) in keys.iter().zip(map.chunks(8)) {
                let id = global(&starts, u64::from_le_bytes(e.try_into().unwrap()));
                assert_eq!(&v.get(id).unwrap(), k);
            }
        }
        // the range files are gone: the batch files and maps are left
        assert_eq!(std::fs::read_dir(&tmp).unwrap().count(), 8);
    }
}
