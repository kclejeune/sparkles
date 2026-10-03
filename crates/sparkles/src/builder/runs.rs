//! Sorted runs of permutation keys on disk, their k-way merge, and the sort within the
//! runs of a first column that turns one sorted permutation into another.
//!
//! A run file is a sequence of blocks of up to [`RUN_BLOCK`] keys. A block starts with its
//! row count and the byte length of each column (five little-endian `u32`), followed by
//! the four columns, each encoded as in a permutation block (delta + zig-zag varint, then
//! LZ4). Sorted keys compress to about a third of their 32 bytes.

use crate::error::{Error, Result};
use crate::index::{Key, decode_column, encode_column};
use rayon::prelude::*;
use std::cmp::Reverse;
use std::collections::BinaryHeap;
use std::collections::binary_heap::PeekMut;
use std::fs::File;
use std::io::{BufRead, BufReader, BufWriter, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, sync_channel};
use std::thread::Scope;

/// Keys per block of a run file.
const RUN_BLOCK: usize = 32 * 1024;
/// Blocks encoded in parallel before they are written.
const ENCODE_GROUP: usize = 64;
/// Decoded blocks a reader thread keeps ahead of the merge.
const READ_AHEAD: usize = 2;
/// Bytes of a block header: rows and four column lengths.
const HEADER: usize = 20;

/// Write sorted keys as a run file.
pub(super) fn write_run(path: &Path, keys: &[Key]) -> Result<()> {
    let mut w = BufWriter::with_capacity(1 << 20, File::create(path)?);
    for group in keys.chunks(RUN_BLOCK * ENCODE_GROUP) {
        let blocks: Vec<Vec<u8>> = group.par_chunks(RUN_BLOCK).map(encode_block).collect();
        for b in blocks {
            w.write_all(&b)?;
        }
    }
    w.flush()?;
    Ok(())
}

fn encode_block(rows: &[Key]) -> Vec<u8> {
    let mut col = Vec::with_capacity(rows.len());
    let mut scratch = Vec::new();
    let mut out = Vec::with_capacity(HEADER + rows.len() * 12);
    out.extend_from_slice(&(rows.len() as u32).to_le_bytes());
    out.extend_from_slice(&[0; HEADER - 4]);
    for c in 0..4 {
        col.clear();
        col.extend(rows.iter().map(|k| k[c]));
        let enc = encode_column(&col, &mut scratch);
        out[4 + 4 * c..8 + 4 * c].copy_from_slice(&(enc.len() as u32).to_le_bytes());
        out.extend_from_slice(&enc);
    }
    out
}

/// Reads a run file block by block.
pub(super) struct RunReader {
    r: BufReader<File>,
    buf: Vec<u8>,
}

impl RunReader {
    pub(super) fn open(path: &Path) -> Result<RunReader> {
        Ok(RunReader {
            r: BufReader::with_capacity(1 << 20, File::open(path)?),
            buf: Vec::new(),
        })
    }

    /// The next block's keys, `None` at the end of the file.
    pub(super) fn next_block(&mut self) -> Result<Option<Vec<Key>>> {
        if self.r.fill_buf()?.is_empty() {
            return Ok(None);
        }
        let mut head = [0u8; HEADER];
        self.r.read_exact(&mut head)?;
        let u32_at = |i: usize| u32::from_le_bytes(head[i..i + 4].try_into().unwrap()) as usize;
        let rows = u32_at(0);
        if rows == 0 || rows > RUN_BLOCK {
            return Err(Error::Corrupt(format!("run block of {rows} rows")));
        }
        let mut keys = vec![[0u64; 4]; rows];
        for c in 0..4 {
            self.buf.resize(u32_at(4 + 4 * c), 0);
            self.r.read_exact(&mut self.buf)?;
            for (k, v) in keys.iter_mut().zip(decode_column(&self.buf, rows)?) {
                k[c] = v;
            }
        }
        Ok(Some(keys))
    }
}

enum Source {
    /// decoded in the merging thread
    Inline(RunReader),
    /// decoded ahead by a thread of its own
    Thread(Receiver<Result<Vec<Key>>>),
}

