//! Predicate patterns, the "pattern trick" of QLever (X11 Phase 1).
//!
//! A pattern is the set of predicates one subject has. A generation keeps, beside its
//! permutations, the file `patterns.bin`, which gives every subject the number of its
//! pattern and lists the patterns. The question "which predicates do these subjects have,
//! and how many of the subjects have each" then costs one lookup per subject and one
//! addition per pattern and predicate, instead of a scan of every triple of every
//! subject. With no input at all, the per-predicate totals stored with each table answer
//! it without touching a subject.
//!
//! The file holds two tables. The default table covers the default graph. The named table
//! covers the union of the named graphs, which is what a query reads as its default graph
//! when the union default graph is on, and what `GRAPH <urn:x-arq:UnionGraph>` reads.
//! Classes are not part of a pattern, unlike a characteristic set of the statistics,
//! because `?s a <C>` is a join with POS that the table does not need to answer.
//!
//! # The delta
//!
//! The tables describe the base index. A snapshot with a delta (pending commits, an open
//! transaction, a branch's base overlay, a past state replayed from the log) is corrected
//! per subject: every subject that an inserted or deleted quad of the table's graphs
//! names is read again from SPO with the delta merged in, and that set of predicates
//! replaces its base pattern ([`Correction`]). The work grows with the subjects the delta
//! touches, which compaction bounds, and the caller gives up past a limit.
//!
//! # File format, version 1
//!
//! All integers are little-endian. The file starts with the magic `SPKPATT\0`, the
//! version as a u32 and the number of tables as a u32, followed by one directory entry of
//! 96 bytes per table. Each table then stores, at the offsets its entry names:
//!
//! * the patterns, each a varint count and the predicate ids as varints, the first in
//!   full and the others as the difference to the one before;
//! * the pattern directory, 16 bytes per pattern: the offset of its bytes and the number
//!   of subjects that have it;
//! * the predicate totals, 16 bytes per predicate in id order: the predicate and the
//!   number of subjects that have it;
//! * the subjects in blocks of [`BLOCK_SUBJECTS`], each a varint count and per subject the
//!   difference to the subject before as a varint (the first block entry's difference is
//!   to the block's first subject, so it is 0) and its pattern number as a varint;
//! * the block index, 16 bytes per block: its first subject and the offset of its bytes.
//!
//! A footer of 24 bytes ends the file: the length of everything before it, a checksum
//! (the first 8 bytes of a SHA-256 over the header, the directory and a SHA-256 of the
//! rest) and the magic `SPKPATE\0`. Opening a file checks its magic, version and layout.
//! `sparkles check` also checks the checksum and every subject's pattern against SPO.
//! A full build numbers the patterns by decreasing subject count, so that frequent ones
//! have small numbers. A partial compaction appends the new patterns of the subjects it
//! touches and keeps patterns that lost their last subject until the next full build.

use crate::error::{Error, Result};
use crate::id::Id;
use crate::index::{Key, Perm, PermIndex};
use crate::store::{Chunk, Snapshot};
use rustc_hash::FxHashMap;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::fs::File;
use std::io::{BufReader, BufWriter, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// The file of a generation's predicate patterns.
pub const FILE: &str = "patterns.bin";
/// The version of `patterns.bin` this build writes (see [`crate::formats::PATTERNS`]).
pub const VERSION: u32 = 1;
const MAGIC: &[u8; 8] = b"SPKPATT\0";
const END: &[u8; 8] = b"SPKPATE\0";
/// Subjects per block of a table.
pub const BLOCK_SUBJECTS: usize = 1024;
const HEADER: usize = 16;
const DIR: usize = 96;
const FOOTER: usize = 24;
/// The default memory budget of the pattern map while a generation is built.
pub const DEFAULT_BUDGET: usize = 1 << 30;
/// The bytes a pattern of `n` predicates is charged while a build collects the patterns:
/// its predicates, the boxed slice and the map entry with its counts.
fn charge(n: usize) -> usize {
    n * 8 + 64
}

/// The graphs a table covers.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Scope {
    /// the default graph
    Default,
    /// the union of the named graphs
    Named,
}

impl Scope {
    pub const ALL: [Scope; 2] = [Scope::Default, Scope::Named];

    pub fn index(self) -> usize {
        self as usize
    }

    /// Whether a quad of graph `g` counts in this table.
    #[inline]
    pub fn accepts(self, g: u64) -> bool {
        match self {
            Scope::Default => g == Id::DEFAULT_GRAPH.0,
            Scope::Named => g != Id::DEFAULT_GRAPH.0,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Scope::Default => "default",
            Scope::Named => "named",
        }
    }

    fn from_code(c: u32) -> Option<Scope> {
        match c {
            0 => Some(Scope::Default),
            1 => Some(Scope::Named),
            _ => None,
        }
    }
}

/// What `meta.json` records about a generation's patterns (`patterns`).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PatternInfo {
    /// the version of `patterns.bin`, 0 when the generation has none
    pub format: u32,
    /// why the generation has no patterns
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub skipped: Option<String>,
    /// patterns per table, default then named
    #[serde(default)]
    pub patterns: [u64; 2],
    /// subjects per table, default then named
    #[serde(default)]
    pub subjects: [u64; 2],
    /// the size of `patterns.bin`
    #[serde(default)]
    pub bytes: u64,
}

impl PatternInfo {
    pub fn skipped(why: impl Into<String>) -> PatternInfo {
        PatternInfo {
            skipped: Some(why.into()),
            ..Default::default()
        }
    }
}

// ------------------------------------------------------------------ varints ------

fn put_varint(out: &mut Vec<u8>, mut v: u64) {
    while v >= 0x80 {
        out.push((v as u8) | 0x80);
        v >>= 7;
    }
    out.push(v as u8);
}

#[inline]
fn get_varint(buf: &[u8], pos: &mut usize) -> Option<u64> {
    let mut v = 0u64;
    let mut shift = 0u32;
    loop {
        let b = *buf.get(*pos)?;
        *pos += 1;
        if shift == 63 && b > 1 {
            return None;
        }
        v |= u64::from(b & 0x7F) << shift;
        if b < 0x80 {
            return Some(v);
        }
        shift += 7;
        if shift > 63 {
            return None;
        }
    }
}

fn corrupt(what: impl std::fmt::Display) -> Error {
    Error::Corrupt(format!("{FILE}: {what}"))
}

// ------------------------------------------------------------------ writing ------

/// A table's directory entry.
#[derive(Clone, Copy, Debug, Default)]
struct Table {
    scope: u32,
    block_subjects: u32,
    patterns: u64,
    subjects: u64,
    blocks: u64,
    preds: u64,
    pat_bytes: u64,
    pat_bytes_len: u64,
    pat_dir: u64,
    pred_tot: u64,
    block_data: u64,
    block_data_len: u64,
    block_idx: u64,
}

impl Table {
    fn encode(&self) -> [u8; DIR] {
        let mut b = [0u8; DIR];
        b[0..4].copy_from_slice(&self.scope.to_le_bytes());
        b[4..8].copy_from_slice(&self.block_subjects.to_le_bytes());
        let words = [
            self.patterns,
            self.subjects,
            self.blocks,
            self.preds,
            self.pat_bytes,
            self.pat_bytes_len,
            self.pat_dir,
            self.pred_tot,
            self.block_data,
            self.block_data_len,
            self.block_idx,
        ];
        for (i, w) in words.iter().enumerate() {
            b[8 + i * 8..16 + i * 8].copy_from_slice(&w.to_le_bytes());
        }
        b
    }

