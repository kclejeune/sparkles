//! The change log: the net quad changes of every commit, kept apart from the index
//! generations so that history reaches back past compactions and bulk commits.
//!
//! A write-ahead log belongs to one generation and goes with it. The change log is per
//! dataset and holds each commit's net changes as vocabulary keys, which mean the same
//! thing in every generation. History queries ([`Store::history`]) and diffs read it.
//!
//! **Files.** A persistent store keeps `<root>/changes/`, a sequence of segments. A
//! segment `<first>.log` (its first commit, 20 digits) starts with a 32-byte header,
//! the magic `SPKCHGL\0`, the format (`1`, u32 LE), flags (u32, 0) and the dataset
//! UUID, and then holds one record per commit:
//!
//! | bytes | field |
//! |---|---|
//! | 4 | body length (u32 LE) |
//! | 4 | CRC-32 of the body |
//! | 1 | type: 1 changes, 2 summary (the changes were not recorded), 3 gap |
//! | 8 | the commit (u64 LE); for a gap, the last commit it covers |
//! | … | a gap: its first commit (u64 LE) |
//! | … | otherwise: timestamp (i64 LE), kind, flags (bit 0 bulk), inserted and deleted (varints), author and message (varint length and UTF-8) |
//! | … | changes: the record's terms (varint count, then varint length and key each), the changes (varint count, then the operation byte, 1 add or 0 remove, and the varint term numbers of graph, subject, predicate and object) |
//!
//! Removals come before additions, each ordered by graph, subject, predicate and object.
//! The default graph's key is empty.
//!
//! A segment that is full (`change_log_segment_bytes`) is sealed: an index file
//! `<first>.idx` is written next to it, with the offset and timestamp of every record
//! and a sorted list of (term hash, record number) pairs for the subjects, predicates,
//! objects and graphs of its changes. The open segment keeps the same index in memory.
//! A query with a subject, predicate, object or graph reads only the records whose
//! hashes match, and checks the keys themselves.
//!
//! **Writing.** A commit queues its changes in memory, as the ids of its generation,
//! and returns. A background thread turns queued commits into records and appends them
//! without syncing, so the commit path does no more I/O than before. A history query
//! appends what is queued before it reads. The log is synced before anything could make
//! its records unrecoverable: before a compaction or bulk commit switches `CURRENT`
//! (the old generation's write-ahead log is the only other copy of its changes), and
//! when the store closes. A crash can lose an unsynced tail, which the next open
//! recovers from the current generation's write-ahead log. Commits recovered that way
//! have no author, which the write-ahead log does not hold.
//!
//! **Bulk commits** have no write-ahead log records. Their changes are recorded when
//! they are small: the dataset was empty and the new state holds at most
//! `change_log_bulk_max_quads` quads, both states together hold at most that many, or the
//! quads the commit can have changed number at most that many. Those are the quads it
//! loads, the quads of the graphs it drops and its transaction's own changes, counted with
//! duplicates, so a commit that rewrites many quads it keeps can be a summary even when it
//! changes few. A larger one is recorded as a summary with the commit's counts. The build
//! collects those quads, and the log keeps the ones that differ between the states, so a
//! small commit into a large dataset reads neither state in full. History queries report
//! the commits they could not see.
//!
//! **Retention.** Whole segments are dropped from the oldest, by commit count, age or
//! total size. The open segment always stays.
//!
//! In-memory stores keep the same records in memory.

#[cfg(test)]
pub(super) mod open_tests;
mod pager;
mod readonly;

pub(crate) use pager::HistoryPages;

use super::diff::{Keys, QuadKey};
use super::*;
use rustc_hash::FxHashMap;
use serde::{Deserialize, Serialize};
use std::collections::VecDeque;
use std::sync::Weak;
use std::time::{Duration, Instant};

/// The directory of a persistent store's change log.
pub const CHANGES_DIR: &str = "changes";
/// A dataset's own change log settings.
pub const CHANGE_LOG_FILE: &str = "changelog.json";

const SEG_MAGIC: &[u8; 8] = b"SPKCHGL\0";
const IDX_MAGIC: &[u8; 8] = b"SPKCHGI\0";
const FORMAT: u32 = 1;
const SEG_HEADER: usize = 32;
const IDX_HEADER: usize = 64;

const REC_CHANGES: u8 = 1;
const REC_SUMMARY: u8 = 2;
const REC_GAP: u8 = 3;

const ROLE_G: u8 = b'g';
const ROLE_S: u8 = b's';
const ROLE_P: u8 = b'p';
const ROLE_O: u8 = b'o';

/// The background writer waits this long after a commit, so that it appends a batch.
const DEBOUNCE: Duration = Duration::from_millis(5);
/// The background writer syncs at most this often.
const SYNC_EVERY: Duration = Duration::from_secs(1);
/// The most zeros written ahead of the open segment's records at once.
const PREALLOC: u64 = 1 << 20;
/// Sealed segment indexes kept in memory.
const INDEX_CACHE: usize = 8;

/// A dataset's change log settings (`changelog.json`). Unset fields take the server's
/// defaults ([`StoreOptions::change_log`] and the options next to it).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChangeLogSettings {
    /// record changes (`None`: the server's default)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub enabled: Option<bool>,
    /// keep at least the last this many commits
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub keep_commits: Option<u64>,
    /// keep at least the commits of the last this many milliseconds
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub keep_age_ms: Option<u64>,
    /// the most disk (or memory) the log may use; 0 is unlimited (`None`: the server's
    /// default)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_bytes: Option<u64>,
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct SettingsFile {
    format: u32,
    #[serde(flatten)]
    settings: ChangeLogSettings,
}

pub(crate) fn read_settings(root: &Path) -> Result<ChangeLogSettings> {
    match std::fs::read(root.join(CHANGE_LOG_FILE)) {
        Ok(b) => {
            let mut v: serde_json::Value = serde_json::from_slice(&b)
                .map_err(|e| Error::Corrupt(format!("{CHANGE_LOG_FILE}: {e}")))?;
            crate::formats::check_settings(CHANGE_LOG_FILE, &v)?;
            // a file without the member is format 1
            if let Some(o) = v.as_object_mut() {
                o.entry("format").or_insert(1.into());
            }
            let f: SettingsFile = serde_json::from_value(v)
                .map_err(|e| Error::Corrupt(format!("{CHANGE_LOG_FILE}: {e}")))?;
            Ok(f.settings)
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(ChangeLogSettings::default()),
        Err(e) => Err(e.into()),
    }
}

fn write_settings(root: &Path, s: &ChangeLogSettings) -> Result<()> {
    if *s == ChangeLogSettings::default() {
        match std::fs::remove_file(root.join(CHANGE_LOG_FILE)) {
            Ok(()) => return sync_dir(root),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(e) => return Err(e.into()),
        }
    }
    let f = SettingsFile {
        format: 1,
        settings: *s,
    };
    write_atomic(
        &root.join(CHANGE_LOG_FILE),
        &crate::formats::settings_to_vec(CHANGE_LOG_FILE, &f)?,
    )
}

/// The commit a recorded change belongs to.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ChangeCommit {
    pub seq: u64,
    /// milliseconds since the Unix epoch
    pub timestamp_ms: i64,
    pub kind: CommitKind,
    /// made by rebuilding the generation (a bulk commit)
    pub bulk: bool,
    /// the commit's counts, as its catalog record has them
    pub inserted: u64,
    pub deleted: u64,
    /// who made the commit, when the writer said (a server records its caller)
    pub author: Option<Arc<str>>,
    /// the commit message
    pub message: Option<Arc<str>>,
}

impl ChangeCommit {
    pub(crate) fn of(c: &CommitInfo, author: Option<Arc<str>>, message: Option<Arc<str>>) -> Self {
        ChangeCommit {
            seq: c.seq,
            timestamp_ms: c.timestamp_ms,
            kind: c.kind,
            bulk: c.bulk,
            inserted: c.inserted,
            deleted: c.deleted,
            author,
            message,
        }
    }

    /// RFC 3339 timestamp in UTC with milliseconds.
    pub fn timestamp(&self) -> String {
        commit::rfc3339_ms(self.timestamp_ms)
    }
}

/// What a queued commit carries until the background writer records it.
pub(crate) enum PendingBody {
    /// the changes of a write-ahead log commit, as ids of `generation`, in log order
    /// (a quad may change more than once)
    Log {
        generation: Arc<Generation>,
        changes: Vec<(u8, [Id; 4])>,
    },
    /// net changes already turned into keys: `true` adds the quad
    Keys(Vec<(QuadKey, bool)>),
    /// the changes are not recorded (a large bulk commit)
    Summary,
    /// commits `from` through the commit of the entry are not recorded
    Gap { from: u64 },
}

pub(crate) struct Pending {
    pub commit: ChangeCommit,
    pub body: PendingBody,
}

/// Why some commits have no recorded changes.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum UnrecordedReason {
    /// older than the oldest commit the log keeps (it started later, or retention
    /// dropped them)
    BeforeLog,
    /// a bulk commit too large to record
    Bulk,
    /// the log could not record them (it was off, or a crash lost them with their
    /// generation)
    Gap,
}

/// Commits `from` through `to` whose changes are not recorded.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub struct Unrecorded {
    pub from: u64,
    pub to: u64,
    pub reason: UnrecordedReason,
}

/// One commit's record, as [`ChangeLog::commits`] reads it.
pub(crate) enum LogCommit {
    /// the commit's net changes, `true` for an addition
    Changes(Arc<ChangeCommit>, Vec<(bool, QuadKey)>),
    /// a bulk commit whose changes were not recorded
    Summary(Arc<ChangeCommit>),
    /// a commit the log has no record of, or only a gap record
    Missing,
}

impl ChangeCommit {
    /// The commit as a catalog record would have it, for a commit the catalog no longer
    /// keeps. The size of the dataset after it is not recorded, so `quads` is 0.
    pub(crate) fn info(&self) -> CommitInfo {
        CommitInfo {
            seq: self.seq,
            timestamp_ms: self.timestamp_ms,
            kind: self.kind,
            inserted: self.inserted,
            deleted: self.deleted,
            quads: 0,
            generation: 0,
            bulk: self.bulk,
            exact: true,
            reconstructed: false,
            default_graph: true,
            unvalidated: false,
        }
    }
}

/// One decoded record.
enum Record {
    Changes(Arc<ChangeCommit>, Vec<(bool, QuadKey)>),
    Summary(Arc<ChangeCommit>),
    Gap { from: u64, through: u64 },
}

impl Record {
    fn first(&self) -> u64 {
        match self {
            Record::Changes(c, _) | Record::Summary(c) => c.seq,
            Record::Gap { from, .. } => *from,
        }
    }

    fn last(&self) -> u64 {
        match self {
            Record::Changes(c, _) | Record::Summary(c) => c.seq,
            Record::Gap { through, .. } => *through,
        }
    }

    fn timestamp(&self) -> Option<i64> {
        match self {
            Record::Changes(c, _) | Record::Summary(c) => Some(c.timestamp_ms),
            Record::Gap { .. } => None,
        }
    }
}

// ------------------------------------------------------------------ encoding ------

fn put_varint(out: &mut Vec<u8>, v: u64) {
    crate::vocab::write_varint(out, v);
}

fn put_str(out: &mut Vec<u8>, s: Option<&str>) {
    let s = s.unwrap_or("");
    put_varint(out, s.len() as u64);
    out.extend_from_slice(s.as_bytes());
}

/// A bounds-checked reader of a record body.
struct Cur<'a> {
    b: &'a [u8],
    i: usize,
}

impl<'a> Cur<'a> {
    fn bytes(&mut self, n: usize) -> Option<&'a [u8]> {
        let s = self.b.get(self.i..self.i.checked_add(n)?)?;
        self.i += n;
        Some(s)
    }

    fn u8(&mut self) -> Option<u8> {
        Some(self.bytes(1)?[0])
    }

    fn u64(&mut self) -> Option<u64> {
        Some(u64::from_le_bytes(self.bytes(8)?.try_into().ok()?))
    }

    fn varint(&mut self) -> Option<u64> {
        let mut v = 0u64;
        for shift in (0..64).step_by(7) {
            let b = self.u8()?;
            v |= ((b & 0x7F) as u64) << shift;
            if b < 0x80 {
                return Some(v);
            }
        }
        None
    }

    fn str(&mut self) -> Option<Option<Arc<str>>> {
        let n = self.varint()? as usize;
        let s = std::str::from_utf8(self.bytes(n)?).ok()?;
        Some((!s.is_empty()).then(|| Arc::from(s)))
    }
}

fn crc32(b: &[u8]) -> u32 {
    let mut c = flate2::Crc::new();
    c.update(b);
    c.sum()
}

fn encode_meta(out: &mut Vec<u8>, c: &ChangeCommit) {
    out.extend_from_slice(&c.timestamp_ms.to_le_bytes());
    out.push(c.kind.code());
    out.push(u8::from(c.bulk));
    put_varint(out, c.inserted);
    put_varint(out, c.deleted);
    put_str(out, c.author.as_deref());
    put_str(out, c.message.as_deref());
}

