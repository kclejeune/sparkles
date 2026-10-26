//! Durable commit identity.
//!
//! Every database has a **dataset id** (a UUID created with it, in `dataset.json`) and a
//! gap-free **commit sequence**: every committed write that changes data gets the next
//! `seq`. Commit 0 is the root, written when the database is created, or when a database
//! made by an older version is first opened (a `baseline` commit). The id, timestamp and
//! kind of a WAL commit live in the WAL commit record itself, so a commit id is durable
//! exactly when its data is, and replay reproduces it. A bulk commit (a rebuilt
//! generation) records its commit in `gen-NNNN/commit.json`, written before `CURRENT`
//! switches.
//!
//! `commits.bin` is a catalog of fixed 64-byte records, one per commit, for listing and
//! lookup. It is derived data: appended without fsync, rebuilt from the WAL and
//! `commit.json` on open, and synced before a generation (and its WAL) is replaced.

use crate::error::{Error, Result};
use serde::Serialize;
use std::fs::{File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

/// A dataset id (a version 4 UUID), named here for crates that do not depend on `uuid`.
pub type DatasetId = uuid::Uuid;

/// What produced a commit.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum CommitKind {
    /// root commit of a new database
    Create,
    /// root commit written when a database from an older version is first opened
    Baseline,
    /// SPARQL Update (including LOAD)
    Update,
    GspPut,
    GspPost,
    GspDelete,
    Upload,
    /// bulk load (`sparkles load`, `Dataset::load_*`, `Store::load`)
    Load,
    /// inference materialization
    Reason,
    /// removal of materialized inferences
    ReasonClear,
    /// library write transaction
    Transaction,
    /// vectors written by a vector index's embedding worker
    Embed,
    /// an RDF Patch applied (`POST /{ds}/patch`, `sparkles patch`, `Store::apply_patch`)
    Patch,
    /// a merge of another branch's changes (`POST /$/merge/{ds}`, `Store::merge`)
    Merge,
    /// a revert of an earlier commit (`POST /$/revert/{ds}`, `Store::revert`)
    Revert,
    /// another branch's commit applied again (`POST /$/cherry-pick/{ds}`,
    /// `Store::cherry_pick`)
    CherryPick,
    /// community reports written by a GraphRAG build (spec C22 §4). An older release
    /// reads the code as [`Unknown`](CommitKind::Unknown).
    Graphrag,
    /// a WAL commit written by an older version after this one had upgraded the database
    Unknown,
}

impl CommitKind {
    const ALL: [CommitKind; 18] = [
        CommitKind::Create,
        CommitKind::Baseline,
        CommitKind::Update,
        CommitKind::GspPut,
        CommitKind::GspPost,
        CommitKind::GspDelete,
        CommitKind::Upload,
        CommitKind::Load,
        CommitKind::Reason,
        CommitKind::ReasonClear,
        CommitKind::Transaction,
        CommitKind::Embed,
        CommitKind::Patch,
        CommitKind::Merge,
        CommitKind::Revert,
        CommitKind::CherryPick,
        CommitKind::Graphrag,
        CommitKind::Unknown,
    ];

    /// On-disk code.
    pub fn code(self) -> u8 {
        match self {
            CommitKind::Create => 0,
            CommitKind::Baseline => 1,
            CommitKind::Update => 2,
            CommitKind::GspPut => 3,
            CommitKind::GspPost => 4,
            CommitKind::GspDelete => 5,
            CommitKind::Upload => 6,
            CommitKind::Load => 7,
            CommitKind::Reason => 8,
            CommitKind::ReasonClear => 9,
            CommitKind::Transaction => 10,
            CommitKind::Embed => 11,
            CommitKind::Patch => 12,
            CommitKind::Merge => 13,
            CommitKind::Revert => 14,
            CommitKind::CherryPick => 15,
            CommitKind::Graphrag => 16,
            CommitKind::Unknown => 255,
        }
    }

    pub fn from_code(c: u8) -> CommitKind {
        Self::ALL
            .into_iter()
            .find(|k| k.code() == c)
            .unwrap_or(CommitKind::Unknown)
    }

    /// JSON / display name.
    pub fn name(self) -> &'static str {
        match self {
            CommitKind::Create => "create",
            CommitKind::Baseline => "baseline",
            CommitKind::Update => "update",
            CommitKind::GspPut => "gsp-put",
            CommitKind::GspPost => "gsp-post",
            CommitKind::GspDelete => "gsp-delete",
            CommitKind::Upload => "upload",
            CommitKind::Load => "load",
            CommitKind::Reason => "reason",
            CommitKind::ReasonClear => "reason-clear",
            CommitKind::Transaction => "transaction",
            CommitKind::Embed => "embed",
            CommitKind::Patch => "patch",
            CommitKind::Merge => "merge",
            CommitKind::Revert => "revert",
            CommitKind::CherryPick => "cherry-pick",
            CommitKind::Graphrag => "graphrag",
            CommitKind::Unknown => "unknown",
        }
    }

    pub fn from_name(s: &str) -> Option<CommitKind> {
        Self::ALL.into_iter().find(|k| k.name() == s)
    }
}

/// One commit: the state after a write that changed data.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CommitInfo {
    pub seq: u64,
    /// milliseconds since the Unix epoch (non-decreasing along the sequence)
    pub timestamp_ms: i64,
    pub kind: CommitKind,
    /// quads added, relative to the parent commit
    pub inserted: u64,
    /// quads removed, relative to the parent commit
    pub deleted: u64,
    /// quads in the dataset after this commit
    pub quads: u64,
    /// generation number the commit was made in (`gen-NNNN`; 0 for in-memory stores)
    pub generation: u32,
    /// made by rebuilding the generation rather than through the WAL
    pub bulk: bool,
    /// `inserted` / `deleted` are exact (a bulk commit that also deleted quads may count
    /// a quad deleted and re-added as both)
    pub exact: bool,
    /// rebuilt from a WAL record without commit metadata
    pub reconstructed: bool,
    /// the commit may have changed the default graph (`false` only when it is known to
    /// have changed named graphs alone)
    pub default_graph: bool,
    /// the write skipped the write-time validation the dataset requires (a bypass)
    pub unvalidated: bool,
}

impl CommitInfo {
    pub fn parent(&self) -> Option<u64> {
        self.seq.checked_sub(1)
    }

    /// RFC 3339 timestamp in UTC with milliseconds.
    pub fn timestamp(&self) -> String {
        rfc3339_ms(self.timestamp_ms)
    }

    pub fn generation_name(&self) -> String {
        generation_name(self.generation)
    }

    /// The reference form `commit:<seq>`.
    pub fn reference(&self) -> String {
        format!("commit:{}", self.seq)
    }
}

pub(crate) fn generation_name(n: u32) -> String {
    if n == 0 {
        "mem".to_string()
    } else {
        format!("gen-{n:04}")
    }
}

/// Generation number of a generation directory name (`gen-0007` → 7; 0 otherwise).
pub(crate) fn generation_number(name: &str) -> u32 {
    name.strip_prefix("gen-")
        .and_then(|n| n.parse().ok())
        .unwrap_or(0)
}

impl Serialize for CommitInfo {
    fn serialize<S: serde::Serializer>(&self, s: S) -> std::result::Result<S::Ok, S::Error> {
        use serde::ser::SerializeMap;
        let mut m = s.serialize_map(None)?;
        self.serialize_fields(&mut m)?;
        m.end()
    }
}

impl CommitInfo {
    fn serialize_fields<M: serde::ser::SerializeMap>(
        &self,
        m: &mut M,
    ) -> std::result::Result<(), M::Error> {
        m.serialize_entry("seq", &self.seq)?;
        m.serialize_entry("parent", &self.parent())?;
        m.serialize_entry("ref", &self.reference())?;
        m.serialize_entry("timestamp", &self.timestamp())?;
        m.serialize_entry("kind", self.kind.name())?;
        m.serialize_entry("inserted", &self.inserted)?;
        m.serialize_entry("deleted", &self.deleted)?;
        m.serialize_entry("quads", &self.quads)?;
        m.serialize_entry("generation", &self.generation_name())?;
        m.serialize_entry("bulk", &self.bulk)?;
        m.serialize_entry("exact", &self.exact)?;
        if self.reconstructed {
            m.serialize_entry("reconstructed", &true)?;
        }
        if self.unvalidated {
            m.serialize_entry("unvalidated", &true)?;
        }
        Ok(())
    }
}

/// A commit with its annotation, serialized as the commit's members plus `message` and
/// `digest` (hex) when it has them.
#[derive(Clone, Copy, Debug)]
pub struct AnnotatedCommit<'a> {
    pub commit: &'a CommitInfo,
    pub annotation: Option<&'a crate::annotations::Annotation>,
}

impl Serialize for AnnotatedCommit<'_> {
    fn serialize<S: serde::Serializer>(&self, s: S) -> std::result::Result<S::Ok, S::Error> {
        use serde::ser::SerializeMap;
        let mut m = s.serialize_map(None)?;
        self.commit.serialize_fields(&mut m)?;
        if let Some(a) = self.annotation {
            if let Some(msg) = &a.message {
                m.serialize_entry("message", msg.as_ref())?;
            }
            if let Some(d) = a.digest_hex() {
                m.serialize_entry("digest", &d)?;
            }
        }
        m.end()
    }
}