    fn decode(b: &[u8]) -> Table {
        let u32_at = |o: usize| u32::from_le_bytes(b[o..o + 4].try_into().unwrap());
        let w = |i: usize| u64::from_le_bytes(b[8 + i * 8..16 + i * 8].try_into().unwrap());
        Table {
            scope: u32_at(0),
            block_subjects: u32_at(4),
            patterns: w(0),
            subjects: w(1),
            blocks: w(2),
            preds: w(3),
            pat_bytes: w(4),
            pat_bytes_len: w(5),
            pat_dir: w(6),
            pred_tot: w(7),
            block_data: w(8),
            block_data_len: w(9),
            block_idx: w(10),
        }
    }
}

/// Visits the patterns of a table in number order, as (predicates, subjects).
pub(crate) type PatternSource<'a> =
    Box<dyn FnOnce(&mut dyn FnMut(&[u64], u64) -> Result<()>) -> Result<()> + 'a>;
/// Visits the subjects of a table in id order, as (subject, pattern number).
pub(crate) type SubjectSource<'a> =
    Box<dyn FnOnce(&mut dyn FnMut(u64, u32) -> Result<()>) -> Result<()> + 'a>;

/// One table to write.
pub(crate) struct TableInput<'a> {
    pub scope: Scope,
    pub patterns: PatternSource<'a>,
    pub subjects: SubjectSource<'a>,
}

/// Writes `patterns.bin` front to back, hashing what it writes.
struct FileWriter {
    f: BufWriter<File>,
    pos: u64,
    hash: Sha256,
    tables: Vec<Table>,
    n: usize,
}

impl FileWriter {
    fn create(path: &Path, n: usize) -> Result<FileWriter> {
        let f = File::create(path)?;
        let mut w = FileWriter {
            f: BufWriter::with_capacity(1 << 20, f),
            pos: 0,
            hash: Sha256::new(),
            tables: Vec::new(),
            n,
        };
        // the header and the directory are written last, over these zeros
        let head = vec![0u8; HEADER + n * DIR];
        w.f.write_all(&head)?;
        w.pos = head.len() as u64;
        Ok(w)
    }

    fn put(&mut self, b: &[u8]) -> Result<()> {
        self.f.write_all(b)?;
        self.hash.update(b);
        self.pos += b.len() as u64;
        Ok(())
    }

    fn table(&mut self, input: TableInput<'_>) -> Result<()> {
        let mut t = Table {
            scope: input.scope.index() as u32,
            block_subjects: BLOCK_SUBJECTS as u32,
            pat_bytes: self.pos,
            ..Default::default()
        };
        // the patterns, then their directory and the predicate totals
        let mut dir: Vec<(u64, u64)> = Vec::new();
        let mut totals: FxHashMap<u64, u64> = FxHashMap::default();
        let mut buf: Vec<u8> = Vec::new();
        let start = self.pos;
        let mut err: Option<Error> = None;
        (input.patterns)(&mut |preds: &[u64], subjects: u64| {
            if err.is_some() {
                return Ok(());
            }
            if preds.windows(2).any(|w| w[0] >= w[1]) {
                return Err(Error::Corrupt(
                    "predicate pattern not sorted or with a repeated predicate".into(),
                ));
            }
            buf.clear();
            put_varint(&mut buf, preds.len() as u64);
            let mut prev = 0u64;
            for (i, &p) in preds.iter().enumerate() {
                put_varint(&mut buf, if i == 0 { p } else { p - prev });
                prev = p;
                if subjects > 0 {
                    *totals.entry(p).or_default() += subjects;
                }
            }
            dir.push((self.pos - start, subjects));
            if let Err(e) = self.put(&buf) {
                err = Some(e);
            }
            Ok(())
        })?;
        if let Some(e) = err {
            return Err(e);
        }
        t.patterns = dir.len() as u64;
        t.pat_bytes_len = self.pos - start;
        t.pat_dir = self.pos;
        let mut out = Vec::with_capacity(dir.len().min(1 << 16) * 16);
        for (off, n) in &dir {
            out.extend_from_slice(&off.to_le_bytes());
            out.extend_from_slice(&n.to_le_bytes());
            if out.len() >= 1 << 20 {
                self.put(&out)?;
                out.clear();
            }
        }
        self.put(&out)?;
        drop(dir);
        let mut totals: Vec<(u64, u64)> = totals.into_iter().collect();
        totals.sort_unstable();
        t.preds = totals.len() as u64;
        t.pred_tot = self.pos;
        out.clear();
        for (p, n) in &totals {
            out.extend_from_slice(&p.to_le_bytes());
            out.extend_from_slice(&n.to_le_bytes());
        }
        self.put(&out)?;
        drop(totals);
        // the subjects in blocks, then the block index
        t.block_data = self.pos;
        let mut index: Vec<(u64, u64)> = Vec::new();
        let mut block: Vec<(u64, u32)> = Vec::with_capacity(BLOCK_SUBJECTS);
        let mut last: Option<u64> = None;
        let npatterns = t.patterns;
        let mut subjects = 0u64;
        let block_data = t.block_data;
        let mut flush = |w: &mut FileWriter, block: &mut Vec<(u64, u32)>| -> Result<()> {
            let Some(&(first, _)) = block.first() else {
                return Ok(());
            };
            index.push((first, w.pos - block_data));
            buf.clear();
            put_varint(&mut buf, block.len() as u64);
            let mut prev = first;
            for &(s, pid) in block.iter() {
                put_varint(&mut buf, s - prev);
                put_varint(&mut buf, u64::from(pid));
                prev = s;
            }
            block.clear();
            w.put(&buf)
        };
        (input.subjects)(&mut |s: u64, pid: u32| {
            if last.is_some_and(|l| l >= s) {
                return Err(Error::Corrupt(format!(
                    "pattern subjects out of order at {s:#x}"
                )));
            }
            if u64::from(pid) >= npatterns {
                return Err(Error::Corrupt(format!(
                    "subject {s:#x} has pattern {pid} of {npatterns}"
                )));
            }
            last = Some(s);
            subjects += 1;
            block.push((s, pid));
            if block.len() == BLOCK_SUBJECTS {
                flush(self, &mut block)?;
            }
            Ok(())
        })?;
        flush(self, &mut block)?;
        t.subjects = subjects;
        t.block_data_len = self.pos - t.block_data;
        t.blocks = index.len() as u64;
        t.block_idx = self.pos;
        out.clear();
        for (first, off) in &index {
            out.extend_from_slice(&first.to_le_bytes());
            out.extend_from_slice(&off.to_le_bytes());
        }
        self.put(&out)?;
        self.tables.push(t);
        Ok(())
    }

    /// Write the footer, the header and the directory, and sync the file. Returns its
    /// size.
    fn finish(mut self) -> Result<u64> {
        if self.tables.len() != self.n {
            return Err(Error::Corrupt(format!(
                "{} pattern tables written, {} announced",
                self.tables.len(),
                self.n
            )));
        }
        let mut head = Vec::with_capacity(HEADER + self.n * DIR);
        head.extend_from_slice(MAGIC);
        head.extend_from_slice(&VERSION.to_le_bytes());
        head.extend_from_slice(&(self.n as u32).to_le_bytes());
        for t in &self.tables {
            head.extend_from_slice(&t.encode());
        }
        let rest = std::mem::take(&mut self.hash).finalize();
        let sum = checksum(&head, &rest);
        let body = self.pos;
        self.f.write_all(&body.to_le_bytes())?;
        self.f.write_all(&sum.to_le_bytes())?;
        self.f.write_all(END)?;
        let mut f = self.f.into_inner().map_err(|e| e.into_error())?;
        f.seek(SeekFrom::Start(0))?;
        f.write_all(&head)?;
        f.sync_all()?;
        Ok(body + FOOTER as u64)
    }
}

