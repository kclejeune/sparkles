//! Prewarm lists (spec X07 §7).
//!
//! A server started with `--prewarm` records which parts of its index it used, and the
//! next start with `--prewarm` reads them again in the background once it is ready, as
//! PostgreSQL's `pg_prewarm` does with its auto-prewarm worker. The list,
//! `<root>/prewarm.json`, names the generation it describes and holds two kinds of entry.
//! The block columns and segments that the decoded-block cache held are decoded into the
//! cache again, and the pages of the generation's other files (the vocabulary, its
//! offsets and numeric column, the segment directories) that were in the page cache are
//! read into it again. A list written for another generation, after a compaction, is
//! ignored. A list with a format version this build does not know is refused with an
//! error, and the store is served without a prewarm.
//!
//! Recording takes the residency of the page cache from `mincore(2)`, which reports the
//! pages of a file that any process left there, so a list made right after a load holds
//! every page the load wrote. [`PAGE_LIMIT`] bounds the pages a list keeps, and
//! [`Store::prewarm`] stops decoding when the block cache is full.

use super::Store;
use crate::error::{Error, Result};
use crate::index::{ColMask, Perm};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Instant;

/// The list's file name in the database directory.
pub const FILE: &str = "prewarm.json";
/// The `format` field of a list.
const FORMAT: &str = "sparkles-prewarm";
/// The version of the list this build writes and reads ([`crate::formats::PREWARM`]). A
/// reader refuses any other.
pub const VERSION: u32 = crate::formats::PREWARM.writes;
/// The most bytes of file pages a list records. The pages past it are left out, in file
/// order, so that a store much larger than memory does not make every start read
/// gigabytes it would evict again.
pub const PAGE_LIMIT: u64 = 4 << 30;
/// Decoded bytes of a whole block of four columns.
const WHOLE_BLOCK: u64 = crate::index::BLOCK_ROWS as u64 * 8 * 4;
/// Bytes read by one call when a prewarm waits for recorded pages.
const READ_CHUNK: usize = 1 << 20;

/// Held while a list is written, by any store of the process.
static RECORDING: parking_lot::Mutex<()> = parking_lot::Mutex::new(());

#[derive(Serialize, Deserialize)]
struct List {
    format: String,
    version: u32,
    /// the generation directory's name
    generation: String,
    /// per permutation name, `[block, part, mask]`: part 0 is the whole columns of
    /// `mask`, part `s + 1` the columns of `mask` of segment `s`
    columns: BTreeMap<String, Vec<[u32; 3]>>,
    /// per file of the generation directory, `[offset, length]` byte ranges
    pages: BTreeMap<String, Vec<[u64; 2]>>,
}

/// What [`Store::record_prewarm`] wrote.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
pub struct Recorded {
    /// block parts (whole columns or segments) listed
    pub parts: usize,
    /// bytes of file pages listed
    pub page_bytes: u64,
}

/// What [`Store::prewarm`] read.
#[derive(Clone, Debug, Default, Serialize)]
pub struct Prewarmed {
    /// block parts decoded into the cache
    pub parts: usize,
    /// bytes of file pages read
    pub page_bytes: u64,
    /// time spent reading pages and decoding, in milliseconds
    pub elapsed_ms: f64,
    /// why the prewarm read nothing or stopped early, when it did
    pub skipped: Option<String>,
}

impl Store {
    /// The path of this store's prewarm list, `None` for an in-memory store.
    pub fn prewarm_path(&self) -> Option<PathBuf> {
        self.root().map(|r| r.join(FILE))
    }