/// The outcome of a write: the new commit, or the unchanged head when the write had no
/// net effect.
#[derive(Clone, Debug, PartialEq)]
pub struct Receipt {
    pub dataset_id: uuid::Uuid,
    pub committed: bool,
    pub commit: CommitInfo,
    /// what a write guard found (write-time validation), when one ran
    pub validation: Option<std::sync::Arc<crate::guard::ValidationSummary>>,
    /// the commit's message and change digest (serialized inside `commit`)
    pub annotation: crate::annotations::Annotation,
}

impl Serialize for Receipt {
    fn serialize<S: serde::Serializer>(&self, s: S) -> std::result::Result<S::Ok, S::Error> {
        use serde::ser::SerializeMap;
        let mut m = s.serialize_map(None)?;
        m.serialize_entry("datasetId", &self.dataset_id)?;
        m.serialize_entry("committed", &self.committed)?;
        m.serialize_entry(
            "commit",
            &AnnotatedCommit {
                commit: &self.commit,
                annotation: Some(&self.annotation),
            },
        )?;
        if let Some(v) = &self.validation {
            m.serialize_entry("validation", v.as_ref())?;
        }
        m.end()
    }
}

/// A page of the commit catalog.
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CommitPage {
    pub commits: Vec<CommitInfo>,
    /// oldest commit whose metadata is still available
    pub first_retained: u64,
    /// false while the catalog lags the WAL after a write error
    pub complete: bool,
}

/// Which commits to list.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CommitRange {
    /// newest first
    Latest,
    /// newest first, with `seq` below this one
    Before(u64),
    /// oldest first, with `seq` above this one
    After(u64),
}

// ------------------------------------------------------------------- time ------

pub(crate) fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as i64)
}

/// `2026-09-30T14:03:11.482Z`
pub fn rfc3339_ms(ms: i64) -> String {
    let secs = ms.div_euclid(1000);
    let milli = ms.rem_euclid(1000);
    // civil-from-days (Howard Hinnant)
    let days = secs.div_euclid(86400);
    let sod = secs.rem_euclid(86400);
    let z = days + 719468;
    let era = z.div_euclid(146097);
    let doe = z - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let dd = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    format!(
        "{y:04}-{m:02}-{dd:02}T{:02}:{:02}:{:02}.{milli:03}Z",
        sod / 3600,
        (sod % 3600) / 60,
        sod % 60
    )
}

/// Parse the timestamps written by [`rfc3339_ms`] (and the same form without
/// milliseconds).
pub(crate) fn parse_rfc3339_ms(s: &str) -> Option<i64> {
    let b = s.as_bytes();
    if b.len() < 20 || *b.last()? != b'Z' {
        return None;
    }
    let num = |r: std::ops::Range<usize>| s.get(r)?.parse::<i64>().ok();
    let (y, mo, d) = (num(0..4)?, num(5..7)?, num(8..10)?);
    let (h, mi, sec) = (num(11..13)?, num(14..16)?, num(17..19)?);
    let ms = if b[19] == b'.' { num(20..23)? } else { 0 };
    // days-from-civil (Howard Hinnant)
    let y = if mo <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = (mo + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146097 + doe - 719468;
    Some(((days * 86400 + h * 3600 + mi * 60 + sec) * 1000) + ms)
}

/// Parse an RFC 3339 date-time with any offset (`Z`, `+02:00`, `-05:30`), to
/// milliseconds; digits beyond milliseconds are truncated. A space where the offset's
/// sign belongs is read as `+` (a `+` in a query string decodes to a space).
pub(crate) fn parse_rfc3339_offset(s: &str) -> Option<i64> {
    let b = s.as_bytes();
    if b.len() < 20 || !matches!(b[10], b'T' | b't') || b[4] != b'-' || b[7] != b'-' {
        return None;
    }
    let num = |r: std::ops::Range<usize>| -> Option<i64> {
        let t = s.get(r)?;
        t.bytes()
            .all(|c| c.is_ascii_digit())
            .then(|| t.parse().ok())?
    };
    let (_year, mo, d) = (num(0..4)?, num(5..7)?, num(8..10)?);
    let (h, mi, sec) = (num(11..13)?, num(14..16)?, num(17..19)?);
    if b[13] != b':' || b[16] != b':' || !(1..=12).contains(&mo) || !(1..=31).contains(&d) {
        return None;
    }
    if h > 23 || mi > 59 || sec > 60 {
        return None;
    }
    let mut i = 19;
    let mut ms = 0;
    if b[i] == b'.' {
        let start = i + 1;
        i = start;
        while i < b.len() && b[i].is_ascii_digit() {
            i += 1;
        }
        if i == start {
            return None;
        }
        let frac = &s[start..i.min(start + 3)];
        ms = frac.parse::<i64>().ok()? * 10i64.pow(3 - frac.len() as u32);
    }
    let offset_min = match b.get(i)? {
        b'Z' | b'z' if i + 1 == b.len() => 0,
        sign @ (b'+' | b'-' | b' ') if i + 6 == b.len() && b[i + 3] == b':' => {
            let (oh, om) = (num(i + 1..i + 3)?, num(i + 4..i + 6)?);
            let m = oh * 60 + om;
            if *sign == b'-' { -m } else { m }
        }
        _ => return None,
    };
    let z = format!("{}Z", &s[..19]);
    let base = parse_rfc3339_ms(&z)?;
    Some(base + ms - offset_min * 60_000)
}

// ------------------------------------------------------------- dataset.json ------

/// Dataset capabilities supported by this reader (1: legacy, 2: branches,
/// 3: relinked generations with an immutable base overlay, 4: framed delta
/// vocabularies, see [`crate::vocab::delta`], 5: write-ahead logs with a header record,
/// see [`crate::store::wal`]). [`crate::formats::DATASET`] lists them.
pub const DATASET_READER: u32 = 5;
fn legacy_reader() -> u32 {
    1
}

/// Parse the content of a `dataset.json`. The version members are checked on the raw
/// JSON first, so that a file from a newer release whose other members changed shape is
/// refused as [`Error::Unsupported`] and not reported as damaged.
fn parse_dataset(bytes: &[u8]) -> Result<DatasetFile> {
    let v: serde_json::Value =
        serde_json::from_slice(bytes).map_err(|e| Error::Corrupt(format!("dataset.json: {e}")))?;
    check_dataset_value(&v)?;
    serde_json::from_value(v).map_err(|e| Error::Corrupt(format!("dataset.json: {e}")))
}

/// Share version refusal with the bounded read-only decoder. This check grants
/// no profile authority and does not publish or raise any reader requirement.
fn check_dataset_value(v: &serde_json::Value) -> Result<()> {
    let member = |name: &str| -> Result<u64> {
        match v.get(name) {
            None | Some(serde_json::Value::Null) => Ok(1),
            Some(n) => n
                .as_u64()
                .ok_or_else(|| Error::Corrupt(format!("dataset.json: {name} is not a version"))),
        }
    };
    let format = member("format")?;
    let minimum_reader = member("minimumReader")?;
    if format != 1 || minimum_reader > u64::from(DATASET_READER) {
        let by = v
            .get("writtenBy")
            .and_then(|w| w.as_str())
            // A damaged header must not format an unbounded native string.
            .filter(|value| value.len() <= 256)
            .map(|v| format!(", written by Sparkles {v},"))
            .unwrap_or_default();
        return Err(Error::Unsupported(format!(
            "dataset.json{by} has format {format} and requires reader {minimum_reader}; Sparkles {} reads format 1 up to reader {}. Open the database with a newer release",
            crate::formats::SPARKLES_VERSION,
            DATASET_READER
        )));
    }
    Ok(())
}

/// Decode the same identity and version contract without mutable Store startup.
/// Both Serde passes and their live buffers use the caller's preparation budget.
pub(crate) fn read_dataset_file_controlled(
    root: &Path,
    budget: &mut crate::store::cold::ColdBudget<'_>,
) -> Result<Option<uuid::Uuid>> {
    let start = budget.used();
    budget.charge(
        (root.as_os_str().len() as u64)
            .saturating_mul(2)
            .saturating_add(128),
    )?;
    let result = (|| {
        let bytes = match budget.read_file(&root.join("dataset.json")) {
            Ok(bytes) => bytes,
            Err(Error::Io(error)) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(None);
            }
            Err(error) => return Err(error),
        };
        let raw: crate::store::cold::Loaded<serde_json::Value> = budget.json(&bytes)?;
        check_dataset_value(&raw.value)?;
        let parsed: DatasetFile = budget.value_fields(raw.value, DATASET_FIELDS)?;
        let id = parsed.id;
        drop(parsed);
        budget.release(raw.charge);
        budget.drop_vec(bytes);
        budget.check()?;
        Ok(Some(id))
    })();
    // Closure-owned paths, parser scratch and containers have dropped even on
    // interruption or version refusal. Only a scalar UUID escapes this scope.
    budget.release(budget.used() - start);
    result
}

/// Exercise the actual closed scalar reader with caller-attributed allocation
/// instrumentation. This returns no source, access or managed-admission authority.
#[doc(hidden)]
#[cfg(feature = "failpoints")]
pub fn read_dataset_identity_limited_for_test(
    root: &Path,
    options: &crate::history::HistoryOptions,
    max_memory_bytes: u64,
) -> Result<Option<uuid::Uuid>> {
    let mut budget = crate::store::cold::ColdBudget::new(crate::history::ReadLimit {
        options,
        memory: Some(max_memory_bytes),
        held: 0,
    })?;
    read_dataset_file_controlled(root, &mut budget)
}