fn checksum(head: &[u8], rest: &[u8]) -> u64 {
    let mut h = Sha256::new();
    h.update(head);
    h.update(rest);
    u64::from_le_bytes(h.finalize()[..8].try_into().unwrap())
}

/// Write `patterns.bin` into `dir` from `tables`, synced. Returns the file's size.
pub(crate) fn write_tables(dir: &Path, tables: Vec<TableInput<'_>>) -> Result<u64> {
    let path = dir.join(FILE);
    let result = (|| {
        let mut w = FileWriter::create(&path, tables.len())?;
        for t in tables {
            w.table(t)?;
        }
        w.finish()
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&path);
    }
    result
}

// ------------------------------------------------------------------ building -----

/// Collects the predicate patterns of the subjects while a build streams SPO, in key
/// order. Each subject's two pattern numbers (default graph, named graphs) go to a
/// temporary file in subject order; [`PatternCollector::finish`] renumbers the patterns
/// by decreasing subject count and writes `patterns.bin`.
pub(crate) struct PatternCollector {
    cur: Option<u64>,
    sets: [Vec<u64>; 2],
    ids: FxHashMap<Box<[u64]>, u32>,
    /// subjects per pattern (in order of first appearance) and table
    counts: [Vec<u64>; 2],
    bytes: usize,
    budget: usize,
    skipped: Option<String>,
    run_path: PathBuf,
    run: Option<BufWriter<File>>,
    buf: Vec<u8>,
    last: u64,
    entries: u64,
}

impl PatternCollector {
    /// A collector whose temporary file lives in `tmp`, and whose pattern map may use
    /// `budget` bytes.
    pub(crate) fn new(tmp: &Path, budget: usize) -> PatternCollector {
        let run_path = tmp.join("patterns.run");
        let (run, skipped) = match File::create(&run_path) {
            Ok(f) => (Some(BufWriter::with_capacity(1 << 20, f)), None),
            Err(e) => (None, Some(format!("the temporary pattern file: {e}"))),
        };
        PatternCollector {
            cur: None,
            sets: [Vec::new(), Vec::new()],
            ids: FxHashMap::default(),
            counts: [Vec::new(), Vec::new()],
            bytes: 0,
            budget,
            skipped,
            run_path,
            run,
            buf: Vec::new(),
            last: 0,
            entries: 0,
        }
    }

    fn give_up(&mut self, why: String) {
        tracing::warn!(target: "sparkles::builder", "predicate patterns not built: {why}");
        self.skipped = Some(why);
        self.ids = FxHashMap::default();
        self.counts = [Vec::new(), Vec::new()];
        self.run = None;
        let _ = std::fs::remove_file(&self.run_path);
    }

    /// One SPO key (subject, predicate, object, graph), in key order.
    #[inline]
    pub(crate) fn push(&mut self, k: &Key) {
        if self.skipped.is_some() {
            return;
        }
        if self.cur != Some(k[0]) {
            self.subject_done();
            self.cur = Some(k[0]);
        }
        let set = &mut self.sets[usize::from(!Scope::Default.accepts(k[3]))];
        if set.last() != Some(&k[1]) {
            set.push(k[1]);
        }
    }

    fn intern(&mut self, t: usize) -> Option<u32> {
        if let Some(&id) = self.ids.get(&self.sets[t][..]) {
            self.counts[t][id as usize] += 1;
            return Some(id);
        }
        self.bytes += charge(self.sets[t].len());
        if self.bytes > self.budget {
            let why = format!(
                "the patterns need more than their memory budget of {} bytes (pattern_bytes)",
                self.budget
            );
            self.give_up(why);
            return None;
        }
        let id = self.ids.len() as u32;
        self.ids.insert(self.sets[t].clone().into_boxed_slice(), id);
        for c in &mut self.counts {
            c.push(0);
        }
        self.counts[t][id as usize] = 1;
        Some(id)
    }

    fn subject_done(&mut self) {
        let Some(s) = self.cur.take() else {
            return;
        };
        let mut codes = [0u64; 2];
        for (t, code) in codes.iter_mut().enumerate() {
            if self.sets[t].is_empty() {
                continue;
            }
            match self.intern(t) {
                Some(id) => *code = u64::from(id) + 1,
                None => return,
            }
            self.sets[t].clear();
        }
        self.buf.clear();
        put_varint(&mut self.buf, s - self.last);
        put_varint(&mut self.buf, codes[0]);
        put_varint(&mut self.buf, codes[1]);
        self.last = s;
        self.entries += 1;
        if let Some(run) = &mut self.run
            && let Err(e) = run.write_all(&self.buf)
        {
            self.give_up(format!("the temporary pattern file: {e}"));
        }
    }

    /// Write `patterns.bin` into `dir`, or record why the generation has none.
    pub(crate) fn finish(mut self, dir: &Path) -> Result<PatternInfo> {
        self.subject_done();
        if let Some(why) = self.skipped.take() {
            return Ok(PatternInfo::skipped(why));
        }
        let Some(run) = self.run.take() else {
            return Ok(PatternInfo::skipped(
                "the temporary pattern file is missing",
            ));
        };
        run.into_inner().map_err(|e| e.into_error())?.sync_data()?;
        let mut sets: Vec<Box<[u64]>> = vec![Box::default(); self.ids.len()];
        for (set, id) in std::mem::take(&mut self.ids) {
            sets[id as usize] = set;
        }
        // per table: the new number of each pattern, and the patterns in that order
        let mut renumber: [Vec<u32>; 2] = [Vec::new(), Vec::new()];
        let mut order: [Vec<u32>; 2] = [Vec::new(), Vec::new()];
        for t in 0..2 {
            let counts = &self.counts[t];
            let mut o: Vec<u32> = (0..sets.len() as u32)
                .filter(|&i| counts[i as usize] > 0)
                .collect();
            o.sort_unstable_by(|&a, &b| {
                counts[b as usize]
                    .cmp(&counts[a as usize])
                    .then_with(|| sets[a as usize].cmp(&sets[b as usize]))
            });
            let mut r = vec![u32::MAX; sets.len()];
            for (new, &old) in o.iter().enumerate() {
                r[old as usize] = new as u32;
            }
            renumber[t] = r;
            order[t] = o;
        }
        let entries = self.entries;
        let run_path = self.run_path.clone();
        let mut info = PatternInfo {
            format: VERSION,
            ..Default::default()
        };
        for (t, o) in order.iter().enumerate() {
            info.patterns[t] = o.len() as u64;
            info.subjects[t] = o.iter().map(|&i| self.counts[t][i as usize]).sum();
        }
        let tables: Vec<TableInput<'_>> = Scope::ALL
            .iter()
            .map(|&scope| {
                let t = scope.index();
                let (order, renumber, counts, sets, run_path) =
                    (&order[t], &renumber[t], &self.counts[t], &sets, &run_path);
                TableInput {
                    scope,
                    patterns: Box::new(move |f| {
                        for &i in order {
                            f(&sets[i as usize], counts[i as usize])?;
                        }
                        Ok(())
                    }),
                    subjects: Box::new(move |f| {
                        read_run(run_path, entries, |s, codes| match codes[t] {
                            0 => Ok(()),
                            c => f(s, renumber[(c - 1) as usize]),
                        })
                    }),
                }
            })
            .collect();
        info.bytes = write_tables(dir, tables)?;
        let _ = std::fs::remove_file(&run_path);
        Ok(info)
    }
}