/// A framed record: length, CRC and body.
fn encode(r: &Record) -> Vec<u8> {
    let mut body = Vec::with_capacity(64);
    match r {
        Record::Gap { from, through } => {
            body.push(REC_GAP);
            body.extend_from_slice(&through.to_le_bytes());
            body.extend_from_slice(&from.to_le_bytes());
        }
        Record::Summary(c) => {
            body.push(REC_SUMMARY);
            body.extend_from_slice(&c.seq.to_le_bytes());
            encode_meta(&mut body, c);
        }
        Record::Changes(c, changes) => {
            body.push(REC_CHANGES);
            body.extend_from_slice(&c.seq.to_le_bytes());
            encode_meta(&mut body, c);
            // the record's own term table: a term the commit names more than once is
            // written once
            let mut terms: FxHashMap<&[u8], u64> = FxHashMap::default();
            let mut order: Vec<&[u8]> = Vec::new();
            for (_, k) in changes {
                for t in k {
                    let n = terms.len() as u64;
                    terms.entry(&t[..]).or_insert_with(|| {
                        order.push(&t[..]);
                        n
                    });
                }
            }
            put_varint(&mut body, order.len() as u64);
            for t in &order {
                put_varint(&mut body, t.len() as u64);
                body.extend_from_slice(t);
            }
            put_varint(&mut body, changes.len() as u64);
            for (add, k) in changes {
                body.push(u8::from(*add));
                for t in k {
                    put_varint(&mut body, terms[&t[..]]);
                }
            }
        }
    }
    let mut out = Vec::with_capacity(body.len() + 8);
    out.extend_from_slice(&(body.len() as u32).to_le_bytes());
    out.extend_from_slice(&crc32(&body).to_le_bytes());
    out.extend_from_slice(&body);
    out
}

fn decode_meta(c: &mut Cur<'_>, seq: u64) -> Option<ChangeCommit> {
    let ts = c.u64()? as i64;
    let kind = CommitKind::from_code(c.u8()?);
    let flags = c.u8()?;
    Some(ChangeCommit {
        seq,
        timestamp_ms: ts,
        kind,
        bulk: flags & 1 != 0,
        inserted: c.varint()?,
        deleted: c.varint()?,
        author: c.str()?,
        message: c.str()?,
    })
}

/// Decode a record body (its CRC already checked).
fn decode(body: &[u8]) -> Option<Record> {
    decode_with(body, false)
}

/// The bytes an `Arc<[u8]>` or `Arc<str>` of `len` bytes allocates.
fn arc_slice_bytes(len: usize) -> u64 {
    (16 + len as u64).next_multiple_of(8)
}

/// What decoding a record body allocates, read from its length prefixes before
/// anything is allocated (item 829): the commit's metadata with its author and message,
/// the term table with its terms, and the vector of changes. `None` at damage, and
/// when a count is larger than the bytes left could hold, so a damaged count is never
/// admitted.
#[derive(Clone, Copy, Debug, Default)]
pub(super) struct Footprint {
    pub(super) meta: u64,
    pub(super) terms: u64,
    pub(super) changes: u64,
}

/// Maximum entries processed between cooperative cancellation polls in history
/// reads and decoding.
const CHECK_STRIDE: usize = 1024;

pub(super) fn footprint_checked(
    body: &[u8],
    check: &mut dyn FnMut() -> Result<()>,
) -> Result<Option<Footprint>> {
    check()?;
    let mut c = Cur { b: body, i: 0 };
    let Some((ty, _)) = c.u8().zip(c.u64()) else {
        return Ok(None);
    };
    let mut f = Footprint::default();
    if ty == REC_GAP {
        check()?;
        return Ok(Some(f));
    }
    if ty != REC_SUMMARY && ty != REC_CHANGES {
        return Ok(None);
    }
    if c.u64().is_none()
        || c.u8().is_none()
        || c.u8().is_none()
        || c.varint().is_none()
        || c.varint().is_none()
    {
        return Ok(None);
    }
    f.meta = arc_slice_bytes(std::mem::size_of::<ChangeCommit>());
    for _ in 0..2 {
        let Some(n) = c.varint().and_then(|v| usize::try_from(v).ok()) else {
            return Ok(None);
        };
        if c.bytes(n).is_none() {
            return Ok(None);
        }
        if n > 0 {
            f.meta += arc_slice_bytes(n);
        }
    }
    if ty == REC_SUMMARY {
        check()?;
        return Ok(Some(f));
    }
    let Some(n) = c.varint().and_then(|v| usize::try_from(v).ok()) else {
        return Ok(None);
    };
    // each term takes at least its one-byte length
    if n > body.len() - c.i {
        return Ok(None);
    }
    f.terms = n as u64 * std::mem::size_of::<Arc<[u8]>>() as u64;
    for idx in 0..n {
        if idx.is_multiple_of(CHECK_STRIDE) {
            check()?;
        }
        let Some(len) = c.varint().and_then(|v| usize::try_from(v).ok()) else {
            return Ok(None);
        };
        if c.bytes(len).is_none() {
            return Ok(None);
        }
        f.terms += arc_slice_bytes(len);
    }
    let Some(m) = c.varint().and_then(|v| usize::try_from(v).ok()) else {
        return Ok(None);
    };
    // each change takes at least five bytes
    if m > (body.len() - c.i) / 5 {
        return Ok(None);
    }
    f.changes = m as u64 * std::mem::size_of::<(bool, QuadKey)>() as u64;
    check()?;
    Ok(Some(f))
}

/// Decode a record body. `exact` reserves the term table and the changes at their
/// counts, which the caller has admitted from [`footprint_checked`]; otherwise each
/// reservation is capped, since a damaged count must not allocate before the bytes run
/// out.
fn decode_with(body: &[u8], exact: bool) -> Option<Record> {
    let cap = |n: usize| if exact { n } else { n.min(1 << 16) };
    let mut c = Cur { b: body, i: 0 };
    let ty = c.u8()?;
    let seq = c.u64()?;
    let r = match ty {
        REC_GAP => {
            let from = c.u64()?;
            if from > seq {
                return None;
            }
            Record::Gap { from, through: seq }
        }
        REC_SUMMARY => Record::Summary(Arc::new(decode_meta(&mut c, seq)?)),
        REC_CHANGES => {
            let meta = Arc::new(decode_meta(&mut c, seq)?);
            let n = c.varint()? as usize;
            let mut terms: Vec<Arc<[u8]>> = Vec::with_capacity(cap(n));
            for _ in 0..n {
                let len = c.varint()? as usize;
                terms.push(Arc::from(c.bytes(len)?));
            }
            let m = c.varint()? as usize;
            let mut changes = Vec::with_capacity(cap(m));
            for _ in 0..m {
                let add = c.u8()? == 1;
                let mut k: [Option<Arc<[u8]>>; 4] = Default::default();
                for slot in &mut k {
                    *slot = Some(terms.get(c.varint()? as usize)?.clone());
                }
                changes.push((add, k.map(|t| t.expect("filled above"))));
            }
            Record::Changes(meta, changes)
        }
        _ => return None,
    };
    (c.i == body.len()).then_some(r)
}

/// Checked variant of `decode_with(body, true)` for admitted paged reads. Polls `check`
/// at entry and exit, at most every [`CHECK_STRIDE`] term or change entries, and in
/// 64 KiB chunks while validating UTF-8 or copying metadata and term payloads so stop
/// errors propagate as `Err` instead of being mistaken for corruption.
fn decode_admitted_checked(
    body: &[u8],
    check: &mut dyn FnMut() -> Result<()>,
) -> Result<Option<Record>> {
    check()?;
    let mut c = Cur { b: body, i: 0 };
    let Some((ty, seq)) = c.u8().zip(c.u64()) else {
        return Ok(None);
    };
    let r = match ty {
        REC_GAP => {
            let Some(from) = c.u64() else {
                return Ok(None);
            };
            if from > seq {
                return Ok(None);
            }
            Record::Gap { from, through: seq }
        }
        REC_SUMMARY => {
            let Some(meta) = readonly::meta_checked(&mut c, seq, check)? else {
                return Ok(None);
            };
            Record::Summary(meta)
        }
        REC_CHANGES => {
            let Some(meta) = readonly::meta_checked(&mut c, seq, check)? else {
                return Ok(None);
            };
            let Some(n) = c.varint().and_then(|v| usize::try_from(v).ok()) else {
                return Ok(None);
            };
            if n > body.len() - c.i {
                return Ok(None);
            }
            let mut terms: Vec<Arc<[u8]>> = Vec::new();
            terms.try_reserve_exact(n).map_err(|_| {
                Error::HistoryUnsupported("history term-table allocation was refused".into())
            })?;
            let mut bytes_since_check: usize = 0;
            for idx in 0..n {
                if idx.is_multiple_of(CHECK_STRIDE) {
                    check()?;
                    bytes_since_check = 0;
                }
                let Some(len) = c.varint().and_then(|v| usize::try_from(v).ok()) else {
                    return Ok(None);
                };
                let Some(raw) = c.bytes(len) else {
                    return Ok(None);
                };
                if len > (64 << 10) {
                    terms.push(readonly::arc_bytes_checked(raw, check)?);
                    bytes_since_check = 0;
                } else {
                    if bytes_since_check.saturating_add(len) > (64 << 10) {
                        check()?;
                        bytes_since_check = 0;
                    }
                    terms.push(Arc::from(raw));
                    bytes_since_check = bytes_since_check.saturating_add(len);
                }
            }
            let Some(m) = c.varint().and_then(|v| usize::try_from(v).ok()) else {
                return Ok(None);
            };
            if m > (body.len() - c.i) / 5 {
                return Ok(None);
            }
            check()?;
            let mut changes = Vec::new();
            changes.try_reserve_exact(m).map_err(|_| {
                Error::HistoryUnsupported("history change-table allocation was refused".into())
            })?;
            for idx in 0..m {
                if idx.is_multiple_of(CHECK_STRIDE) {
                    check()?;
                }
                let Some(add_byte) = c.u8() else {
                    return Ok(None);
                };
                let add = add_byte == 1;
                let mut k: [Option<Arc<[u8]>>; 4] = Default::default();
                for slot in &mut k {
                    let Some(tid) = c.varint().and_then(|v| usize::try_from(v).ok()) else {
                        return Ok(None);
                    };
                    let Some(term) = terms.get(tid) else {
                        return Ok(None);
                    };
                    *slot = Some(term.clone());
                }
                changes.push((add, k.map(|t| t.expect("filled above"))));
            }
            Record::Changes(meta, changes)
        }
        _ => return Ok(None),
    };
    check()?;
    Ok((c.i == body.len()).then_some(r))
}

fn crc32_checked(body: &[u8], check: &mut dyn FnMut() -> Result<()>) -> Result<u32> {
    check()?;
    let mut crc = flate2::Crc::new();
    for chunk in body.chunks(64 << 10) {
        check()?;
        #[cfg(test)]
        open_tests::observe(open_tests::Event::IndexCrcChunk);
        crc.update(chunk);
    }
    check()?;
    Ok(crc.sum())
}

/// Checked variant of [`frame`] that hashes the body in 64 KiB chunks with stop polls.
fn frame_checked<'a>(
    buf: &'a [u8],
    at: usize,
    check: &mut dyn FnMut() -> Result<()>,
) -> Result<Option<(&'a [u8], usize)>> {
    check()?;
    let Some(end_h) = at.checked_add(8) else {
        return Ok(None);
    };
    let Some(h) = buf.get(at..end_h) else {
        return Ok(None);
    };
    let len = u32::from_le_bytes(h[0..4].try_into().unwrap()) as usize;
    let crc = u32::from_le_bytes(h[4..8].try_into().unwrap());
    if len == 0 {
        return Ok(None);
    }
    let Some(end) = end_h.checked_add(len) else {
        return Ok(None);
    };
    let Some(body) = buf.get(end_h..end) else {
        return Ok(None);
    };
    Ok((crc32_checked(body, check)? == crc).then_some((body, end)))
}

/// The next framed record of `buf` at `at`: (body, the offset after it). `None` at the
/// end, at a torn record or at damage.
fn frame(buf: &[u8], at: usize) -> Option<(&[u8], usize)> {
    let h = buf.get(at..at + 8)?;
    let len = u32::from_le_bytes(h[0..4].try_into().ok()?) as usize;
    let crc = u32::from_le_bytes(h[4..8].try_into().ok()?);
    if len == 0 {
        return None;
    }
    let body = buf.get(at + 8..at + 8 + len)?;
    (crc32(body) == crc).then_some((body, at + 8 + len))
}

/// The FNV-1a hash of a term key in a role (graph, subject, predicate, object). Stable
/// across processes, since sealed indexes hold it.
fn term_hash(role: u8, key: &[u8]) -> u64 {
    let mut h = 0xcbf2_9ce4_8422_2325u64;
    for &b in std::iter::once(&role).chain(key) {
        h = (h ^ b as u64).wrapping_mul(0x100_0000_01b3);
    }
    h
}

const ROLES: [u8; 4] = [ROLE_G, ROLE_S, ROLE_P, ROLE_O];

// ------------------------------------------------------------------- segments ------

/// Where a record is and what it covers.
#[derive(Clone, Copy, Debug)]
struct RecEntry {
    /// the last commit it covers (a commit's own number)
    seq: u64,
    offset: u64,
    /// the commit's timestamp (`i64::MIN` for a gap)
    ts: i64,
    /// a summary or gap record: its commits' changes are not recorded
    hole: Option<(u64, UnrecordedReason)>,
}

impl RecEntry {
    fn decode(c: &mut Cur<'_>) -> Option<Self> {
        let seq = c.u64()?;
        let offset = c.u64()?;
        let ts = c.u64()? as i64;
        let hole = match c.u8()? {
            0 => None,
            1 => Some((c.u64()?, UnrecordedReason::Bulk)),
            _ => Some((c.u64()?, UnrecordedReason::Gap)),
        };
        Some(Self {
            seq,
            offset,
            ts,
            hole,
        })
    }
}

