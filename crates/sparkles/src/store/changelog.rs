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
//! they are small enough to compare cheaply (the dataset was empty, or both states
//! together hold at most `change_log_bulk_max_quads` quads), and otherwise as a summary
//! with the commit's counts. History queries report the commits they could not see.
//!
//! **Retention.** Whole segments are dropped from the oldest, by commit count, age or
//! total size. The open segment always stays.
//!
//! In-memory stores keep the same records in memory.

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
            let f: SettingsFile = serde_json::from_slice(&b)
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
        &serde_json::to_vec_pretty(&f).expect("settings serialize"),
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
            let mut terms: Vec<Arc<[u8]>> = Vec::with_capacity(n.min(1 << 16));
            for _ in 0..n {
                let len = c.varint()? as usize;
                terms.push(Arc::from(c.bytes(len)?));
            }
            let m = c.varint()? as usize;
            let mut changes = Vec::with_capacity(m.min(1 << 16));
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
        if b.len() < IDX_HEADER || &b[0..8] != IDX_MAGIC {
            return None;
        }
        let u32_at = |i: usize| u32::from_le_bytes(b[i..i + 4].try_into().unwrap());
        let u64_at = |i: usize| u64::from_le_bytes(b[i..i + 8].try_into().unwrap());
        if u32_at(8) != FORMAT || b[16..32] != *dataset_id.as_bytes() {
            return None;
        }
        let (first, last) = (u64_at(32), u64_at(40));
        let (nrecs, nposts) = (u32_at(48) as usize, u32_at(52) as usize);
        let body = &b[IDX_HEADER..];
        if crc32(body) != u32_at(56) {
            return None;
        }
        let mut c = Cur { b: body, i: 0 };
        let mut ix = SegIndex::default();
        for _ in 0..nrecs {
            let seq = c.u64()?;
            let offset = c.u64()?;
            let ts = c.u64()? as i64;
            let hole = match c.u8()? {
                0 => None,
                1 => Some((c.u64()?, UnrecordedReason::Bulk)),
                _ => Some((c.u64()?, UnrecordedReason::Gap)),
            };
            ix.recs.push(RecEntry {
                seq,
                offset,
                ts,
                hole,
            });
        }
        for _ in 0..nposts {
            let h = c.u64()?;
            let o = u32::from_le_bytes(c.bytes(4)?.try_into().ok()?);
            ix.postings.push((h, o));
        }
        (c.i == body.len()).then_some((first, last, ix))
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
    dataset_id: uuid::Uuid,
    /// `<root>/changes` (persistent stores)
    dir: Option<PathBuf>,
    root: Option<PathBuf>,
    limits: LogLimits,
    settings: Mutex<ChangeLogSettings>,
    enabled: AtomicBool,
    inner: Mutex<Inner>,
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
    /// An in-memory store's log.
    pub(crate) fn memory(dataset_id: uuid::Uuid, limits: LogLimits) -> Arc<ChangeLog> {
        Self::new(
            dataset_id,
            None,
            limits,
            ChangeLogSettings::default(),
            Inner::empty(),
        )
    }

    fn new(
        dataset_id: uuid::Uuid,
        root: Option<&Path>,
        limits: LogLimits,
        settings: ChangeLogSettings,
        inner: Inner,
    ) -> Arc<ChangeLog> {
        let enabled = settings.enabled.unwrap_or(limits.default_on);
        Arc::new_cyclic(|me| ChangeLog {
            dataset_id,
            dir: root.map(|r| r.join(CHANGES_DIR)),
            root: root.map(Path::to_path_buf),
            limits,
            settings: Mutex::new(settings),
            enabled: AtomicBool::new(enabled),
            inner: Mutex::new(inner),
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
        let log = Self::new(dataset_id, Some(root), limits, settings, inner);
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
            )?),
            _ => None,
        };
        Ok(Some(Inner {
            segs,
            file,
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
        self.inner.lock().last
    }

    /// Hand queued commits to the background writer, or not (tests that look at the
    /// queue, or simulate a crash before anything was written).
    pub fn set_background(&self, on: bool) {
        self.background.store(on, Ordering::Relaxed);
    }

    /// Queue a commit (the commit path: no I/O).
    pub(crate) fn push(&self, p: Pending) {
        if !self.is_enabled() {
            return;
        }
        self.pending.lock().push_back(p);
        if self.background.load(Ordering::Relaxed) && !self.scheduled.swap(true, Ordering::AcqRel) {
            worker().send(self.me.clone());
        }
    }

    /// Append every queued commit. With `sync`, the log is durable afterwards.
    pub(crate) fn flush(&self, sync: bool) -> Result<()> {
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
                    tracing::warn!(error = %e, "the change log stopped recording");
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
        match &mut inner.file {
            Some(f) => {
                f.write_all(&bytes)?;
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
        let b = std::fs::read(&path)?;
        let (_, _, ix) = SegIndex::decode(&b, self.dataset_id)
            .ok_or_else(|| Error::Corrupt(format!("{}: damaged index", path.display())))?;
        let ix = Arc::new(ix);
        inner.cache.push_front((first, ix.clone()));
        inner.cache.truncate(INDEX_CACHE);
        Ok(ix)
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

    /// Drop sealed segments that retention no longer keeps. `now_ms` applies the age
    /// limit (none without it).
    pub(crate) fn enforce(&self, now_ms: Option<i64>) -> Result<usize> {
        let mut inner = self.inner.lock();
        self.enforce_locked(&mut inner, now_ms)
    }

    fn enforce_locked(&self, inner: &mut Inner, now_ms: Option<i64>) -> Result<usize> {
        let s = *self.settings.lock();
        let max = s.max_bytes.unwrap_or(self.limits.default_max_bytes);
        let head = inner.last.unwrap_or(0);
        let mut total: u64 = inner.segs.iter().map(|s| s.bytes).sum();
        let mut dropped = 0;
        while inner.segs.len() > 1 && inner.segs[0].sealed {
            let seg = &inner.segs[0];
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
        if let Some(root) = &self.root {
            write_settings(root, &s)?;
        }
        let on = s.enabled.unwrap_or(self.limits.default_on);
        let was = self.enabled.swap(on, Ordering::AcqRel);
        *self.settings.lock() = s;
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

    /// The commit whose timestamp is the newest at or before `ms` (`None`: none is
    /// recorded that early).
    pub(crate) fn commit_at_time(&self, ms: i64) -> Result<Option<u64>> {
        let mut inner = self.inner.lock();
        let mut best = None;
        for i in 0..inner.segs.len() {
            let ix = self.index(&mut inner, i)?;
            let n = ix.recs.partition_point(|r| r.ts <= ms);
            // a gap tells no time of its own
            if let Some(r) = ix.recs[..n]
                .iter()
                .rev()
                .find(|r| !matches!(r.hole, Some((_, UnrecordedReason::Gap))))
            {
                best = Some(r.seq);
            }
            if n < ix.recs.len() {
                break;
            }
        }
        Ok(best)
    }

    /// Read the changes of commits `from` through `to` that match `filter`, in commit
    /// order (newest first with `descending`), calling `visit` for each until it
    /// returns `false`. Returns the commits in the range whose changes are not
    /// recorded.
    pub(crate) fn scan(
        &self,
        from: u64,
        to: u64,
        filter: &LogFilter,
        descending: bool,
        check: &mut dyn FnMut() -> Result<()>,
        visit: &mut Visit<'_>,
    ) -> Result<Vec<Unrecorded>> {
        let mut inner = self.inner.lock();
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
            let ix = self.index(&mut inner, i)?;
            let lo = ix.recs.partition_point(|r| r.seq < from);
            let hi = ix.recs.partition_point(|r| r.seq <= to);
            for r in &ix.recs[lo..hi] {
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
                let Record::Changes(c, changes) = self.read(&inner, i, e.offset)? else {
                    continue;
                };
                for (add, k) in &changes {
                    if filter.matches(k) && !visit(&c, *add, k)? {
                        stop = true;
                        if descending {
                            break 'segs;
                        }
                        break;
                    }
                }
                if stop {
                    break;
                }
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

    /// The bytes on disk (or in memory).
    pub fn bytes(&self) -> u64 {
        self.inner.lock().segs.iter().map(|s| s.bytes).sum()
    }
}

impl Inner {
    fn empty() -> Inner {
        Inner {
            segs: Vec::new(),
            file: None,
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
    let tmp = path.with_extension("idx.tmp");
    {
        let mut f = File::create(&tmp)?;
        f.write_all(bytes)?;
        f.sync_all()?;
    }
    std::fs::rename(&tmp, path)?;
    Ok(())
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
                            tracing::warn!(error = %e, "change log write failed");
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
        let last = match log.last() {
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

    /// Queue the changes of bulk commit `c`, from state `before` to `after` (writer
    /// lock held, before the commit is published).
    pub(super) fn log_bulk_commit(
        &self,
        c: &CommitInfo,
        message: Option<Arc<str>>,
        author: Option<Arc<str>>,
        before: &Snapshot,
        after: &Snapshot,
    ) {
        let Some(log) = self.changelog.as_ref().filter(|l| l.is_enabled()) else {
            return;
        };
        let body = state_change_keys(before, after, log.limits.bulk_max_quads)
            .map_or(PendingBody::Summary, PendingBody::Keys);
        log.push(Pending {
            commit: ChangeCommit::of(c, author, message),
            body,
        });
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
        let w = self.writer.lock();
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
