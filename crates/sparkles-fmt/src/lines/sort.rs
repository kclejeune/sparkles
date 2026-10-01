//! `sort` for the line formats: the statements of one run (between sort barriers) sorted
//! by the bytes of their formatted line, which is the order of `LC_ALL=C sort`, and ties
//! in source order. A statement moves with its leading comments and its trailing comment.
//! Identical statements without comments merge into the first one; one with comments is
//! always kept.
//!
//! The run is sorted in memory up to the budget (`LinesConfig::sort_memory`). Beyond it,
//! sorted runs spill to a directory of their own under `LinesConfig::spill_dir`,
//! LZ4-framed, and a k-way merge reads them back (in several passes when there are more
//! than [`FAN_IN`] of them). The directory goes when the [`Sorter`] does: on success,
//! on any error, and when a panic unwinds.

use lz4_flex::frame::{FrameDecoder, FrameEncoder};
use std::cmp::{Ordering, Reverse};
use std::collections::BinaryHeap;
use std::fs::File;
use std::io::{self, BufReader, BufWriter, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering as AtomicOrdering};

/// The most sorted runs merged at once.
pub const FAN_IN: usize = 64;

/// A statement to sort, with what it prints.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Record {
    /// what it sorts by: the formatted statement without its comment (or the statement as
    /// written, under `# sparkles-fmt: ignore`)
    pub key: Box<str>,
    /// the printed lines (leading comments, then the statement line) when they are more
    /// than `key`; `None` for a statement without comments, which is just `key`
    pub text: Option<Box<str>>,
    /// the index of its first input line: source order
    pub seq: u64,
    /// how many lines it prints
    pub lines: u32,
    /// every printed line equals its input line
    pub same: bool,
}

impl Record {
    /// Bytes it holds in memory, roughly.
    fn size(&self) -> u64 {
        (self.key.len() + self.text.as_ref().map_or(0, |t| t.len()) + 64) as u64
    }

    fn cmp_key(&self, other: &Record) -> Ordering {
        self.key
            .as_bytes()
            .cmp(other.key.as_bytes())
            .then(self.seq.cmp(&other.seq))
    }

    fn write_to(&self, w: &mut impl Write) -> io::Result<()> {
        w.write_all(&(self.key.len() as u64).to_le_bytes())?;
        w.write_all(self.key.as_bytes())?;
        let text = self.text.as_deref().map(str::as_bytes);
        w.write_all(&text.map_or(0, |t| t.len() as u64 + 1).to_le_bytes())?;
        w.write_all(text.unwrap_or_default())?;
        w.write_all(&self.seq.to_le_bytes())?;
        w.write_all(&self.lines.to_le_bytes())?;
        w.write_all(&[u8::from(self.same)])
    }

    /// The next record of a run, `None` at its end.
    fn read_from(r: &mut impl Read) -> io::Result<Option<Record>> {
        let mut n = [0u8; 8];
        match r.read_exact(&mut n) {
            Ok(()) => {}
            Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => return Ok(None),
            Err(e) => return Err(e),
        }
        let string = |r: &mut dyn Read, len: u64| -> io::Result<Box<str>> {
            let mut b = vec![0; len as usize];
            r.read_exact(&mut b)?;
            String::from_utf8(b)
                .map(String::into_boxed_str)
                .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))
        };
        let key = string(r, u64::from_le_bytes(n))?;
        r.read_exact(&mut n)?;
        let text = match u64::from_le_bytes(n) {
            0 => None,
            len => Some(string(r, len - 1)?),
        };
        r.read_exact(&mut n)?;
        let seq = u64::from_le_bytes(n);
        let mut lines = [0u8; 4];
        r.read_exact(&mut lines)?;
        let mut same = [0u8; 1];
        r.read_exact(&mut same)?;
        Ok(Some(Record {
            key,
            text,
            seq,
            lines: u32::from_le_bytes(lines),
            same: same[0] != 0,
        }))
    }
}

/// A directory of sorted runs, removed with everything in it when dropped.
#[derive(Debug)]
pub struct SpillDir {
    path: PathBuf,
}