    /// Write the prewarm list of the current generation, replacing the last one in one
    /// rename. Returns `None` for a store without a generation directory of its own.
    pub fn record_prewarm(&self) -> Result<Option<Recorded>> {
        let Some(path) = self.prewarm_path() else {
            return Ok(None);
        };
        // a periodic recording and the one at shutdown may overlap: one at a time, from
        // the snapshot to the rename, so that the later one, of the newer generation,
        // replaces the earlier one and their temporary files never mix
        let _one = RECORDING.lock();
        let snap = self.snapshot();
        let generation = &snap.generation;
        let Some(dir) = generation.dir.as_deref() else {
            return Ok(None);
        };
        // a branch's linked generation reads files of another directory
        if generation.link.is_some() {
            return Ok(None);
        }
        let perms: Vec<&crate::index::PermIndex> = generation.perms.iter().collect();
        let held = snap.cache.held_parts(&perms);
        let mut list = List {
            format: FORMAT.into(),
            version: VERSION,
            generation: generation.name.clone(),
            columns: BTreeMap::new(),
            pages: BTreeMap::new(),
        };
        let mut recorded = Recorded::default();
        for (idx, parts) in perms.iter().zip(held) {
            if parts.is_empty() {
                continue;
            }
            recorded.parts += parts.len();
            list.columns.insert(
                idx.perm.name().to_string(),
                parts
                    .into_iter()
                    .map(|(b, p, m)| [b, p, m as u32])
                    .collect(),
            );
        }
        let mut budget = PAGE_LIMIT;
        for name in page_files(dir)? {
            if budget == 0 {
                break;
            }
            let ranges = resident_ranges(&dir.join(&name), &mut budget)?;
            if !ranges.is_empty() {
                recorded.page_bytes += ranges.iter().map(|r| r[1]).sum::<u64>();
                list.pages.insert(name, ranges);
            }
        }
        let bytes = serde_json::to_vec(&list).map_err(|e| Error::Invalid(e.to_string()))?;
        let tmp = path.with_extension("json.tmp");
        std::fs::write(&tmp, &bytes)?;
        std::fs::rename(&tmp, &path)?;
        Ok(Some(recorded))
    }

    /// Read the parts and pages of this store's prewarm list into the block cache and the
    /// page cache. Missing lists and lists of another generation read nothing and say so in
    /// [`Prewarmed::skipped`]. A list of an unknown format or version is an error.
    /// `stop` is asked between reads, and a `true` ends the prewarm early.
    pub fn prewarm(&self, stop: &dyn Fn() -> bool) -> Result<Prewarmed> {
        let t0 = Instant::now();
        let mut done = Prewarmed::default();
        let finish = |mut d: Prewarmed, why: Option<String>| {
            d.elapsed_ms = t0.elapsed().as_secs_f64() * 1e3;
            d.skipped = why;
            Ok(d)
        };
        let Some(path) = self.prewarm_path() else {
            return finish(done, Some("the store is in memory".into()));
        };
        let bytes = match std::fs::read(&path) {
            Ok(b) => b,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                return finish(done, Some("no prewarm list was recorded".into()));
            }
            Err(e) => return Err(e.into()),
        };
        let list = parse(&path, &bytes)?;
        let snap = self.snapshot();
        let generation = snap.generation.clone();
        if list.generation != generation.name {
            return finish(
                done,
                Some(format!(
                    "the list describes generation {}, and the store serves {}",
                    list.generation, generation.name
                )),
            );
        }
        let Some(dir) = generation.dir.as_deref() else {
            return finish(done, Some("the generation has no directory".into()));
        };
        let stopped = || stop() || self.snapshot().generation.uid != generation.uid;
        // the pages first: they are the vocabulary and directory reads of a lookup, and
        // they are quick to read compared with decoding blocks
        for (name, ranges) in &list.pages {
            if !page_file_name(name) {
                return Err(bad(
                    &path,
                    format!("{name:?} is not a file of a generation"),
                ));
            }
            match read_ranges(&dir.join(name), ranges, &stopped)? {
                Some(n) => done.page_bytes += n,
                None => return finish(done, Some("stopped".into())),
            }
        }
        let cache = &snap.cache;
        for (name, parts) in &list.columns {
            let perm = Perm::ALL
                .iter()
                .copied()
                .find(|p| p.name() == name)
                .ok_or_else(|| bad(&path, format!("{name:?} is not a permutation")))?;
            let idx = generation.perm(perm);
            for &[b, part, mask] in parts {
                if stopped() {
                    return finish(done, Some("stopped".into()));
                }
                // the cache's total, since a view's own share of a shared cache costs a
                // walk over every entry to count, and room for one more whole block, since
                // a full cache evicts as it takes more
                if cache.total_bytes() + WHOLE_BLOCK > cache.capacity() {
                    return finish(done, Some("the block cache is full".into()));
                }
                let b = b as usize;
                let mask = mask as ColMask & crate::index::ALL_COLS;
                if b >= idx.blocks.len() || mask == 0 {
                    return Err(bad(&path, format!("{name} block {b} is not in the index")));
                }
                if part == 0 {
                    cache.get_cols(idx, b, mask)?;
                } else {
                    let s = part as usize - 1;
                    match idx.segment_count(b) {
                        Some(n) if s < n => cache.prewarm_segment(idx, b, s, mask)?,
                        _ => {
                            return Err(bad(&path, format!("{name} block {b} has no segment {s}")));
                        }
                    }
                }
                done.parts += 1;
            }
        }
        finish(done, None)
    }
}