/// Read the temporary file of a [`PatternCollector`]: `entries` subjects with their two
/// pattern codes (0: none, else the pattern number plus one).
fn read_run(
    path: &Path,
    entries: u64,
    mut f: impl FnMut(u64, [u64; 2]) -> Result<()>,
) -> Result<()> {
    let mut r = BufReader::with_capacity(1 << 20, File::open(path)?);
    let bad = || Error::Corrupt(format!("temporary pattern file {}", path.display()));
    let mut byte = || -> Result<u8> {
        let mut b = [0u8; 1];
        r.read_exact(&mut b).map_err(|_| bad())?;
        Ok(b[0])
    };
    let mut varint = || -> Result<u64> {
        let mut v = 0u64;
        let mut shift = 0;
        loop {
            let b = byte()?;
            v |= u64::from(b & 0x7F) << shift;
            if b < 0x80 {
                return Ok(v);
            }
            shift += 7;
            if shift > 63 {
                return Err(bad());
            }
        }
    };
    let mut s = 0u64;
    for _ in 0..entries {
        s += varint()?;
        let a = varint()?;
        let b = varint()?;
        f(s, [a, b])?;
    }
    Ok(())
}

// ------------------------------------------------------------------ reading ------

/// An opened `patterns.bin`.
pub struct PatternFile {
    map: memmap2::Mmap,
    tables: [Option<Table>; 2],
}

impl std::fmt::Debug for PatternFile {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PatternFile")
            .field("bytes", &self.map.len())
            .finish()
    }
}

impl PatternFile {
    /// Open the patterns of the generation in `dir`: `None` when it has none. A file of a
    /// version this build does not read is refused with
    /// [`crate::formats::PATTERNS`]'s message, and a damaged one is [`Error::Corrupt`].
    pub fn open(dir: &Path) -> Result<Option<PatternFile>> {
        let path = dir.join(FILE);
        let f = match File::open(&path) {
            Ok(f) => f,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(e.into()),
        };
        let len = f.metadata()?.len() as usize;
        if len < HEADER + FOOTER {
            return Err(corrupt("shorter than its header"));
        }
        // SAFETY: the file is never written after its generation is published, and a
        // generation's files are removed only once no reader holds the generation
        let map = unsafe { memmap2::Mmap::map(&f)? };
        if &map[..8] != MAGIC {
            return Err(corrupt("not a pattern file (bad magic)"));
        }
        let version = u32::from_le_bytes(map[8..12].try_into().unwrap());
        let kind = &crate::formats::PATTERNS;
        if !kind.reads(version) {
            return Err(Error::Unsupported(format!(
                "{} has patterns version {version}; Sparkles {} reads version {}. The \
                 patterns are not used: queries read the permutations instead, and the next \
                 full compaction writes them again",
                path.display(),
                crate::formats::SPARKLES_VERSION,
                kind.readable()
            )));
        }
        let n = u32::from_le_bytes(map[12..16].try_into().unwrap()) as usize;
        let foot = len - FOOTER;
        if &map[foot + 16..] != END {
            return Err(corrupt("incomplete (no end marker)"));
        }
        let body = u64::from_le_bytes(map[foot..foot + 8].try_into().unwrap());
        if body != foot as u64 || n > 2 || HEADER + n * DIR > foot {
            return Err(corrupt("its length does not match its header"));
        }
        let mut tables: [Option<Table>; 2] = [None, None];
        let inside = |off: u64, len: u64| {
            off.checked_add(len)
                .is_some_and(|e| off >= (HEADER + n * DIR) as u64 && e <= foot as u64)
        };
        for i in 0..n {
            let t = Table::decode(&map[HEADER + i * DIR..HEADER + (i + 1) * DIR]);
            let scope = Scope::from_code(t.scope).ok_or_else(|| corrupt("unknown table"))?;
            let ok = t.block_subjects > 0
                && inside(t.pat_bytes, t.pat_bytes_len)
                && t.patterns
                    .checked_mul(16)
                    .is_some_and(|l| inside(t.pat_dir, l))
                && t.preds
                    .checked_mul(16)
                    .is_some_and(|l| inside(t.pred_tot, l))
                && inside(t.block_data, t.block_data_len)
                && t.blocks
                    .checked_mul(16)
                    .is_some_and(|l| inside(t.block_idx, l))
                && t.patterns <= u64::from(u32::MAX)
                && t.blocks <= t.subjects
                && t.subjects
                    <= t.blocks
                        .saturating_mul(u64::from(t.block_subjects).max(1) * 2);
            if !ok || tables[scope.index()].is_some() {
                return Err(corrupt(format!("the {} table's layout", scope.name())));
            }
            tables[scope.index()] = Some(t);
        }
        Ok(Some(PatternFile { map, tables }))
    }

    /// The size of the file.
    pub fn bytes(&self) -> u64 {
        self.map.len() as u64
    }

    /// The table of `scope`, if the file has it.
    pub fn table(&self, scope: Scope) -> Option<TableRef<'_>> {
        self.tables[scope.index()]
            .as_ref()
            .map(|t| TableRef { file: self, t })
    }

    #[inline]
    fn u64_at(&self, off: u64) -> u64 {
        let o = off as usize;
        u64::from_le_bytes(self.map[o..o + 8].try_into().unwrap())
    }

    /// Check the checksum of the whole file.
    pub fn verify_checksum(&self) -> Result<()> {
        let foot = self.map.len() - FOOTER;
        let n = u32::from_le_bytes(self.map[12..16].try_into().unwrap()) as usize;
        let head = &self.map[..HEADER + n * DIR];
        let rest = Sha256::digest(&self.map[HEADER + n * DIR..foot]);
        let want = u64::from_le_bytes(self.map[foot + 8..foot + 16].try_into().unwrap());
        if checksum(head, &rest) != want {
            return Err(corrupt("checksum mismatch"));
        }
        Ok(())
    }
}

/// One table of a [`PatternFile`].
#[derive(Clone, Copy)]
pub struct TableRef<'a> {
    file: &'a PatternFile,
    t: &'a Table,
}

impl<'a> TableRef<'a> {
    pub fn patterns(&self) -> u64 {
        self.t.patterns
    }

    pub fn subjects(&self) -> u64 {
        self.t.subjects
    }

    /// The number of distinct predicates of the table.
    pub fn predicates(&self) -> u64 {
        self.t.preds
    }

    /// The subjects that have pattern `pid`.
    pub fn pattern_subjects(&self, pid: u32) -> u64 {
        self.file.u64_at(self.t.pat_dir + u64::from(pid) * 16 + 8)
    }

    /// The predicates of pattern `pid`, in id order, into `out` (cleared first).
    pub fn pattern(&self, pid: u32, out: &mut Vec<u64>) -> Result<()> {
        out.clear();
        if u64::from(pid) >= self.t.patterns {
            return Err(corrupt(format!("pattern {pid} of {}", self.t.patterns)));
        }
        let off = self.file.u64_at(self.t.pat_dir + u64::from(pid) * 16);
        let lo = self.t.pat_bytes as usize;
        let bytes = &self.file.map[lo..lo + self.t.pat_bytes_len as usize];
        let mut pos = off as usize;
        let bad = || corrupt(format!("pattern {pid}"));
        let n = get_varint(bytes, &mut pos).ok_or_else(bad)?;
        if n > bytes.len() as u64 {
            return Err(bad());
        }
        let mut prev = 0u64;
        for i in 0..n {
            let d = get_varint(bytes, &mut pos).ok_or_else(bad)?;
            let p = if i == 0 {
                d
            } else {
                prev.checked_add(d).filter(|_| d > 0).ok_or_else(bad)?
            };
            out.push(p);
            prev = p;
        }
        Ok(())
    }