impl SpillDir {
    /// A new, unique directory under `parent`.
    pub fn create(parent: &Path) -> io::Result<SpillDir> {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        #[cfg(not(target_family = "wasm"))]
        let pid = std::process::id();
        #[cfg(target_family = "wasm")]
        let pid = 0;
        loop {
            let n = NEXT.fetch_add(1, AtomicOrdering::Relaxed);
            let path = parent.join(format!("sparkles-fmt-sort-{pid}-{n}"));
            match std::fs::create_dir(&path) {
                Ok(()) => return Ok(SpillDir { path }),
                Err(e) if e.kind() == io::ErrorKind::AlreadyExists => continue,
                Err(e) => return Err(e),
            }
        }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for SpillDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

/// The statements of one sort run.
pub struct Sorter {
    mem: Vec<Record>,
    bytes: u64,
    budget: u64,
    parent: PathBuf,
    dir: Option<SpillDir>,
    runs: Vec<PathBuf>,
    next_run: u64,
    /// records pushed into the current run
    pub pushed: u64,
}

impl Sorter {
    pub fn new(budget: u64, spill_parent: &Path) -> Sorter {
        Sorter {
            mem: Vec::new(),
            bytes: 0,
            budget,
            parent: spill_parent.to_path_buf(),
            dir: None,
            runs: Vec::new(),
            next_run: 0,
            pushed: 0,
        }
    }

    pub fn is_empty(&self) -> bool {
        self.pushed == 0
    }

    pub fn push(&mut self, r: Record) -> io::Result<()> {
        self.bytes += r.size();
        self.pushed += 1;
        self.mem.push(r);
        if self.bytes > self.budget {
            self.spill()?;
        }
        Ok(())
    }

    /// Write the records in memory as a sorted run.
    fn spill(&mut self) -> io::Result<()> {
        self.mem.sort_unstable_by(Record::cmp_key);
        let records = std::mem::take(&mut self.mem);
        self.bytes = 0;
        let path = self.new_run_path()?;
        let mut w = FrameEncoder::new(BufWriter::new(File::create(&path)?));
        for r in &records {
            r.write_to(&mut w)?;
        }
        w.finish().map_err(io::Error::other)?.flush()?;
        self.runs.push(path);
        Ok(())
    }

    fn new_run_path(&mut self) -> io::Result<PathBuf> {
        if self.dir.is_none() {
            self.dir = Some(SpillDir::create(&self.parent)?);
        }
        let dir = self.dir.as_ref().expect("created above");
        self.next_run += 1;
        Ok(dir.path().join(format!("run-{}.lz4", self.next_run)))
    }

    /// Hand the run's records to `emit` in order and start a new run. The spilled runs
    /// are deleted; the directory stays for later runs until the sorter goes.
    pub fn finish(&mut self, emit: &mut dyn FnMut(Record) -> io::Result<()>) -> io::Result<()> {
        self.pushed = 0;
        if self.runs.is_empty() {
            let mut mem = std::mem::take(&mut self.mem);
            self.bytes = 0;
            mem.sort_by(Record::cmp_key);
            for r in mem {
                emit(r)?;
            }
            return Ok(());
        }
        if !self.mem.is_empty() {
            self.spill()?;
        }
        // merge down to one pass of at most FAN_IN runs
        while self.runs.len() > FAN_IN {
            let group: Vec<PathBuf> = self.runs.drain(..FAN_IN).collect();
            let path = self.new_run_path()?;
            let mut w = FrameEncoder::new(BufWriter::new(File::create(&path)?));
            merge(&group, &mut |r| r.write_to(&mut w))?;
            w.finish().map_err(io::Error::other)?.flush()?;
            for p in &group {
                let _ = std::fs::remove_file(p);
            }
            self.runs.push(path);
        }
        let runs = std::mem::take(&mut self.runs);
        let result = merge(&runs, emit);
        for p in &runs {
            let _ = std::fs::remove_file(p);
        }
        result
    }

    /// Whether any run spilled to disk since the sorter was made.
    pub fn spilled(&self) -> bool {
        self.dir.is_some()
    }
}

/// The k-way merge of sorted runs, in order.
fn merge(runs: &[PathBuf], emit: &mut dyn FnMut(Record) -> io::Result<()>) -> io::Result<()> {
    struct Head(Record, usize);
    impl PartialEq for Head {
        fn eq(&self, o: &Head) -> bool {
            self.cmp(o) == Ordering::Equal
        }
    }
    impl Eq for Head {}
    impl PartialOrd for Head {
        fn partial_cmp(&self, o: &Head) -> Option<Ordering> {
            Some(self.cmp(o))
        }
    }
    impl Ord for Head {
        fn cmp(&self, o: &Head) -> Ordering {
            self.0.cmp_key(&o.0)
        }
    }
    let mut readers = runs
        .iter()
        .map(|p| Ok(FrameDecoder::new(BufReader::new(File::open(p)?))))
        .collect::<io::Result<Vec<_>>>()?;
    let mut heap = BinaryHeap::with_capacity(readers.len());
    for (i, r) in readers.iter_mut().enumerate() {
        if let Some(rec) = Record::read_from(r)? {
            heap.push(Reverse(Head(rec, i)));
        }
    }
    while let Some(Reverse(Head(rec, i))) = heap.pop() {
        emit(rec)?;
        if let Some(next) = Record::read_from(&mut readers[i])? {
            heap.push(Reverse(Head(next, i)));
        }
    }
    Ok(())
}

/// Merges identical statements without comments, and checks the order: records arrive
/// sorted, and only comment-free duplicates of an emitted statement go.
#[derive(Default)]
pub struct Dedup {
    last: Option<(Box<str>, u64)>,
    /// whether a comment-free record with the last key was emitted
    plain_emitted: bool,
    pub merged: u64,
    pub emitted: u64,
    /// the order was wrong (a formatter bug)
    pub unsorted: bool,
}

impl Dedup {
    /// Whether to print `r` (records must come in sorted order); `plain`: it prints
    /// without comments, so a later duplicate without comments merges into it.
    pub fn keep(&mut self, r: &Record, plain: bool) -> bool {
        let same_key = match &self.last {
            Some((k, seq)) => {
                match k.as_bytes().cmp(r.key.as_bytes()).then(seq.cmp(&r.seq)) {
                    Ordering::Greater | Ordering::Equal => self.unsorted = true,
                    Ordering::Less => {}
                }
                **k == *r.key
            }
            None => false,
        };
        if !same_key {
            self.plain_emitted = false;
        }
        self.last = Some((r.key.clone(), r.seq));
        if plain {
            if self.plain_emitted {
                self.merged += 1;
                return false;
            }
            self.plain_emitted = true;
        }
        self.emitted += 1;
        true
    }

    /// Start a new run.
    pub fn reset(&mut self) {
        self.last = None;
        self.plain_emitted = false;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rec(key: &str, seq: u64, comment: bool) -> Record {
        Record {
            key: key.into(),
            text: comment.then(|| format!("{key} # c").into()),
            seq,
            lines: 1,
            same: false,
        }
    }

    fn sorted(budget: u64, records: &[Record], dir: &Path) -> (Vec<Record>, bool) {
        let mut s = Sorter::new(budget, dir);
        for r in records {
            s.push(r.clone()).unwrap();
        }
        let spilled = s.spilled();
        let mut out = Vec::new();
        s.finish(&mut |r| {
            out.push(r);
            Ok(())
        })
        .unwrap();
        drop(s);
        (out, spilled)
    }

    fn test_dir(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!(
            "sparkles-fmt-sort-test-{}-{name}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn in_memory_and_spilled_runs_agree() {
        let dir = test_dir("agree");
        let records: Vec<Record> = (0..2000u64)
            .map(|i| rec(&format!("<http://e/{}> .", (i * 7919) % 613), i, i % 5 == 0))
            .collect();
        let (mem, spilled) = sorted(u64::MAX, &records, &dir);
        assert!(!spilled);
        // a budget of a few records: many runs, merged in several passes
        let (disk, spilled) = sorted(2_000, &records, &dir);
        assert!(spilled);
        assert_eq!(mem, disk);
        assert!(
            mem.windows(2)
                .all(|w| w[0].cmp_key(&w[1]) == Ordering::Less)
        );
        // the spill directory is gone with the sorter
        assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 0);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn dedup_keeps_commented_duplicates() {
        let mut d = Dedup::default();
        let v = [
            rec("a", 1, false),
            rec("a", 2, true),
            rec("a", 3, false),
            rec("b", 0, false),
        ];
        let kept: Vec<u64> = v
            .iter()
            .filter(|r| d.keep(r, r.text.is_none()))
            .map(|r| r.seq)
            .collect();
        assert_eq!(kept, [1, 2, 0]);
        assert_eq!((d.emitted, d.merged, d.unsorted), (3, 1, false));
        assert!(!d.keep(&rec("b", 5, false), true));
        d.keep(&rec("a", 9, false), true);
        assert!(d.unsorted);
    }

    #[test]
    fn records_round_trip() {
        let mut b = Vec::new();
        let r = Record {
            key: "k é".into(),
            text: Some("# c\nk é".into()),
            seq: 42,
            lines: 2,
            same: true,
        };
        r.write_to(&mut b).unwrap();
        rec("x", 1, false).write_to(&mut b).unwrap();
        let mut s = b.as_slice();
        assert_eq!(Record::read_from(&mut s).unwrap(), Some(r));
        assert_eq!(Record::read_from(&mut s).unwrap(), Some(rec("x", 1, false)));
        assert_eq!(Record::read_from(&mut s).unwrap(), None);
    }
}