/// The read position in one sorted run.
pub(super) struct Cursor {
    src: Source,
    block: Vec<Key>,
    pos: usize,
}

impl Cursor {
    /// A run decoded in the thread that reads it.
    pub(super) fn inline(path: &Path) -> Result<Cursor> {
        Ok(Cursor {
            src: Source::Inline(RunReader::open(path)?),
            block: Vec::new(),
            pos: 0,
        })
    }

    /// A run decoded ahead by a thread of `scope`.
    pub(super) fn threaded<'s>(scope: &'s Scope<'s, '_>, path: PathBuf) -> Cursor {
        let (tx, rx) = sync_channel(READ_AHEAD);
        scope.spawn(move || {
            let mut r = match RunReader::open(&path) {
                Ok(r) => r,
                Err(e) => {
                    let _ = tx.send(Err(e));
                    return;
                }
            };
            loop {
                match r.next_block() {
                    Ok(Some(b)) => {
                        // the merge stopped early (an error elsewhere)
                        if tx.send(Ok(b)).is_err() {
                            return;
                        }
                    }
                    Ok(None) => return,
                    Err(e) => {
                        let _ = tx.send(Err(e));
                        return;
                    }
                }
            }
        });
        Cursor {
            src: Source::Thread(rx),
            block: Vec::new(),
            pos: 0,
        }
    }

    #[inline]
    fn next(&mut self) -> Result<Option<Key>> {
        while self.pos == self.block.len() {
            let b = match &mut self.src {
                Source::Inline(r) => r.next_block()?,
                Source::Thread(rx) => match rx.recv() {
                    Ok(b) => Some(b?),
                    Err(_) => None,
                },
            };
            match b {
                Some(b) => {
                    self.block = b;
                    self.pos = 0;
                }
                None => {
                    self.block = Vec::new();
                    self.pos = 0;
                    return Ok(None);
                }
            }
        }
        let k = self.block[self.pos];
        self.pos += 1;
        Ok(Some(k))
    }
}

/// Merge sorted runs, calling `out` for every key in order (repeats included).
pub(super) fn merge(cursors: &mut [Cursor], mut out: impl FnMut(Key) -> Result<()>) -> Result<()> {
    if let [c] = cursors {
        while let Some(k) = c.next()? {
            out(k)?;
        }
        return Ok(());
    }
    let mut heap = BinaryHeap::with_capacity(cursors.len());
    for (i, c) in cursors.iter_mut().enumerate() {
        if let Some(k) = c.next()? {
            heap.push(Reverse((k, i)));
        }
    }
    while let Some(mut top) = heap.peek_mut() {
        let Reverse((k, i)) = *top;
        out(k)?;
        match cursors[i].next()? {
            // replacing the top sifts it down once, where a pop and a push sift twice
            Some(n) => *top = Reverse((n, i)),
            None => {
                PeekMut::pop(top);
            }
        }
    }
    Ok(())
}

/// Groups of at least this many keys are sorted in parallel.
const PAR_SORT_MIN: usize = 1 << 16;

/// Sorts the keys of each run of equal first columns, which arrive in first-column order:
/// a permutation sorted on the same first column as the input, by the other columns.
/// A run larger than the memory budget is sorted in parts spilled to disk and merged.
pub(super) struct Regroup {
    buf: Vec<Key>,
    cur: Option<u64>,
    budget: usize,
    spills: Vec<PathBuf>,
    tmp: PathBuf,
    name: String,
    spilled: usize,
}

impl Regroup {
    pub(super) fn new(tmp: &Path, name: &str, budget: usize) -> Regroup {
        Regroup {
            buf: Vec::new(),
            cur: None,
            budget: budget.max(1),
            spills: Vec::new(),
            tmp: tmp.to_path_buf(),
            name: name.to_string(),
            spilled: 0,
        }
    }

    #[inline]
    pub(super) fn push(&mut self, k: Key, out: &mut impl FnMut(Key) -> Result<()>) -> Result<()> {
        if self.cur != Some(k[0]) {
            self.finish(out)?;
            self.cur = Some(k[0]);
        }
        self.buf.push(k);
        if self.buf.len() >= self.budget {
            self.spill()?;
        }
        Ok(())
    }