/// The index of one segment.
#[derive(Clone, Default)]
struct SegIndex {
    recs: Vec<RecEntry>,
    /// (term hash, record number), sorted (sealed segments)
    postings: Vec<(u64, u32)>,
    /// term hash → record numbers, ascending (the open segment)
    live: FxHashMap<u64, Vec<u32>>,
}

impl SegIndex {
    fn note(&mut self, r: &Record, offset: u64) {
        let ord = self.recs.len() as u32;
        let hole = match r {
            Record::Changes(..) => None,
            Record::Summary(_) => Some((r.first(), UnrecordedReason::Bulk)),
            Record::Gap { from, .. } => Some((*from, UnrecordedReason::Gap)),
        };
        // a gap takes the timestamp before it, so that timestamps stay ordered
        let prev = self.recs.last().map_or(i64::MIN, |e| e.ts);
        self.recs.push(RecEntry {
            seq: r.last(),
            offset,
            ts: r.timestamp().unwrap_or(prev),
            hole,
        });
        if let Record::Changes(_, changes) = r {
            for (_, k) in changes {
                for (role, t) in ROLES.iter().zip(k) {
                    let v = self.live.entry(term_hash(*role, t)).or_default();
                    if v.last() != Some(&ord) {
                        v.push(ord);
                    }
                }
            }
        }
    }

    /// Move the live postings into the sorted list.
    fn seal(&mut self) {
        let mut p: Vec<(u64, u32)> = std::mem::take(&mut self.postings);
        for (h, ords) in self.live.drain() {
            p.extend(ords.into_iter().map(|o| (h, o)));
        }
        p.sort_unstable();
        p.dedup();
        self.postings = p;
    }

    /// The record numbers that may hold `hash`, ascending.
    fn lookup(&self, hash: u64, out: &mut Vec<u32>) {
        let lo = self.postings.partition_point(|p| p.0 < hash);
        out.extend(
            self.postings[lo..]
                .iter()
                .take_while(|p| p.0 == hash)
                .map(|p| p.1),
        );
        if let Some(v) = self.live.get(&hash) {
            out.extend_from_slice(v);
        }
    }

    fn encode(&self, dataset_id: uuid::Uuid, first: u64, last: u64) -> Vec<u8> {
        let mut body = Vec::with_capacity(self.recs.len() * 33 + self.postings.len() * 12);
        for r in &self.recs {
            body.extend_from_slice(&r.seq.to_le_bytes());
            body.extend_from_slice(&r.offset.to_le_bytes());
            body.extend_from_slice(&r.ts.to_le_bytes());
            match r.hole {
                None => body.push(0),
                Some((from, reason)) => {
                    body.push(match reason {
                        UnrecordedReason::Bulk => 1,
                        _ => 2,
                    });
                    body.extend_from_slice(&from.to_le_bytes());
                }
            }
        }
        for (h, o) in &self.postings {
            body.extend_from_slice(&h.to_le_bytes());
            body.extend_from_slice(&o.to_le_bytes());
        }
        let mut out = Vec::with_capacity(IDX_HEADER + body.len());
        out.extend_from_slice(IDX_MAGIC);
        out.extend_from_slice(&FORMAT.to_le_bytes());
        out.extend_from_slice(&0u32.to_le_bytes());
        out.extend_from_slice(dataset_id.as_bytes());
        out.extend_from_slice(&first.to_le_bytes());
        out.extend_from_slice(&last.to_le_bytes());
        out.extend_from_slice(&(self.recs.len() as u32).to_le_bytes());
        out.extend_from_slice(&(self.postings.len() as u32).to_le_bytes());
        out.extend_from_slice(&crc32(&body).to_le_bytes());
        out.extend_from_slice(&[0u8; 4]);
        debug_assert_eq!(out.len(), IDX_HEADER);
        out.extend_from_slice(&body);
        out
    }

    /// Decode an index file: (first, last, index).
    fn decode(b: &[u8], dataset_id: uuid::Uuid) -> Option<(u64, u64, SegIndex)> {
        Self::decode_with(b, dataset_id, None).ok().flatten()
    }

    fn decode_with(
        b: &[u8],
        dataset_id: uuid::Uuid,
        mut check: Option<&mut dyn FnMut() -> Result<()>>,
    ) -> Result<Option<(u64, u64, SegIndex)>> {
        if b.len() < IDX_HEADER || &b[0..8] != IDX_MAGIC {
            return Ok(None);
        }
        let u32_at = |i: usize| u32::from_le_bytes(b[i..i + 4].try_into().unwrap());
        let u64_at = |i: usize| u64::from_le_bytes(b[i..i + 8].try_into().unwrap());
        if u32_at(8) != FORMAT || b[16..32] != *dataset_id.as_bytes() {
            return Ok(None);
        }
        let (first, last) = (u64_at(32), u64_at(40));
        let (nrecs, nposts) = (u32_at(48) as usize, u32_at(52) as usize);
        let body = &b[IDX_HEADER..];
        let actual_crc = match check.as_deref_mut() {
            Some(check) => crc32_checked(body, check)?,
            None => {
                #[cfg(test)]
                open_tests::observe(open_tests::Event::IndexCrcChunk);
                crc32(body)
            }
        };
        if actual_crc != u32_at(56) {
            return Ok(None);
        }
        let mut c = Cur { b: body, i: 0 };
        let mut ix = SegIndex::default();
        #[cfg(test)]
        open_tests::observe(open_tests::Event::IndexRecords);
        for idx in 0..nrecs {
            if idx.is_multiple_of(CHECK_STRIDE)
                && let Some(check) = check.as_deref_mut()
            {
                check()?;
            }
            #[cfg(test)]
            open_tests::observe(open_tests::Event::IndexRecord);
            let Some(entry) = RecEntry::decode(&mut c) else {
                return Ok(None);
            };
            ix.recs.push(entry);
        }
        for idx in 0..nposts {
            if idx.is_multiple_of(CHECK_STRIDE)
                && let Some(check) = check.as_deref_mut()
            {
                check()?;
            }
            #[cfg(test)]
            open_tests::observe(open_tests::Event::IndexPosting);
            let Some((h, raw)) = c.u64().zip(c.bytes(4)) else {
                return Ok(None);
            };
            let o = u32::from_le_bytes(raw.try_into().unwrap());
            ix.postings.push((h, o));
        }
        if c.i != body.len() {
            return Ok(None);
        }
        if let Some(check) = check {
            check()?;
        }
        Ok(Some((first, last, ix)))
    }
}

struct Segment {
    /// the first commit it covers
    first: u64,
    /// the last commit it covers
    last: u64,
    /// the newest commit timestamp it holds (`i64::MIN` for none)
    last_ts: i64,
    /// bytes, its header included
    bytes: u64,
    /// the file (persistent stores)
    path: Option<PathBuf>,
    /// the bytes (in-memory stores)
    mem: Vec<u8>,
    sealed: bool,
    /// the index: always for the open segment and in memory, loaded on demand for a
    /// sealed persistent one
    index: Option<Arc<SegIndex>>,
}

impl Segment {
    fn idx_path(&self) -> Option<PathBuf> {
        self.path.as_ref().map(|p| p.with_extension("idx"))
    }
}

fn seg_name(first: u64) -> String {
    format!("{first:020}.log")
}

fn seg_header(dataset_id: uuid::Uuid) -> [u8; SEG_HEADER] {
    let mut h = [0u8; SEG_HEADER];
    h[0..8].copy_from_slice(SEG_MAGIC);
    h[8..12].copy_from_slice(&FORMAT.to_le_bytes());
    h[16..32].copy_from_slice(dataset_id.as_bytes());
    h
}

/// The records of a segment's bytes: (record, offset), stopping at the first torn or
/// damaged one. Returns the records and the length of the good prefix.
fn scan_bytes(buf: &[u8]) -> (Vec<(Record, u64)>, usize) {
    let mut out = Vec::new();
    let mut at = SEG_HEADER;
    while let Some((body, next)) = frame(buf, at) {
        let Some(r) = decode(body) else { break };
        // records follow each other without holes
        if let Some((prev, _)) = out.last()
            && r.first() != Record::last(prev) + 1
        {
            break;
        }
        out.push((r, at as u64));
        at = next;
    }
    (out, at.min(buf.len()).max(SEG_HEADER.min(buf.len())))
}

// ------------------------------------------------------------------ the log ------

/// How the log is configured (from the store's options and the dataset's settings).
#[derive(Clone, Copy, Debug)]
pub(crate) struct LogLimits {
    pub segment_bytes: u64,
    pub default_max_bytes: u64,
    pub default_on: bool,
    pub bulk_max_quads: u64,
}

struct Inner {
    segs: Vec<Segment>,
    /// the open segment's file
    file: Option<File>,
    /// the open segment file's length: records, then zeros written ahead of them
    alloc: u64,
    /// unsynced bytes were written
    dirty: bool,
    /// the newest commit covered (`None`: nothing recorded yet)
    last: Option<u64>,
    /// the log stopped recording after an error
    failed: Option<String>,
    last_sync: Instant,
    /// recently used sealed indexes, by segment first commit
    cache: VecDeque<(u64, Arc<SegIndex>)>,
}

/// A dataset's change log (see the module documentation).
pub struct ChangeLog {
    /// A cold attachment owns descriptors only, never a writer or index sidecar.
    readonly: Option<readonly::ReadOnlyLog>,
    dataset_id: uuid::Uuid,
    /// `<root>/changes` (persistent stores)
    dir: Option<PathBuf>,
    root: Option<PathBuf>,
    limits: LogLimits,
    settings: Mutex<ChangeLogSettings>,
    enabled: AtomicBool,
    inner: Mutex<Inner>,
    /// The first commits that history readers still need, with the number of readers
    /// pinning each (see [`LogPin`]). Retention keeps every segment at or after the
    /// oldest, and the log cannot be turned off while any is held. Lock order: `inner`
    /// before `pins`, and only `settings` is locked while `pins` is held, by
    /// [`ChangeLog::set_settings`].
    pins: Mutex<std::collections::BTreeMap<u64, usize>>,
    pending: Mutex<VecDeque<Pending>>,
    /// the background writer has been told about the queue
    scheduled: AtomicBool,
    /// hand queued commits to the background writer (tests turn it off)
    background: AtomicBool,
    me: Weak<ChangeLog>,
}

/// A summary of the log for status reports.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ChangeLogStatus {
    pub enabled: bool,
    /// the oldest and newest commits covered
    pub first: Option<u64>,
    pub last: Option<u64>,
    pub segments: usize,
    pub bytes: u64,
    /// commits queued and not yet appended
    pub pending: usize,
    pub settings: ChangeLogSettings,
    /// the effective size limit (0: unlimited)
    pub max_bytes: u64,
    /// why the log stopped recording, if it did
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

impl ChangeLog {
    pub(super) fn read_only_through(&self) -> Option<u64> {
        self.readonly.as_ref().map(|reader| reader.through)
    }

    /// An in-memory store's log.
    pub(crate) fn memory(dataset_id: uuid::Uuid, limits: LogLimits) -> Arc<ChangeLog> {
        Self::new(
            dataset_id,
            None,
            limits,
            ChangeLogSettings::default(),
            Inner::empty(),
            None,
        )
    }

    fn new(
        dataset_id: uuid::Uuid,
        root: Option<&Path>,
        limits: LogLimits,
        settings: ChangeLogSettings,
        inner: Inner,
        readonly: Option<readonly::ReadOnlyLog>,
    ) -> Arc<ChangeLog> {
        let enabled = settings.enabled.unwrap_or(limits.default_on);
        Arc::new_cyclic(|me| ChangeLog {
            readonly,
            dataset_id,
            dir: root.map(|r| r.join(CHANGES_DIR)),
            root: root.map(Path::to_path_buf),
            limits,
            settings: Mutex::new(settings),
            enabled: AtomicBool::new(enabled),
            inner: Mutex::new(inner),
            pins: Mutex::new(std::collections::BTreeMap::new()),
            pending: Mutex::new(VecDeque::new()),
            scheduled: AtomicBool::new(false),
            background: AtomicBool::new(true),
            me: me.clone(),
        })
    }

    /// Open a persistent store's log: sealed segments are listed (their indexes are
    /// read on demand, and rebuilt if missing), the open segment is read and a torn
    /// tail truncated. A log of another dataset is set aside.
    pub(crate) fn open(
        root: &Path,
        dataset_id: uuid::Uuid,
        limits: LogLimits,
    ) -> Result<Arc<ChangeLog>> {
        let settings = read_settings(root)?;
        let dir = root.join(CHANGES_DIR);
        let inner = match Self::open_dir(&dir, dataset_id)? {
            Some(i) => i,
            None => Inner::empty(),
        };
        let log = Self::new(dataset_id, Some(root), limits, settings, inner, None);
        if !log.enabled.load(Ordering::Relaxed) && dir.exists() {
            // turned off while the store was closed: nothing is kept
            log.remove_all()?;
        }
        Ok(log)
    }