    /// (predicate, subjects that have it), in predicate order.
    pub fn predicate_totals(&self) -> impl Iterator<Item = (u64, u64)> + 'a {
        let (file, t) = (self.file, *self.t);
        (0..t.preds).map(move |i| {
            (
                file.u64_at(t.pred_tot + i * 16),
                file.u64_at(t.pred_tot + i * 16 + 8),
            )
        })
    }

    fn block_first(&self, b: u64) -> u64 {
        self.file.u64_at(self.t.block_idx + b * 16)
    }

    /// The block that holds subject `s` if any block does: the last whose first subject
    /// is not above it.
    fn find_block(&self, s: u64) -> Option<u64> {
        let (mut lo, mut hi) = (0u64, self.t.blocks);
        while lo < hi {
            let mid = lo + (hi - lo) / 2;
            if self.block_first(mid) <= s {
                lo = mid + 1;
            } else {
                hi = mid;
            }
        }
        lo.checked_sub(1)
    }

    /// The subjects of block `b` with their pattern numbers, into `out` (cleared first).
    fn block(&self, b: u64, out: &mut Vec<(u64, u32)>) -> Result<()> {
        out.clear();
        let first = self.block_first(b);
        let start = self.file.u64_at(self.t.block_idx + b * 16 + 8);
        let end = if b + 1 < self.t.blocks {
            self.file.u64_at(self.t.block_idx + (b + 1) * 16 + 8)
        } else {
            self.t.block_data_len
        };
        let bad = || corrupt(format!("subject block {b}"));
        if start > end || end > self.t.block_data_len {
            return Err(bad());
        }
        let lo = (self.t.block_data + start) as usize;
        let bytes = &self.file.map[lo..(self.t.block_data + end) as usize];
        let mut pos = 0usize;
        let n = get_varint(bytes, &mut pos).ok_or_else(bad)?;
        if n == 0 || n > bytes.len() as u64 {
            return Err(bad());
        }
        let mut s = first;
        for i in 0..n {
            let d = get_varint(bytes, &mut pos).ok_or_else(bad)?;
            if i > 0 && d == 0 {
                return Err(bad());
            }
            s = s.checked_add(d).ok_or_else(bad)?;
            let pid = get_varint(bytes, &mut pos).ok_or_else(bad)?;
            if pid >= self.t.patterns {
                return Err(bad());
            }
            out.push((s, pid as u32));
        }
        Ok(())
    }

    /// Visit every subject with its pattern number, in subject order, until `f` returns
    /// `false`.
    pub fn for_each_until(&self, mut f: impl FnMut(u64, u32) -> Result<bool>) -> Result<()> {
        let mut rows = Vec::with_capacity(BLOCK_SUBJECTS);
        for b in 0..self.t.blocks {
            self.block(b, &mut rows)?;
            for &(s, pid) in &rows {
                if !f(s, pid)? {
                    return Ok(());
                }
            }
        }
        Ok(())
    }

    /// A cursor for looking up subjects, fastest in ascending order.
    pub fn cursor(&self) -> Cursor<'a> {
        Cursor {
            t: *self,
            b: None,
            rows: Vec::new(),
            i: 0,
        }
    }
}

/// Looks subjects up in a table, keeping the block of the last lookup.
pub struct Cursor<'a> {
    t: TableRef<'a>,
    b: Option<u64>,
    rows: Vec<(u64, u32)>,
    i: usize,
}

impl Cursor<'_> {
    /// The pattern number of subject `s` in the base table.
    pub fn seek(&mut self, s: u64) -> Result<Option<u32>> {
        let same = self.b.is_some_and(|b| {
            self.t.block_first(b) <= s
                && (b + 1 >= self.t.t.blocks || s < self.t.block_first(b + 1))
        });
        if !same {
            let Some(b) = self.t.find_block(s) else {
                return Ok(None);
            };
            self.t.block(b, &mut self.rows)?;
            self.b = Some(b);
            self.i = 0;
        }
        if self.rows.get(self.i).is_some_and(|r| r.0 > s) {
            self.i = 0;
        }
        while self.rows.get(self.i).is_some_and(|r| r.0 < s) {
            self.i += 1;
        }
        Ok(self.rows.get(self.i).filter(|r| r.0 == s).map(|r| r.1))
    }
}

// ------------------------------------------------------------- generation state ---

/// A generation's patterns as opened: the file, or why there is none to use.
#[derive(Clone, Default)]
pub struct Loaded {
    pub file: Option<Arc<PatternFile>>,
    /// why `file` is `None`
    pub missing: Option<Arc<str>>,
}

impl Loaded {
    /// Open the patterns of the generation in `dir`, whose metadata records `info`. A
    /// file this build cannot use is left unused with a warning, and queries read the
    /// permutations instead: the patterns are derived data.
    pub fn open(dir: &Path, info: Option<&PatternInfo>) -> Loaded {
        match PatternFile::open(dir) {
            Ok(Some(f)) => Loaded {
                file: Some(Arc::new(f)),
                missing: None,
            },
            Ok(None) => {
                let why = match info {
                    Some(PatternInfo {
                        skipped: Some(why), ..
                    }) => why.clone(),
                    Some(i) if i.format > 0 => {
                        tracing::warn!(
                            target: "sparkles::store",
                            "{}: {FILE} is missing; queries read the permutations instead",
                            dir.display()
                        );
                        format!("{FILE} is missing")
                    }
                    _ => "the generation was built without predicate patterns".to_string(),
                };
                Loaded {
                    file: None,
                    missing: Some(why.into()),
                }
            }
            Err(e) => {
                tracing::warn!(
                    target: "sparkles::store",
                    "{}: predicate patterns not used: {e}",
                    dir.display()
                );
                Loaded {
                    file: None,
                    missing: Some(e.to_string().into()),
                }
            }
        }
    }
}

// ------------------------------------------------------------------ the delta ----

/// A subject whose predicates the delta may have changed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Touched {
    pub subject: u64,
    /// its pattern in the base table
    pub base: Option<u32>,
    /// its predicates in the snapshot, in id order (empty: it is no longer a subject)
    pub now: Vec<u64>,
}

/// What a snapshot's delta changes in one table.
#[derive(Clone, Debug, Default)]
pub struct Correction {
    /// sorted by subject
    pub touched: Vec<Touched>,
}

impl Correction {
    pub fn get(&self, s: u64) -> Option<&Touched> {
        self.touched
            .binary_search_by_key(&s, |t| t.subject)
            .ok()
            .map(|i| &self.touched[i])
    }

    pub fn is_empty(&self) -> bool {
        self.touched.is_empty()
    }
}

/// The subjects that inserted or deleted quads of `scope`'s graphs name in the delta of
/// `snap`, sorted, or `None` when there are more than `limit`.
pub fn touched_subjects(snap: &Snapshot, scope: Scope, limit: usize) -> Option<Vec<u64>> {
    let pi = Perm::Spo.index();
    let mut out: Vec<u64> = Vec::new();
    for set in [&snap.delta.ins[pi], &snap.delta.del[pi]] {
        let mut last = None;
        for k in set.iter() {
            if scope.accepts(k[3]) && last != Some(k[0]) {
                last = Some(k[0]);
                out.push(k[0]);
                if out.len() > limit.saturating_mul(2) {
                    return None;
                }
            }
        }
    }
    out.sort_unstable();
    out.dedup();
    (out.len() <= limit).then_some(out)
}

/// The predicates subject `s` has in `scope`'s graphs of `snap`, base and delta merged,
/// into `out` (cleared first), in id order.
pub fn current_predicates(snap: &Snapshot, scope: Scope, s: u64, out: &mut Vec<u64>) -> Result<()> {
    out.clear();
    let mut add = |k: &Key| {
        if scope.accepts(k[3]) && out.last() != Some(&k[1]) {
            out.push(k[1]);
        }
    };
    snap.scan(Perm::Spo, &[s], |c| {
        match c {
            Chunk::Block(b, lo, hi) => (lo..hi).for_each(|i| add(&b.key(i))),
            Chunk::Row(k) => add(&k),
        }
        Ok(true)
    })
}