fn bad(path: &Path, what: String) -> Error {
    Error::Invalid(format!("{}: {what}", path.display()))
}

/// Parse a list, refusing a format or version this build does not read before it reads
/// any other field.
fn parse(path: &Path, bytes: &[u8]) -> Result<List> {
    let v: serde_json::Value =
        serde_json::from_slice(bytes).map_err(|e| bad(path, format!("not JSON: {e}")))?;
    if v.get("format").and_then(|f| f.as_str()) != Some(FORMAT) {
        return Err(bad(path, "is not a Sparkles prewarm list".into()));
    }
    match v.get("version").and_then(|n| n.as_u64()) {
        Some(n) if u32::try_from(n).is_ok_and(|n| crate::formats::PREWARM.reads(n)) => {}
        Some(n) => {
            return Err(Error::Unsupported(format!(
                "{}: prewarm list version {n} is not one this build reads (it reads version {VERSION})",
                path.display()
            )));
        }
        None => return Err(bad(path, "has no version".into())),
    }
    serde_json::from_value(v).map_err(|e| bad(path, e.to_string()))
}

/// Whether `name` may be listed for its pages: a plain file name in the generation
/// directory, other than a permutation's block data, which the parts cover.
fn page_file_name(name: &str) -> bool {
    !name.is_empty()
        && !name.contains(['/', '\\'])
        && name != "."
        && name != ".."
        && !Perm::ALL
            .iter()
            .any(|p| name == format!("{}.dat", p.name()))
}

/// The files of `dir` whose pages a list records, by name.
fn page_files(dir: &Path) -> Result<Vec<String>> {
    let mut names = Vec::new();
    for e in std::fs::read_dir(dir)? {
        let e = e?;
        if !e.file_type()?.is_file() {
            continue;
        }
        if let Some(name) = e.file_name().to_str()
            && page_file_name(name)
        {
            names.push(name.to_string());
        }
    }
    names.sort();
    Ok(names)
}

/// The byte ranges of `path` that are in the page cache, coalesced, taking at most
/// `budget` bytes and lowering it by what they hold.
#[cfg(unix)]
fn resident_ranges(path: &Path, budget: &mut u64) -> Result<Vec<[u64; 2]>> {
    use std::os::unix::fs::MetadataExt;
    let f = std::fs::File::open(path)?;
    let meta = f.metadata()?;
    let len = meta.len() as usize;
    if len == 0 {
        return Ok(Vec::new());
    }
    // Linux answers mincore for a file the caller neither owns nor may write as if every
    // page were resident, so such a file is left out
    // SAFETY: geteuid has no preconditions
    if meta.uid() != unsafe { libc::geteuid() } {
        return Ok(Vec::new());
    }
    // SAFETY: the mapping is only passed to mincore, which reads no file data
    let map = unsafe { memmap2::Mmap::map(&f)? };
    let ps = page_size();
    let pages = len.div_ceil(ps);
    let mut vec = vec![0u8; pages];
    // SAFETY: `map` is a live mapping of `len` bytes, and `vec` has one byte per page
    let rc = unsafe { libc::mincore(map.as_ptr().cast_mut().cast(), len, vec.as_mut_ptr().cast()) };
    if rc != 0 {
        return Err(std::io::Error::last_os_error().into());
    }
    let mut out: Vec<[u64; 2]> = Vec::new();
    for (i, &r) in vec.iter().enumerate() {
        if r & 1 == 0 || *budget == 0 {
            continue;
        }
        let off = (i * ps) as u64;
        let n = (ps as u64).min(len as u64 - off).min(*budget);
        *budget -= n;
        match out.last_mut() {
            Some(last) if last[0] + last[1] == off => last[1] += n,
            _ => out.push([off, n]),
        }
    }
    Ok(out)
}