/// The versions `<root>/dataset.json` records, if it exists and this build reads it.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DatasetVersions {
    pub format: u32,
    pub minimum_reader: u32,
    /// the release that last opened the database for writing, when one recorded it
    pub written_by: Option<String>,
}

/// The versions of `<root>/dataset.json` (`None` without one).
pub fn read_dataset_versions(root: &Path) -> Result<Option<DatasetVersions>> {
    Ok(read_dataset(root)?.map(|f| DatasetVersions {
        format: f.format,
        minimum_reader: f.minimum_reader,
        written_by: f.written_by,
    }))
}

/// `writtenBy` of `<root>/dataset.json` without checking its reader, for messages about
/// a database this build may not read.
pub fn read_written_by(root: &Path) -> Option<String> {
    let b = std::fs::read(root.join("dataset.json")).ok()?;
    let v: serde_json::Value = serde_json::from_slice(&b).ok()?;
    v.get("writtenBy")?.as_str().map(str::to_string)
}

/// Record that this release opens the database at `root` for writing: `writtenBy`
/// becomes this release, and `minimumReader` rises to the reader of the files this
/// release creates (a log with a header record, [`crate::store::wal::HEADER_READER`]).
/// `dataset.json` is rewritten only when either changes.
pub(crate) fn note_writer(root: &Path) -> Result<()> {
    let Some(mut ds) = read_dataset(root)? else {
        return Ok(());
    };
    let reader = ds.minimum_reader.max(crate::store::wal::HEADER_READER);
    let version = crate::formats::SPARKLES_VERSION;
    if ds.minimum_reader == reader && ds.written_by.as_deref() == Some(version) {
        return Ok(());
    }
    ds.minimum_reader = reader;
    ds.written_by = Some(version.to_string());
    write_dataset(root, &ds)
}

/// Check compatibility without changing any database files.
pub fn check_dataset_compatibility(root: &Path) -> Result<()> {
    read_dataset(root).map(|_| ())
}

/// Publish the minimum reader needed by branches before publishing their table.
pub(crate) fn require_branch_reader(root: &Path) -> Result<()> {
    require_reader(root, 2)
}

pub(crate) fn require_reader(root: &Path, reader: u32) -> Result<()> {
    let Some(mut ds) = read_dataset(root)? else {
        return Ok(());
    };
    if ds.minimum_reader < reader {
        ds.minimum_reader = reader;
        write_dataset(root, &ds)?;
    }
    Ok(())
}

// The declaration owns both the ordinary schema and the read-only projection's
// field names. A newly typed field cannot silently become an ignored extra there.
macro_rules! dataset_file {
    ($($(#[$attr:meta])* $field:ident: $ty:ty => $wire:literal),* $(,)?) => {
        #[derive(Debug, serde::Serialize, serde::Deserialize)]
        struct DatasetFile {
            $($(#[$attr])* #[serde(rename = $wire)] $field: $ty,)*
            // Ordinary readers preserve extensions on subsequent writes.
            #[serde(flatten)]
            extra: serde_json::Map<String, serde_json::Value>,
        }
        const DATASET_FIELDS: &[&str] = &[$($wire,)*];
    };
}

dataset_file! {
    format: u32 => "format",
    #[serde(default = "legacy_reader")]
    minimum_reader: u32 => "minimumReader",
    #[serde(default, skip_serializing_if = "Option::is_none")]
    written_by: Option<String> => "writtenBy",
    id: uuid::Uuid => "id",
    created: String => "created",
    origin: String => "origin",
    #[serde(default, skip_serializing_if = "Option::is_none")]
    forked_from: Option<ForkedFrom> => "forkedFrom",
    #[serde(default, skip_serializing_if = "Option::is_none")]
    restored_from: Option<RestoredFrom> => "restoredFrom",
}

/// Where a cloned dataset came from: the source's dataset id and the commit (`seq`) of
/// the snapshot it copied. The clone is a new lineage with its own commit sequence.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ForkedFrom {
    pub id: uuid::Uuid,
    pub seq: u64,
}

/// Where a restored dataset came from (`restoredFrom` of `dataset.json` and of the
/// server's dataset info): the backup repository and backup, and the source dataset id
/// and commit (`seq`) the backup captured.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RestoredFrom {
    /// the repository's name on the server (or CLI configuration) that restored it
    pub repository: String,
    /// the backup's name in that repository
    pub backup: String,
    pub dataset_id: uuid::Uuid,
    pub seq: u64,
}

/// Give a closed database directory `root` a new dataset id: rewrite `dataset.json`
/// (`origin: "restore"`, `forkedFrom: forked_from`, a new `created`), the `datasetId`
/// of the current `gen-NNNN/commit.json`, and the `commits.bin` header (the UUID and the
/// header CRC). Commit metadata and sequence numbers are kept, so the next commit is
/// `forked_from.seq + 1` under `new_id`. WAL records carry no dataset id and are left
/// alone. Every rewritten file is replaced atomically and synced.
///
/// Used by restores whose identity rule mints a new id; the directory must not be open.
pub fn reidentify(root: &Path, new_id: uuid::Uuid, forked_from: ForkedFrom) -> Result<()> {
    let mut ds = read_dataset(root)?
        .ok_or_else(|| Error::Invalid(format!("{} has no dataset.json", root.display())))?;
    let old_id = ds.id;
    // every generation of the old lineage (a restore has only the current one)
    for e in std::fs::read_dir(root)? {
        let e = e?;
        let name = e.file_name().to_string_lossy().into_owned();
        let digits = name.strip_prefix("gen-").unwrap_or("");
        if !e.file_type()?.is_dir()
            || digits.is_empty()
            || !digits.bytes().all(|b| b.is_ascii_digit())
        {
            continue;
        }
        let path = e.path().join("commit.json");
        let b = match std::fs::read(&path) {
            Ok(b) => b,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
            Err(e) => return Err(e.into()),
        };
        let mut f = parse_gen_commit(&path, &b)?;
        if f.dataset_id == old_id {
            f.dataset_id = new_id;
            crate::store::write_atomic(&path, &serde_json::to_vec_pretty(&f).unwrap())?;
        }
    }
    // the catalog header (records carry no dataset id)
    let catalog = root.join("commits.bin");
    match std::fs::read(&catalog) {
        Ok(mut buf) => match decode_header(&buf) {
            Some((id, first)) if id == old_id => {
                buf[..REC].copy_from_slice(&encode_header(new_id, first));
                crate::store::write_atomic(&catalog, &buf)?;
            }
            Some((id, _)) => {
                return Err(Error::Corrupt(format!(
                    "{}: belongs to dataset {id}, not {old_id}",
                    catalog.display()
                )));
            }
            // rebuilt from the WAL and commit.json at the next open
            None => {}
        },
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(e.into()),
    }
    crate::history::reidentify_file(root, old_id, new_id)?;
    crate::annotations::reidentify(root, old_id, new_id)?;
    // dataset.json last: its id is what the other files are checked against
    ds.id = new_id;
    ds.origin = "restore".to_string();
    ds.created = rfc3339_ms(now_ms());
    ds.forked_from = Some(forked_from);
    write_dataset(root, &ds)?;
    crate::store::sync_dir(root)
}

/// Preserve the blank-node ordinal space of a standalone branch restore.
pub fn reserve_branch_ordinals(root: &Path, id: uuid::Uuid, next: u64) -> Result<()> {
    crate::store::write_initial_table(root, id, next)
}

/// `restoredFrom` of `<root>/dataset.json`, if the database was restored from a backup.
pub fn read_restored_from(root: &Path) -> Result<Option<RestoredFrom>> {
    Ok(read_dataset(root)?.and_then(|f| f.restored_from))
}

/// Record in `<root>/dataset.json` (replaced atomically) the backup a restored database
/// was made from. The directory must not be open.
pub fn set_restored_from(root: &Path, from: &RestoredFrom) -> Result<()> {
    let mut ds = read_dataset(root)?
        .ok_or_else(|| Error::Invalid(format!("{} has no dataset.json", root.display())))?;
    ds.restored_from = Some(from.clone());
    write_dataset(root, &ds)
}

fn read_dataset(root: &Path) -> Result<Option<DatasetFile>> {
    match std::fs::read(root.join("dataset.json")) {
        Ok(b) => parse_dataset(&b).map(Some),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e.into()),
    }
}

fn write_dataset(root: &Path, ds: &DatasetFile) -> Result<()> {
    crate::store::write_atomic(
        &root.join("dataset.json"),
        &serde_json::to_vec_pretty(ds).unwrap(),
    )
}

/// `forkedFrom` of `<root>/dataset.json`, if the database is a clone.
pub(crate) fn read_forked_from(root: &Path) -> Result<Option<ForkedFrom>> {
    Ok(read_dataset(root)?.and_then(|f| f.forked_from))
}

/// `dataset.json` of a clone.
pub(crate) fn clone_dataset_file_bytes(
    id: uuid::Uuid,
    created_ms: i64,
    from: ForkedFrom,
) -> Vec<u8> {
    serde_json::to_vec_pretty(&DatasetFile {
        format: 1,
        minimum_reader: crate::store::wal::HEADER_READER,
        written_by: Some(crate::formats::SPARKLES_VERSION.to_string()),
        id,
        created: rfc3339_ms(created_ms),
        origin: "clone".to_string(),
        forked_from: Some(from),
        restored_from: None,
        extra: Default::default(),
    })
    .unwrap()
}

/// The dataset id of `dataset.json`'s content.
pub(crate) fn dataset_id_of(bytes: &[u8]) -> Result<uuid::Uuid> {
    Ok(parse_dataset(bytes)?.id)
}

/// When `<root>/dataset.json` says the dataset was created (milliseconds since the
/// epoch), if it can be read.
pub(crate) fn read_dataset_created(root: Option<&Path>) -> Option<i64> {
    read_dataset(root?)
        .ok()
        .flatten()
        .and_then(|f| parse_rfc3339_ms(&f.created))
}

/// `<root>/dataset.json`: the dataset id, if the database has one yet.
pub(crate) fn read_dataset_file(root: &Path) -> Result<Option<uuid::Uuid>> {
    Ok(read_dataset(root)?.map(|f| f.id))
}

pub(crate) fn dataset_file_bytes(id: uuid::Uuid, origin: &str, created_ms: i64) -> Vec<u8> {
    serde_json::to_vec_pretty(&DatasetFile {
        format: 1,
        minimum_reader: crate::store::wal::HEADER_READER,
        written_by: Some(crate::formats::SPARKLES_VERSION.to_string()),
        id,
        created: rfc3339_ms(created_ms),
        origin: origin.to_string(),
        forked_from: None,
        restored_from: None,
        extra: Default::default(),
    })
    .unwrap()
}

// ---------------------------------------------------------- gen/commit.json ------

#[derive(serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct GenCommitFile {
    format: u32,
    dataset_id: uuid::Uuid,
    base_seq: u64,
    /// `create`, `baseline`, `bulk`, `compaction` or `clone`
    origin: String,
    commit: CommitJson,
    /// Where strict framing starts in the generation's headerless (version 0) log, in
    /// bytes. A release that writes commit metadata records the log's logical end here
    /// once, at its first write open of the log, before it appends anything. Absent in
    /// files written before the member existed, and for logs with a header record.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    wal_strict_from: Option<u64>,
    /// Members this build does not know, added by a later release under the additive
    /// rule of X12 section 4.3. They are kept when the file is rewritten.
    #[serde(flatten)]
    extra: serde_json::Map<String, serde_json::Value>,
}