    fn open_dir(dir: &Path, dataset_id: uuid::Uuid) -> Result<Option<Inner>> {
        let entries = match std::fs::read_dir(dir) {
            Ok(e) => e,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(e.into()),
        };
        let mut logs: Vec<(u64, PathBuf)> = Vec::new();
        let mut idxs: Vec<(u64, PathBuf)> = Vec::new();
        for e in entries {
            let e = e?;
            let path = e.path();
            let name = e.file_name().to_string_lossy().into_owned();
            if name.ends_with(".tmp") {
                let _ = std::fs::remove_file(&path);
                continue;
            }
            let Some((stem, ext)) = name.split_once('.') else {
                continue;
            };
            let Ok(first) = stem.parse::<u64>() else {
                continue;
            };
            match ext {
                "log" => logs.push((first, path)),
                "idx" => idxs.push((first, path)),
                _ => {}
            }
        }
        logs.sort();
        // an index whose segment is gone (retention removed the log first)
        for (first, p) in &idxs {
            if !logs.iter().any(|(f, _)| f == first) {
                let _ = std::fs::remove_file(p);
            }
        }
        let mut segs: Vec<Segment> = Vec::new();
        let n = logs.len();
        for (i, (first, path)) in logs.into_iter().enumerate() {
            let last_seg = i + 1 == n;
            let mut header = [0u8; SEG_HEADER];
            let len = {
                let mut f = File::open(&path)?;
                let len = f.metadata()?.len();
                if len < SEG_HEADER as u64 || f.read_exact(&mut header).is_err() {
                    // a segment cut short as it was created: nothing in it
                    drop(f);
                    if last_seg {
                        std::fs::remove_file(&path)?;
                        continue;
                    }
                    return Err(Error::Corrupt(format!(
                        "{}: a sealed change log segment without a header",
                        path.display()
                    )));
                }
                len
            };
            if &header[0..8] != SEG_MAGIC || header[16..32] != *dataset_id.as_bytes() {
                // another dataset's log (a copied directory): set it aside
                let aside =
                    dir.with_file_name(format!("{CHANGES_DIR}.stale-{}", crate::commit::now_ms()));
                tracing::warn!(
                    target: "sparkles::store::changelog",
                    dir = %dir.display(),
                    "the change log does not belong to this dataset; set aside as {}",
                    aside.display()
                );
                std::fs::rename(dir, &aside)?;
                return Ok(None);
            }
            let mut seg = Segment {
                first,
                last: first.saturating_sub(1),
                last_ts: i64::MIN,
                bytes: len,
                path: Some(path.clone()),
                mem: Vec::new(),
                sealed: !last_seg,
                index: None,
            };
            if !last_seg {
                // a sealed segment: its index file says what it covers
                let idx = std::fs::read(path.with_extension("idx"))
                    .ok()
                    .and_then(|b| SegIndex::decode(&b, dataset_id));
                match idx {
                    Some((f, l, ix)) if f == first => {
                        seg.last = l;
                        seg.last_ts = ix.recs.iter().map(|r| r.ts).max().unwrap_or(i64::MIN);
                    }
                    _ => {
                        // the index was never written (a crash while sealing): rebuild it
                        let buf = std::fs::read(&path)?;
                        let (recs, good) = scan_bytes(&buf);
                        let mut ix = SegIndex::default();
                        for (r, off) in &recs {
                            ix.note(r, *off);
                        }
                        ix.seal();
                        seg.last = recs.last().map_or(first.saturating_sub(1), |r| r.0.last());
                        seg.last_ts = ix.recs.iter().map(|r| r.ts).max().unwrap_or(i64::MIN);
                        if good < buf.len() {
                            tracing::warn!(
                                target: "sparkles::store::changelog",
                                "{}: damaged change log records from byte {good} are ignored",
                                path.display()
                            );
                        }
                        let idx_path = path.with_extension("idx");
                        write_synced_atomic(&idx_path, &ix.encode(dataset_id, first, seg.last))?;
                    }
                }
            } else {
                // the open segment: read it, truncate a torn tail, index it in memory
                let buf = std::fs::read(&path)?;
                let (recs, good) = scan_bytes(&buf);
                if good < buf.len() {
                    let f = OpenOptions::new().write(true).open(&path)?;
                    f.set_len(good as u64)?;
                    f.sync_all()?;
                    seg.bytes = good as u64;
                }
                let mut ix = SegIndex::default();
                for (r, off) in &recs {
                    ix.note(r, *off);
                }
                seg.last = recs.last().map_or(first.saturating_sub(1), |r| r.0.last());
                seg.last_ts = ix.recs.iter().map(|r| r.ts).max().unwrap_or(i64::MIN);
                seg.index = Some(Arc::new(ix));
                if recs.is_empty() {
                    // an empty open segment is created again with the next record
                    std::fs::remove_file(&path)?;
                    continue;
                }
            }
            segs.push(seg);
        }
        sync_dir(dir)?;
        let last = segs.last().map(|s| s.last);
        let file = match segs.last() {
            Some(s) if !s.sealed => Some(super::wal::open_for_append(
                s.path.as_ref().expect("persistent"),
                super::wal::PLAIN_VERSION,
            )?),
            _ => None,
        };
        let alloc = match &file {
            Some(f) => f.metadata()?.len(),
            None => 0,
        };
        Ok(Some(Inner {
            segs,
            file,
            alloc,
            dirty: false,
            last,
            failed: None,
            last_sync: Instant::now(),
            cache: VecDeque::new(),
        }))
    }

    pub fn is_enabled(&self) -> bool {
        self.enabled.load(Ordering::Relaxed)
    }

    /// The newest commit recorded (queued commits not included).
    pub(crate) fn last(&self) -> Option<u64> {
        if self.readonly.is_some() {
            return self.status().last;
        }
        self.inner.lock().last
    }

    /// Hand queued commits to the background writer, or not (tests that look at the
    /// queue, or simulate a crash before anything was written).
    pub fn set_background(&self, on: bool) {
        self.background.store(on, Ordering::Relaxed);
    }

    /// Queue a commit (the commit path: no I/O).
    pub(crate) fn push(&self, p: Pending) {
        if self.readonly.is_some() || !self.is_enabled() {
            return;
        }
        self.pending.lock().push_back(p);
        if self.background.load(Ordering::Relaxed) && !self.scheduled.swap(true, Ordering::AcqRel) {
            worker().send(self.me.clone());
        }
    }

    /// Append every queued commit. With `sync`, the log is durable afterwards.
    pub(crate) fn flush(&self, sync: bool) -> Result<()> {
        if self.readonly.is_some() {
            // This mode cannot queue a record or own a writable file.
            return Ok(());
        }
        let mut inner = self.inner.lock();
        self.scheduled.store(false, Ordering::Release);
        loop {
            let batch: Vec<Pending> = self.pending.lock().drain(..).collect();
            if batch.is_empty() {
                break;
            }
            for p in batch {
                if inner.failed.is_some() {
                    continue;
                }
                if let Err(e) = self.append_pending(&mut inner, p) {
                    tracing::warn!(target: "sparkles::store::changelog", error = %e, "the change log stopped recording");
                    inner.failed = Some(e.to_string());
                }
            }
        }
        if sync || inner.last_sync.elapsed() >= SYNC_EVERY {
            self.sync_locked(&mut inner)?;
        }
        Ok(())
    }

    fn sync_locked(&self, inner: &mut Inner) -> Result<()> {
        if inner.dirty
            && let Some(f) = &inner.file
        {
            f.sync_data()?;
        }
        inner.dirty = false;
        inner.last_sync = Instant::now();
        Ok(())
    }

    /// Turn a queued commit into a record and append it.
    fn append_pending(&self, inner: &mut Inner, p: Pending) -> Result<()> {
        let commit = Arc::new(p.commit);
        let rec = match p.body {
            PendingBody::Log {
                generation,
                changes,
            } => {
                // net changes: a quad changed twice in one transaction cancels out
                let mut net: FxHashMap<[Id; 4], bool> = FxHashMap::default();
                for (op, q) in &changes {
                    match net.entry(*q) {
                        std::collections::hash_map::Entry::Occupied(e) => {
                            e.remove();
                        }
                        std::collections::hash_map::Entry::Vacant(e) => {
                            e.insert(*op == WAL_INSERT);
                        }
                    }
                }
                let mut keys = Keys::new(&generation);
                let mut out = Vec::with_capacity(net.len());
                for (q, add) in net {
                    out.push((keys.quad(&q)?, add));
                }
                Record::Changes(commit, sorted(out))
            }
            PendingBody::Keys(k) => Record::Changes(commit, sorted(k)),
            PendingBody::Summary => Record::Summary(commit),
            PendingBody::Gap { from } => Record::Gap {
                from,
                through: commit.seq,
            },
        };
        self.append(inner, rec)
    }

    fn append(&self, inner: &mut Inner, rec: Record) -> Result<()> {
        // records follow each other: a hole becomes a gap, a repeat is skipped
        if let Some(last) = inner.last {
            if rec.last() <= last {
                return Ok(());
            }
            if rec.first() > last + 1 {
                self.append(
                    inner,
                    Record::Gap {
                        from: last + 1,
                        through: rec.first() - 1,
                    },
                )?;
            }
        }
        let rec = match rec {
            // part of a gap may already be recorded
            Record::Gap { from, through } if inner.last.is_some_and(|l| from <= l) => Record::Gap {
                from: inner.last.unwrap() + 1,
                through,
            },
            r => r,
        };
        let bytes = encode(&rec);
        if inner.segs.last().is_none_or(|s| s.sealed) {
            self.start_segment(inner, rec.first())?;
        }
        let seg = inner.segs.last_mut().expect("started above");
        let offset = seg.bytes;
        match &inner.file {
            Some(f) => {
                // records overwrite zeros written ahead of them, so that a sync of
                // the file needs no journal commit (as the write-ahead log does)
                super::wal::write_commit(f, offset, &mut inner.alloc, &bytes, PREALLOC)?;
                inner.dirty = true;
            }
            None => seg.mem.extend_from_slice(&bytes),
        }
        seg.bytes += bytes.len() as u64;
        seg.last = rec.last();
        if let Some(ts) = rec.timestamp() {
            seg.last_ts = seg.last_ts.max(ts);
        }
        Arc::make_mut(seg.index.get_or_insert_with(Default::default)).note(&rec, offset);
        inner.last = Some(rec.last());
        if seg.bytes >= self.limits.segment_bytes {
            self.seal(inner)?;
            self.enforce_locked(inner, None)?;
        }
        Ok(())
    }

    fn start_segment(&self, inner: &mut Inner, first: u64) -> Result<()> {
        let header = seg_header(self.dataset_id);
        let mut seg = Segment {
            first,
            last: first.saturating_sub(1),
            last_ts: i64::MIN,
            bytes: SEG_HEADER as u64,
            path: None,
            mem: Vec::new(),
            sealed: false,
            index: Some(Arc::new(SegIndex::default())),
        };
        match &self.dir {
            Some(dir) => {
                std::fs::create_dir_all(dir)?;
                let path = dir.join(seg_name(first));
                let mut f = OpenOptions::new()
                    .create(true)
                    .truncate(true)
                    .write(true)
                    .open(&path)?;
                f.write_all(&header)?;
                f.sync_all()?;
                sync_dir(dir)?;
                if let Some(root) = &self.root {
                    sync_dir(root)?;
                }
                seg.path = Some(path);
                inner.alloc = SEG_HEADER as u64;
                inner.file = Some(f);
            }
            None => seg.mem.extend_from_slice(&header),
        }
        inner.segs.push(seg);
        Ok(())
    }

    /// Seal the open segment: its index is written next to it, and both are durable.
    fn seal(&self, inner: &mut Inner) -> Result<()> {
        let Some(seg) = inner.segs.last_mut().filter(|s| !s.sealed) else {
            return Ok(());
        };
        let mut ix = seg
            .index
            .take()
            .map(|a| Arc::try_unwrap(a).unwrap_or_else(|a| (*a).clone()))
            .unwrap_or_default();
        ix.seal();
        if let (Some(f), Some(idx)) = (&inner.file, seg.idx_path()) {
            // a sealed segment ends with its last record
            f.set_len(seg.bytes)?;
            f.sync_data()?;
            write_synced_atomic(&idx, &ix.encode(self.dataset_id, seg.first, seg.last))?;
            if let Some(dir) = &self.dir {
                sync_dir(dir)?;
            }
        }
        seg.sealed = true;
        let ix = Arc::new(ix);
        if seg.path.is_some() {
            // the sealed index is read from its file from now on (and cached)
            inner.cache.push_front((seg.first, ix));
            inner.cache.truncate(INDEX_CACHE);
        } else {
            seg.index = Some(ix);
        }
        inner.file = None;
        inner.dirty = false;
        Ok(())
    }

    /// The index of segment `i`.
    fn index(&self, inner: &mut Inner, i: usize) -> Result<Arc<SegIndex>> {
        self.index_with(inner, i, None)
    }

    fn index_with(
        &self,
        inner: &mut Inner,
        i: usize,
        mut check: Option<&mut dyn FnMut() -> Result<()>>,
    ) -> Result<Arc<SegIndex>> {
        if let Some(ix) = &inner.segs[i].index {
            return Ok(ix.clone());
        }
        let first = inner.segs[i].first;
        if let Some(p) = inner.cache.iter().position(|c| c.0 == first) {
            let c = inner.cache.remove(p).expect("found");
            let ix = c.1.clone();
            inner.cache.push_front(c);
            return Ok(ix);
        }
        let path = inner.segs[i]
            .idx_path()
            .expect("a sealed persistent segment");
        let b = match check.as_deref_mut() {
            Some(check) => Self::read_index_checked(&path, check)?,
            None => {
                #[cfg(test)]
                open_tests::observe(open_tests::Event::IndexReadAttempt);
                let bytes = std::fs::read(&path)?;
                #[cfg(test)]
                open_tests::observe(open_tests::Event::IndexReadChunk);
                bytes
            }
        };
        let (_, _, ix) = SegIndex::decode_with(&b, self.dataset_id, check)?
            .ok_or_else(|| Error::Corrupt(format!("{}: damaged index", path.display())))?;
        let ix = Arc::new(ix);
        inner.cache.push_front((first, ix.clone()));
        inner.cache.truncate(INDEX_CACHE);
        Ok(ix)
    }