#[cfg(not(unix))]
fn resident_ranges(_path: &Path, _budget: &mut u64) -> Result<Vec<[u64; 2]>> {
    Ok(Vec::new())
}

#[cfg(unix)]
fn page_size() -> usize {
    // SAFETY: sysconf has no preconditions
    let p = unsafe { libc::sysconf(libc::_SC_PAGESIZE) };
    if p > 0 { p as usize } else { 4096 }
}

/// How far the read-ahead hints run ahead of the reads. A bounded window keeps a stop from
/// leaving a long queue of hinted reads behind it.
const HINT_AHEAD: u64 = 64 << 20;

/// Read `ranges` of `path` into the page cache: hint the ranges up to [`HINT_AHEAD`]
/// bytes ahead of the reads, refilling the hints when half of that is left, so that their
/// reads overlap, and read them through to wait for them.
/// Returns the bytes read, or `None` when `stop` said to stop. Ranges past the end of the
/// file are cut off, since a list may be older than a file the store rewrote.
fn read_ranges(path: &Path, ranges: &[[u64; 2]], stop: &dyn Fn() -> bool) -> Result<Option<u64>> {
    read_ranges_with(path, ranges, stop, HINT_AHEAD, &mut |f, off, n, _| {
        will_need(f, off, n)
    })
}

#[cfg(target_os = "linux")]
fn will_need(f: &std::fs::File, off: u64, n: u64) {
    use std::os::fd::AsRawFd;
    // a hint only: a kernel that refuses it reads the ranges as before
    // SAFETY: posix_fadvise reads no memory of this process
    unsafe {
        libc::posix_fadvise(
            f.as_raw_fd(),
            off as libc::off_t,
            n as libc::off_t,
            libc::POSIX_FADV_WILLNEED,
        );
    }
}

#[cfg(not(target_os = "linux"))]
fn will_need(_: &std::fs::File, _: u64, _: u64) {}

/// [`read_ranges`] with the hint made by `hint(file, offset, length, read)`, where `read`
/// is the length of the ranges read through so far, and with `ahead` for [`HINT_AHEAD`].
fn read_ranges_with(
    path: &Path,
    ranges: &[[u64; 2]],
    stop: &dyn Fn() -> bool,
    ahead: u64,
    hint: &mut dyn FnMut(&std::fs::File, u64, u64, u64),
) -> Result<Option<u64>> {
    let f = match std::fs::File::open(path) {
        Ok(f) => f,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Some(0)),
        Err(e) => return Err(e.into()),
    };
    let len = f.metadata()?.len();
    // an empty range is dropped: a hint of length 0 would mean the rest of the file
    let clipped: Vec<(u64, u64)> = ranges
        .iter()
        .filter(|&&[off, n]| off < len && n > 0)
        .map(|&[off, n]| (off, n.min(len - off)))
        .collect();
    // the next hint starts at range `hi`, offset `hat` into it. `hinted` counts the bytes
    // hinted so far, and `done` the length of the ranges read through, which a file that
    // shrank since the open may have read less of
    let (mut hi, mut hat, mut hinted, mut done) = (0usize, 0u64, 0u64, 0u64);
    let mut buf = vec![0u8; READ_CHUNK];
    let mut total = 0u64;
    for &(off, n) in &clipped {
        let mut at = off;
        while at < off + n {
            let read = done + (at - off);
            if hinted < read + ahead / 2 {
                while hi < clipped.len() && hinted < read + ahead {
                    if stop() {
                        return Ok(None);
                    }
                    let (ho, hn) = clipped[hi];
                    let step = (hn - hat).min(read + ahead - hinted);
                    hint(&f, ho + hat, step, read);
                    hinted += step;
                    hat += step;
                    if hat == hn {
                        hi += 1;
                        hat = 0;
                    }
                }
            }
            if stop() {
                return Ok(None);
            }
            let want = (READ_CHUNK as u64).min(off + n - at) as usize;
            let got = read_at(&f, &mut buf[..want], at)?;
            if got == 0 {
                break;
            }
            at += got as u64;
            total += got as u64;
        }
        done += n;
    }
    Ok(Some(total))
}