#[derive(serde::Serialize, serde::Deserialize)]
struct CommitJson {
    seq: u64,
    timestamp: String,
    kind: String,
    inserted: u64,
    deleted: u64,
    quads: u64,
    generation: String,
    bulk: bool,
    exact: bool,
    #[serde(default)]
    reconstructed: bool,
    /// absent in files written before the flag existed: assume the default graph changed
    #[serde(default = "yes")]
    default_graph: bool,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    unvalidated: bool,
    /// members a later release added, kept on rewrite (see [`GenCommitFile`])
    #[serde(flatten)]
    extra: serde_json::Map<String, serde_json::Value>,
}

fn yes() -> bool {
    true
}

pub(crate) fn gen_commit_bytes(dataset_id: uuid::Uuid, origin: &str, c: &CommitInfo) -> Vec<u8> {
    serde_json::to_vec_pretty(&GenCommitFile {
        format: 1,
        dataset_id,
        base_seq: c.seq,
        origin: origin.to_string(),
        wal_strict_from: None,
        extra: Default::default(),
        commit: CommitJson {
            seq: c.seq,
            timestamp: c.timestamp(),
            kind: c.kind.name().to_string(),
            inserted: c.inserted,
            deleted: c.deleted,
            quads: c.quads,
            generation: c.generation_name(),
            bulk: c.bulk,
            exact: c.exact,
            reconstructed: c.reconstructed,
            default_graph: c.default_graph,
            unvalidated: c.unvalidated,
            extra: Default::default(),
        },
    })
    .unwrap()
}

/// The commit a generation's base index holds, from `gen-NNNN/commit.json`, with the
/// dataset id and the file's origin (`create`, `baseline`, `bulk`, `compaction`).
pub(crate) fn read_gen_commit(dir: &Path) -> Result<Option<(uuid::Uuid, CommitInfo, String)>> {
    let path = dir.join("commit.json");
    let b = match std::fs::read(&path) {
        Ok(b) => b,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e.into()),
    };
    let f = parse_gen_commit(&path, &b)?;
    let decoded = decode_gen_commit_file(f, &path)?;
    Ok(Some((decoded.dataset_id, decoded.commit, decoded.origin)))
}

/// Physical generation identity decoded without opening or recovering a Store.
/// This is historical metadata, not a managed-admission or authenticity token.
pub(crate) struct RetainedGenerationCommit {
    pub(crate) dataset_id: uuid::Uuid,
    pub(crate) commit: CommitInfo,
    pub(crate) origin: String,
    pub(crate) wal_strict_from: Option<u64>,
}

fn decode_gen_commit_file(f: GenCommitFile, path: &Path) -> Result<RetainedGenerationCommit> {
    let bad = |m: &str| Error::Corrupt(format!("{}: {m}", path.display()));
    let c = f.commit;
    Ok(RetainedGenerationCommit {
        dataset_id: f.dataset_id,
        commit: CommitInfo {
            seq: c.seq,
            timestamp_ms: parse_rfc3339_ms(&c.timestamp).ok_or_else(|| bad("timestamp"))?,
            kind: CommitKind::from_name(&c.kind).unwrap_or(CommitKind::Unknown),
            inserted: c.inserted,
            deleted: c.deleted,
            quads: c.quads,
            generation: generation_number(&c.generation),
            bulk: c.bulk,
            exact: c.exact,
            reconstructed: c.reconstructed,
            default_graph: c.default_graph,
            unvalidated: c.unvalidated,
        },
        origin: f.origin,
        wal_strict_from: f.wal_strict_from,
    })
}

pub(crate) fn read_gen_commit_controlled(
    dir: &Path,
    budget: &mut crate::store::cold::ColdBudget<'_>,
) -> Result<Option<crate::store::cold::Loaded<RetainedGenerationCommit>>> {
    let path = dir.join("commit.json");
    let bytes = match budget.read_file(&path) {
        Ok(bytes) => bytes,
        Err(Error::Io(error)) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error),
    };
    let parsed: crate::store::cold::Loaded<GenCommitFile> = budget.json(&bytes)?;
    if parsed.value.format != 1 {
        return Err(Error::Unsupported(format!(
            "{} has format {}; this release reads format 1",
            path.display(),
            parsed.value.format
        )));
    }
    budget.drop_vec(bytes);
    let value = decode_gen_commit_file(parsed.value, &path)?;
    let charge =
        std::mem::size_of::<RetainedGenerationCommit>() as u64 + value.origin.capacity() as u64;
    if charge > parsed.charge {
        budget.charge(charge - parsed.charge)?;
    } else {
        budget.release(parsed.charge - charge);
    }
    budget.check()?;
    Ok(Some(crate::store::cold::Loaded { value, charge }))
}

/// Parse `gen-NNNN/commit.json`, refusing a `format` this build does not read.
fn parse_gen_commit(path: &Path, b: &[u8]) -> Result<GenCommitFile> {
    let f: GenCommitFile = serde_json::from_slice(b)
        .map_err(|e| Error::Corrupt(format!("{}: {e}", path.display())))?;
    if f.format != 1 {
        return Err(Error::Unsupported(format!(
            "{} has format {}; this release reads format 1",
            path.display(),
            f.format
        )));
    }
    Ok(f)
}

/// The strict-framing offset of a generation's headerless log (`walStrictFrom` of
/// `gen-NNNN/commit.json`, see [`GenCommitFile`]): `None` when the file or the member is
/// missing.
pub(crate) fn wal_strict_from(dir: &Path) -> Result<Option<u64>> {
    let path = dir.join("commit.json");
    match std::fs::read(&path) {
        Ok(b) => Ok(parse_gen_commit(&path, &b)?.wal_strict_from),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e.into()),
    }
}

/// Record `at` as the strict-framing offset of the generation in `dir`, keeping every
/// other member of its `commit.json`, durably (the file is replaced atomically and the
/// directory synced) before the caller appends to the log.
pub(crate) fn set_wal_strict_from(dir: &Path, at: u64) -> Result<()> {
    let path = dir.join("commit.json");
    let mut f = parse_gen_commit(&path, &std::fs::read(&path)?)?;
    f.wal_strict_from = Some(at);
    crate::store::write_atomic(&path, &serde_json::to_vec_pretty(&f).unwrap())
}

// ----------------------------------------------------------------- catalog ------

const MAGIC: &[u8; 8] = b"SPKCMTS\0";
pub(crate) const REC: usize = 64;

fn crc32(parts: &[&[u8]]) -> u32 {
    let mut c = flate2::Crc::new();
    for p in parts {
        c.update(p);
    }
    c.sum()
}