    /// Read the entire sidecar, polling before each read and every interrupted retry.
    fn read_index_checked(path: &Path, check: &mut dyn FnMut() -> Result<()>) -> Result<Vec<u8>> {
        check()?;
        let mut file = File::open(path)?;
        let mut bytes = Vec::new();
        let mut chunk = [0; 64 << 10];
        loop {
            check()?;
            #[cfg(test)]
            open_tests::observe(open_tests::Event::IndexReadAttempt);
            match file.read(&mut chunk) {
                Ok(0) => return Ok(bytes),
                Ok(n) => {
                    bytes.try_reserve(n).map_err(|error| {
                        std::io::Error::new(std::io::ErrorKind::OutOfMemory, error)
                    })?;
                    bytes.extend_from_slice(&chunk[..n]);
                    #[cfg(test)]
                    open_tests::observe(open_tests::Event::IndexReadChunk);
                }
                Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {}
                Err(error) => return Err(error.into()),
            }
        }
    }

    /// Read the record of segment `i` at `offset`.
    fn read(&self, inner: &Inner, i: usize, offset: u64) -> Result<Record> {
        let seg = &inner.segs[i];
        let damaged = || {
            Error::Corrupt(format!(
                "change log segment {}: damaged record at byte {offset}",
                seg.first
            ))
        };
        if seg.path.is_none() {
            let (body, _) = frame(&seg.mem, offset as usize).ok_or_else(damaged)?;
            return decode(body).ok_or_else(damaged);
        }
        let path = seg.path.as_ref().expect("checked");
        let f = File::open(path)?;
        let mut h = [0u8; 8];
        read_exact_at(&f, &mut h, offset)?;
        let len = u32::from_le_bytes(h[0..4].try_into().unwrap()) as usize;
        let mut buf = vec![0u8; 8 + len];
        buf[..8].copy_from_slice(&h);
        read_exact_at(&f, &mut buf[8..], offset + 8)?;
        let (body, _) = frame(&buf, 0).ok_or_else(damaged)?;
        decode(body).ok_or_else(damaged)
    }

    /// Read the record of segment `i` at `offset` as [`ChangeLog::read`] does, admitting
    /// each allocation before it is made (item 829): the frame of a file-backed segment
    /// before it is read, then the decoded record from its [`footprint_checked`] before
    /// it is decoded. `admit` is told the bytes the record holds at each step, the frame
    /// included until decoding ends, and `check` is polled across reads, CRC chunks,
    /// admissions, footprint scanning and decoding.
    fn read_admitted(
        &self,
        inner: &Inner,
        i: usize,
        offset: u64,
        admit: &mut Admit<'_>,
        check: &mut dyn FnMut() -> Result<()>,
    ) -> Result<Record> {
        check()?;
        let seg = &inner.segs[i];
        let damaged = || {
            Error::Corrupt(format!(
                "change log segment {}: damaged record at byte {offset}",
                seg.first
            ))
        };
        let decode_admitted =
            |body: &[u8],
             frame: u64,
             admit: &mut Admit<'_>,
             check: &mut dyn FnMut() -> Result<()>| {
                let f = footprint_checked(body, check)?.ok_or_else(damaged)?;
                let held = f.meta + f.terms + f.changes;
                check()?;
                admit(frame + held)?;
                check()?;
                let r = decode_admitted_checked(body, check)?.ok_or_else(damaged)?;
                Ok::<_, Error>((r, held))
            };
        if seg.path.is_none() {
            let (body, _) = frame_checked(&seg.mem, offset as usize, check)?.ok_or_else(damaged)?;
            return Ok(decode_admitted(body, 0, admit, check)?.0);
        }
        let path = seg.path.as_ref().expect("checked");
        let f = File::open(path)?;
        let mut h = [0u8; 8];
        readonly::read_exact_at_checked(&f, &mut h, offset, check)?;
        let len = u32::from_le_bytes(h[0..4].try_into().unwrap()) as usize;
        let expected_crc = u32::from_le_bytes(h[4..8].try_into().unwrap());
        if len == 0 {
            return Err(damaged());
        }
        check()?;
        admit(8 + len as u64)?;
        check()?;
        let mut body = Vec::new();
        body.try_reserve_exact(len).map_err(|_| {
            Error::HistoryUnsupported("history-log record allocation was refused".into())
        })?;
        let mut crc = flate2::Crc::new();
        while body.len() < len {
            check()?;
            let start = body.len();
            let end = start.saturating_add(64 << 10).min(len);
            body.resize(end, 0);
            let read_offset = offset
                .checked_add(8)
                .and_then(|n| n.checked_add(start as u64))
                .ok_or_else(|| Error::Corrupt("history-log offset overflow".into()))?;
            readonly::read_exact_at_checked(&f, &mut body[start..end], read_offset, check)?;
            check()?;
            crc.update(&body[start..end]);
        }
        check()?;
        if crc.sum() != expected_crc {
            return Err(damaged());
        }
        let (r, held) = decode_admitted(&body, 8 + body.len() as u64, admit, check)?;
        drop(body);
        admit(held)?;
        check()?;
        Ok(r)
    }

    /// Drop sealed segments that retention no longer keeps. `now_ms` applies the age
    /// limit (none without it).
    pub(crate) fn enforce(&self, now_ms: Option<i64>) -> Result<usize> {
        self.require_writable()?;
        let mut inner = self.inner.lock();
        self.enforce_locked(&mut inner, now_ms)
    }

    fn enforce_locked(&self, inner: &mut Inner, now_ms: Option<i64>) -> Result<usize> {
        let s = *self.settings.lock();
        let max = s.max_bytes.unwrap_or(self.limits.default_max_bytes);
        let head = inner.last.unwrap_or(0);
        let mut total: u64 = inner.segs.iter().map(|s| s.bytes).sum();
        let mut dropped = 0;
        // a history reader's pin keeps its commits, past the size limit too
        let pinned = self.pins.lock().first_key_value().map(|(c, _)| *c);
        while inner.segs.len() > 1 && inner.segs[0].sealed {
            let seg = &inner.segs[0];
            if pinned.is_some_and(|c| seg.last >= c) {
                break;
            }
            // a segment some limit keeps stays, unless the size limit needs the room
            let kept_by_count = s
                .keep_commits
                .is_some_and(|n| seg.last.saturating_add(n) > head);
            let kept_by_age = match (s.keep_age_ms, now_ms) {
                (Some(age), Some(now)) => {
                    seg.last_ts >= now.saturating_sub(age.min(i64::MAX as u64) as i64)
                }
                (Some(_), None) => true,
                _ => false,
            };
            let over = max > 0 && total > max;
            let windowed = s.keep_commits.is_some() || s.keep_age_ms.is_some();
            let drop = over || (windowed && !kept_by_count && !kept_by_age);
            if !drop {
                break;
            }
            let seg = inner.segs.remove(0);
            total -= seg.bytes;
            inner.cache.retain(|c| c.0 != seg.first);
            if let Some(p) = &seg.path {
                // the log first: an index without its log is removed at the next open
                std::fs::remove_file(p)?;
                let _ = std::fs::remove_file(p.with_extension("idx"));
            }
            dropped += 1;
        }
        if dropped > 0
            && let Some(dir) = &self.dir
        {
            sync_dir(dir)?;
        }
        Ok(dropped)
    }

    fn remove_all(&self) -> Result<()> {
        self.require_writable()?;
        self.pending.lock().clear();
        let mut inner = self.inner.lock();
        *inner = Inner::empty();
        if let Some(dir) = &self.dir {
            match std::fs::remove_dir_all(dir) {
                Ok(()) => {}
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => return Err(e.into()),
            }
            if let Some(root) = &self.root {
                sync_dir(root)?;
            }
        }
        Ok(())
    }

    pub fn settings(&self) -> ChangeLogSettings {
        *self.settings.lock()
    }

    /// Change the settings: turning the log off removes it, and turning it on starts it
    /// at the next commit (`head` is the current one).
    pub(crate) fn set_settings(&self, s: ChangeLogSettings, head: u64) -> Result<()> {
        self.require_writable()?;
        let on = s.enabled.unwrap_or(self.limits.default_on);
        // Turning the log off waits for no reader: it is refused while a history
        // reader pins the log, before the settings are written. The pins stay locked
        // until the log is off, so a reader pinning later sees it off.
        let pins = (!on).then(|| self.pins.lock());
        if pins.as_ref().is_some_and(|p| !p.is_empty()) {
            return Err(Error::Conflict(
                "the change log cannot be turned off while a history query reads it".into(),
            ));
        }
        if let Some(root) = &self.root {
            write_settings(root, &s)?;
        }
        let was = self.enabled.swap(on, Ordering::AcqRel);
        *self.settings.lock() = s;
        drop(pins);
        if was && !on {
            self.remove_all()?;
        } else if !was && on {
            // the commits up to now were not recorded
            let mut inner = self.inner.lock();
            if head > 0 {
                self.append(
                    &mut inner,
                    Record::Gap {
                        from: 1,
                        through: head,
                    },
                )?;
            }
        }
        if on {
            self.enforce(None)?;
        }
        Ok(())
    }

    pub fn status(&self) -> ChangeLogStatus {
        if let Some(reader) = &self.readonly {
            let settings = *self.settings.lock();
            let (first, last, error) = match reader.coverage() {
                Ok((first, last)) => (first, last, None),
                Err(error) => (None, None, Some(error.to_string())),
            };
            return ChangeLogStatus {
                enabled: self.is_enabled(),
                first,
                last,
                segments: reader.segments.len(),
                bytes: self.bytes(),
                pending: 0,
                settings,
                max_bytes: settings.max_bytes.unwrap_or(self.limits.default_max_bytes),
                error,
            };
        }
        let pending = self.pending.lock().len();
        let inner = self.inner.lock();
        let s = *self.settings.lock();
        ChangeLogStatus {
            enabled: self.is_enabled(),
            first: inner.segs.first().map(|s| s.first),
            last: inner.last,
            segments: inner.segs.len(),
            bytes: inner.segs.iter().map(|s| s.bytes).sum(),
            pending,
            settings: s,
            max_bytes: s.max_bytes.unwrap_or(self.limits.default_max_bytes),
            error: inner.failed.clone(),
        }
    }

    /// The newest recorded commit at or before `ms`, with cooperative checks.
    pub(crate) fn commit_at_time_checked(
        &self,
        ms: i64,
        check: &mut dyn FnMut() -> Result<()>,
    ) -> Result<Option<u64>> {
        if let Some(reader) = &self.readonly {
            return reader.commit_at_time(ms, check);
        }
        check()?;
        let mut inner = self.inner.lock();
        let mut best = None;
        for i in 0..inner.segs.len() {
            check()?;
            #[cfg(test)]
            open_tests::observe(open_tests::Event::TimestampIndex);
            let ix = self.index_with(&mut inner, i, Some(check))?;
            let n = ix.recs.partition_point(|r| r.ts <= ms);
            // a gap tells no time of its own
            for (idx, r) in ix.recs[..n].iter().rev().enumerate() {
                if idx != 0 && idx.is_multiple_of(CHECK_STRIDE) {
                    check()?;
                }
                #[cfg(test)]
                open_tests::observe(open_tests::Event::TimestampRecord);
                if !matches!(r.hole, Some((_, UnrecordedReason::Gap))) {
                    best = Some(r.seq);
                    break;
                }
            }
            #[cfg(test)]
            open_tests::observe(open_tests::Event::TimestampIndexed);
            if n < ix.recs.len() {
                break;
            }
        }
        check()?;
        Ok(best)
    }