#[cfg(unix)]
fn read_at(f: &std::fs::File, buf: &mut [u8], at: u64) -> std::io::Result<usize> {
    std::os::unix::fs::FileExt::read_at(f, buf, at)
}

#[cfg(windows)]
fn read_at(f: &std::fs::File, buf: &mut [u8], at: u64) -> std::io::Result<usize> {
    std::os::windows::fs::FileExt::seek_read(f, buf, at)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::io::RdfFormat;
    use crate::io::Source;
    use crate::sparql::QueryOptions;
    use crate::store::StoreOptions;

    fn load(store: &Store, nt: String) {
        store
            .load(&[Source::from_bytes(nt.into_bytes(), RdfFormat::Turtle, None)])
            .unwrap();
    }

    fn store_with_data(dir: &Path) -> Store {
        let store = Store::open(dir, StoreOptions::default()).unwrap();
        let mut nt = String::new();
        for i in 0..5000 {
            nt.push_str(&format!(
                "<http://e/s{i}> <http://e/p> \"v{i}\" .\n<http://e/s{i}> <http://e/q> <http://e/o{}> .\n",
                i % 37
            ));
        }
        load(&store, nt);
        store.compact().unwrap();
        store
    }

    fn lookup(store: &Store) {
        let q = "SELECT ?v WHERE { <http://e/s4321> <http://e/p> ?v }";
        let r = crate::sparql::query(store.snapshot(), q, &QueryOptions::default()).unwrap();
        assert_eq!(r.test_application_rows().len(), 1);
    }

    #[test]
    fn a_recorded_list_restores_the_cached_parts_on_the_next_open() {
        let tmp = tempfile::tempdir().unwrap();
        let store = store_with_data(tmp.path());
        lookup(&store);
        let rec = store.record_prewarm().unwrap().unwrap();
        assert!(rec.parts > 0, "{rec:?}");
        drop(store);
        let store = Store::open(tmp.path(), StoreOptions::default()).unwrap();
        assert_eq!(store.snapshot().cache.entries(), 0);
        let done = store.prewarm(&|| false).unwrap();
        assert_eq!(done.skipped, None);
        assert_eq!(done.parts, rec.parts);
        assert!(store.snapshot().cache.entries() > 0);
        // the restored parts serve the lookup without a decode
        let misses = store.snapshot().cache.misses();
        lookup(&store);
        assert_eq!(store.snapshot().cache.misses(), misses);
    }

    #[test]
    fn a_missing_or_stale_list_reads_nothing() {
        let tmp = tempfile::tempdir().unwrap();
        let store = store_with_data(tmp.path());
        let done = store.prewarm(&|| false).unwrap();
        assert_eq!(done.parts, 0);
        assert!(done.skipped.unwrap().contains("no prewarm list"));
        lookup(&store);
        store.record_prewarm().unwrap().unwrap();
        // a compaction writes another generation, which the list does not describe
        load(&store, "<http://e/x> <http://e/p> \"x\" .\n".into());
        store.compact().unwrap();
        let done = store.prewarm(&|| false).unwrap();
        assert_eq!(done.parts, 0);
        assert!(done.skipped.unwrap().contains("describes generation"));
    }

    #[test]
    fn an_unknown_version_is_refused() {
        let tmp = tempfile::tempdir().unwrap();
        let store = store_with_data(tmp.path());
        lookup(&store);
        store.record_prewarm().unwrap().unwrap();
        let path = store.prewarm_path().unwrap();
        let mut v: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        v["version"] = 2.into();
        std::fs::write(&path, serde_json::to_vec(&v).unwrap()).unwrap();
        let e = store.prewarm(&|| false).unwrap_err().to_string();
        assert!(e.contains("version 2 is not one this build reads"), "{e}");
        std::fs::write(&path, br#"{"format":"other","version":1}"#).unwrap();
        let e = store.prewarm(&|| false).unwrap_err().to_string();
        assert!(e.contains("not a Sparkles prewarm list"), "{e}");
    }

    #[test]
    fn a_list_cannot_name_files_outside_the_generation() {
        let tmp = tempfile::tempdir().unwrap();
        let store = store_with_data(tmp.path());
        lookup(&store);
        store.record_prewarm().unwrap().unwrap();
        let path = store.prewarm_path().unwrap();
        let mut v: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        v["pages"] = serde_json::json!({"../CURRENT": [[0, 10]]});
        std::fs::write(&path, serde_json::to_vec(&v).unwrap()).unwrap();
        let e = store.prewarm(&|| false).unwrap_err().to_string();
        assert!(e.contains("is not a file of a generation"), "{e}");
        v["pages"] = serde_json::json!({});
        v["columns"] = serde_json::json!({"spo": [[999999, 0, 15]]});
        std::fs::write(&path, serde_json::to_vec(&v).unwrap()).unwrap();
        let e = store.prewarm(&|| false).unwrap_err().to_string();
        assert!(e.contains("is not in the index"), "{e}");
    }

    #[test]
    fn a_prewarm_stops_at_a_full_block_cache() {
        let tmp = tempfile::tempdir().unwrap();
        let store = store_with_data(tmp.path());
        lookup(&store);
        store.record_prewarm().unwrap().unwrap();
        drop(store);
        let opts = StoreOptions {
            cache_bytes: 64 << 10,
            ..StoreOptions::default()
        };
        let store = Store::open(tmp.path(), opts).unwrap();
        let done = store.prewarm(&|| false).unwrap();
        assert_eq!(done.parts, 0);
        assert_eq!(done.skipped.as_deref(), Some("the block cache is full"));
    }

    #[test]
    fn a_stop_ends_the_prewarm() {
        let tmp = tempfile::tempdir().unwrap();
        let store = store_with_data(tmp.path());
        lookup(&store);
        store.record_prewarm().unwrap().unwrap();
        let done = store.prewarm(&|| true).unwrap();
        assert_eq!(done.parts, 0);
        assert_eq!(done.skipped.as_deref(), Some("stopped"));
    }

    #[test]
    fn a_stopped_read_makes_no_hints_and_a_full_one_hints_each_range_once() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("f");
        std::fs::write(&path, vec![7u8; 100_000]).unwrap();
        let ranges = [
            [0, 4096],
            [8192, 0],
            [50_000, 10_000],
            [99_000, 5_000],
            [200_000, 10],
        ];
        let mut hints = Vec::new();
        let mut log = |_: &std::fs::File, o, n, _| hints.push((o, n));
        let got = read_ranges_with(&path, &ranges, &|| true, HINT_AHEAD, &mut log);
        assert_eq!(got.unwrap(), None);
        assert!(hints.is_empty(), "{hints:?}");

        let mut log = |_: &std::fs::File, o, n, _| hints.push((o, n));
        let got = read_ranges_with(&path, &ranges, &|| false, HINT_AHEAD, &mut log);
        assert_eq!(got.unwrap(), Some(4096 + 10_000 + 1_000));
        assert_eq!(hints, [(0, 4096), (50_000, 10_000), (99_000, 1_000)]);
    }

    #[test]
    fn the_hints_stay_within_the_window_ahead_of_the_reads() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("f");
        let size = 20 * READ_CHUNK as u64 + 123;
        std::fs::write(&path, vec![1u8; size as usize]).unwrap();
        let ahead = 4 * READ_CHUNK as u64;
        let ranges = [
            [0, 9 * READ_CHUNK as u64 + 5],
            [9 * READ_CHUNK as u64 + 100, size],
        ];
        let (mut hinted, mut next, mut calls) = (0u64, 0u64, 0);
        let mut check = |_: &std::fs::File, o: u64, n: u64, read: u64| {
            assert!(n > 0);
            assert!(o >= next, "hints in order and once");
            next = o + n;
            hinted += n;
            calls += 1;
            assert!(hinted <= read + ahead, "{hinted} hinted at {read} read");
        };
        let got = read_ranges_with(&path, &ranges, &|| false, ahead, &mut check);
        let want = 9 * READ_CHUNK as u64 + 5 + (size - 9 * READ_CHUNK as u64 - 100);
        assert_eq!(got.unwrap(), Some(want));
        assert_eq!(hinted, want);
        assert!(calls > 2, "the window refills as the reads go");
    }
}
