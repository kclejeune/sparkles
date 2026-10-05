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
    /// a WAL commit written by an older version after this one had upgraded the database
    Unknown,
}

impl CommitKind {
    const ALL: [CommitKind; 17] = [
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

/// Dataset capabilities supported by this reader (1: legacy, 2: branches).
pub const DATASET_READER: u32 = 2;
fn legacy_reader() -> u32 {
    1
}

fn check_dataset(f: DatasetFile) -> Result<DatasetFile> {
    if f.format != 1 || f.minimum_reader > DATASET_READER {
        return Err(Error::Unsupported(format!(
            "dataset format {} requires reader {}, this build supports format 1 and reader {}",
            f.format, f.minimum_reader, DATASET_READER
        )));
    }
    Ok(f)
}

/// Check compatibility without changing any database files.
pub fn check_dataset_compatibility(root: &Path) -> Result<()> {
    read_dataset(root).map(|_| ())
}

/// Publish the minimum reader needed by branches before publishing their table.
pub(crate) fn require_branch_reader(root: &Path) -> Result<()> {
    let Some(mut ds) = read_dataset(root)? else {
        return Ok(());
    };
    if ds.minimum_reader < 2 {
        ds.minimum_reader = 2;
        write_dataset(root, &ds)?;
    }
    Ok(())
}

#[derive(serde::Serialize, serde::Deserialize)]
struct DatasetFile {
    format: u32,
    #[serde(default = "legacy_reader", rename = "minimumReader")]
    minimum_reader: u32,
    id: uuid::Uuid,
    created: String,
    origin: String,
    /// the dataset and commit a clone was made from
    #[serde(
        default,
        rename = "forkedFrom",
        skip_serializing_if = "Option::is_none"
    )]
    forked_from: Option<ForkedFrom>,
    /// the backup a restored dataset was made from
    #[serde(
        default,
        rename = "restoredFrom",
        skip_serializing_if = "Option::is_none"
    )]
    restored_from: Option<RestoredFrom>,
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
        let mut f: GenCommitFile = serde_json::from_slice(&b)
            .map_err(|e| Error::Corrupt(format!("{}: {e}", path.display())))?;
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
        Ok(b) => check_dataset(
            serde_json::from_slice(&b).map_err(|e| Error::Corrupt(format!("dataset.json: {e}")))?,
        )
        .map(Some),
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
        minimum_reader: 1,
        id,
        created: rfc3339_ms(created_ms),
        origin: "clone".to_string(),
        forked_from: Some(from),
        restored_from: None,
    })
    .unwrap()
}

/// The dataset id of `dataset.json`'s content.
pub(crate) fn dataset_id_of(bytes: &[u8]) -> Result<uuid::Uuid> {
    let f: DatasetFile =
        serde_json::from_slice(bytes).map_err(|e| Error::Corrupt(format!("dataset.json: {e}")))?;
    Ok(check_dataset(f)?.id)
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
        minimum_reader: 1,
        id,
        created: rfc3339_ms(created_ms),
        origin: origin.to_string(),
        forked_from: None,
        restored_from: None,
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
    let bad = |m: &str| Error::Corrupt(format!("{}: {m}", path.display()));
    let f: GenCommitFile = serde_json::from_slice(&b).map_err(|e| bad(&e.to_string()))?;
    let c = f.commit;
    Ok(Some((
        f.dataset_id,
        CommitInfo {
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
        f.origin,
    )))
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

    /// Append a commit (called with the writer lock held, after the commit is durable).
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

/// Fill bytes 9..33 of a WAL commit record (`rec[0]` = op, `rec[1..9]` = next blank
/// node): seq, timestamp, kind, version, flags (byte 27) and a CRC over the
/// transaction's data records followed by bytes 0..29 of this record.
pub(crate) fn seal_wal_commit(
    rec: &mut [u8; 33],
    seq: u64,
    ts: i64,
    kind: CommitKind,
    flags: u8,
    data: &[u8],
) {
    rec[9..17].copy_from_slice(&seq.to_le_bytes());
    rec[17..25].copy_from_slice(&ts.to_le_bytes());
    rec[25] = kind.code();
    rec[26] = WAL_COMMIT_V2;
    rec[27] = flags;
    rec[28] = 0;
    let crc = crc32(&[data, &rec[..29]]);
    rec[29..33].copy_from_slice(&crc.to_le_bytes());
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
    let crc = u32::from_le_bytes(rec[29..33].try_into().unwrap());
    if crc != crc32(&[data, &rec[..29]]) {
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
}