    /// Read the changes of commits `from` through `to` that match `filter`, in commit
    /// order (newest first with `descending`), calling `visit` for each until it
    /// returns `false`. Returns the commits in the range whose changes are not
    /// recorded, and without `holes_after_stop` only those found before the scan stops.
    /// With `paged`, each record is read by [`ChangeLog::read_admitted`] and released
    /// (an admission of 0) once its changes are visited, and the scan stops where the
    /// page is full, before the index of a segment is loaded and before a record is
    /// read; a read-only log is not read so.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn scan(
        &self,
        from: u64,
        to: u64,
        filter: &LogFilter,
        descending: bool,
        holes_after_stop: bool,
        no_wait: bool,
        check: &mut dyn FnMut() -> Result<()>,
        mut paged: Option<&mut PageHooks<'_, '_>>,
        visit: &mut Visit<'_>,
    ) -> Result<Vec<Unrecorded>> {
        if let Some(reader) = &self.readonly {
            return reader.scan(from, to, filter, descending, check, visit);
        }
        let mut inner = if no_wait {
            self.inner.try_lock().ok_or(Error::WriterBusy)?
        } else {
            self.inner.lock()
        };
        let mut holes: Vec<Unrecorded> = Vec::new();
        if from > to {
            return Ok(holes);
        }
        let Some(newest) = inner.last else {
            holes.push(Unrecorded {
                from,
                to,
                reason: UnrecordedReason::BeforeLog,
            });
            return Ok(holes);
        };
        let oldest = inner.segs.first().map_or(newest + 1, |s| s.first);
        if from < oldest {
            holes.push(Unrecorded {
                from,
                to: to.min(oldest - 1),
                reason: UnrecordedReason::BeforeLog,
            });
        }
        if to > newest {
            holes.push(Unrecorded {
                from: from.max(newest + 1),
                to,
                reason: UnrecordedReason::Gap,
            });
        }
        let hashes = filter.hashes();
        let n = inner.segs.len();
        let order: Vec<usize> = if descending {
            (0..n).rev().collect()
        } else {
            (0..n).collect()
        };
        let mut stop = false;
        'segs: for i in order {
            if no_wait || paged.is_some() {
                check()?;
            }
            let (sf, sl) = (inner.segs[i].first, inner.segs[i].last);
            if sl < from || sf > to {
                continue;
            }
            // holes between segments (damage) are reported as gaps
            if i > 0 && inner.segs[i - 1].last + 1 < sf {
                let (a, b) = (inner.segs[i - 1].last + 1, sf - 1);
                if a <= to && b >= from {
                    holes.push(Unrecorded {
                        from: a.max(from),
                        to: b.min(to),
                        reason: UnrecordedReason::Gap,
                    });
                }
            }
            // a full page stops before it loads the next segment's index; a commit is
            // one record, so the segment holds no part of the commit read last
            if !stop
                && let Some(p) = paged.as_deref_mut()
                && (p.full)(if descending { sl.min(to) } else { sf.max(from) })
            {
                stop = true;
            }
            if stop && !holes_after_stop {
                break;
            }
            let ix = self.index(&mut inner, i)?;
            let lo = ix.recs.partition_point(|r| r.seq < from);
            let hi = ix.recs.partition_point(|r| r.seq <= to);
            // A gap can begin in the requested window but end after it. That
            // first overhanging record has no candidate changes, yet its unknown
            // range still intersects the query and must not look complete.
            let hole_hi = hi
                + usize::from(
                    ix.recs
                        .get(hi)
                        .is_some_and(|record| record.hole.is_some_and(|(first, _)| first <= to)),
                );
            for r in &ix.recs[lo..hole_hi] {
                if let Some((f, reason)) = r.hole {
                    holes.push(Unrecorded {
                        from: f.max(from),
                        to: r.seq.min(to),
                        reason,
                    });
                }
            }
            if stop {
                continue;
            }
            // the records that may match
            let cands: Vec<u32> = match &hashes {
                None => (lo as u32..hi as u32).collect(),
                Some(roles) => {
                    let mut acc: Option<Vec<u32>> = None;
                    for hs in roles {
                        let mut v = Vec::new();
                        for h in hs {
                            ix.lookup(*h, &mut v);
                        }
                        v.retain(|&o| (lo as u32..hi as u32).contains(&o));
                        v.sort_unstable();
                        v.dedup();
                        acc = Some(match acc {
                            None => v,
                            Some(a) => a
                                .into_iter()
                                .filter(|o| v.binary_search(o).is_ok())
                                .collect(),
                        });
                    }
                    acc.unwrap_or_default()
                }
            };
            let iter: Box<dyn Iterator<Item = &u32>> = if descending {
                Box::new(cands.iter().rev())
            } else {
                Box::new(cands.iter())
            };
            for &o in iter {
                check()?;
                let e = ix.recs[o as usize];
                if e.hole.is_some() {
                    continue;
                }
                if let Some(p) = paged.as_deref_mut()
                    && (p.full)(e.seq)
                {
                    stop = true;
                    break;
                }
                let record = match paged.as_deref_mut() {
                    Some(p) => self.read_admitted(&inner, i, e.offset, p.admit, check)?,
                    None => self.read(&inner, i, e.offset)?,
                };
                if let Record::Changes(c, changes) = &record {
                    let is_paged = paged.is_some();
                    for (idx, (add, k)) in changes.iter().enumerate() {
                        if (no_wait || is_paged) && idx.is_multiple_of(CHECK_STRIDE) {
                            check()?;
                        }
                        #[cfg(test)]
                        if is_paged {
                            settings_format_tests::observe_paged_filter();
                        }
                        if filter.matches(k) && !visit(c, *add, k)? {
                            stop = true;
                            break;
                        }
                    }
                }
                drop(record);
                if let Some(p) = paged.as_deref_mut() {
                    (p.admit)(0)?;
                    check()?;
                }
                if stop {
                    if descending {
                        break 'segs;
                    }
                    break;
                }
            }
            if stop && !holes_after_stop {
                break;
            }
        }
        holes.sort_by_key(|h| h.from);
        // a hole reported twice (a gap past the end and a gap record) is merged
        holes.dedup_by(|b, a| {
            if a.reason == b.reason && b.from <= a.to + 1 {
                a.to = a.to.max(b.to);
                true
            } else {
                false
            }
        });
        Ok(holes)
    }

    /// Read the records of commits `from` through `to` in commit order, calling `visit`
    /// for each until it returns `false`. The first commit the log has no record of is
    /// visited as [`LogCommit::Missing`], and ends the read. The caller must not touch
    /// the log from `visit`, which runs under its lock.
    pub(crate) fn commits(
        &self,
        from: u64,
        to: u64,
        check: &mut dyn FnMut() -> Result<()>,
        visit: &mut dyn FnMut(LogCommit) -> Result<bool>,
    ) -> Result<()> {
        if let Some(reader) = &self.readonly {
            return reader.commits(from, to, check, visit);
        }
        if from > to {
            return Ok(());
        }
        let mut inner = self.inner.lock();
        let mut next = from;
        for i in 0..inner.segs.len() {
            let (sf, sl) = (inner.segs[i].first, inner.segs[i].last);
            if sl < next {
                continue;
            }
            if sf > next {
                // older than the log, or a hole between segments
                visit(LogCommit::Missing)?;
                return Ok(());
            }
            let ix = self.index(&mut inner, i)?;
            let lo = ix.recs.partition_point(|r| r.seq < next);
            let hi = ix.recs.partition_point(|r| r.seq <= to);
            for e in &ix.recs[lo..hi] {
                check()?;
                let item = match e.hole {
                    Some((_, UnrecordedReason::Gap)) => LogCommit::Missing,
                    _ => match self.read(&inner, i, e.offset)? {
                        Record::Changes(c, changes) => LogCommit::Changes(c, changes),
                        Record::Summary(c) => LogCommit::Summary(c),
                        Record::Gap { .. } => LogCommit::Missing,
                    },
                };
                next = e.seq + 1;
                if !visit(item)? {
                    return Ok(());
                }
            }
            if next > to {
                return Ok(());
            }
        }
        if next <= to {
            visit(LogCommit::Missing)?;
        }
        Ok(())
    }

    /// The bytes on disk (or in memory).
    pub fn bytes(&self) -> u64 {
        if let Some(reader) = &self.readonly {
            return reader.segments.iter().map(|s| s.extent).sum();
        }
        self.inner.lock().segs.iter().map(|s| s.bytes).sum()
    }
}

impl Inner {
    fn empty() -> Inner {
        Inner {
            segs: Vec::new(),
            file: None,
            alloc: 0,
            dirty: false,
            last: None,
            failed: None,
            last_sync: Instant::now(),
            cache: VecDeque::new(),
        }
    }
}

/// Order net changes as records hold them: removals first, then additions, each by
/// graph, subject, predicate and object.
fn sorted(mut v: Vec<(QuadKey, bool)>) -> Vec<(bool, QuadKey)> {
    v.sort_unstable_by(|a, b| (a.1, &a.0).cmp(&(b.1, &b.0)));
    v.into_iter().map(|(k, add)| (add, k)).collect()
}

/// Write a file through a temporary one, durably.
fn write_synced_atomic(path: &Path, bytes: &[u8]) -> Result<()> {
    Ok(crate::fsutil::write_atomic_via(
        path,
        &path.with_extension("idx.tmp"),
        bytes,
    )?)
}

#[cfg(unix)]
fn read_exact_at(f: &File, buf: &mut [u8], at: u64) -> std::io::Result<()> {
    std::os::unix::fs::FileExt::read_exact_at(f, buf, at)
}

#[cfg(windows)]
fn read_exact_at(f: &File, mut buf: &mut [u8], mut at: u64) -> std::io::Result<()> {
    while !buf.is_empty() {
        let n = std::os::windows::fs::FileExt::seek_read(f, buf, at)?;
        if n == 0 {
            return Err(std::io::ErrorKind::UnexpectedEof.into());
        }
        buf = &mut buf[n..];
        at += n as u64;
    }
    Ok(())
}

/// What a scan calls for each change: its commit, `true` for an addition, and its quad.
/// Returning `false` stops the scan.
pub(crate) type Visit<'a> = dyn FnMut(&Arc<ChangeCommit>, bool, &QuadKey) -> Result<bool> + 'a;
/// Told the bytes a record being read holds now, before each allocation (item 829).
pub(crate) type Admit<'a> = dyn FnMut(u64) -> Result<()> + 'a;

/// What a paged history read gives [`ChangeLog::scan`] (item 829).
pub(crate) struct PageHooks<'a, 'b> {
    /// told the bytes a record being read holds now, before each allocation
    pub(crate) admit: &'a mut Admit<'b>,
    /// whether the page is full before a record of commit `seq` is read, or before a
    /// segment whose next commit is `seq` is opened: the scan stops there
    pub(crate) full: &'a mut dyn FnMut(u64) -> bool,
}

/// Which changes a scan returns: for each of graph, subject, predicate and object, the
/// keys it may have (`None`: any).
#[derive(Clone, Debug, Default)]
pub(crate) struct LogFilter {
    pub keys: [Option<Vec<Vec<u8>>>; 4],
}

impl LogFilter {
    /// The term hashes to look up, per constrained role (`None`: no constraint).
    fn hashes(&self) -> Option<Vec<Vec<u64>>> {
        let v: Vec<Vec<u64>> = ROLES
            .iter()
            .zip(&self.keys)
            .filter_map(|(role, ks)| {
                ks.as_ref()
                    .map(|ks| ks.iter().map(|k| term_hash(*role, k)).collect())
            })
            .collect();
        (!v.is_empty()).then_some(v)
    }

    fn matches(&self, k: &QuadKey) -> bool {
        self.keys.iter().zip(k).all(|(ks, t)| {
            ks.as_ref()
                .is_none_or(|ks| ks.iter().any(|x| x[..] == t[..]))
        })
    }
}

// ------------------------------------------------------- the background writer ------

struct Worker {
    tx: Mutex<std::sync::mpsc::Sender<Weak<ChangeLog>>>,
}

impl Worker {
    fn send(&self, w: Weak<ChangeLog>) {
        let _ = self.tx.lock().send(w);
    }
}

/// One thread appends the queued commits of every store's log.
fn worker() -> &'static Worker {
    static W: std::sync::OnceLock<Worker> = std::sync::OnceLock::new();
    W.get_or_init(|| {
        let (tx, rx) = std::sync::mpsc::channel::<Weak<ChangeLog>>();
        std::thread::Builder::new()
            .name("sparkles-changelog".into())
            .spawn(move || {
                crate::threads::started();
                while let Ok(first) = rx.recv() {
                    std::thread::sleep(DEBOUNCE);
                    let mut batch = vec![first];
                    while let Ok(w) = rx.try_recv() {
                        batch.push(w);
                    }
                    for w in batch {
                        if let Some(log) = w.upgrade()
                            && let Err(e) = log.flush(false)
                        {
                            tracing::warn!(target: "sparkles::store::changelog", error = %e, "change log write failed");
                        }
                    }
                }
            })
            .expect("spawn the change log writer");
        Worker { tx: Mutex::new(tx) }
    })
}

// --------------------------------------------------------------- the store side ------

impl Store {
    /// The store's change log.
    pub fn change_log(&self) -> Option<&Arc<ChangeLog>> {
        self.changelog.as_ref()
    }

    /// The limits of a store's change log, from its options.
    pub(crate) fn log_limits(opts: &StoreOptions) -> LogLimits {
        LogLimits {
            segment_bytes: opts.change_log_segment_bytes.max(4096),
            default_max_bytes: opts.change_log_max_bytes,
            default_on: opts.change_log,
            bulk_max_quads: opts.change_log_bulk_max_quads,
        }
    }