fn encode_record(c: &CommitInfo) -> [u8; REC] {
    let mut r = [0u8; REC];
    r[0..8].copy_from_slice(&c.seq.to_le_bytes());
    r[8..16].copy_from_slice(&c.timestamp_ms.to_le_bytes());
    r[16..24].copy_from_slice(&c.inserted.to_le_bytes());
    r[24..32].copy_from_slice(&c.deleted.to_le_bytes());
    r[32..40].copy_from_slice(&c.quads.to_le_bytes());
    r[40..44].copy_from_slice(&c.generation.to_le_bytes());
    r[44] = c.kind.code();
    // bit 3 is set when the default graph is known to be unchanged, so records written
    // before the flag existed read as "may have changed it"
    r[45] = c.exact as u8
        | (c.bulk as u8) << 1
        | (c.reconstructed as u8) << 2
        | (!c.default_graph as u8) << 3
        | (c.unvalidated as u8) << 4;
    let crc = crc32(&[&r[..60]]);
    r[60..64].copy_from_slice(&crc.to_le_bytes());
    r
}

pub(crate) fn decode_record(r: &[u8]) -> Option<CommitInfo> {
    let crc = u32::from_le_bytes(r[60..64].try_into().unwrap());
    if crc != crc32(&[&r[..60]]) {
        return None;
    }
    let u = |i: usize| u64::from_le_bytes(r[i..i + 8].try_into().unwrap());
    Some(CommitInfo {
        seq: u(0),
        timestamp_ms: u(8) as i64,
        inserted: u(16),
        deleted: u(24),
        quads: u(32),
        generation: u32::from_le_bytes(r[40..44].try_into().unwrap()),
        kind: CommitKind::from_code(r[44]),
        exact: r[45] & 1 != 0,
        bulk: r[45] & 2 != 0,
        reconstructed: r[45] & 4 != 0,
        default_graph: r[45] & 8 == 0,
        unvalidated: r[45] & 16 != 0,
    })
}

fn encode_header(id: uuid::Uuid, first_seq: u64) -> [u8; REC] {
    let mut h = [0u8; REC];
    h[0..8].copy_from_slice(MAGIC);
    h[8..12].copy_from_slice(&1u32.to_le_bytes());
    h[12..16].copy_from_slice(&(REC as u32).to_le_bytes());
    h[16..32].copy_from_slice(id.as_bytes());
    h[32..40].copy_from_slice(&first_seq.to_le_bytes());
    let crc = crc32(&[&h[..60]]);
    h[60..64].copy_from_slice(&crc.to_le_bytes());
    h
}

/// `(dataset id, first seq)` of a valid header.
pub(crate) fn decode_header(h: &[u8]) -> Option<(uuid::Uuid, u64)> {
    (h.len() >= REC
        && &h[0..8] == MAGIC
        && h[8..12] == 1u32.to_le_bytes()
        && h[12..16] == (REC as u32).to_le_bytes()
        && u32::from_le_bytes(h[60..64].try_into().unwrap()) == crc32(&[&h[..60]]))
    .then(|| {
        (
            uuid::Uuid::from_bytes(h[16..32].try_into().unwrap()),
            u64::from_le_bytes(h[32..40].try_into().unwrap()),
        )
    })
}

/// Read the valid, consecutive records of a catalog file without modifying it (used by
/// lock-free readers such as `sparkles log`).
pub fn read_catalog(path: &Path) -> Result<Option<(uuid::Uuid, Vec<CommitInfo>)>> {
    let mut buf = Vec::new();
    match File::open(path) {
        Ok(mut f) => f.read_to_end(&mut buf)?,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e.into()),
    };
    let Some((id, first)) = decode_header(&buf) else {
        return Ok(None);
    };
    let mut out = Vec::new();
    for (i, r) in buf[REC..].as_chunks::<REC>().0.iter().enumerate() {
        match decode_record(r) {
            Some(c) if c.seq == first + i as u64 => out.push(c),
            _ => break,
        }
    }
    Ok(Some((id, out)))
}

/// Stream the same valid consecutive prefix as read_catalog. A caller can retain
/// only the requested commit and head instead of copying an entire catalog.
pub(crate) fn walk_catalog_controlled(
    path: &Path,
    budget: &mut crate::store::cold::ColdBudget<'_>,
    mut visit: impl FnMut(CommitInfo, &mut crate::store::cold::ColdBudget<'_>) -> Result<()>,
) -> Result<Option<(uuid::Uuid, u64, Option<CommitInfo>)>> {
    use std::io::BufReader;
    budget.check()?;
    let file = match File::open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.into()),
    };
    let capacity = (budget.available() / 8).clamp(REC as u64, 64 << 10);
    budget.charge(capacity)?;
    let mut reader = BufReader::with_capacity(capacity as usize, file);
    let result = (|| {
        let mut record = [0; REC];
        match reader.read_exact(&mut record) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::UnexpectedEof => return Ok(None),
            Err(error) => return Err(error.into()),
        }
        budget.check()?;
        // A known primary-file magic with a future version must refuse before
        // any valid-prefix/reconciliation path can treat it as a missing catalog.
        if &record[..8] == MAGIC {
            crate::formats::COMMITS.check(
                "retained commit catalog",
                u32::from_le_bytes(record[8..12].try_into().unwrap()),
                None,
            )?;
        }
        let Some((id, first)) = decode_header(&record) else {
            return Ok(None);
        };
        let mut last = None;
        let mut offset = 0u64;
        loop {
            budget.check()?;
            match reader.read_exact(&mut record) {
                Ok(()) => {}
                Err(error) if error.kind() == std::io::ErrorKind::UnexpectedEof => break,
                Err(error) => return Err(error.into()),
            }
            let Some(commit) = decode_record(&record) else {
                break;
            };
            if first.checked_add(offset) != Some(commit.seq) {
                break;
            }
            visit(commit, budget)?;
            last = Some(commit);
            let Some(next) = offset.checked_add(1) else {
                break;
            };
            offset = next;
        }
        budget.check()?;
        Ok(Some((id, first, last)))
    })();
    drop(reader);
    budget.release(capacity);
    result
}

/// The commit catalog: every retained commit record, in memory, backed by `commits.bin`
/// for persistent stores and bounded to the newest `ring` entries for in-memory ones.
pub(crate) struct Catalog {
    path: Option<PathBuf>,
    file: Option<File>,
    /// seq of `records[0]`
    first: u64,
    records: std::collections::VecDeque<CommitInfo>,
    /// records not yet written to the file after a write error
    pending: Vec<CommitInfo>,
    ring: Option<usize>,
    /// the newest commit that may have changed the default graph: exact while the
    /// records reach back to it, otherwise the commit before the first record
    last_default: u64,
    /// test hook: appends to the file fail
    #[cfg(any(test, feature = "failpoints"))]
    pub(crate) fail_writes: bool,
}

impl Catalog {
    pub fn memory(root: CommitInfo, ring: usize) -> Catalog {
        Catalog {
            path: None,
            file: None,
            first: root.seq,
            records: [root].into(),
            pending: Vec::new(),
            ring: Some(ring.max(1)),
            last_default: root.seq,
            #[cfg(any(test, feature = "failpoints"))]
            fail_writes: false,
        }
    }

    /// Open `commits.bin` and reconcile it with the commits established by the
    /// generation and the WAL: `base` (the generation's commit) and `replayed` (WAL
    /// commits after it, in order). Records beyond the head are dropped, missing ones are
    /// appended; if the file cannot supply history up to `base`, a new one starts at
    /// `base` (the old file is kept aside).
    pub fn open(
        path: &Path,
        dataset_id: uuid::Uuid,
        base: CommitInfo,
        replayed: &[CommitInfo],
    ) -> Result<Catalog> {
        let head = replayed.last().map_or(base.seq, |c| c.seq);
        let existing = read_catalog(path)?;
        let (mut first, mut records) = match existing {
            Some((id, _)) if id != dataset_id => {
                return Err(Error::Corrupt(format!(
                    "{}: belongs to dataset {id}, not {dataset_id}",
                    path.display()
                )));
            }
            Some((_, recs)) => {
                let first = recs.first().map_or(base.seq, |c| c.seq);
                (first, recs)
            }
            None => (base.seq, Vec::new()),
        };
        let valid_len = records.len();
        records.retain(|c| c.seq <= head);
        let dropped = valid_len - records.len();
        if dropped > 0 {
            tracing::warn!(
                target: "sparkles::commit",
                "{}: dropped {dropped} commit records beyond the head {head}",
                path.display()
            );
        }
        let last = records.last().map(|c| c.seq);
        let gap = match last {
            None => first != base.seq,
            Some(l) => l + 1 < base.seq,
        };
        let mut intact = !existing_is_invalid(path)? && dropped == 0;
        if gap {
            // history between the catalog and the generation's base is gone
            if path.exists() {
                let aside = path.with_extension(format!("bin.corrupt-{}", now_ms()));
                tracing::warn!(
                    target: "sparkles::commit",
                    "{}: history before commit {} is missing; keeping the old catalog as {}",
                    path.display(),
                    base.seq,
                    aside.display()
                );
                std::fs::rename(path, aside)?;
            }
            first = base.seq;
            records.clear();
            intact = false;
        }
        let add = |c: &CommitInfo, records: &mut Vec<CommitInfo>| {
            if records.last().is_none_or(|l| c.seq == l.seq + 1) && c.seq >= first {
                records.push(*c);
            }
        };
        if records.last().is_none_or(|l| l.seq < base.seq) {
            add(&base, &mut records);
        }
        for c in replayed {
            if records.last().is_none_or(|l| l.seq < c.seq) {
                add(c, &mut records);
            }
        }
        // the file is kept and appended to only if it is exactly the valid records read
        let mut buf = Vec::with_capacity((records.len() + 1) * REC);
        buf.extend_from_slice(&encode_header(dataset_id, first));
        for c in &records {
            buf.extend_from_slice(&encode_record(c));
        }
        let on_disk = std::fs::metadata(path).map_or(0, |m| m.len() as usize);
        if intact && on_disk == REC * (1 + valid_len) && on_disk <= buf.len() {
            if on_disk < buf.len() {
                let mut f = OpenOptions::new().append(true).open(path)?;
                f.write_all(&buf[on_disk..])?;
                f.sync_data()?;
            }
        } else {
            crate::store::write_synced(path, &buf)?;
        }
        let file = OpenOptions::new().append(true).open(path)?;
        let last_default = records
            .iter()
            .rev()
            .find(|c| c.default_graph)
            .map_or(first.saturating_sub(1), |c| c.seq);
        Ok(Catalog {
            path: Some(path.to_path_buf()),
            file: Some(file),
            first,
            records: records.into(),
            pending: Vec::new(),
            ring: None,
            last_default,
            #[cfg(any(test, feature = "failpoints"))]
            fail_writes: false,
        })
    }