/// The correction of `table` (of `scope`) for the delta of `snap`, or `None` when the
/// delta touches more than `limit` subjects. `check` is called now and then.
pub fn correction(
    snap: &Snapshot,
    table: TableRef<'_>,
    scope: Scope,
    limit: usize,
    check: &dyn Fn() -> Result<()>,
) -> Result<Option<Correction>> {
    let Some(subjects) = touched_subjects(snap, scope, limit) else {
        return Ok(None);
    };
    let mut cursor = table.cursor();
    let mut touched = Vec::with_capacity(subjects.len());
    let mut now = Vec::new();
    for (i, s) in subjects.into_iter().enumerate() {
        if i % 256 == 255 {
            check()?;
        }
        let base = cursor.seek(s)?;
        current_predicates(snap, scope, s, &mut now)?;
        touched.push(Touched {
            subject: s,
            base,
            now: std::mem::take(&mut now),
        });
    }
    Ok(Some(Correction { touched }))
}

// ------------------------------------------------------------------ answers ------

/// Decoded patterns, the frequent ones (small numbers) kept.
struct Patterns<'a> {
    t: TableRef<'a>,
    kept: Vec<Option<Vec<u64>>>,
    buf: Vec<u64>,
}

/// Patterns kept decoded at most, those with the smallest numbers (the most subjects
/// after a full build).
const KEPT_PATTERNS: usize = 4096;