    fn spill(&mut self) -> Result<()> {
        self.buf.par_sort_unstable();
        let p = self
            .tmp
            .join(format!("{}-spill-{}", self.name, self.spilled));
        self.spilled += 1;
        write_run(&p, &self.buf)?;
        self.spills.push(p);
        self.buf.clear();
        Ok(())
    }

    /// Emit the current run of first columns.
    pub(super) fn finish(&mut self, out: &mut impl FnMut(Key) -> Result<()>) -> Result<()> {
        if self.spills.is_empty() {
            if self.buf.len() >= PAR_SORT_MIN {
                self.buf.par_sort_unstable();
            } else {
                self.buf.sort_unstable();
            }
            for &k in &self.buf {
                out(k)?;
            }
        } else {
            if !self.buf.is_empty() {
                self.spill()?;
            }
            let mut cursors = self
                .spills
                .iter()
                .map(|p| Cursor::inline(p))
                .collect::<Result<Vec<_>>>()?;
            merge(&mut cursors, &mut *out)?;
            for p in self.spills.drain(..) {
                std::fs::remove_file(p)?;
            }
        }
        // a large run's buffer is not kept for the many small ones after it
        if self.buf.capacity() > PAR_SORT_MIN * 16 {
            self.buf = Vec::new();
        } else {
            self.buf.clear();
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn keys(n: u64, seed: u64) -> Vec<Key> {
        let mut x = seed;
        let mut v: Vec<Key> = (0..n)
            .map(|_| {
                x = x
                    .wrapping_mul(6364136223846793005)
                    .wrapping_add(1442695040888963407);
                [x >> 60, (x >> 20) & 0xFF, x & 0xFFFF, 1 << 60]
            })
            .collect();
        v.sort_unstable();
        v
    }

    #[test]
    fn runs_round_trip_and_merge() {
        let dir = tempfile::tempdir().unwrap();
        let a = keys(100_000, 1);
        let b = keys(3, 2);
        let c = keys(RUN_BLOCK as u64, 3);
        let mut all = Vec::new();
        let mut paths = Vec::new();
        for (i, k) in [&a, &b, &c, &Vec::new()].into_iter().enumerate() {
            let p = dir.path().join(format!("r{i}"));
            write_run(&p, k).unwrap();
            paths.push(p);
            all.extend_from_slice(k);
        }
        all.sort_unstable();
        let mut got = Vec::new();
        std::thread::scope(|s| {
            let mut cs: Vec<Cursor> = paths
                .iter()
                .map(|p| Cursor::threaded(s, p.clone()))
                .collect();
            merge(&mut cs, |k| {
                got.push(k);
                Ok(())
            })
            .unwrap();
        });
        assert_eq!(got, all);
        // a damaged run is an error, not a panic
        let p = &paths[0];
        let mut bytes = std::fs::read(p).unwrap();
        bytes.truncate(bytes.len() / 2);
        std::fs::write(p, bytes).unwrap();
        let mut c = vec![Cursor::inline(p).unwrap()];
        assert!(merge(&mut c, |_| Ok(())).is_err());
    }

    #[test]
    fn regroup_sorts_within_first_column_and_spills() {
        let dir = tempfile::tempdir().unwrap();
        // first column sorted, the rest in any order
        let mut input: Vec<Key> = keys(50_000, 7);
        for k in input.iter_mut() {
            k.swap(1, 2);
        }
        input.sort_by_key(|k| k[0]);
        let mut want = input.clone();
        want.sort_unstable();
        for budget in [100, 1000, usize::MAX] {
            let mut rg = Regroup::new(dir.path(), "t", budget);
            let mut got = Vec::new();
            let mut out = |k: Key| {
                got.push(k);
                Ok(())
            };
            for &k in &input {
                rg.push(k, &mut out).unwrap();
            }
            rg.finish(&mut out).unwrap();
            assert_eq!(got, want, "budget {budget}");
        }
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 0);
    }
}