    /// Create a new catalog file holding `root`.
    pub fn create(path: &Path, dataset_id: uuid::Uuid, root: CommitInfo) -> Result<()> {
        let mut buf = encode_header(dataset_id, root.seq).to_vec();
        buf.extend_from_slice(&encode_record(&root));
        crate::store::write_synced(path, &buf)
    }

    /// Append a durable commit, in sequence order under the catalog mutex.
    /// Publication is serialized by the writer or the opt-in prefix coordinator.
    /// A file error is logged and retried with the next append; it never fails the commit.
    pub fn append(&mut self, c: CommitInfo) {
        if c.default_graph {
            self.last_default = c.seq;
        }
        self.records.push_back(c);
        if let Some(ring) = self.ring
            && self.records.len() > ring
        {
            self.records.pop_front();
            self.first += 1;
        }
        if self.file.is_some() {
            self.pending.push(c);
            self.flush_pending();
        }
    }

    fn flush_pending(&mut self) {
        let Some(f) = self.file.as_mut() else {
            return;
        };
        #[cfg(any(test, feature = "failpoints"))]
        if self.fail_writes {
            tracing::error!(target: "sparkles::commit", "commit catalog: write failed (failpoint); will retry");
            return;
        }
        let mut buf = Vec::with_capacity(self.pending.len() * REC);
        for c in &self.pending {
            buf.extend_from_slice(&encode_record(c));
        }
        match f.write_all(&buf).and_then(|_| f.flush()) {
            Ok(()) => self.pending.clear(),
            Err(e) => tracing::error!(
                target: "sparkles::commit",
                "commit catalog {}: {e}; will retry",
                self.path.as_deref().unwrap_or(Path::new("?")).display()
            ),
        }
    }

    /// Make every record durable (before a generation's WAL is discarded).
    pub fn sync(&mut self) -> Result<()> {
        if !self.pending.is_empty() {
            self.flush_pending();
            if !self.pending.is_empty() {
                return Err(Error::Invalid(
                    "the commit catalog could not be written".into(),
                ));
            }
        }
        if let Some(f) = &self.file {
            f.sync_data()?;
        }
        Ok(())
    }

    pub fn complete(&self) -> bool {
        self.pending.is_empty()
    }

    /// Write pending records (without `fsync`) and return the file's length in bytes,
    /// which then ends at the newest record. Fails with `Conflict("catalog-lagging: …")`
    /// (retryable) while the file lags the commits (after a write error). For backups,
    /// which read the file up to this length through their own handle.
    pub fn flushed_len(&mut self) -> Result<u64> {
        if self.path.is_none() {
            return Err(Error::unsupported(
                "the commit catalog of an in-memory store",
            ));
        }
        if !self.pending.is_empty() {
            self.flush_pending();
        }
        if !self.pending.is_empty() {
            return Err(Error::Conflict(format!(
                "catalog-lagging: the commit catalog lags {} commits after a write error; retry later",
                self.pending.len()
            )));
        }
        match &self.file {
            Some(f) => Ok(f.metadata()?.len()),
            None => Err(Error::unsupported("the commit catalog has no file")),
        }
    }

    /// Whether a commit after `after`, up to `at`, may have changed the default graph. A
    /// commit the catalog no longer holds counts as a change.
    pub fn default_graph_changed(&self, after: u64, at: u64) -> bool {
        if self.last_default <= after {
            return false;
        }
        if self.last_default <= at {
            return true;
        }
        (after + 1..=at).any(|s| self.get(s).is_none_or(|c| c.default_graph))
    }

    pub fn get(&self, seq: u64) -> Option<CommitInfo> {
        let i = seq.checked_sub(self.first)?;
        self.records.get(i as usize).copied()
    }

    /// The last retained commit at or before `ms` (timestamps never decrease).
    pub fn at_time(&self, ms: i64) -> Option<CommitInfo> {
        let i = self.records.partition_point(|c| c.timestamp_ms <= ms);
        i.checked_sub(1).and_then(|i| self.records.get(i).copied())
    }

    /// The first retained commit made at or after `ms` (timestamps never decrease).
    pub fn first_at_or_after(&self, ms: i64) -> Option<u64> {
        let i = self.records.partition_point(|c| c.timestamp_ms < ms);
        self.records.get(i).map(|c| c.seq)
    }

    /// Drop the records of the commits before `cutoff` (never the newest one). A
    /// persistent catalog is rewritten to a new file that replaces the old one
    /// atomically, so readers without the lock see either. Returns the records dropped.
    pub fn prune_before(&mut self, cutoff: u64) -> Result<u64> {
        let last = self.records.back().map_or(self.first, |c| c.seq);
        let cutoff = cutoff.min(last);
        if cutoff <= self.first {
            return Ok(0);
        }
        let n = cutoff - self.first;
        if let Some(path) = self.path.clone() {
            self.flush_pending();
            if !self.pending.is_empty() {
                return Err(Error::Invalid(
                    "the commit catalog could not be written".into(),
                ));
            }
            let mut f = OpenOptions::new().read(true).open(&path)?;
            let mut h = [0u8; REC];
            f.read_exact(&mut h)?;
            let Some((id, _)) = decode_header(&h) else {
                return Err(Error::Corrupt(format!(
                    "{}: the header is damaged",
                    path.display()
                )));
            };
            let mut buf = Vec::with_capacity((self.records.len() - n as usize + 1) * REC);
            buf.extend_from_slice(&encode_header(id, cutoff));
            for c in self.records.iter().skip(n as usize) {
                buf.extend_from_slice(&encode_record(c));
            }
            crate::store::write_atomic(&path, &buf)?;
            self.file = Some(OpenOptions::new().append(true).open(&path)?);
        }
        self.records.drain(..n as usize);
        self.first = cutoff;
        Ok(n)
    }

    /// The first retained record.
    pub fn first(&self) -> Option<CommitInfo> {
        self.records.front().copied()
    }

    /// The last commit made in generation `generation`, if the catalog has one.
    pub fn last_in_generation(&self, generation: u32) -> Option<u64> {
        self.records
            .iter()
            .rev()
            .find(|c| c.generation == generation)
            .map(|c| c.seq)
    }

    pub fn page(&self, range: CommitRange, limit: usize) -> CommitPage {
        let n = self.records.len();
        let idx = |seq: u64| seq.saturating_sub(self.first).min(n as u64) as usize;
        let commits: Vec<CommitInfo> = match range {
            CommitRange::Latest => self.records.iter().rev().take(limit).copied().collect(),
            CommitRange::Before(b) => self
                .records
                .range(..idx(b))
                .rev()
                .take(limit)
                .copied()
                .collect(),
            CommitRange::After(a) => {
                let start = if a < self.first { 0 } else { idx(a + 1) };
                self.records.range(start..).take(limit).copied().collect()
            }
        };
        CommitPage {
            commits,
            first_retained: self.first,
            complete: self.complete(),
        }
    }
}

/// Whether an existing catalog file has an unreadable header (to be rewritten).
fn existing_is_invalid(path: &Path) -> Result<bool> {
    let mut f = match File::open(path) {
        Ok(f) => f,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(e) => return Err(e.into()),
    };
    let mut h = [0u8; REC];
    f.seek(SeekFrom::Start(0))?;
    Ok(f.read_exact(&mut h).is_err() || decode_header(&h).is_none())
}

// ------------------------------------------------------------ WAL records ------

/// Version byte of a WAL commit record carrying commit metadata.
pub(crate) const WAL_COMMIT_V2: u8 = 2;

/// The flag of byte 27 of a WAL commit record: the write bypassed write-time
/// validation. Records written before it existed have zero there.
pub(crate) const WAL_FLAG_UNVALIDATED: u8 = 1;

/// The flag of byte 27 of a WAL commit record: the commit has an annotation (a message or
/// a change digest) that was appended to `annotations.bin` without its own sync, because
/// the commit belongs to a group commit and the group's fence syncs both files. Only
/// records of such commits carry it. Replay uses it to find a commit that reached the disk
/// without its annotation (see `annotations`). Readers that predate it ignore the bit.
pub(crate) const WAL_FLAG_ANNOTATED: u8 = 2;