impl<'a> Patterns<'a> {
    fn new(t: TableRef<'a>) -> Patterns<'a> {
        Patterns {
            t,
            kept: Vec::new(),
            buf: Vec::new(),
        }
    }

    fn get(&mut self, pid: u32) -> Result<&[u64]> {
        let i = pid as usize;
        if i < KEPT_PATTERNS {
            if self.kept.len() <= i {
                self.kept.resize(i + 1, None);
            }
            if self.kept[i].is_none() {
                let mut v = Vec::new();
                self.t.pattern(pid, &mut v)?;
                self.kept[i] = Some(v);
            }
            return Ok(self.kept[i].as_deref().unwrap());
        }
        self.t.pattern(pid, &mut self.buf)?;
        Ok(&self.buf)
    }
}

fn into_sorted(m: FxHashMap<u64, u64>) -> Vec<(u64, u64)> {
    let mut v: Vec<(u64, u64)> = m.into_iter().filter(|&(_, n)| n > 0).collect();
    v.sort_unstable();
    v
}

/// The number of subjects that have each predicate, over every subject of the table
/// corrected by `corr`, in predicate order and without zeros.
pub fn count_all(t: TableRef<'_>, corr: &Correction) -> Result<Vec<(u64, u64)>> {
    let mut totals: FxHashMap<u64, i64> =
        t.predicate_totals().map(|(p, n)| (p, n as i64)).collect();
    let mut pats = Patterns::new(t);
    for x in &corr.touched {
        if let Some(pid) = x.base {
            for &p in pats.get(pid)? {
                *totals.entry(p).or_default() -= 1;
            }
        }
        for &p in &x.now {
            *totals.entry(p).or_default() += 1;
        }
    }
    let mut v: Vec<(u64, u64)> = totals
        .into_iter()
        .filter(|&(_, n)| n > 0)
        .map(|(p, n)| (p, n as u64))
        .collect();
    v.sort_unstable();
    Ok(v)
}

/// The sum of the weights of the subjects that have each predicate, for `subjects`
/// (distinct, in ascending order, each with its weight) and the table corrected by
/// `corr`, in predicate order and without zeros.
pub fn count_subjects(
    t: TableRef<'_>,
    corr: &Correction,
    subjects: &[(u64, u64)],
    check: &dyn Fn() -> Result<()>,
) -> Result<Vec<(u64, u64)>> {
    let mut per_pattern: FxHashMap<u32, u64> = FxHashMap::default();
    let mut totals: FxHashMap<u64, u64> = FxHashMap::default();
    let mut cursor = t.cursor();
    for (i, &(s, w)) in subjects.iter().enumerate() {
        if i % 4096 == 4095 {
            check()?;
        }
        if let Some(x) = corr.get(s) {
            for &p in &x.now {
                *totals.entry(p).or_default() += w;
            }
            continue;
        }
        if let Some(pid) = cursor.seek(s)? {
            *per_pattern.entry(pid).or_default() += w;
        }
    }
    let mut pats = Patterns::new(t);
    for (i, (pid, n)) in per_pattern.into_iter().enumerate() {
        if i % 4096 == 4095 {
            check()?;
        }
        for &p in pats.get(pid)? {
            *totals.entry(p).or_default() += n;
        }
    }
    Ok(into_sorted(totals))
}

/// The predicates subject `s` has, base and delta, into `out` (cleared first).
pub fn predicates_of(t: TableRef<'_>, corr: &Correction, s: u64, out: &mut Vec<u64>) -> Result<()> {
    out.clear();
    if let Some(x) = corr.get(s) {
        out.extend_from_slice(&x.now);
        return Ok(());
    }
    if let Some(pid) = t.cursor().seek(s)? {
        t.pattern(pid, out)?;
    }
    Ok(())
}

/// Visit every (subject, predicate) pair of the table corrected by `corr`, in subject
/// order and then predicate order, or only those with predicate `only`.
pub fn for_each_pair(
    t: TableRef<'_>,
    corr: &Correction,
    only: Option<u64>,
    check: &dyn Fn() -> Result<()>,
    mut f: impl FnMut(u64, u64) -> Result<()>,
) -> Result<()> {
    // with one predicate: the patterns that hold it, found once
    let holds: Option<Vec<bool>> = match only {
        Some(p) => {
            let mut v = vec![false; t.patterns() as usize];
            let mut buf = Vec::new();
            for (pid, x) in v.iter_mut().enumerate() {
                if pid % 65536 == 65535 {
                    check()?;
                }
                t.pattern(pid as u32, &mut buf)?;
                *x = buf.binary_search(&p).is_ok();
            }
            Some(v)
        }
        None => None,
    };
    let mut pats = Patterns::new(t);
    let emit_now = |s: u64, now: &[u64], f: &mut dyn FnMut(u64, u64) -> Result<()>| {
        for &p in now {
            if only.is_none_or(|o| o == p) {
                f(s, p)?;
            }
        }
        Ok::<(), Error>(())
    };
    let mut next = 0usize;
    let touched = &corr.touched;
    let mut n = 0u64;
    t.for_each_until(|s, pid| {
        n += 1;
        if n.is_multiple_of(65536) {
            check()?;
        }
        while let Some(x) = touched.get(next).filter(|x| x.subject < s) {
            emit_now(x.subject, &x.now, &mut f)?;
            next += 1;
        }
        if let Some(x) = touched.get(next).filter(|x| x.subject == s) {
            emit_now(s, &x.now, &mut f)?;
            next += 1;
            return Ok(true);
        }
        match (&holds, only) {
            (Some(h), Some(p)) => {
                if h[pid as usize] {
                    f(s, p)?;
                }
            }
            _ => {
                for &p in pats.get(pid)? {
                    f(s, p)?;
                }
            }
        }
        Ok(true)
    })?;
    for x in &touched[next..] {
        emit_now(x.subject, &x.now, &mut f)?;
    }
    Ok(())
}

/// The subjects the delta may touch at most for a reader to correct the tables per
/// subject on `snap`: one SPO probe each, so a share of the quads a scan reads instead.
pub fn touched_limit(snap: &Snapshot) -> usize {
    (snap.len() / crate::sparql::stats::PROBE_ROWS).clamp(1024, 1_000_000) as usize
}

/// The predicates that at least one of `subjects` (distinct, ascending) has in any graph
/// of `snap`, from both tables corrected for the delta, or `None` when the generation has
/// no patterns or the delta touches too many subjects. Every predicate one of the
/// subjects has in any graph is in the set.
pub fn predicates_of_subjects(
    snap: &Snapshot,
    subjects: &[u64],
    check: &dyn Fn() -> Result<()>,
) -> Result<Option<rustc_hash::FxHashSet<u64>>> {
    let Some(file) = snap.generation.patterns.file.clone() else {
        return Ok(None);
    };
    let weighted: Vec<(u64, u64)> = subjects.iter().map(|&s| (s, 1)).collect();
    let mut out = rustc_hash::FxHashSet::default();
    for scope in Scope::ALL {
        let Some(t) = file.table(scope) else {
            return Ok(None);
        };
        let Some(corr) = snap.counts.pattern_correction(scope, snap, |snap| {
            correction(snap, t, scope, touched_limit(snap), check)
        }) else {
            return Ok(None);
        };
        out.extend(
            count_subjects(t, &corr, &weighted, check)?
                .into_iter()
                .map(|(p, _)| p),
        );
    }
    Ok(Some(out))
}

// ------------------------------------------------------------ partial compaction --

/// Write the patterns of the generation that a partial compaction of `snap` writes into
/// `dir`: the base's tables with each subject the delta touches given its predicates
/// now. A new set of predicates becomes a new pattern with the next number, and a
/// pattern that lost its last subject stays until the next full build.
pub(crate) fn rewrite(
    snap: &Snapshot,
    dir: &Path,
    check: &dyn Fn() -> Result<()>,
) -> Result<PatternInfo> {
    let Some(file) = snap.generation.patterns.file.clone() else {
        return Ok(PatternInfo::skipped(
            "the generation it was compacted from had none; the next full compaction builds them",
        ));
    };
    struct Plan {
        scope: Scope,
        corr: Correction,
        /// the pattern number of each touched subject now (`u32::MAX`: none)
        now: Vec<u32>,
        added: Vec<Vec<u64>>,
        counts: Vec<u64>,
        base_patterns: u32,
    }
    let mut plans = Vec::new();
    for scope in Scope::ALL {
        let Some(t) = file.table(scope) else {
            return Ok(PatternInfo::skipped(format!(
                "the generation it was compacted from has no {} table",
                scope.name()
            )));
        };
        let corr = correction(snap, t, scope, usize::MAX, check)?.unwrap_or_default();
        // the distinct new sets, then their numbers among the old patterns
        let mut sets: FxHashMap<&[u64], u32> = FxHashMap::default();
        for x in &corr.touched {
            if !x.now.is_empty() {
                sets.insert(&x.now, u32::MAX);
            }
        }
        let mut found = 0usize;
        if !sets.is_empty() {
            let mut buf = Vec::new();
            for pid in 0..t.patterns() as u32 {
                if pid % 65536 == 65535 {
                    check()?;
                }
                t.pattern(pid, &mut buf)?;
                if let Some(id) = sets.get_mut(&buf[..])
                    && *id == u32::MAX
                {
                    *id = pid;
                    found += 1;
                    if found == sets.len() {
                        break;
                    }
                }
            }
        }
        let base_patterns = t.patterns() as u32;
        let mut added: Vec<Vec<u64>> = Vec::new();
        let mut next = base_patterns;
        let mut counts: Vec<u64> = (0..base_patterns).map(|p| t.pattern_subjects(p)).collect();
        let mut now = Vec::with_capacity(corr.touched.len());
        for x in &corr.touched {
            if let Some(b) = x.base {
                counts[b as usize] = counts[b as usize].saturating_sub(1);
            }
            if x.now.is_empty() {
                now.push(u32::MAX);
                continue;
            }
            let id = sets.get_mut(&x.now[..]).unwrap();
            if *id == u32::MAX {
                *id = next;
                next = next
                    .checked_add(1)
                    .ok_or_else(|| Error::Corrupt("too many predicate patterns".into()))?;
                added.push(x.now.clone());
                counts.push(0);
            }
            counts[*id as usize] += 1;
            now.push(*id);
        }
        drop(sets);
        plans.push(Plan {
            scope,
            corr,
            now,
            added,
            counts,
            base_patterns,
        });
    }
    let mut info = PatternInfo {
        format: VERSION,
        ..Default::default()
    };
    let file = &file;
    let tables: Vec<TableInput<'_>> = plans
        .iter()
        .map(|p| {
            let t = file.table(p.scope).unwrap();
            let i = p.scope.index();
            info.patterns[i] = p.counts.len() as u64;
            info.subjects[i] = p.counts.iter().sum();
            TableInput {
                scope: p.scope,
                patterns: Box::new(move |f| {
                    let mut buf = Vec::new();
                    for pid in 0..p.base_patterns {
                        t.pattern(pid, &mut buf)?;
                        f(&buf, p.counts[pid as usize])?;
                    }
                    for (j, set) in p.added.iter().enumerate() {
                        f(set, p.counts[p.base_patterns as usize + j])?;
                    }
                    Ok(())
                }),
                subjects: Box::new(move |f| {
                    let touched = &p.corr.touched;
                    let mut next = 0usize;
                    let put = |j: usize, f: &mut dyn FnMut(u64, u32) -> Result<()>| match p.now[j] {
                        u32::MAX => Ok(()),
                        pid => f(touched[j].subject, pid),
                    };
                    t.for_each_until(|s, pid| {
                        while next < touched.len() && touched[next].subject < s {
                            put(next, f)?;
                            next += 1;
                        }
                        if next < touched.len() && touched[next].subject == s {
                            put(next, f)?;
                            next += 1;
                        } else {
                            f(s, pid)?;
                        }
                        Ok(true)
                    })?;
                    while next < touched.len() {
                        put(next, f)?;
                        next += 1;
                    }
                    Ok(())
                }),
            }
        })
        .collect();
    info.bytes = write_tables(dir, tables)?;
    Ok(info)
}

// ------------------------------------------------------------------ checking -----

/// Check the patterns of the generation in `dir` against its SPO permutation: the
/// checksum, every subject's pattern, the subjects of each pattern and the predicate
/// totals. Returns the problems found (at most `max`), and a summary.
pub fn verify(dir: &Path, max: usize) -> Result<(Vec<String>, String)> {
    let Some(file) = PatternFile::open(dir)? else {
        return Ok((Vec::new(), "no predicate patterns".into()));
    };
    let mut problems: Vec<String> = Vec::new();
    let note = |problems: &mut Vec<String>, s: String| {
        if problems.len() < max {
            problems.push(s);
        }
    };
    if let Err(e) = file.verify_checksum() {
        note(&mut problems, e.to_string());
        return Ok((problems, "damaged".into()));
    }
    let spo = PermIndex::open(dir, Perm::Spo)?;
    let mut summary = Vec::new();
    for scope in Scope::ALL {
        let Some(t) = file.table(scope) else {
            note(&mut problems, format!("no {} table", scope.name()));
            continue;
        };
        // every subject's predicates from SPO, against the table in subject order
        let mut counts = vec![0u64; t.patterns() as usize];
        let mut totals: FxHashMap<u64, u64> = FxHashMap::default();
        let mut entries: Vec<(u64, u32)> = Vec::new();
        t.for_each_until(|s, pid| {
            entries.push((s, pid));
            Ok(true)
        })?;
        let mut at = 0usize;
        let mut buf = Vec::new();
        let mut cur: Option<u64> = None;
        let mut set: Vec<u64> = Vec::new();
        let mut done = |s: u64, set: &mut Vec<u64>, problems: &mut Vec<String>| -> Result<()> {
            while at < entries.len() && entries[at].0 < s {
                note(
                    problems,
                    format!(
                        "{} table: subject {:#x} has a pattern but no triples",
                        scope.name(),
                        entries[at].0
                    ),
                );
                at += 1;
            }
            let have = (at < entries.len() && entries[at].0 == s).then(|| entries[at].1);
            if have.is_some() {
                at += 1;
            }
            match (have, set.is_empty()) {
                (None, true) => {}
                (None, false) => note(
                    problems,
                    format!("{} table: subject {s:#x} has no pattern", scope.name()),
                ),
                (Some(pid), _) => {
                    t.pattern(pid, &mut buf)?;
                    if buf != *set {
                        note(
                            problems,
                            format!(
                                "{} table: subject {s:#x} has pattern {pid}, which is not its predicates",
                                scope.name()
                            ),
                        );
                    }
                    counts[pid as usize] += 1;
                    for &p in &buf {
                        *totals.entry(p).or_default() += 1;
                    }
                }
            }
            set.clear();
            Ok(())
        };
        for b in 0..spo.blocks.len() {
            let block = spo.decode_block(b)?;
            for i in 0..block.len() {
                let k = block.key(i);
                if cur != Some(k[0]) {
                    if let Some(s) = cur {
                        done(s, &mut set, &mut problems)?;
                    }
                    cur = Some(k[0]);
                }
                if scope.accepts(k[3]) && set.last() != Some(&k[1]) {
                    set.push(k[1]);
                }
            }
        }
        if let Some(s) = cur {
            done(s, &mut set, &mut problems)?;
        }
        done(u64::MAX, &mut Vec::new(), &mut problems)?;
        for (pid, &n) in counts.iter().enumerate() {
            if t.pattern_subjects(pid as u32) != n {
                note(
                    &mut problems,
                    format!(
                        "{} table: pattern {pid} records {} subjects, {n} have it",
                        scope.name(),
                        t.pattern_subjects(pid as u32)
                    ),
                );
            }
        }
        let stored: Vec<(u64, u64)> = t.predicate_totals().collect();
        if stored != into_sorted(totals) {
            note(
                &mut problems,
                format!("{} table: the predicate totals are off", scope.name()),
            );
        }
        summary.push(format!(
            "{}: {} patterns, {} subjects",
            scope.name(),
            t.patterns(),
            t.subjects()
        ));
    }
    Ok((
        problems,
        format!("{} ({} bytes)", summary.join("; "), file.bytes()),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn build(keys: &[Key], budget: usize) -> (tempfile::TempDir, PatternInfo) {
        let dir = tempfile::tempdir().unwrap();
        let mut c = PatternCollector::new(dir.path(), budget);
        let mut keys = keys.to_vec();
        keys.sort_unstable();
        keys.dedup();
        for k in &keys {
            c.push(k);
        }
        let info = c.finish(dir.path()).unwrap();
        (dir, info)
    }

    fn dg() -> u64 {
        Id::DEFAULT_GRAPH.0
    }

    #[test]
    fn a_build_round_trips_and_counts() {
        let g = dg();
        let mut keys = Vec::new();
        // 3000 subjects over two blocks and more, with three patterns
        for s in 1..=3000u64 {
            keys.push([s * 3, 10, 1, g]);
            if s % 2 == 0 {
                keys.push([s * 3, 20, 1, g]);
                keys.push([s * 3, 20, 2, g]);
            }
            if s % 3 == 0 {
                keys.push([s * 3, 30, 1, 77]);
            }
        }
        let (dir, info) = build(&keys, DEFAULT_BUDGET);
        assert_eq!(info.format, VERSION);
        assert_eq!(info.subjects, [3000, 1000]);
        let f = PatternFile::open(dir.path()).unwrap().unwrap();
        f.verify_checksum().unwrap();
        let t = f.table(Scope::Default).unwrap();
        assert_eq!(t.patterns(), 2);
        // the most frequent pattern first: {10} and {10, 20} have 1500 each, ordered by set
        let mut v = Vec::new();
        t.pattern(0, &mut v).unwrap();
        assert_eq!(v, vec![10]);
        assert_eq!(
            t.predicate_totals().collect::<Vec<_>>(),
            vec![(10, 3000), (20, 1500)]
        );
        let mut c = t.cursor();
        assert_eq!(c.seek(6).unwrap(), Some(1));
        assert_eq!(c.seek(7).unwrap(), None);
        assert_eq!(c.seek(3).unwrap(), Some(0));
        assert_eq!(c.seek(9000).unwrap(), Some(1));
        assert_eq!(c.seek(9001).unwrap(), None);
        let corr = Correction::default();
        let counted = count_subjects(t, &corr, &[(3, 1), (6, 2), (7, 5)], &|| Ok(())).unwrap();
        assert_eq!(counted, vec![(10, 3), (20, 2)]);
        assert_eq!(count_all(t, &corr).unwrap(), vec![(10, 3000), (20, 1500)]);
        let n = f.table(Scope::Named).unwrap();
        assert_eq!(count_all(n, &corr).unwrap(), vec![(30, 1000)]);
        let mut pairs = Vec::new();
        for_each_pair(n, &corr, Some(30), &|| Ok(()), |s, p| {
            pairs.push((s, p));
            Ok(())
        })
        .unwrap();
        assert_eq!(pairs.len(), 1000);
        assert_eq!(pairs[0], (9, 30));
    }

    #[test]
    fn the_budget_skips_the_patterns() {
        let g = dg();
        let keys: Vec<Key> = (1..100u64).map(|s| [s, s, 1, g]).collect();
        let (dir, info) = build(&keys, 1024);
        assert!(
            info.skipped.as_deref().unwrap().contains("budget"),
            "{info:?}"
        );
        assert!(PatternFile::open(dir.path()).unwrap().is_none());
    }

    #[test]
    fn unknown_versions_and_damage_are_refused() {
        let g = dg();
        let (dir, _) = build(&[[1, 2, 3, g]], DEFAULT_BUDGET);
        let path = dir.path().join(FILE);
        let good = std::fs::read(&path).unwrap();
        let mut newer = good.clone();
        newer[8..12].copy_from_slice(&2u32.to_le_bytes());
        std::fs::write(&path, &newer).unwrap();
        let e = PatternFile::open(dir.path()).unwrap_err().to_string();
        assert!(e.contains("version 2") && e.contains("not used"), "{e}");
        let loaded = Loaded::open(dir.path(), None);
        assert!(loaded.file.is_none());
        assert!(loaded.missing.unwrap().contains("version 2"));
        let mut cut = good.clone();
        cut.truncate(good.len() - 3);
        std::fs::write(&path, &cut).unwrap();
        assert!(matches!(
            PatternFile::open(dir.path()),
            Err(Error::Corrupt(_))
        ));
        let mut flipped = good.clone();
        let mid = HEADER + 2 * DIR + 1;
        flipped[mid] ^= 0xFF;
        std::fs::write(&path, &flipped).unwrap();
        let f = PatternFile::open(dir.path()).unwrap().unwrap();
        assert!(f.verify_checksum().is_err());
    }

    #[test]
    fn varints_reject_overlong_input() {
        let mut pos = 0;
        assert_eq!(get_varint(&[0xFF; 11], &mut pos), None);
        let mut out = Vec::new();
        put_varint(&mut out, u64::MAX);
        let mut pos = 0;
        assert_eq!(get_varint(&out, &mut pos), Some(u64::MAX));
    }
}