    /// Record what the open found: the commits after the last recorded one, read from
    /// the write-ahead logs that still have them. A commit no log has is a gap.
    pub(crate) fn recover_change_log(&self) -> Result<()> {
        let Some(log) = &self.changelog else {
            return Ok(());
        };
        if !log.is_enabled() || self.history.is_none() {
            return Ok(());
        }
        let head = self.head_commit().seq;
        let recorded = match log.last() {
            Some(l) if l > head => {
                // records past the head belong to a state this dataset no longer has
                // (files copied from elsewhere): start over
                tracing::warn!(
                    target: "sparkles::store::changelog",
                    last = l,
                    head,
                    "the change log is ahead of the data; it starts again"
                );
                log.remove_all()?;
                None
            }
            l => l,
        };
        let last = match recorded {
            Some(l) => l,
            None => {
                // a new log: it starts where the current generation's log does
                let base = {
                    let live = self.snapshot();
                    let current = commit::generation_number(&live.generation.name);
                    let h = self.history.as_ref().expect("persistent").lock();
                    h.gens.get(&current).map_or(head, |g| g.base.seq)
                };
                if base > 0 {
                    let head = self.head_commit();
                    log.push(Pending {
                        commit: ChangeCommit {
                            seq: base,
                            ..ChangeCommit::of(&head, None, None)
                        },
                        body: PendingBody::Gap { from: 1 },
                    });
                }
                base
            }
        };
        if last >= head {
            return log.flush(true);
        }
        let t0 = Instant::now();
        let mut n = 0u64;
        for step in self.diff_plan(last, head)? {
            match step {
                super::diff::Step::Log {
                    generation,
                    after,
                    through,
                } => {
                    let (gen_, mut cursor) = self.open_log(generation, after)?;
                    while let Some((seq, txn)) = cursor.next()? {
                        if seq > after && seq <= through {
                            let changes: Vec<(u8, [Id; 4])> = txn
                                .as_chunks::<WAL_REC>()
                                .0
                                .iter()
                                .filter(|record| wal_is_quad(record[0]))
                                .map(|d| (d[0], super::wal::record_quad(d)))
                                .collect();
                            log.push(Pending {
                                commit: self.change_commit(seq),
                                body: PendingBody::Log {
                                    generation: gen_.clone(),
                                    changes,
                                },
                            });
                            n += 1;
                        }
                        if seq >= through {
                            break;
                        }
                    }
                }
                super::diff::Step::Compare { a, b } => {
                    // a bulk commit (or a collected generation): compare when both
                    // states are still readable and small enough
                    let bulk = b == a + 1 && self.commit(b).is_some_and(|c| c.bulk);
                    let body = if bulk {
                        self.bulk_change_keys(a, b, log.limits.bulk_max_quads)
                            .map_or(PendingBody::Summary, PendingBody::Keys)
                    } else {
                        PendingBody::Gap { from: a + 1 }
                    };
                    log.push(Pending {
                        commit: self.change_commit(b),
                        body,
                    });
                }
            }
        }
        log.flush(true)?;
        tracing::info!(
            target: "sparkles::store::changelog",
            commits = n,
            ms = t0.elapsed().as_secs_f64() * 1e3,
            "recovered the change log from the write-ahead log"
        );
        Ok(())
    }

    /// A commit as the change log records it, from the catalog and its annotation.
    fn change_commit(&self, seq: u64) -> ChangeCommit {
        let c = self.commit(seq).unwrap_or(CommitInfo {
            seq,
            timestamp_ms: 0,
            kind: CommitKind::Unknown,
            inserted: 0,
            deleted: 0,
            quads: 0,
            generation: 0,
            bulk: false,
            exact: false,
            reconstructed: true,
            default_graph: true,
            unvalidated: false,
        });
        let message = self.annotation(seq).and_then(|a| a.message);
        ChangeCommit::of(&c, None, message)
    }

    /// The net changes from readable commit `a` to `b` as keys, when the two states are
    /// small enough to compare (`None` otherwise, or when either is gone).
    fn bulk_change_keys(&self, a: u64, b: u64, max: u64) -> Option<Vec<(QuadKey, bool)>> {
        let o = crate::history::HistoryOptions::default();
        let (sa, _) = self.snapshot_at(&crate::history::At::Commit(a), &o).ok()?;
        let (sb, _) = self.snapshot_at(&crate::history::At::Commit(b), &o).ok()?;
        state_change_keys(&sa, &sb, max)
    }

    /// The most quads a bulk build is to collect for the change log (`None` when the log
    /// is off).
    pub(super) fn bulk_change_cap(&self) -> Option<u64> {
        let log = self.changelog.as_ref().filter(|l| l.is_enabled())?;
        Some(log.limits.bulk_max_quads)
    }

    /// The change log's record of bulk commit `c`, from state `before` to `after`, when
    /// the log is on (writer lock held, before the commit is published). `candidates` are
    /// the quads that the build found can have changed, for a commit into a dataset too
    /// large to compare.
    pub(super) fn bulk_change(
        &self,
        c: &CommitInfo,
        message: Option<Arc<str>>,
        author: Option<Arc<str>>,
        before: &Snapshot,
        after: &Snapshot,
        candidates: Option<BulkCandidates<'_>>,
    ) -> Option<Pending> {
        let log = self.changelog.as_ref().filter(|l| l.is_enabled())?;
        let max = log.limits.bulk_max_quads;
        let body = state_change_keys(before, after, max)
            .or_else(|| candidates.and_then(|k| candidate_changes(before, after, k, max)))
            .map_or(PendingBody::Summary, PendingBody::Keys);
        Some(Pending {
            commit: ChangeCommit::of(c, author, message),
            body,
        })
    }

    /// Write every queued commit to the change log and sync it (the background writer
    /// does this on its own shortly after each commit).
    pub fn flush_change_log(&self) -> Result<()> {
        self.sync_change_log()
    }

    /// Append what is queued and sync, before the generation whose write-ahead log holds
    /// the queued changes may go.
    pub(super) fn sync_change_log(&self) -> Result<()> {
        match &self.changelog {
            Some(log) if log.is_enabled() => log.flush(true),
            _ => Ok(()),
        }
    }

    /// The dataset's change log settings.
    pub fn change_log_settings(&self) -> ChangeLogSettings {
        self.changelog
            .as_ref()
            .map(|l| l.settings())
            .unwrap_or_default()
    }

    /// Change the dataset's change log settings (see [`ChangeLogSettings`]).
    pub fn set_change_log_settings(&self, s: ChangeLogSettings) -> Result<ChangeLogStatus> {
        let Some(log) = &self.changelog else {
            return Err(Error::HistoryUnsupported(
                "this store has no change log".into(),
            ));
        };
        if s.max_bytes.is_some_and(|b| b > 0 && b < 4096) {
            return Err(Error::Invalid(
                "changeLog.maxBytes must be 0 (unlimited) or at least 4096".into(),
            ));
        }
        // the writer lock keeps commits out while the log starts or stops
        let w = self.guarded_writer();
        log.flush(false)?;
        log.set_settings(s, w.head.seq)?;
        drop(w);
        self.quota.invalidate();
        Ok(log.status())
    }

    /// The change log's state (`None` for a store without one).
    pub fn change_log_status(&self) -> Option<ChangeLogStatus> {
        self.changelog.as_ref().map(|l| l.status())
    }
}

/// The net changes from state `a` to state `b` as keys, if both together hold at most
/// `max` quads, or `a` is empty and `b` holds at most `max` (`None` otherwise).
fn state_change_keys(a: &Snapshot, b: &Snapshot, max: u64) -> Option<Vec<(QuadKey, bool)>> {
    if a.is_empty() {
        if b.len() > max {
            return None;
        }
        let mut keys = Keys::new(&b.generation);
        let mut out = Vec::with_capacity(b.len() as usize);
        let mut failed = false;
        b.for_each_quad(|q| {
            match keys.quad(q) {
                Ok(k) => out.push((k, true)),
                Err(_) => failed = true,
            }
            Ok(())
        })
        .ok()?;
        return (!failed).then_some(out);
    }
    if a.len().saturating_add(b.len()) > max {
        return None;
    }
    let o = DiffOptions::default();
    let changes = super::diff::compare_changes(a, b, &o).ok()?;
    Some(
        changes
            .into_iter()
            .map(|(op, q)| (quad_key(&q), op == DiffOp::Add))
            .collect(),
    )
}

/// The quads that a bulk build found can have changed. Every change of the commit is one
/// of them, and the log checks each against both states.
pub(super) struct BulkCandidates<'a> {
    /// the quads of the build's sources, `None` when there were more than the limit
    pub sources: Option<Vec<QuadKey>>,
    /// the quads of the graphs the build dropped, `None` when there were more than the limit
    pub dropped: Option<Vec<[Id; 4]>>,
    /// the snapshot whose ids `dropped`, `log` and `bulk` use
    pub view: &'a Snapshot,
    /// the changes of the transaction before its bulk batch, if it had one
    pub log: &'a [(u8, [Id; 4])],
    /// the quads of the transaction's bulk batch
    pub bulk: &'a [[Id; 4]],
}

/// The net changes from `before` to `after` of a bulk commit whose changed quads are among
/// `k`, if there are at most `max` of those (`None` otherwise).
fn candidate_changes(
    before: &Snapshot,
    after: &Snapshot,
    k: BulkCandidates<'_>,
    max: u64,
) -> Option<Vec<(QuadKey, bool)>> {
    let mut quads = k.sources?;
    let dropped = k.dropped?;
    let n = quads.len() as u64 + dropped.len() as u64 + k.log.len() as u64 + k.bulk.len() as u64;
    if n > max {
        return None;
    }
    let mut keys = Keys::new(&k.view.generation);
    for q in dropped
        .iter()
        .chain(k.bulk)
        .chain(k.log.iter().map(|(_, q)| q))
    {
        quads.push(keys.quad(q).ok()?);
    }
    quads.sort_unstable();
    quads.dedup();
    // a term missing from a state's dictionary means the quad is absent there; a read
    // error means nothing is known, and the commit becomes a summary
    let holds = |s: &Snapshot, q: &QuadKey| -> Option<bool> {
        let ids: Option<Vec<Id>> = [1, 2, 3, 0]
            .iter()
            .map(|&i| super::diff::key_id(s, &q[i]))
            .collect();
        match ids {
            None => Some(false),
            Some(i) => s.contains(&[i[0], i[1], i[2], i[3]]).ok(),
        }
    };
    let mut out = Vec::new();
    for q in quads {
        match (holds(before, &q)?, holds(after, &q)?) {
            (false, true) => out.push((q, true)),
            (true, false) => out.push((q, false)),
            _ => {}
        }
    }
    Some(out)
}

/// The key of a quad (the default graph's is empty).
pub(crate) fn quad_key(q: &Quad) -> QuadKey {
    let g: Arc<[u8]> = match &q.graph_name {
        GraphName::DefaultGraph => Arc::from(&[][..]),
        GraphName::NamedNode(n) => Arc::from(id::iri_key(n.as_str())),
        GraphName::BlankNode(b) => Arc::from(id::term_key(&Term::BlankNode(b.clone()))),
    };
    let s: Term = q.subject.clone().into();
    [
        g,
        Arc::from(id::term_key(&s)),
        Arc::from(id::iri_key(q.predicate.as_str())),
        Arc::from(id::term_key(&q.object)),
    ]
}

#[cfg(test)]
mod settings_format_tests {
    use super::*;
    use std::cell::Cell;

    thread_local! {
        static PAGED_FILTER_EVALUATIONS: Cell<Option<usize>> = const { Cell::new(None) };
    }

    pub(super) fn observe_paged_filter() {
        PAGED_FILTER_EVALUATIONS.with(|count| {
            if let Some(n) = count.get() {
                count.set(Some(n + 1));
            }
        });
    }

    struct CountPagedFilters;

    impl CountPagedFilters {
        fn new() -> Self {
            PAGED_FILTER_EVALUATIONS.with(|count| assert!(count.replace(Some(0)).is_none()));
            Self
        }

        fn get(&self) -> usize {
            PAGED_FILTER_EVALUATIONS.with(|count| count.get().unwrap())
        }
    }

    impl Drop for CountPagedFilters {
        fn drop(&mut self) {
            PAGED_FILTER_EVALUATIONS.with(|count| count.set(None));
        }
    }