/// Fill bytes 9..33 of a WAL commit record (`rec[0]` = op, `rec[1..9]` = next blank
/// node): seq, timestamp, kind, version, flags (byte 27) and a CRC over the
/// transaction's data records followed by bytes 0..29 of this record. Byte 28 is zero,
/// which says that every commit before this one was durable when it was written.
pub(crate) fn seal_wal_commit(
    rec: &mut [u8; 33],
    seq: u64,
    ts: i64,
    kind: CommitKind,
    flags: u8,
    data: &[u8],
) {
    seal_wal_commit_unfenced(rec, seq, ts, kind, flags, 0, data);
}

/// [`seal_wal_commit`] for a commit written while the `unfenced` commits just before it
/// may not have reached the disk yet, as group commit does. The count goes in byte 28.
/// Replay reads it with [`wal_commit_unfenced`] to learn which commits were durable
/// when the record was written. Releases that predate the count wrote zero there, and
/// they synced every commit before writing the next one, so zero is accurate for them.
pub(crate) fn seal_wal_commit_unfenced(
    rec: &mut [u8; 33],
    seq: u64,
    ts: i64,
    kind: CommitKind,
    flags: u8,
    unfenced: u8,
    data: &[u8],
) {
    rec[9..17].copy_from_slice(&seq.to_le_bytes());
    rec[17..25].copy_from_slice(&ts.to_le_bytes());
    rec[25] = kind.code();
    rec[26] = WAL_COMMIT_V2;
    rec[27] = flags;
    rec[28] = unfenced;
    let crc = crc32(&[data, &rec[..29]]);
    rec[29..33].copy_from_slice(&crc.to_le_bytes());
}

/// How many commits just before a version-2 WAL commit record may not have been
/// durable when the record was written (see [`seal_wal_commit_unfenced`]). Only a
/// record whose checksum matches says anything.
pub(crate) fn wal_commit_unfenced(rec: &[u8]) -> u64 {
    rec[28] as u64
}

/// Commit metadata of a WAL commit record (seq, timestamp, kind and flags): `None` for
/// a legacy record (version 0), `Some(Err)` for a version-2 record whose CRC does not
/// match.
pub(crate) fn open_wal_commit(
    rec: &[u8],
    data: &[u8],
) -> Option<Result<(u64, i64, CommitKind, u8), ()>> {
    if rec[26] != WAL_COMMIT_V2 {
        return None;
    }
    let mut crc = flate2::Crc::new();
    crc.update(data);
    open_wal_commit_with_crc(rec, crc)
}

/// The identical commit codec after a controlled streaming pass over its data.
/// A decoded record is metadata, not proof of admission or actor authenticity.
pub(crate) fn open_wal_commit_with_crc(
    rec: &[u8],
    mut data_crc: flate2::Crc,
) -> Option<Result<(u64, i64, CommitKind, u8), ()>> {
    if rec[26] != WAL_COMMIT_V2 {
        return None;
    }
    let crc = u32::from_le_bytes(rec[29..33].try_into().unwrap());
    data_crc.update(&rec[..29]);
    if crc != data_crc.sum() {
        return Some(Err(()));
    }
    Some(Ok((
        u64::from_le_bytes(rec[9..17].try_into().unwrap()),
        i64::from_le_bytes(rec[17..25].try_into().unwrap()),
        CommitKind::from_code(rec[25]),
        rec[27],
    )))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn controlled_dataset_identity_preserves_version_precedence_and_read_only_bytes() {
        use crate::history::{HistoryOptions, ReadLimit};
        use crate::store::cold::ColdBudget;
        let root = tempfile::tempdir().unwrap();
        let id = uuid::Uuid::new_v4();
        let options = HistoryOptions::default();
        let original = dataset_file_bytes(id, "create", 1234);
        for bytes in [
            original.clone(),
            format!(
                "{{\"format\":99,{}",
                std::str::from_utf8(&original)
                    .unwrap()
                    .trim_start_matches('{')
            )
            .into_bytes(),
            br#"{"format":99,"id":false}"#.to_vec(),
            br#"{"format":1,"minimumReader":4294967295,"id":false}"#.to_vec(),
            br#"{"format":1,"minimumReader":[],"id":false}"#.to_vec(),
            br#"{"format":1,"minimumReader":1,"id":false}"#.to_vec(),
        ] {
            std::fs::write(root.path().join("dataset.json"), &bytes).unwrap();
            let mut budget = ColdBudget::new(ReadLimit {
                options: &options,
                memory: Some(4 << 20),
                held: 0,
            })
            .unwrap();
            let actual = read_dataset_file_controlled(root.path(), &mut budget);
            match read_dataset_file(root.path()) {
                Ok(expected) => assert_eq!(actual.unwrap(), expected),
                Err(Error::Unsupported(_)) => assert!(matches!(actual, Err(Error::Unsupported(_)))),
                Err(Error::Corrupt(_)) => assert!(matches!(actual, Err(Error::Corrupt(_)))),
                other => panic!("unexpected ordinary decode {other:?}"),
            }
            assert_eq!(budget.used(), 0);
            assert_eq!(
                std::fs::read(root.path().join("dataset.json")).unwrap(),
                bytes
            );
        }
        std::fs::write(root.path().join("dataset.json"), &original).unwrap();
        let mut tiny = ColdBudget::new(ReadLimit {
            options: &options,
            memory: Some(1),
            held: 0,
        })
        .unwrap();
        assert!(matches!(
            read_dataset_file_controlled(root.path(), &mut tiny),
            Err(Error::BudgetExceeded(_))
        ));
        assert_eq!(tiny.used(), 0);
        let cancel = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let controlled = HistoryOptions {
            cancel: Some(cancel.clone()),
            deadline: None,
        };
        let mut budget = ColdBudget::new(ReadLimit {
            options: &controlled,
            memory: Some(4 << 20),
            held: 0,
        })
        .unwrap();
        cancel.store(true, std::sync::atomic::Ordering::Relaxed);
        assert!(matches!(
            read_dataset_file_controlled(root.path(), &mut budget),
            Err(Error::Cancelled)
        ));
        assert_eq!(budget.used(), 0);
        assert_eq!(
            std::fs::read(root.path().join("dataset.json")).unwrap(),
            original
        );
    }

    #[test]
    fn controlled_dataset_extensions_are_preserved_on_disk_and_discarded_cooperatively() {
        use crate::history::{HistoryOptions, ReadLimit};
        use crate::store::cold::ColdBudget;
        use std::sync::{
            Arc,
            atomic::{AtomicBool, Ordering},
        };
        let root = tempfile::tempdir().unwrap();
        let id = uuid::Uuid::new_v4();
        let mut value: serde_json::Value =
            serde_json::from_slice(&dataset_file_bytes(id, "create", 1234)).unwrap();
        let object = value.as_object_mut().unwrap();
        for n in 0..4000 {
            object.insert(
                format!("extension{n}"),
                serde_json::json!([n, {"unknown": [null, false]}]),
            );
        }
        let bytes = serde_json::to_vec(&value).unwrap();
        std::fs::write(root.path().join("dataset.json"), &bytes).unwrap();
        let cancel = Arc::new(AtomicBool::new(false));
        let options = HistoryOptions {
            cancel: Some(cancel.clone()),
            deadline: None,
        };
        let mut budget = ColdBudget::new(ReadLimit {
            options: &options,
            memory: Some(128 << 20),
            held: 0,
        })
        .unwrap();
        assert_eq!(
            read_dataset_file_controlled(root.path(), &mut budget).unwrap(),
            Some(id)
        );
        assert_eq!(budget.used(), 0);
        // Ordinary codecs continue retaining extensions for subsequent rewrites.
        assert_eq!(
            read_dataset(root.path()).unwrap().unwrap().extra.len(),
            4000
        );
        let observer = |stage: &str, at: usize| {
            if stage == "metadata value decode" && at == 50 {
                cancel.store(true, Ordering::Relaxed);
            }
        };
        budget.observe_keys(&observer);
        assert!(matches!(
            read_dataset_file_controlled(root.path(), &mut budget),
            Err(Error::Cancelled)
        ));
        assert_eq!(budget.used(), 0);
        assert_eq!(
            std::fs::read(root.path().join("dataset.json")).unwrap(),
            bytes
        );
        cancel.store(false, Ordering::Relaxed);
        for field in ["format", "minimumReader"] {
            let bytes = format!("{{\"{field}\":\"{}\"}}", "x".repeat(512 << 10)).into_bytes();
            std::fs::write(root.path().join("dataset.json"), &bytes).unwrap();
            let error = read_dataset_file_controlled(root.path(), &mut budget).unwrap_err();
            assert!(matches!(error, Error::Corrupt(_)));
            assert!(error.to_string().len() < 128);
            assert_eq!(budget.used(), 0);
        }
    }

    #[test]
    fn controlled_catalog_refuses_future_primary_version_without_reconciliation() {
        use crate::history::{HistoryOptions, ReadLimit};
        use crate::store::cold::ColdBudget;
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("commits.bin");
        let mut bytes = encode_header(uuid::Uuid::new_v4(), 10);
        bytes[8..12].copy_from_slice(&99u32.to_le_bytes());
        let checksum = crc32(&[&bytes[..60]]);
        bytes[60..].copy_from_slice(&checksum.to_le_bytes());
        std::fs::write(&path, bytes).unwrap();
        let options = HistoryOptions::default();
        let mut budget = ColdBudget::new(ReadLimit {
            options: &options,
            memory: Some(4096),
            held: 0,
        })
        .unwrap();
        let mut visits = 0;
        assert!(matches!(
            walk_catalog_controlled(&path, &mut budget, |_, _| {
                visits += 1;
                Ok(())
            }),
            Err(Error::Unsupported(_))
        ));
        assert_eq!(visits, 0);
        assert_eq!(budget.used(), 0);
        assert_eq!(std::fs::read(&path).unwrap(), bytes);
    }

    fn sample_commit(seq: u64) -> CommitInfo {
        CommitInfo {
            seq,
            timestamp_ms: 1_700_000_000_000 + seq as i64,
            kind: CommitKind::Update,
            inserted: 2,
            deleted: 1,
            quads: 100 + seq,
            generation: 7,
            bulk: false,
            exact: true,
            reconstructed: false,
            default_graph: false,
            unvalidated: true,
        }
    }

    #[test]
    fn controlled_generation_commit_uses_the_existing_physical_codec_and_releases_scratch() {
        use crate::{
            history::{HistoryOptions, ReadLimit},
            store::cold::ColdBudget,
        };
        let dir = tempfile::tempdir().unwrap();
        let id = uuid::Uuid::new_v4();
        let commit = sample_commit(42);
        let path = dir.path().join("commit.json");
        std::fs::write(&path, gen_commit_bytes(id, "compaction", &commit)).unwrap();
        set_wal_strict_from(dir.path(), 33 * 8).unwrap();
        let before = std::fs::read(&path).unwrap();
        let options = HistoryOptions::default();
        let mut budget = ColdBudget::new(ReadLimit {
            options: &options,
            memory: Some(1 << 20),
            held: 0,
        })
        .unwrap();
        let loaded = read_gen_commit_controlled(dir.path(), &mut budget)
            .unwrap()
            .unwrap();
        let normal = read_gen_commit(dir.path()).unwrap().unwrap();
        assert_eq!(
            (
                loaded.value.dataset_id,
                loaded.value.commit,
                loaded.value.origin.as_str()
            ),
            (normal.0, normal.1, normal.2.as_str())
        );
        assert_eq!(loaded.value.wal_strict_from, Some(33 * 8));
        assert_eq!(budget.used(), loaded.charge);
        drop(loaded.value);
        budget.release(loaded.charge);
        assert_eq!(budget.used(), 0);
        assert_eq!(before, std::fs::read(&path).unwrap());
        let mut value: serde_json::Value = serde_json::from_slice(&before).unwrap();
        value["format"] = 123.into();
        std::fs::write(&path, serde_json::to_vec(&value).unwrap()).unwrap();
        assert!(matches!(
            read_gen_commit_controlled(dir.path(), &mut budget),
            Err(Error::Unsupported(_))
        ));
    }

    #[test]
    fn controlled_catalog_streams_large_valid_prefix_without_retaining_the_catalog() {
        use crate::{
            history::{HistoryOptions, ReadLimit},
            store::cold::ColdBudget,
        };
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("commits.bin");
        let id = uuid::Uuid::new_v4();
        let mut bytes = encode_header(id, 10).to_vec();
        for seq in 10..10_010 {
            bytes.extend_from_slice(&encode_record(&sample_commit(seq)));
        }
        // Later valid records cannot skip an invalid entry in the consecutive prefix.
        bytes[REC * 6001 + 20] ^= 1;
        std::fs::write(&path, &bytes).unwrap();
        let normal = read_catalog(&path).unwrap().unwrap();
        assert_eq!(normal.1.len(), 6000);
        let options = HistoryOptions::default();
        let mut budget = ColdBudget::new(ReadLimit {
            options: &options,
            memory: Some(4096),
            held: 0,
        })
        .unwrap();
        let mut count = 0;
        let mut selected = None;
        let summary = walk_catalog_controlled(&path, &mut budget, |commit, _| {
            assert_eq!(commit, normal.1[count]);
            count += 1;
            if commit.seq == 42 {
                selected = Some(commit);
            }
            Ok(())
        })
        .unwrap()
        .unwrap();
        assert_eq!(summary, (id, 10, normal.1.last().copied()));
        assert_eq!(count, 6000);
        assert_eq!(selected, Some(sample_commit(42)));
        assert_eq!(budget.used(), 0);
        assert_eq!(bytes, std::fs::read(&path).unwrap());
    }

    #[test]
    fn controlled_catalog_checks_inside_scan_and_releases_temporary_buffer_on_cancel() {
        use crate::{
            history::{HistoryOptions, ReadLimit},
            store::cold::ColdBudget,
        };
        use std::sync::{
            Arc,
            atomic::{AtomicBool, Ordering},
        };
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("commits.bin");
        let mut bytes = encode_header(uuid::Uuid::new_v4(), 0).to_vec();
        for seq in 0..200 {
            bytes.extend_from_slice(&encode_record(&sample_commit(seq)));
        }
        std::fs::write(&path, &bytes).unwrap();
        let cancel = Arc::new(AtomicBool::new(false));
        let options = HistoryOptions {
            cancel: Some(cancel.clone()),
            deadline: None,
        };
        let mut budget = ColdBudget::new(ReadLimit {
            options: &options,
            memory: Some(4096),
            held: 0,
        })
        .unwrap();
        let mut visits = 0;
        let result = walk_catalog_controlled(&path, &mut budget, |_, _| {
            visits += 1;
            if visits == 8 {
                cancel.store(true, Ordering::Relaxed);
            }
            Ok(())
        });
        assert!(matches!(result, Err(Error::Cancelled)));
        assert_eq!(visits, 8);
        assert_eq!(budget.used(), 0);
        assert_eq!(bytes, std::fs::read(&path).unwrap());
    }

    #[test]
    fn timestamps_round_trip() {
        for ms in [0, 1_727_704_991_482, 951_782_400_000, -1, 4_102_444_799_999] {
            let s = rfc3339_ms(ms);
            assert_eq!(parse_rfc3339_ms(&s), Some(ms), "{s}");
        }
        assert_eq!(rfc3339_ms(1_727_704_991_482), "2024-09-30T14:03:11.482Z");
    }

    #[test]
    fn records_round_trip_and_detect_damage() {
        let c = CommitInfo {
            seq: 42,
            timestamp_ms: 1_727_704_991_482,
            kind: CommitKind::GspPut,
            inserted: 3,
            deleted: 1,
            quads: 1204,
            generation: 7,
            bulk: false,
            exact: true,
            reconstructed: false,
            default_graph: true,
            unvalidated: false,
        };
        let mut r = encode_record(&c);
        assert_eq!(decode_record(&r), Some(c));
        // a record without the flag bit (older versions) may have changed the default graph
        assert_eq!(r[45] & 8, 0);
        let named_only = CommitInfo {
            default_graph: false,
            unvalidated: false,
            ..c
        };
        assert_eq!(decode_record(&encode_record(&named_only)), Some(named_only));
        r[20] ^= 1;
        assert_eq!(decode_record(&r), None);
        for k in CommitKind::ALL {
            assert_eq!(CommitKind::from_code(k.code()), k);
            assert_eq!(CommitKind::from_name(k.name()), Some(k));
        }
    }

    #[test]
    fn a_newer_dataset_json_with_other_members_is_unsupported() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("dataset.json"),
            br#"{"format": 2, "minimumReader": 9, "id": 5}"#,
        )
        .unwrap();
        let e = read_dataset(dir.path()).expect_err("refused");
        assert!(matches!(e, Error::Unsupported(_)), "{e:?}");
        let e =
            dataset_id_of(br#"{"format": 2, "minimumReader": 9, "id": 5}"#).expect_err("refused");
        assert!(matches!(e, Error::Unsupported(_)), "{e:?}");
    }

    #[test]
    fn unknown_dataset_members_survive_a_write_open() {
        use crate::store::{Store, StoreOptions};
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("db");
        drop(Store::open(&root, StoreOptions::default()).unwrap());
        let path = root.join("dataset.json");
        let edit = |f: &dyn Fn(&mut serde_json::Value)| {
            let mut v: serde_json::Value =
                serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
            f(&mut v);
            std::fs::write(&path, serde_json::to_vec_pretty(&v).unwrap()).unwrap();
        };
        let read = || -> serde_json::Value {
            serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap()
        };
        edit(&|v| {
            v["futureMember"] = serde_json::json!({"a": 1});
            v["writtenBy"] = "9.9.9".into();
        });
        drop(Store::open(&root, StoreOptions::default()).unwrap());
        let v = read();
        assert_eq!(v["writtenBy"], crate::formats::SPARKLES_VERSION);
        assert_eq!(v["futureMember"], serde_json::json!({"a": 1}));
        // a member that a later release added never shadows a known one
        assert_eq!(v["format"], 1);
        assert_eq!(v["minimumReader"], 5);
        // raising the reader keeps it too
        edit(&|v| v["minimumReader"] = 1.into());
        require_reader(&root, 5).unwrap();
        let v = read();
        assert_eq!(v["minimumReader"], 5);
        assert_eq!(v["futureMember"], serde_json::json!({"a": 1}));
    }
}