    #[test]
    fn a_newer_change_log_file_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let newer = crate::formats::settings_file(CHANGE_LOG_FILE)
            .unwrap()
            .writes
            + 1;
        std::fs::write(
            dir.path().join(CHANGE_LOG_FILE),
            format!(r#"{{"format": {newer}, "keepCommits": 5}}"#),
        )
        .unwrap();
        assert!(matches!(
            read_settings(dir.path()),
            Err(Error::Unsupported(_))
        ));
        std::fs::write(dir.path().join(CHANGE_LOG_FILE), r#"{"keepCommits": 5}"#).unwrap();
        assert_eq!(read_settings(dir.path()).unwrap().keep_commits, Some(5));
    }

    fn test_commit(seq: u64, author: Option<String>, message: Option<String>) -> Arc<ChangeCommit> {
        Arc::new(ChangeCommit {
            seq,
            timestamp_ms: 1_700_000_000_000,
            kind: CommitKind::Update,
            bulk: false,
            inserted: 1,
            deleted: 0,
            author: author.map(Arc::from),
            message: message.map(Arc::from),
        })
    }

    /// Checked footprint, CRC, decode and filtered-scan helpers poll within the 1,024-entry
    /// and 64 KiB bounds and propagate typed stop errors instead of `Corrupt`.
    #[test]
    fn admitted_history_decode_checks_inside_large_records() {
        let g: Arc<[u8]> = Arc::from(&[][..]);
        let s: Arc<[u8]> = Arc::from(id::iri_key("urn:s"));
        let p: Arc<[u8]> = Arc::from(id::iri_key("urn:p"));

        // 1. Large single term (256 KiB): CRC and decode both poll in <= 64 KiB chunks.
        let big_term: Arc<[u8]> = Arc::from(id::term_key(&Term::Literal(
            oxrdf::Literal::new_simple_literal("t".repeat(256 << 10)),
        )));
        let big_frame = encode(&Record::Changes(
            test_commit(1, None, None),
            vec![(true, [g.clone(), s.clone(), p.clone(), big_term])],
        ));
        let mut crc_polls = 0usize;
        let (big_body, _) = frame_checked(&big_frame, 0, &mut || {
            crc_polls += 1;
            Ok(())
        })
        .unwrap()
        .unwrap();
        assert!(crc_polls >= 6, "expected >= 6 CRC polls, got {crc_polls}");
        let mut stop_at = 0usize;
        let err = frame_checked(&big_frame, 0, &mut || {
            stop_at += 1;
            if stop_at == 3 {
                Err(Error::Cancelled)
            } else {
                Ok(())
            }
        });
        assert!(matches!(err, Err(Error::Cancelled)));

        let mut decode_polls = 0usize;
        assert!(
            decode_admitted_checked(big_body, &mut || {
                decode_polls += 1;
                Ok(())
            })
            .unwrap()
            .is_some()
        );
        assert!(
            decode_polls >= 6,
            "expected >= 6 decode polls for 256 KiB term, got {decode_polls}"
        );
        for fail_on in [2usize, 4] {
            let mut n = 0usize;
            let res = decode_admitted_checked(big_body, &mut || {
                n += 1;
                if n == fail_on {
                    Err(Error::Cancelled)
                } else {
                    Ok(())
                }
            });
            assert!(matches!(res, Err(Error::Cancelled)), "fail_on={fail_on}");
        }

        // 2. Many terms (3,000 distinct terms): footprint and decode poll every <= 1,024 terms.
        let many_terms_changes: Vec<(bool, QuadKey)> = (0..3_000u32)
            .map(|i| {
                let o: Arc<[u8]> = Arc::from(id::iri_key(&format!("urn:o:{i}")));
                (true, [g.clone(), s.clone(), p.clone(), o])
            })
            .collect();
        let many_terms_frame = encode(&Record::Changes(
            test_commit(2, None, None),
            many_terms_changes,
        ));
        let many_terms_body = &many_terms_frame[8..];
        let mut fp_polls = 0usize;
        assert!(
            footprint_checked(many_terms_body, &mut || {
                fp_polls += 1;
                Ok(())
            })
            .unwrap()
            .is_some()
        );
        assert!(
            fp_polls >= 3_000 / CHECK_STRIDE + 2,
            "expected >= {} footprint polls, got {fp_polls}",
            3_000 / CHECK_STRIDE + 2
        );
        let mut n = 0usize;
        assert!(matches!(
            footprint_checked(many_terms_body, &mut || {
                n += 1;
                if n == 2 {
                    Err(Error::Cancelled)
                } else {
                    Ok(())
                }
            }),
            Err(Error::Cancelled)
        ));

        let mut dt_polls = 0usize;
        assert!(
            decode_admitted_checked(many_terms_body, &mut || {
                dt_polls += 1;
                Ok(())
            })
            .unwrap()
            .is_some()
        );
        assert!(
            dt_polls >= 2 * (3_000 / CHECK_STRIDE) + 2,
            "expected polls across both terms and changes, got {dt_polls}"
        );

        // 3. Many changes (3,000 changes over a 4-term table): change loop polls every <= 1,024.
        let o0: Arc<[u8]> = Arc::from(id::iri_key("urn:o:0"));
        let many_changes: Vec<(bool, QuadKey)> = (0..3_000usize)
            .map(|i| {
                (
                    i.is_multiple_of(2),
                    [g.clone(), s.clone(), p.clone(), o0.clone()],
                )
            })
            .collect();
        let many_changes_frame = encode(&Record::Changes(test_commit(3, None, None), many_changes));
        let many_changes_body = &many_changes_frame[8..];
        assert!(
            decode_admitted_checked(many_changes_body, &mut || Ok(()))
                .unwrap()
                .is_some()
        );
        // Four polls precede the change loop. Its second poll must occur after
        // decoding change 1,023 and before decoding change 1,024.
        let change_bytes = 5;
        let changes_start = many_changes_body.len() - 3_000 * change_bytes;
        for index in [1_023, 1_024] {
            let mut damaged = many_changes_body.to_vec();
            damaged[changes_start + index * change_bytes + 4] = 99;
            assert!(matches!(
                decode_admitted_checked(&damaged, &mut || Ok(())),
                Ok(None)
            ));
            let mut polls = 0;
            let result = decode_admitted_checked(&damaged, &mut || {
                polls += 1;
                if polls == 6 {
                    Err(Error::Timeout)
                } else {
                    Ok(())
                }
            });
            if index == 1_023 {
                assert!(
                    matches!(result, Ok(None)),
                    "damage must precede the stride poll"
                );
            } else {
                assert!(
                    matches!(result, Err(Error::Timeout)),
                    "the decode stride poll must precede damage"
                );
            }
        }

        // 4. Large author and message (256 KiB each): UTF-8 and copy walks poll every <= 64 KiB.
        let big_meta_frame = encode(&Record::Summary(test_commit(
            4,
            Some("a".repeat(256 << 10)),
            Some("m".repeat(256 << 10)),
        )));
        let big_meta_body = &big_meta_frame[8..];
        let mut meta_polls = 0usize;
        assert!(
            decode_admitted_checked(big_meta_body, &mut || {
                meta_polls += 1;
                Ok(())
            })
            .unwrap()
            .is_some()
        );
        // 2 fields * (UTF-8 4 chunks + copy 4 chunks) >= 16 polls
        assert!(
            meta_polls >= 16,
            "expected >= 16 metadata polls, got {meta_polls}"
        );
        let mut n = 0usize;
        assert!(matches!(
            decode_admitted_checked(big_meta_body, &mut || {
                n += 1;
                if n == 8 {
                    Err(Error::Cancelled)
                } else {
                    Ok(())
                }
            }),
            Err(Error::Cancelled)
        ));

        // 5. All changes rejected by a filter in paged scan still poll every <= 1,024 changes.
        let store = Store::in_memory(StoreOptions::default());
        store.change_log().unwrap().set_background(false);
        let mut mixed =
            String::from("<urn:s_match> <urn:p_other> 1 . <urn:s_other> <urn:p_match> 2 . ");
        for j in 0..3_500 {
            mixed.push_str(&format!("<urn:s_match> <urn:p_other> {j} . "));
        }
        crate::sparql::update::update(
            &store,
            &format!("INSERT DATA {{ {mixed} }}"),
            &Default::default(),
        )
        .unwrap();
        store.flush_change_log().unwrap();
        let log = store.change_log().unwrap().clone();
        // Both `<urn:s_match>` and `<urn:p_match>` appear in commit 1, so the segment index
        // selects commit 1, yet no quad has both subject `<urn:s_match>` and predicate `<urn:p_match>`.
        let reject_all = LogFilter {
            keys: [
                None,
                Some(vec![id::iri_key("urn:s_match")]),
                Some(vec![id::iri_key("urn:p_match")]),
                None,
            ],
        };
        let mut visited = 0usize;
        let evaluated = CountPagedFilters::new();
        let mut admit = |_: u64| Ok(());
        let mut full = |_: u64| false;
        log.scan(
            1,
            1,
            &reject_all,
            false,
            false,
            false,
            &mut || Ok(()),
            Some(&mut PageHooks {
                admit: &mut admit,
                full: &mut full,
            }),
            &mut |_, _, _| {
                visited += 1;
                Ok(true)
            },
        )
        .unwrap();
        assert_eq!(visited, 0, "filter must reject every change");
        // One of the initial changes is also in the generated range.
        assert_eq!(evaluated.get(), 3_501);
        drop(evaluated);

        let evaluated = CountPagedFilters::new();
        let mut released = false;
        let res = log.scan(
            1,
            1,
            &reject_all,
            false,
            false,
            false,
            &mut || {
                if evaluated.get() == 1_024 {
                    Err(Error::Cancelled)
                } else {
                    Ok(())
                }
            },
            Some(&mut PageHooks {
                admit: &mut |bytes| {
                    released |= bytes == 0;
                    Ok(())
                },
                full: &mut full,
            }),
            &mut |_, _, _| {
                visited += 1;
                Ok(true)
            },
        );
        assert!(
            matches!(res, Err(Error::Cancelled)),
            "the filter stride poll must stop the scan: {res:?}"
        );
        assert_eq!(evaluated.get(), 1_024);
        assert_eq!(visited, 0);
        assert!(
            !released,
            "the scan must stop before its record release admission"
        );
        drop(evaluated);

        // 6. Malformed trailing reference: Ok(None) without stop, Cancelled if stopped earlier.
        let mut bad_body = many_changes_body.to_vec();
        // The last byte is the object term index varint (0..3) of the 3,000th change; set to 99.
        *bad_body.last_mut().unwrap() = 99;
        assert!(
            decode_admitted_checked(&bad_body, &mut || Ok(()))
                .unwrap()
                .is_none(),
            "malformed trailing term reference must decode as None (corrupt)"
        );
        let mut n = 0usize;
        assert!(
            matches!(
                decode_admitted_checked(&bad_body, &mut || {
                    n += 1;
                    if n == 3 {
                        Err(Error::Cancelled)
                    } else {
                        Ok(())
                    }
                }),
                Err(Error::Cancelled)
            ),
            "cancellation before the malformed tail must report Cancelled, not Corrupt"
        );
    }

    /// Positional chunked reads bound each kernel read and advance between polls to 64 KiB,
    /// poll before retrying an `Interrupted` read, and stop file-backed `read_admitted` with
    /// the typed stop error.
    #[test]
    fn admitted_history_reads_check_between_chunks() {
        use std::cell::Cell;

        // 1. Direct chunk-size, offset-stride and EINTR retry test via `read_exact_at_checked_with`.
        let src: Vec<u8> = (0..(256 << 10)).map(|i| (i % 251) as u8).collect();
        let mut dst = vec![0u8; src.len()];
        let mut calls = 0usize;
        let mut max_chunk = 0usize;
        let mut last_polled_offset = 0u64;
        let current_offset = Cell::new(0u64);
        let mut max_advance = 0u64;
        let mut interrupted_once = false;
        let polls = Cell::new(0usize);
        let mut polls_at_eintr = 0usize;
        readonly::read_exact_at_checked_with(
            &mut dst,
            0,
            &mut || {
                polls.set(polls.get() + 1);
                let cur = current_offset.get();
                max_advance = max_advance.max(cur - last_polled_offset);
                last_polled_offset = cur;
                Ok(())
            },
            |buf, at| {
                calls += 1;
                max_chunk = max_chunk.max(buf.len());
                if calls == 2 && !interrupted_once {
                    interrupted_once = true;
                    polls_at_eintr = polls.get();
                    return Err(std::io::Error::from(std::io::ErrorKind::Interrupted));
                }
                let start = at as usize;
                buf.copy_from_slice(&src[start..start + buf.len()]);
                current_offset.set(at + buf.len() as u64);
                Ok(buf.len())
            },
        )
        .unwrap();
        assert_eq!(dst, src);
        assert!(interrupted_once);
        assert!(max_chunk <= (64 << 10), "max_chunk={max_chunk}");
        assert!(max_advance <= (64 << 10), "max_advance={max_advance}");
        assert!(polls.get() > polls_at_eintr);

        // Cancel on the poll immediately after EINTR: must not retry another read.
        let mut dst2 = vec![0u8; src.len()];
        let mut calls2 = 0usize;
        let mut polls2 = 0usize;
        let eintr_hit = Cell::new(false);
        let mut saw_retry_after_cancel = false;
        let res = readonly::read_exact_at_checked_with(
            &mut dst2,
            0,
            &mut || {
                polls2 += 1;
                if eintr_hit.get() {
                    return Err(Error::Cancelled);
                }
                Ok(())
            },
            |buf, at| {
                calls2 += 1;
                if eintr_hit.get() {
                    saw_retry_after_cancel = true;
                }
                if calls2 == 2 {
                    eintr_hit.set(true);
                    return Err(std::io::Error::from(std::io::ErrorKind::Interrupted));
                }
                let start = at as usize;
                buf.copy_from_slice(&src[start..start + buf.len()]);
                Ok(buf.len())
            },
        );
        assert!(matches!(res, Err(Error::Cancelled)));
        assert!(eintr_hit.get());
        assert!(!saw_retry_after_cancel);
        assert_eq!(calls2, 2);
        assert_eq!(polls2, 3);

        // 2. File-backed `ChangeLog::read_admitted` over a > 256 KiB record.
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path(), StoreOptions::default()).unwrap();
        store.change_log().unwrap().set_background(false);
        crate::sparql::update::update(
            &store,
            &format!(
                "INSERT DATA {{ <urn:s> <urn:p> \"{}\" }}",
                "q".repeat(256 << 10)
            ),
            &Default::default(),
        )
        .unwrap();
        store.flush_change_log().unwrap();
        let log = store.change_log().unwrap().clone();
        let inner = log.inner.lock();
        let mut total_polls = 0usize;
        let mut total_admits = 0usize;
        let rec = log
            .read_admitted(
                &inner,
                0,
                SEG_HEADER as u64,
                &mut |_| {
                    total_admits += 1;
                    Ok(())
                },
                &mut || {
                    total_polls += 1;
                    Ok(())
                },
            )
            .unwrap();
        assert!(matches!(rec, Record::Changes(_, ref c) if c.len() == 1));
        assert!(total_polls >= 12, "expected >= 12 polls, got {total_polls}");
        assert_eq!(total_admits, 3);

        // Cancelling mid-read returns Cancelled (never Corrupt) and stops before decoded admission.
        for (fail_poll, expected_timeout) in [(2usize, false), (5, false), (7, true)] {
            let mut n = 0usize;
            let mut admits = 0usize;
            let res = log.read_admitted(
                &inner,
                0,
                SEG_HEADER as u64,
                &mut |_| {
                    admits += 1;
                    Ok(())
                },
                &mut || {
                    n += 1;
                    if n == fail_poll {
                        Err(if expected_timeout {
                            Error::Timeout
                        } else {
                            Error::Cancelled
                        })
                    } else {
                        Ok(())
                    }
                },
            );
            if expected_timeout {
                assert!(matches!(res, Err(Error::Timeout)), "fail_poll={fail_poll}");
            } else {
                assert!(
                    matches!(res, Err(Error::Cancelled)),
                    "fail_poll={fail_poll}"
                );
            }
            assert!(
                admits <= 1,
                "fail_poll={fail_poll}: expected at most frame admission before decode, got {admits}"
            );
        }
    }
}
