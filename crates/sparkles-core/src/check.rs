//! Read-only integrity check of a database directory (`sparkles check`).
//!
//! [`check`] verifies the files [`Store::open`](crate::store::Store::open) reads, without
//! opening the store: it takes no lock, truncates no WAL, repairs no catalog and rebuilds
//! no full-text index, so it can run next to a server that holds the database. Every file
//! is only read (the index files through read-only memory maps).
//!
//! Each check ends `ok`, `warning` or `error`:
//!
//! * **error**: damage that `Store::open` refuses, that loses acknowledged data or
//!   history, or that queries would read wrongly (a block that does not decode, keys out
//!   of order, permutations that disagree, a WAL checksum mismatch before the last
//!   transaction);
//! * **warning**: a state that open handles by itself (a torn final WAL transaction,
//!   which is truncated; a lagging commit catalog, which is rebuilt; a full-text index
//!   that is caught up or rebuilt), or leftovers of interrupted operations.
//!
//! A server writing meanwhile can cause transient warnings (an in-flight transaction
//! looks like a torn tail; the catalog or the full-text index lags the WAL), never errors.
//! The catalog is read before the WAL for that reason. A compaction during the check
//! switches `CURRENT`; that is reported, and the check should be run again.
//!
//! `--quick` checks metadata only: block metadata instead of decoding every block, the
//! first key of each vocabulary block instead of every key, and segment files' presence
//! instead of their checksums. The WAL and the catalog are checked fully in both modes.

use crate::builder::{FORMAT_VERSION, IndexMeta, Stats};
use crate::commit::{self, CommitInfo};
use crate::error::{Error, Result};
use crate::id::{Id, Tag};
use crate::index::{
    BLOCK_ROWS, BlockMeta, Key, META_BYTES, Perm, decode_column, read_varint_checked,
};
use crate::store::{WAL_COMMIT, WAL_DELETE, WAL_INSERT, WAL_REC};
use crate::vocab::{FC_BLOCK, delta_entries};
use rayon::prelude::*;
use serde::Serialize;
use std::path::{Path, PathBuf};
use std::time::Instant;

/// Issues listed per check; further ones are only counted.
const MAX_ISSUES: usize = 50;

/// What [`check`] does.
#[derive(Clone, Debug, Default)]
pub struct CheckOptions {
    /// Metadata only (see the module documentation).
    pub quick: bool,
}

/// Outcome of one check, and of the whole report (the worst of its checks).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Status {
    Ok,
    Warning,
    Error,
}

impl Status {
    pub fn name(self) -> &'static str {
        match self {
            Status::Ok => "ok",
            Status::Warning => "warning",
            Status::Error => "error",
        }
    }
}

/// One problem found by a check, with where it is.
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Issue {
    /// `warning` or `error`
    pub status: Status,
    pub message: String,
    /// path relative to the database directory
    #[serde(skip_serializing_if = "Option::is_none")]
    pub file: Option<String>,
    /// byte offset in `file`
    #[serde(skip_serializing_if = "Option::is_none")]
    pub offset: Option<u64>,
    /// block number (permutation or vocabulary block)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub block: Option<u64>,
    /// row within the block, or record number
    #[serde(skip_serializing_if = "Option::is_none")]
    pub row: Option<u64>,
    /// commit sequence number
    #[serde(skip_serializing_if = "Option::is_none")]
    pub seq: Option<u64>,
    /// a term id (`tag:payload`) or vocabulary id
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
}

impl Issue {
    fn new(status: Status, message: impl Into<String>) -> Issue {
        Issue {
            status,
            message: message.into(),
            file: None,
            offset: None,
            block: None,
            row: None,
            seq: None,
            id: None,
        }
    }
    fn error(message: impl Into<String>) -> Issue {
        Issue::new(Status::Error, message)
    }
    fn warning(message: impl Into<String>) -> Issue {
        Issue::new(Status::Warning, message)
    }
    fn file(mut self, f: impl Into<String>) -> Issue {
        self.file = Some(f.into());
        self
    }
    fn offset(mut self, o: u64) -> Issue {
        self.offset = Some(o);
        self
    }
    fn block(mut self, b: usize) -> Issue {
        self.block = Some(b as u64);
        self
    }
    fn row(mut self, r: u64) -> Issue {
        self.row = Some(r);
        self
    }
    fn seq(mut self, s: u64) -> Issue {
        self.seq = Some(s);
        self
    }
    fn id(mut self, id: impl Into<String>) -> Issue {
        self.id = Some(id.into());
        self
    }
}

/// One named check.
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Check {
    /// `layout`, `generation`, `vocabulary`, `delta-vocabulary`, `perm.spo` … `perm.gspo`,
    /// `permutations`, `wal`, `catalog`, `text`, `geo`, `reasoning`
    pub name: String,
    pub status: Status,
    /// what was checked, in one line
    pub summary: String,
    pub errors: usize,
    pub warnings: usize,
    pub millis: f64,
    /// the first issues (at most 50); `errors` and `warnings` count all of them
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub issues: Vec<Issue>,
}

/// The result of [`check`].
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CheckReport {
    pub root: PathBuf,
    /// `full` or `quick`
    pub mode: &'static str,
    /// the worst status of any check
    pub status: Status,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dataset_id: Option<uuid::Uuid>,
    /// the generation `CURRENT` names
    #[serde(skip_serializing_if = "Option::is_none")]
    pub generation: Option<String>,
    /// the head commit: the last intact WAL commit, or the generation's base commit
    #[serde(skip_serializing_if = "Option::is_none")]
    pub head: Option<u64>,
    /// quads in the generation's base index (before the WAL)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub index_quads: Option<u64>,
    /// issues over all checks
    pub errors: usize,
    pub warnings: usize,
    pub millis: f64,
    pub checks: Vec<Check>,
}

impl CheckReport {
    /// Process exit code for the command line: 0 clean, 1 errors, 2 warnings only.
    pub fn exit_code(&self) -> i32 {
        match self.status {
            Status::Ok => 0,
            Status::Error => 1,
            Status::Warning => 2,
        }
    }

    /// The check with this name.
    pub fn get(&self, name: &str) -> Option<&Check> {
        self.checks.iter().find(|c| c.name == name)
    }

    /// Plain-text rendering: one line per check, its issues indented below it, and a
    /// summary line.
    pub fn to_text(&self) -> String {
        use std::fmt::Write;
        let mut out = String::new();
        let _ = writeln!(
            out,
            "sparkles check {} ({})",
            self.root.display(),
            self.mode
        );
        let width = self.checks.iter().map(|c| c.name.len()).max().unwrap_or(0);
        for c in &self.checks {
            let _ = writeln!(
                out,
                "{:<7}  {:<width$}  {}  ({})",
                c.status.name(),
                c.name,
                c.summary,
                fmt_ms(c.millis)
            );
            for i in &c.issues {
                let mut at = Vec::new();
                if let Some(f) = &i.file {
                    at.push(f.clone());
                }
                if let Some(b) = i.block {
                    at.push(format!("block {b}"));
                }
                if let Some(r) = i.row {
                    at.push(format!("row {r}"));
                }
                if let Some(o) = i.offset {
                    at.push(format!("offset {o}"));
                }
                if let Some(s) = i.seq {
                    at.push(format!("commit {s}"));
                }
                if let Some(id) = &i.id {
                    at.push(format!("id {id}"));
                }
                let at = if at.is_empty() {
                    String::new()
                } else {
                    format!(" [{}]", at.join(", "))
                };
                let _ = writeln!(out, "         - {}: {}{at}", i.status.name(), i.message);
            }
            let more = c.errors + c.warnings - c.issues.len();
            if more > 0 {
                let _ = writeln!(out, "         - … and {more} more");
            }
        }
        let _ = writeln!(
            out,
            "{}: {} error{}, {} warning{} in {} checks ({})",
            self.status.name(),
            self.errors,
            if self.errors == 1 { "" } else { "s" },
            self.warnings,
            if self.warnings == 1 { "" } else { "s" },
            self.checks.len(),
            fmt_ms(self.millis)
        );
        out
    }
}

fn fmt_ms(ms: f64) -> String {
    if ms >= 1000.0 {
        format!("{:.2} s", ms / 1000.0)
    } else {
        format!("{ms:.1} ms")
    }
}

/// A check being run.
struct Run {
    name: String,
    t0: Instant,
    status: Status,
    errors: usize,
    warnings: usize,
    issues: Vec<Issue>,
}

impl Run {
    fn new(name: impl Into<String>) -> Run {
        Run {
            name: name.into(),
            t0: Instant::now(),
            status: Status::Ok,
            errors: 0,
            warnings: 0,
            issues: Vec::new(),
        }
    }
    fn add(&mut self, i: Issue) {
        self.status = self.status.max(i.status);
        match i.status {
            Status::Error => self.errors += 1,
            Status::Warning => self.warnings += 1,
            Status::Ok => {}
        }
        if self.issues.len() < MAX_ISSUES {
            self.issues.push(i);
        }
    }
    fn extend(&mut self, issues: impl IntoIterator<Item = Issue>) {
        for i in issues {
            self.add(i);
        }
    }
    fn millis(&self) -> f64 {
        self.t0.elapsed().as_secs_f64() * 1000.0
    }
    fn done(self, summary: impl Into<String>) -> Check {
        Check {
            name: self.name,
            status: self.status,
            summary: summary.into(),
            errors: self.errors,
            warnings: self.warnings,
            millis: self.t0.elapsed().as_secs_f64() * 1000.0,
            issues: self.issues,
        }
    }
}

/// Check the database at `root` without modifying it. Fails only when `root` is not a
/// readable directory; everything else is reported in the [`CheckReport`].
pub fn check(root: &Path, opts: &CheckOptions) -> Result<CheckReport> {
    let t0 = Instant::now();
    let md =
        std::fs::metadata(root).map_err(|e| Error::Invalid(format!("{}: {e}", root.display())))?;
    if !md.is_dir() {
        return Err(Error::Invalid(format!(
            "{} is not a directory",
            root.display()
        )));
    }
    let mut c = Checker {
        root,
        full: !opts.quick,
        checks: Vec::new(),
        current: None,
        dataset_id: None,
        base: None,
        origin: None,
        meta: None,
        vocab_len: None,
        dvocab_len: None,
        wal: None,
    };
    c.layout();
    if let Some(gen_name) = c.current.clone() {
        let dir = root.join(&gen_name);
        c.generation(&dir, &gen_name);
        let mut vocab = c.vocabulary(&dir, &gen_name);
        c.delta_vocabulary(&dir, &gen_name);
        let ids = c.permutations(&dir, &gen_name);
        if c.full {
            let refs = ids.checked;
            vocab.run.extend(ids.issues);
            if vocab.run.errors == 0 {
                vocab.summary.push_str(&format!(
                    "; {refs} ids in the permutations checked against it"
                ));
            }
        }
        let mut done = vocab.run.done(vocab.summary);
        done.millis = vocab.millis;
        c.checks.push(done);
        // the catalog lags the WAL, so it is read first; the full-text index too
        let catalog = std::fs::read(root.join("commits.bin"));
        let t_text = Instant::now();
        let text = crate::text::probe(root, c.full);
        let text_ms = t_text.elapsed().as_secs_f64() * 1000.0;
        c.wal_check(&dir, &gen_name);
        c.catalog(catalog);
        c.text(text);
        if let Some(t) = c.checks.iter_mut().find(|x| x.name == "text") {
            t.millis += text_ms;
        }
        let t_geo = Instant::now();
        c.geo(crate::geo::probe(root, c.full));
        if let Some(t) = c.checks.iter_mut().find(|x| x.name == "geo") {
            t.millis += t_geo.elapsed().as_secs_f64() * 1000.0;
        }
        c.vector(root, &dir);
    }
    c.reasoning();
    c.branches();
    // a compaction while checking: the files read may belong to different generations
    if let Some(before) = &c.current {
        let now = std::fs::read_to_string(root.join("CURRENT")).map(|s| s.trim().to_string());
        if now.as_deref().ok() != Some(before.as_str()) {
            let i = Issue::warning(format!(
                "CURRENT changed during the check (from {before}): a compaction or bulk load ran; check again"
            ))
            .file("CURRENT");
            if let Some(layout) = c.checks.iter_mut().find(|x| x.name == "layout") {
                layout.status = layout.status.max(Status::Warning);
                layout.warnings += 1;
                layout.issues.push(i);
            }
        }
    }
    let order = |n: &str| {
        let names = ["layout", "generation", "vocabulary", "delta-vocabulary"];
        names
            .iter()
            .position(|x| *x == n)
            .unwrap_or(if n.starts_with("perm.") { 4 } else { 5 })
    };
    c.checks.sort_by_key(|x| order(&x.name));
    let status = c
        .checks
        .iter()
        .map(|x| x.status)
        .max()
        .unwrap_or(Status::Ok);
    Ok(CheckReport {
        root: root.to_path_buf(),
        mode: if opts.quick { "quick" } else { "full" },
        status,
        dataset_id: c.dataset_id,
        generation: c.current.clone(),
        head: c.wal.as_ref().map(|w| w.head).or(c.base.map(|b| b.seq)),
        index_quads: c.meta.as_ref().map(|m| m.quads),
        errors: c.checks.iter().map(|x| x.errors).sum(),
        warnings: c.checks.iter().map(|x| x.warnings).sum(),
        millis: t0.elapsed().as_secs_f64() * 1000.0,
        checks: c.checks,
    })
}

struct Checker<'a> {
    root: &'a Path,
    full: bool,
    checks: Vec<Check>,
    /// the generation `CURRENT` names, when it exists
    current: Option<String>,
    dataset_id: Option<uuid::Uuid>,
    /// the commit the generation's base index holds (`commit.json`)
    base: Option<CommitInfo>,
    /// `origin` of `commit.json`
    origin: Option<String>,
    meta: Option<IndexMeta>,
    vocab_len: Option<u64>,
    dvocab_len: Option<u64>,
    wal: Option<WalInfo>,
}

/// What the WAL holds, as replay would see it.
struct WalInfo {
    /// the last commit replay would keep
    head: u64,
    /// (seq, timestamp, kind code) of the commits replay would keep, in order
    commits: Vec<(u64, i64, u8)>,
}

/// The vocabulary check, finished after the permutations report their id references.
struct VocabRun {
    run: Run,
    summary: String,
    /// time of the vocabulary pass itself (the check is finished after the permutations)
    millis: f64,
}

/// Issues of a run of vocabulary blocks, and its first and last keys.
type ChunkScan = (Vec<Issue>, Option<(Vec<u8>, Vec<u8>)>);

/// Id references found while decoding the permutations.
#[derive(Default)]
struct IdRefs {
    checked: u64,
    issues: Vec<Issue>,
}

fn is_gen_name(n: &str) -> bool {
    n.strip_prefix("gen-")
        .is_some_and(|d| !d.is_empty() && d.bytes().all(|b| b.is_ascii_digit()))
}

/// `tag:payload` of an id.
fn id_str(id: Id) -> String {
    format!("{:?}", id)
}

/// What is wrong with `id` at quad position `pos` (`0..4` = s, p, o, g), if anything.
/// `delta_len` is `None` where delta-vocabulary ids cannot occur (the base index).
fn id_problem(
    id: Id,
    pos: usize,
    vocab_len: Option<u64>,
    delta_len: Option<u64>,
) -> Option<String> {
    let raw_tag = id.0 >> crate::id::PAYLOAD_BITS;
    if raw_tag > Tag::Date as u64 {
        return Some(format!("unknown id tag {raw_tag}"));
    }
    let tag = id.tag();
    let ok_kind = match pos {
        0 => matches!(tag, Tag::Vocab | Tag::Delta | Tag::BNode),
        1 => matches!(tag, Tag::Vocab | Tag::Delta),
        2 => !matches!(tag, Tag::Undef | Tag::Special | Tag::Local),
        _ => id == Id::DEFAULT_GRAPH || matches!(tag, Tag::Vocab | Tag::Delta | Tag::BNode),
    };
    let what = ["subject", "predicate", "object", "graph"][pos];
    if !ok_kind {
        return Some(format!("{what} id of kind {tag:?} cannot occur there"));
    }
    match tag {
        Tag::Vocab => vocab_len.filter(|&n| id.payload() >= n).map(|n| {
            format!(
                "{what} vocabulary id {} is beyond the vocabulary ({n} terms)",
                id.payload()
            )
        }),
        Tag::Delta => match delta_len {
            None => Some(format!(
                "{what} is a delta-vocabulary id, which the base index never holds"
            )),
            Some(n) if id.payload() >= n => Some(format!(
                "{what} delta-vocabulary id {} is beyond the delta vocabulary ({n} terms)",
                id.payload()
            )),
            Some(_) => None,
        },
        _ => None,
    }
}

/// splitmix64 finalizer
#[inline]
fn mix(mut x: u64) -> u64 {
    x ^= x >> 30;
    x = x.wrapping_mul(0xbf58_476d_1ce4_e5b9);
    x ^= x >> 27;
    x = x.wrapping_mul(0x94d0_49bb_1331_11eb);
    x ^ (x >> 31)
}

/// Two independent 64-bit hashes of a quad (in `[s, p, o, g]` order). Summed over a
/// permutation they identify its set of quads whatever the sort order.
#[inline]
fn quad_hash(q: &[Id; 4]) -> (u64, u64) {
    let (mut a, mut b) = (0x243f_6a88_85a3_08d3u64, 0x1319_8a2e_0370_7344u64);
    for id in q {
        a = mix(a ^ id.0);
        b = mix(b.wrapping_add(id.0).rotate_left(29));
    }
    (a, b)
}

/// Mmap a file read-only (`None` for an empty file).
fn map(path: &Path) -> std::io::Result<Option<memmap2::Mmap>> {
    let f = std::fs::File::open(path)?;
    if f.metadata()?.len() == 0 {
        return Ok(None);
    }
    // SAFETY: generation files are immutable once written; the map is read only.
    Ok(Some(unsafe { memmap2::Mmap::map(&f)? }))
}

impl Checker<'_> {
    // ------------------------------------------------------------------ layout ------

    fn layout(&mut self) {
        let mut run = Run::new("layout");
        let root = self.root;
        match std::fs::read(root.join("CURRENT")) {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => run.add(
                Issue::error("CURRENT is missing: not a database, or its creation never completed")
                    .file("CURRENT"),
            ),
            Err(e) => run.add(Issue::error(format!("CURRENT: {e}")).file("CURRENT")),
            Ok(b) => {
                let name = String::from_utf8_lossy(&b).trim().to_string();
                if !is_gen_name(&name) {
                    run.add(
                        Issue::error(format!("CURRENT names {name:?}, not a generation"))
                            .file("CURRENT"),
                    );
                } else if !root.join(&name).is_dir() {
                    run.add(
                        Issue::error(format!("CURRENT names {name}, which does not exist"))
                            .file("CURRENT"),
                    );
                } else {
                    self.current = Some(name);
                }
            }
        }
        match commit::read_dataset_file(root) {
            Ok(Some(id)) => self.dataset_id = Some(id),
            Ok(None) => run.add(
                Issue::warning(
                    "dataset.json is missing: a database from an older version, which gets a dataset id and a baseline commit when opened",
                )
                .file("dataset.json"),
            ),
            Err(e) => run.add(Issue::error(e.to_string()).file("dataset.json")),
        }
        if let Some(name) = &self.current {
            let file = format!("{name}/commit.json");
            match commit::read_gen_commit(&root.join(name)) {
                Ok(Some((id, c, origin))) => {
                    if let Some(ds) = self.dataset_id
                        && id != ds
                    {
                        run.add(
                            Issue::error(format!(
                                "belongs to dataset {id}, not {ds}: open refuses the database"
                            ))
                            .file(&file),
                        );
                    }
                    self.base = Some(c);
                    self.origin = Some(origin);
                }
                Ok(None) if self.dataset_id.is_some() => run.add(
                    Issue::warning("missing: open records the generation as a baseline commit")
                        .file(&file),
                ),
                Ok(None) => {}
                Err(e) => run.add(Issue::error(e.to_string()).file(&file)),
            }
        }
        if let Ok(b) = std::fs::read(root.join("prefixes.json"))
            && let Err(e) = serde_json::from_slice::<std::collections::BTreeMap<String, String>>(&b)
        {
            run.add(
                Issue::warning(format!(
                    "does not parse ({e}): open falls back to the generation's prefixes"
                ))
                .file("prefixes.json"),
            );
        }
        // leftovers of interrupted operations
        let cur_no = self.current.as_deref().map(commit::generation_number);
        let mut names: Vec<(String, bool)> = std::fs::read_dir(root)
            .map(|rd| {
                rd.flatten()
                    .map(|e| {
                        (
                            e.file_name().to_string_lossy().into_owned(),
                            e.file_type().is_ok_and(|t| t.is_dir()),
                        )
                    })
                    .collect()
            })
            .unwrap_or_default();
        names.sort();
        // generations kept on purpose: named snapshots and the retention window
        let retained = match (self.dataset_id, cur_no) {
            (Some(id), Some(c)) => {
                crate::history::retained_offline(root, id, c).unwrap_or_default()
            }
            _ => Default::default(),
        };
        for (n, is_dir) in &names {
            if *is_dir && is_gen_name(n) && retained.contains_key(&commit::generation_number(n)) {
                continue;
            }
            let msg = if n.ends_with(".tmp") {
                Some("a temporary file of an interrupted write; safe to remove".to_string())
            } else if n == "text.new" || n == "text.old" {
                Some("left by an interrupted full-text rebuild; removed on open".to_string())
            } else if n.ends_with(".deleting") {
                Some("left by an interrupted deletion; safe to remove".to_string())
            } else if n.starts_with("commits.bin.corrupt-") {
                Some("a damaged commit catalog an earlier open set aside".to_string())
            } else if *is_dir && is_gen_name(n) && Some(n.as_str()) != self.current.as_deref() {
                let no = commit::generation_number(n);
                Some(if cur_no.is_some_and(|c| no < c) {
                    "an old generation whose removal after a compaction was interrupted; safe to remove".to_string()
                } else {
                    "newer than CURRENT: an interrupted rebuild, replaced by the next one"
                        .to_string()
                })
            } else {
                None
            };
            if let Some(m) = msg {
                run.add(Issue::warning(format!("leftover: {m}")).file(n.as_str()));
            }
        }
        if let Some(name) = &self.current
            && root.join(name).join("tmp").exists()
        {
            run.add(
                Issue::warning(
                    "leftover: temporary files of an interrupted index build; safe to remove",
                )
                .file(format!("{name}/tmp")),
            );
        }
        let summary = match (&self.current, self.dataset_id) {
            (Some(g), Some(id)) => format!("CURRENT names {g}; dataset {id}"),
            (Some(g), None) => format!("CURRENT names {g}"),
            (None, _) => "no usable generation".to_string(),
        };
        self.checks.push(run.done(summary));
    }

    // -------------------------------------------------------------- generation ------

    fn generation(&mut self, dir: &Path, name: &str) {
        let mut run = Run::new("generation");
        let meta_file = format!("{name}/meta.json");
        match std::fs::read(dir.join("meta.json")) {
            Err(e) => run.add(Issue::error(e.to_string()).file(&meta_file)),
            Ok(b) => match serde_json::from_slice::<IndexMeta>(&b) {
                Err(e) => run.add(Issue::error(format!("does not parse: {e}")).file(&meta_file)),
                Ok(m) => {
                    if m.format_version != FORMAT_VERSION {
                        run.add(
                            Issue::error(format!(
                                "index format {} but this build reads {FORMAT_VERSION}: dump and reload the data",
                                m.format_version
                            ))
                            .file(&meta_file),
                        );
                    }
                    self.meta = Some(m);
                }
            },
        }
        let stats_file = format!("{name}/stats.json");
        match std::fs::read(dir.join("stats.json")) {
            Err(e) => run.add(Issue::error(e.to_string()).file(&stats_file)),
            Ok(b) => match serde_json::from_slice::<Stats>(&b) {
                Err(e) => run.add(Issue::error(format!("does not parse: {e}")).file(&stats_file)),
                Ok(s) => {
                    if let Some(m) = &self.meta
                        && s.quads != m.quads
                    {
                        run.add(
                            Issue::warning(format!(
                                "counts {} quads, meta.json {}: planner statistics are off",
                                s.quads, m.quads
                            ))
                            .file(&stats_file),
                        );
                    }
                }
            },
        }
        if let (Some(b), Some(m), Some(origin)) = (self.base, &self.meta, &self.origin)
            && origin != "baseline"
            && b.quads != m.quads
        {
            run.add(
                Issue::warning(format!(
                    "the base commit {} counts {} quads, the index holds {}",
                    b.seq, b.quads, m.quads
                ))
                .file(format!("{name}/commit.json"))
                .seq(b.seq),
            );
        }
        let summary = match (&self.meta, self.base, &self.origin) {
            (Some(m), Some(b), Some(o)) => format!(
                "{name}: {} quads, {} terms; base commit {} ({o})",
                m.quads, m.terms, b.seq
            ),
            (Some(m), _, _) => format!("{name}: {} quads, {} terms", m.quads, m.terms),
            _ => format!("{name}: no usable metadata"),
        };
        self.checks.push(run.done(summary));
    }

    // -------------------------------------------------------------- vocabulary ------

    fn vocabulary(&mut self, dir: &Path, name: &str) -> VocabRun {
        let mut run = Run::new("vocabulary");
        let off_file = format!("{name}/vocab.off");
        let dat_file = format!("{name}/vocab.dat");
        let off = match std::fs::read(dir.join("vocab.off")) {
            Ok(b) => b,
            Err(e) => {
                run.add(Issue::error(e.to_string()).file(&off_file));
                return VocabRun {
                    millis: run.millis(),
                    run,
                    summary: "unreadable".into(),
                };
            }
        };
        let data = match map(&dir.join("vocab.dat")) {
            Ok(m) => m,
            Err(e) => {
                run.add(Issue::error(e.to_string()).file(&dat_file));
                return VocabRun {
                    millis: run.millis(),
                    run,
                    summary: "unreadable".into(),
                };
            }
        };
        let data: &[u8] = data.as_deref().unwrap_or(&[]);
        if off.len() < 8 || off.len() % 8 != 0 {
            run.add(
                Issue::error(format!(
                    "{} bytes: not a list of 8-byte offsets and a count",
                    off.len()
                ))
                .file(&off_file),
            );
            return VocabRun {
                millis: run.millis(),
                run,
                summary: "unreadable".into(),
            };
        }
        let u64_at = |i: usize| u64::from_le_bytes(off[i * 8..i * 8 + 8].try_into().unwrap());
        let count = u64_at(off.len() / 8 - 1);
        let nb = off.len() / 8 - 1;
        let mut summary = format!("{count} terms in {nb} blocks");
        if nb as u64 != count.div_ceil(FC_BLOCK as u64) {
            run.add(
                Issue::error(format!(
                    "{nb} blocks cannot hold {count} terms ({FC_BLOCK} per block)"
                ))
                .file(&off_file),
            );
            return VocabRun {
                millis: run.millis(),
                run,
                summary,
            };
        }
        if let Some(m) = &self.meta
            && m.terms != count
        {
            run.add(
                Issue::error(format!("{count} terms, but meta.json counts {}", m.terms))
                    .file(&off_file),
            );
        }
        let offsets: Vec<usize> = (0..nb).map(|b| u64_at(b) as usize).collect();
        for b in 0..nb {
            let bad = offsets[b] >= data.len() || (b > 0 && offsets[b] <= offsets[b - 1]);
            if bad {
                run.add(
                    Issue::error(format!(
                        "block {b} starts at byte {}, out of order or past the end of vocab.dat ({} bytes)",
                        offsets[b],
                        data.len()
                    ))
                    .file(&off_file)
                    .block(b),
                );
                return VocabRun {
                    millis: run.millis(),
                    run,
                    summary,
                };
            }
        }
        // ids are usable from here on (the order is what makes them comparable)
        self.vocab_len = Some(count);
        let block_len = |b: usize| FC_BLOCK.min(count as usize - b * FC_BLOCK);
        if self.full {
            // every key, blocks in parallel chunks; a block must end where the next begins
            let chunk = 4096;
            let parts: Vec<ChunkScan> = (0..nb.div_ceil(chunk))
                .into_par_iter()
                .map(|ci| {
                    let mut issues = Vec::new();
                    let mut first: Option<Vec<u8>> = None;
                    let mut key: Vec<u8> = Vec::new();
                    let mut have_prev = false;
                    for b in ci * chunk..((ci + 1) * chunk).min(nb) {
                        let end = if b + 1 < nb {
                            offsets[b + 1]
                        } else {
                            data.len()
                        };
                        let mut pos = offsets[b];
                        let n = block_len(b);
                        for i in 0..n {
                            let id = (b * FC_BLOCK + i) as u64;
                            let at = pos as u64;
                            let parsed = read_varint_checked(data, &mut pos).and_then(|shared| {
                                let len = read_varint_checked(data, &mut pos)?;
                                Some((shared as usize, len as usize))
                            });
                            let Some((shared, len)) = parsed.filter(|&(s, l)| {
                                (i > 0 || s == 0)
                                    && s <= key.len()
                                    && pos.checked_add(l).is_some_and(|e| e <= end)
                            }) else {
                                issues.push(
                                    Issue::error(format!(
                                        "term {id} is not a valid front-coded entry"
                                    ))
                                    .block(b)
                                    .offset(at)
                                    .id(id.to_string()),
                                );
                                // the rest of the block cannot be decoded
                                have_prev = false;
                                break;
                            };
                            let prev = std::mem::take(&mut key);
                            key.extend_from_slice(&prev[..shared]);
                            key.extend_from_slice(&data[pos..pos + len]);
                            pos += len;
                            if have_prev && key <= prev {
                                issues.push(
                                    Issue::error(format!(
                                        "term {id} does not sort after term {}",
                                        id - 1
                                    ))
                                    .block(b)
                                    .offset(at)
                                    .id(id.to_string()),
                                );
                            }
                            if first.is_none() {
                                first = Some(key.clone());
                            }
                            have_prev = true;
                        }
                        if have_prev && pos != end {
                            issues.push(
                                Issue::error(format!(
                                    "block {b} ends at byte {pos}, the next starts at {end}"
                                ))
                                .block(b)
                                .offset(pos as u64),
                            );
                        }
                    }
                    let ends = first.map(|f| (f, key));
                    (issues, ends)
                })
                .collect();
            let mut prev_last: Option<Vec<u8>> = None;
            for (ci, (issues, ends)) in parts.into_iter().enumerate() {
                run.extend(issues.into_iter().map(|i| i.file(&dat_file)));
                if let Some((first, last)) = ends {
                    if let Some(p) = &prev_last
                        && first <= *p
                    {
                        let id = ci * chunk * FC_BLOCK;
                        run.add(
                            Issue::error(format!("term {id} does not sort after term {}", id - 1))
                                .file(&dat_file)
                                .block(ci * chunk)
                                .id(id.to_string()),
                        );
                    }
                    prev_last = Some(last);
                }
            }
            summary.push_str(", every key checked for order");
        } else {
            // the first key of each block: what lookups binary-search
            let mut prev: Option<&[u8]> = None;
            for (b, &start) in offsets.iter().enumerate() {
                let mut pos = start;
                let first = read_varint_checked(data, &mut pos)
                    .filter(|&s| s == 0)
                    .and_then(|_| read_varint_checked(data, &mut pos))
                    .and_then(|len| data.get(pos..pos + len as usize));
                match first {
                    None => run.add(
                        Issue::error(format!("the first key of block {b} is not a valid entry"))
                            .file(&dat_file)
                            .block(b)
                            .offset(start as u64),
                    ),
                    Some(k) => {
                        if prev.is_some_and(|p| k <= p) {
                            run.add(
                                Issue::error(format!(
                                    "block {b} does not sort after block {}",
                                    b - 1
                                ))
                                .file(&dat_file)
                                .block(b)
                                .offset(start as u64),
                            );
                        }
                        prev = Some(k);
                    }
                }
            }
            summary.push_str(", block first keys checked for order");
        }
        match crate::vocab::verify_sparse_index(dir, &offsets, data) {
            None => {}
            Some(Ok(n)) => summary.push_str(&format!(", sparse index of {n} keys")),
            // the server does without an index it cannot read, but would use one that
            // reads well and names other keys
            Some(Err((error, m))) => run.add(
                if error {
                    Issue::error(m)
                } else {
                    Issue::warning(m)
                }
                .file(format!("{name}/vocab.idx")),
            ),
        }
        match crate::vocab::verify_numeric_column(dir, self.full) {
            None => {}
            Some(Ok(n)) => summary.push_str(&format!(", numeric column of {n} values")),
            Some(Err((error, m))) => run.add(
                if error {
                    Issue::error(m)
                } else {
                    Issue::warning(m)
                }
                .file(format!("{name}/{}", crate::vocab::numeric::FILE)),
            ),
        }
        VocabRun {
            millis: run.millis(),
            run,
            summary,
        }
    }

    fn delta_vocabulary(&mut self, dir: &Path, name: &str) {
        let mut run = Run::new("delta-vocabulary");
        let file = format!("{name}/delta.vocab");
        let buf = match std::fs::read(dir.join("delta.vocab")) {
            Ok(b) => b,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Vec::new(),
            Err(e) => {
                run.add(Issue::error(e.to_string()).file(&file));
                self.checks.push(run.done("unreadable"));
                return;
            }
        };
        let (keys, pos) = delta_entries(&buf);
        if pos != buf.len() {
            run.add(
                Issue::warning(format!(
                    "{} bytes of a torn entry at the end: truncated on open",
                    buf.len() - pos
                ))
                .file(&file)
                .offset(pos as u64),
            );
        }
        if self.full {
            let mut seen: rustc_hash::FxHashMap<&[u8], usize> = Default::default();
            for (i, k) in keys.iter().enumerate() {
                if let Some(j) = seen.insert(k, i) {
                    run.add(
                        Issue::error(format!(
                            "entry {i} repeats entry {j}: every later delta id would shift on open"
                        ))
                        .file(&file)
                        .id(i.to_string()),
                    );
                }
            }
        }
        self.dvocab_len = Some(keys.len() as u64);
        self.checks
            .push(run.done(format!("{} terms added by updates", keys.len())));
    }

    // ------------------------------------------------------------ permutations ------

    fn permutations(&mut self, dir: &Path, name: &str) -> IdRefs {
        let full = self.full;
        let quads = self.meta.as_ref().map(|m| m.quads);
        let vocab_len = self.vocab_len;
        let scans: Vec<PermScan> = Perm::ALL
            .par_iter()
            .map(|&p| scan_perm(dir, name, p, quads, vocab_len, full))
            .collect();
        let t0 = Instant::now();
        let mut refs = IdRefs::default();
        let mut run = Run::new("permutations");
        let rows: Vec<(Perm, u64)> = scans
            .iter()
            .filter_map(|s| s.rows.map(|r| (s.perm, r)))
            .collect();
        if let Some(&(_, r0)) = rows.first()
            && rows.iter().any(|&(_, r)| r != r0)
        {
            let list: Vec<String> = rows
                .iter()
                .map(|(p, r)| format!("{} {r}", p.name()))
                .collect();
            run.add(Issue::error(format!(
                "the permutations hold different numbers of rows: {}",
                list.join(", ")
            )));
        }
        let mut summary = match rows.first() {
            Some((_, r)) => format!("{} permutations, {r} rows each (metadata)", rows.len()),
            None => "no readable permutation".into(),
        };
        if full {
            let hashes: Vec<(Perm, (u64, u64))> = scans
                .iter()
                .filter_map(|s| s.hash.map(|h| (s.perm, h)))
                .collect();
            let hash_of = |p: Perm| hashes.iter().find(|(q, _)| *q == p).map(|(_, h)| *h);
            // the most common hash stands for the intended set of quads
            let mut votes: Vec<((u64, u64), usize)> = Vec::new();
            for (_, h) in &hashes {
                match votes.iter_mut().find(|(v, _)| v == h) {
                    Some(v) => v.1 += 1,
                    None => votes.push((*h, 1)),
                }
            }
            votes.sort_by_key(|v| std::cmp::Reverse(v.1));
            let majority = votes
                .first()
                .filter(|v| v.1 * 2 > hashes.len())
                .map(|v| v.0);
            if let (Some(spo), Some(gspo)) = (hash_of(Perm::Spo), hash_of(Perm::Gspo))
                && spo != gspo
            {
                run.add(Issue::error("SPO and GSPO do not hold the same quads"));
            }
            for (p, h) in &hashes {
                if majority.is_none_or(|m| m != *h) {
                    run.add(
                        Issue::error(format!(
                            "{} does not hold the same quads as the other permutations",
                            p.name()
                        ))
                        .file(format!("{name}/{}.dat", p.name())),
                    );
                }
            }
            let skipped = Perm::ALL.len() - hashes.len();
            summary = if skipped == 0 {
                match rows.first() {
                    Some((_, r)) => format!("all 7 permutations hold the same {r} quads"),
                    None => summary,
                }
            } else {
                format!(
                    "{} of 7 permutations compared ({skipped} could not be decoded)",
                    hashes.len()
                )
            };
        }
        let elapsed = t0.elapsed().as_secs_f64() * 1000.0;
        for s in scans {
            refs.checked += s.ids_checked;
            refs.issues.extend(s.id_issues);
            self.checks.push(s.check);
        }
        let mut check = run.done(summary);
        check.millis = elapsed;
        self.checks.push(check);
        refs
    }

    // --------------------------------------------------------------------- WAL ------

    fn wal_check(&mut self, dir: &Path, name: &str) {
        let mut run = Run::new("wal");
        let file = format!("{name}/wal.log");
        let buf = match std::fs::read(dir.join("wal.log")) {
            Ok(b) => b,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                run.add(Issue::warning("missing: created on open").file(&file));
                Vec::new()
            }
            Err(e) => {
                run.add(Issue::error(e.to_string()).file(&file));
                self.checks.push(run.done("unreadable"));
                return;
            }
        };
        // a writer syncs a commit's new delta terms together with its WAL records, so
        // after a crash only commits that were not durable yet can name terms that
        // delta.vocab lacks (a torn tail). Counted again now, it covers every commit
        // before them.
        let dvocab_len = match std::fs::read(dir.join("delta.vocab")) {
            Ok(b) => Some(delta_entries(&b).0.len() as u64),
            Err(_) => self.dvocab_len,
        };
        // the commit the base holds; a generation without commit.json starts a baseline
        let base_seq = self.base.map(|b| b.seq);
        let base_ts = self.base.map_or(i64::MIN, |b| b.timestamp_ms);
        let fold_legacy = match (&self.origin, self.dataset_id) {
            (Some(o), Some(_)) => o == "baseline",
            _ => true,
        };
        // zero bytes after the last record are space preallocated for the next commits
        let zero = |r: &[u8]| r.iter().all(|&b| b == 0);
        let logical = buf
            .iter()
            .rposition(|&b| b != 0)
            .map_or(0, |p| (p + 1).next_multiple_of(WAL_REC).min(buf.len()));
        let preallocated = buf.len() - logical;
        let buf = &buf[..logical];
        let recs = buf.as_chunks::<WAL_REC>().0;
        // whether damage at record `i` is a torn tail that open truncates, by the rule
        // open uses
        let torn_at = |i: usize, prev_seq: Option<u64>| {
            crate::store::wal_torn_from(buf, i, prev_seq.map_or(0, |s| s + 1))
        };
        let (mut prev_seq, mut prev_ts) = (base_seq, base_ts);
        let mut commits: Vec<(u64, i64, u8)> = Vec::new();
        let (mut folded, mut data_recs) = (0usize, 0usize);
        let mut txn_start = 0usize;
        let mut good = 0usize;
        let mut seen_v2 = false;
        let mut torn = false;
        let mut ids_checked = 0u64;
        // the id problems of the current transaction, reported when its commit record
        // shows whether it is torn; and whether one is a delta id beyond delta.vocab
        let mut txn_issues: Vec<Issue> = Vec::new();
        let mut txn_missing_terms = false;
        for (i, rec) in recs.iter().enumerate() {
            let at = (i * WAL_REC) as u64;
            let q: [Id; 4] = std::array::from_fn(|j| {
                Id(u64::from_le_bytes(
                    rec[1 + j * 8..9 + j * 8].try_into().unwrap(),
                ))
            });
            match rec[0] {
                WAL_INSERT | WAL_DELETE => {
                    data_recs += 1;
                    for (pos, id) in q.iter().enumerate() {
                        ids_checked += 1;
                        if let Some(p) = id_problem(*id, pos, self.vocab_len, dvocab_len) {
                            txn_missing_terms |= id.tag() == Tag::Delta
                                && dvocab_len.is_some_and(|n| id.payload() >= n);
                            txn_issues.push(
                                Issue::error(format!("record {i}: {p}"))
                                    .file(&file)
                                    .offset(at)
                                    .row(i as u64)
                                    .id(id_str(*id)),
                            );
                        }
                    }
                }
                WAL_COMMIT => {
                    let data = &buf[txn_start * WAL_REC..i * WAL_REC];
                    let n = i - txn_start;
                    let n = format!("{n} data record{}", if n == 1 { "" } else { "s" });
                    let meta = commit::open_wal_commit(rec, data);
                    let issues = std::mem::take(&mut txn_issues);
                    let missing_terms = std::mem::take(&mut txn_missing_terms);
                    // damage that no later commit record shows was durable is a torn tail
                    let torn_here = (matches!(meta, Some(Err(())))
                        || (missing_terms && matches!(meta, Some(Ok(_)))))
                        && torn_at(i, prev_seq);
                    if torn_here && missing_terms && matches!(meta, Some(Ok(_))) {
                        run.add(
                            Issue::warning(format!(
                                "the transaction ({n} from offset {}) names delta terms that delta.vocab lacks, and no later commit shows it was durable: the terms did not reach the disk before a crash, so it is treated as torn and truncated on open",
                                txn_start * WAL_REC
                            ))
                            .file(&file)
                            .offset(at)
                            .row(i as u64),
                        );
                        torn = true;
                        break;
                    }
                    // a torn transaction is truncated whole, whatever ids it names
                    if !torn_here {
                        for issue in issues {
                            run.add(issue);
                        }
                    }
                    match meta {
                        Some(Err(())) if torn_here => {
                            run.add(
                                Issue::warning(format!(
                                    "the transaction ({n} from offset {}) fails its checksum, and no later commit shows it was durable: it is treated as torn and truncated on open",
                                    txn_start * WAL_REC
                                ))
                                .file(&file)
                                .offset(at)
                                .row(i as u64),
                            );
                            torn = true;
                            break;
                        }
                        Some(Err(())) => {
                            run.add(
                                Issue::error(format!(
                                    "checksum mismatch in the transaction ({n} from offset {}) ending at byte {}: open refuses the database",
                                    txn_start * WAL_REC,
                                    (i + 1) * WAL_REC
                                ))
                                .file(&file)
                                .offset(at)
                                .row(i as u64),
                            );
                            // go on as if it were the next commit, to find further damage
                            let seq = prev_seq.map_or(0, |s| s + 1);
                            prev_seq = Some(seq);
                            commits.push((seq, prev_ts, 255));
                        }
                        Some(Ok((seq, ts, kind, _))) => {
                            seen_v2 = true;
                            let expect = prev_seq.map_or(0, |s| s + 1);
                            if seq != expect {
                                run.add(
                                    Issue::error(match prev_seq {
                                        Some(p) => format!("commit {seq} follows commit {p}: open refuses the database"),
                                        None => format!("commit {seq} is the first commit, expected {expect}"),
                                    })
                                    .file(&file)
                                    .offset(at)
                                    .seq(seq),
                                );
                            }
                            if ts < prev_ts {
                                run.add(
                                    Issue::warning(format!(
                                        "commit {seq} is timestamped before the commit it follows"
                                    ))
                                    .file(&file)
                                    .offset(at)
                                    .seq(seq),
                                );
                            }
                            prev_seq = Some(seq);
                            prev_ts = prev_ts.max(ts);
                            commits.push((seq, ts, kind.code()));
                        }
                        // a record from an older version: part of the baseline, or the next
                        _ if fold_legacy && !seen_v2 => folded += 1,
                        _ => {
                            let seq = prev_seq.map_or(0, |s| s + 1);
                            prev_seq = Some(seq);
                            commits.push((seq, prev_ts, 255));
                        }
                    }
                    good = (i + 1) * WAL_REC;
                    txn_start = i + 1;
                }
                op => {
                    if !torn_at(i, prev_seq) {
                        run.add(
                            Issue::error(format!(
                                "record {i} has unknown type {op}, and a later commit record shows its transaction was durable: open refuses the database"
                            ))
                            .file(&file)
                            .offset(at)
                            .row(i as u64),
                        );
                        continue;
                    }
                    let what = if zero(rec) {
                        "is zeros, so its transaction did not reach the disk whole".to_string()
                    } else {
                        format!(
                            "has unknown type {op}, and no later commit shows its transaction was durable"
                        )
                    };
                    run.add(
                        Issue::warning(format!(
                            "record {i} {what}: the tail from offset {good} is truncated on open"
                        ))
                        .file(&file)
                        .offset(at)
                        .row(i as u64),
                    );
                    torn = true;
                    break;
                }
            }
        }
        // records after the last commit are truncated on open: the terms they name may
        // not have reached delta.vocab, and other problems are reported as before
        if !torn {
            for issue in txn_issues {
                if !issue.message.contains("beyond the delta vocabulary") {
                    run.add(issue);
                }
            }
        }
        if !torn && good < recs.len() * WAL_REC {
            run.add(
                Issue::warning(format!(
                    "{} records after the last commit form an incomplete transaction: truncated on open",
                    recs.len() - good / WAL_REC
                ))
                .file(&file)
                .offset(good as u64),
            );
        }
        let partial = buf.len() % WAL_REC;
        if partial != 0 {
            run.add(
                Issue::warning(format!(
                    "{partial} bytes of a torn record at the end: truncated on open"
                ))
                .file(&file)
                .offset((buf.len() - partial) as u64),
            );
        }
        let head = prev_seq.unwrap_or(0);
        let mut summary = format!("{} records, {} commits", recs.len(), commits.len());
        if let (Some(f), Some(l)) = (commits.first(), commits.last()) {
            summary.push_str(&format!(" ({}..{})", f.0, l.0));
        }
        if folded > 0 {
            summary.push_str(&format!(
                " and {folded} older-version commits that open folds into a baseline"
            ));
        }
        summary.push_str(&format!("; head {head}"));
        if preallocated > 0 {
            summary.push_str(&format!("; {preallocated} bytes preallocated"));
        }
        if self.full || data_recs > 0 {
            summary.push_str(&format!("; {ids_checked} ids checked"));
        }
        self.wal = Some(WalInfo { head, commits });
        self.checks.push(run.done(summary));
    }

    // ----------------------------------------------------------------- catalog ------

    fn catalog(&mut self, bytes: std::io::Result<Vec<u8>>) {
        let mut run = Run::new("catalog");
        let file = "commits.bin";
        let base = self.base;
        let head = self
            .wal
            .as_ref()
            .map_or(base.map_or(0, |b| b.seq), |w| w.head);
        let lost = |s: u64| {
            if s > 0 {
                format!("; the history before commit {s} cannot be recovered")
            } else {
                String::new()
            }
        };
        let bytes = match bytes {
            Ok(b) => b,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                let from = base.map_or(0, |b| b.seq);
                run.add(
                    Issue::warning(format!("missing: rebuilt on open{}", lost(from))).file(file),
                );
                self.checks.push(run.done("missing"));
                return;
            }
            Err(e) => {
                run.add(Issue::error(e.to_string()).file(file));
                self.checks.push(run.done("unreadable"));
                return;
            }
        };
        let Some((id, first)) = commit::decode_header(&bytes) else {
            let from = base.map_or(0, |b| b.seq);
            let msg = format!(
                "the header is damaged: open replaces the catalog{}",
                lost(from)
            );
            run.add(if from > 0 {
                Issue::error(msg).file(file).offset(0)
            } else {
                Issue::warning(msg).file(file).offset(0)
            });
            self.checks.push(run.done("unreadable header"));
            return;
        };
        if let Some(ds) = self.dataset_id
            && id != ds
        {
            run.add(
                Issue::error(format!(
                    "belongs to dataset {id}, not {ds}: open refuses the database"
                ))
                .file(file),
            );
        }
        let rec = commit::REC;
        let body = &bytes[rec..];
        let recs = body.as_chunks::<{ commit::REC }>().0;
        let base_seq = base.map_or(0, |b| b.seq);
        // the records open reads: the valid, consecutive prefix
        let mut valid: Vec<CommitInfo> = Vec::new();
        let mut intact = true;
        for (i, r) in recs.iter().enumerate() {
            let expect = first + i as u64;
            let at = ((i + 1) * rec) as u64;
            let problem = match commit::decode_record(r) {
                None => Some(format!("record for commit {expect} fails its checksum")),
                Some(c) if c.seq != expect => Some(format!(
                    "record {i} holds commit {}, expected {expect}: the sequence has a gap",
                    c.seq
                )),
                Some(c) => {
                    if intact {
                        valid.push(c);
                    }
                    None
                }
            };
            if let Some(p) = problem {
                // records the WAL or commit.json still hold are rebuilt; older ones are lost
                let issue = if expect < base_seq {
                    Issue::error(format!(
                        "{p}: open discards the catalog up to commit {base_seq}, losing that history"
                    ))
                } else {
                    Issue::warning(format!("{p}: rebuilt from the WAL on open"))
                };
                run.add(issue.file(file).offset(at).row(i as u64).seq(expect));
                intact = false;
            }
        }
        let partial = body.len() % rec;
        if partial != 0 {
            run.add(
                Issue::warning(format!(
                    "{partial} bytes of a torn record at the end: rewritten on open"
                ))
                .file(file)
                .offset((bytes.len() - partial) as u64),
            );
        }
        if let Some(last) = valid.last() {
            if last.seq > head {
                run.add(
                    Issue::error(format!(
                        "lists commits up to {} but the data ends at commit {head}: acknowledged commits are missing from the WAL",
                        last.seq
                    ))
                    .file(file)
                    .seq(last.seq),
                );
            } else if last.seq < head && intact {
                run.add(
                    Issue::warning(format!(
                        "lags the WAL by {} commit{} (up to {}): rebuilt on open",
                        head - last.seq,
                        if head - last.seq == 1 { "" } else { "s" },
                        last.seq
                    ))
                    .file(file)
                    .seq(last.seq),
                );
            }
        } else if intact {
            run.add(Issue::warning("holds no records: rebuilt on open").file(file));
        }
        if first > base_seq && base.is_some() {
            run.add(
                Issue::warning(format!(
                    "starts at commit {first}, after the generation's base commit {base_seq}: rebuilt on open"
                ))
                .file(file),
            );
        }
        // agreement with commit.json and the WAL where both hold a commit
        let get = |s: u64| {
            s.checked_sub(first)
                .and_then(|i| valid.get(i as usize))
                .copied()
        };
        if let Some(b) = base
            && let Some(c) = get(b.seq)
            && (c.timestamp_ms != b.timestamp_ms || c.kind != b.kind)
        {
            run.add(
                Issue::warning(format!(
                    "the record of commit {} disagrees with {}/commit.json",
                    b.seq,
                    self.current.as_deref().unwrap_or("?")
                ))
                .file(file)
                .seq(b.seq),
            );
        }
        if let Some(w) = &self.wal {
            for &(seq, ts, kind) in &w.commits {
                if kind == 255 {
                    continue; // reconstructed or damaged: nothing to compare
                }
                if let Some(c) = get(seq)
                    && (c.timestamp_ms != ts || c.kind.code() != kind)
                {
                    run.add(
                        Issue::warning(format!(
                            "the record of commit {seq} disagrees with the WAL"
                        ))
                        .file(file)
                        .seq(seq),
                    );
                }
            }
        }
        let summary = match (valid.first(), valid.last()) {
            (Some(f), Some(l)) => format!("{} records, commits {}..{}", valid.len(), f.seq, l.seq),
            _ => "no records".to_string(),
        };
        self.checks.push(run.done(summary));
    }

    // ------------------------------------------------------------- spatial index ------

    fn geo(&mut self, p: crate::geo::GeoProbe) {
        if !p.configured {
            return;
        }
        let mut run = Run::new("geo");
        if let Some(e) = p.config_error {
            run.add(Issue::error(format!("{e}: the spatial index does not open")).file("geo.json"));
            self.checks.push(run.done("invalid configuration"));
            return;
        }
        if p.unsupported {
            self.checks.push(
                run.done("configured, but this build has no GeoSPARQL support: index not checked"),
            );
            return;
        }
        for (f, problem) in &p.damaged {
            run.add(Issue::warning(format!("{problem}: rebuilt on open")).file(f.clone()));
        }
        let preds = p.config.as_ref().map_or(0, |c| c.predicates.len());
        let files = match (p.files.len(), p.damaged.is_empty()) {
            (0, true) => "no index files yet: built when opened".to_string(),
            (n, _) => format!(
                "{n} index files ({} bytes) {}",
                p.file_bytes,
                if self.full {
                    "with their checksums"
                } else {
                    "(headers and index sections)"
                }
            ),
        };
        self.checks
            .push(run.done(format!("configured for {preds} predicates; {files}")));
    }

    // ----------------------------------------------------------- vector indexes ------

    /// `vector.json` and the current generation's index files (`gen_dir/vectors/`).
    fn vector(&mut self, root: &Path, gen_dir: &Path) {
        let t0 = Instant::now();
        let file = crate::vector::config::CONFIG_FILE;
        let cfg = match std::fs::read(root.join(file)) {
            Ok(b) => serde_json::from_slice::<crate::vector::VectorConfigFile>(&b)
                .map_err(|e| e.to_string())
                .and_then(|f| f.validate().map(|()| f).map_err(|e| e.to_string())),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                if !gen_dir.join(crate::vector::persist::DIR).exists() {
                    return;
                }
                Ok(Default::default())
            }
            Err(e) => Err(e.to_string()),
        };
        let mut run = Run::new("vector");
        let cfg = match cfg {
            Ok(c) => c,
            Err(e) => {
                run.add(Issue::error(format!("{e}: the vector indexes do not open")).file(file));
                self.checks.push(run.done("invalid configuration"));
                return;
            }
        };
        let bad = crate::vector::check_files(gen_dir, self.full);
        for (f, problem) in &bad {
            run.add(
                Issue::warning(format!("{problem}: built again on open"))
                    .file(format!("{}/{f}", crate::vector::persist::DIR)),
            );
        }
        let mut done = run.done(format!(
            "{} indexes configured; {} {}",
            cfg.indexes.len(),
            if bad.is_empty() {
                "index files open"
            } else {
                "some index files are damaged"
            },
            if self.full {
                "with their checksums"
            } else {
                "(headers, ids and graph metadata)"
            }
        ));
        done.millis = t0.elapsed().as_secs_f64() * 1000.0;
        self.checks.push(done);
    }

    // --------------------------------------------------------------- full-text ------

    fn text(&mut self, p: crate::text::TextProbe) {
        if !p.configured {
            return;
        }
        let mut run = Run::new("text");
        if let Some(e) = p.config_error {
            run.add(Issue::error(format!("{e}: open fails")).file("text.json"));
            self.checks.push(run.done("unreadable configuration"));
            return;
        }
        if p.unsupported {
            self.checks.push(
                run.done("configured, but this build has no full-text support: index not checked"),
            );
            return;
        }
        if !p.index_dir {
            run.add(Issue::warning("the index is missing: rebuilt on open").file("text"));
            self.checks.push(run.done("no index"));
            return;
        }
        if let Some(e) = p.open_error {
            run.add(
                Issue::warning(format!("the index does not open ({e}): rebuilt on open"))
                    .file("text"),
            );
            self.checks.push(run.done("unreadable index"));
            return;
        }
        if p.dirty {
            run.add(
                Issue::warning("the index may hold unsynced writes: verified on open")
                    .file("text.dirty"),
            );
        }
        for (f, problem) in &p.damaged {
            let msg = format!("segment file {problem}");
            run.add(
                if p.dirty {
                    Issue::warning(format!("{msg}: rebuilt on open"))
                } else {
                    Issue::error(format!(
                        "{msg}: rebuild the index (`sparkles text-index --loc DB --rebuild`)"
                    ))
                }
                .file(format!("text/{f}")),
            );
        }
        let head = self.wal.as_ref().map(|w| w.head);
        let mut at = String::new();
        match (&p.payload, p.payload_error) {
            (_, Some(e)) => run.add(
                Issue::warning(format!("unreadable commit payload ({e}): rebuilt on open"))
                    .file("text/meta.json"),
            ),
            (Some(pl), None) if !pl.format_ok => run.add(
                Issue::warning("written by another index format: rebuilt on open")
                    .file("text/meta.json"),
            ),
            (Some(pl), None) if !pl.config_matches => run.add(
                Issue::warning("built for another text.json configuration: rebuilt on open")
                    .file("text/meta.json"),
            ),
            (Some(pl), None) => {
                at = format!(" at commit {}", pl.seq);
                if let (Some(head), Some(w)) = (head, &self.wal) {
                    let first_wal = w.commits.first().map(|c| c.0);
                    if pl.seq > head {
                        run.add(
                            Issue::warning(format!(
                                "the index is at commit {}, ahead of the data (commit {head}): rebuilt on open",
                                pl.seq
                            ))
                            .file("text/meta.json")
                            .seq(pl.seq),
                        );
                    } else if pl.seq < head {
                        let covered = first_wal.is_some_and(|f| f <= pl.seq + 1);
                        let msg = if covered {
                            format!(
                                "the index is at commit {}, behind the data (commit {head}): caught up from the WAL on open",
                                pl.seq
                            )
                        } else {
                            format!(
                                "the index is at commit {}, older than the WAL (commit {head}): rebuilt on open",
                                pl.seq
                            )
                        };
                        run.add(Issue::warning(msg).file("text/meta.json").seq(pl.seq));
                    }
                }
            }
            (None, None) => {}
        }
        let verified = if self.full { "verified" } else { "present" };
        self.checks.push(run.done(format!(
            "{} documents in {} segments{at}; {} files {verified}",
            p.docs, p.segments, p.files
        )));
    }

    // --------------------------------------------------------------- reasoning ------

    /// The branch table (`branches.json`): every listed or retired branch has its
    /// directory, which
    /// names this dataset, distinct ordinals below the next one, and a linked
    /// generation whose upstream files hold what its link names; merge records with
    /// valid checksums.
    fn branches(&mut self) {
        let mut run = Run::new("branches");
        let file = crate::store::BRANCHES_FILE;
        let t = match crate::store::read_branch_table(self.root) {
            Ok(None) => {
                self.checks.push(run.done("no branches"));
                return;
            }
            Ok(Some(t)) => t,
            Err(e) => {
                run.add(Issue::error(e.to_string()).file(file));
                self.checks.push(run.done("unreadable"));
                return;
            }
        };
        if let (Some(ds), Some(id)) = (self.dataset_id, t["datasetId"].as_str())
            && ds.to_string() != id
        {
            run.add(Issue::error(format!("belongs to dataset {id}, not {ds}")).file(file));
        }
        let next = t["nextOrdinal"].as_u64().unwrap_or(1);
        let mut ordinals = std::collections::BTreeSet::new();
        let mut entries = t["branches"].as_array().cloned().unwrap_or_default();
        let n = entries.len();
        // retired branches keep their directories for the branches created from them
        let retired = t["retired"].as_array().cloned().unwrap_or_default();
        entries.extend(retired.iter().cloned());
        let mut linked = 0usize;
        for e in &entries {
            let name = e["name"].as_str().unwrap_or("?");
            let id = e["id"].as_str().unwrap_or("");
            let o = e["ordinal"].as_u64().unwrap_or(0);
            if o == 0 || o >= next || !ordinals.insert(o) {
                run.add(
                    Issue::error(format!(
                        "branch {name} has ordinal {o}: ordinals are distinct, from 1 to below {next}"
                    ))
                    .file(file),
                );
            }
            let dir = self.root.join(crate::store::BRANCHES_DIR).join(id);
            let bf = dir.join("branch.json");
            match std::fs::read(&bf)
                .ok()
                .and_then(|b| serde_json::from_slice::<serde_json::Value>(&b).ok())
            {
                Some(b) if b["id"] == e["id"] => {}
                Some(_) => run.add(
                    Issue::error(format!("branch {name}: branch.json names another branch"))
                        .file(file),
                ),
                None => {
                    run.add(
                        Issue::error(format!(
                            "branch {name}: its directory is missing or unreadable"
                        ))
                        .file(file),
                    );
                    continue;
                }
            }
            let first = std::fs::read_to_string(dir.join("CURRENT"))
                .map(|c| dir.join(c.trim()))
                .unwrap_or_else(|_| dir.join("gen-0001"));
            let link = match crate::store::read_link_file(&first) {
                Ok(link) => link,
                Err(e) => {
                    run.add(
                        Issue::error(format!("branch {name}: {e}")).file(
                            first
                                .join("link.json")
                                .strip_prefix(self.root)
                                .unwrap()
                                .to_string_lossy(),
                        ),
                    );
                    None
                }
            };
            if let Some(link) = link {
                linked += 1;
                for seg in link {
                    let sdir = self.root.join(&seg.0);
                    let wal = std::fs::metadata(sdir.join("wal.log"))
                        .map(|m| m.len())
                        .unwrap_or(0);
                    if wal < seg.1 {
                        run.add(Issue::error(format!(
                            "branch {name}: its link reads {} bytes of {}/wal.log, which has {wal}",
                            seg.1, seg.0
                        )).file(file));
                    }
                    let terms = std::fs::read(sdir.join("delta.vocab"))
                        .map(|b| crate::vocab::delta_entries(&b).0.len() as u64)
                        .unwrap_or(0);
                    if !seg.0.contains("branches/") && terms < seg.2 {
                        run.add(Issue::error(format!(
                            "branch {name}: its link names {} delta terms of {}, which has {terms}",
                            seg.2, seg.0
                        )).file(file));
                    }
                }
            }
            for (m, what) in [
                (self.root.join("merges.bin"), "main"),
                (dir.join("merges.bin"), name),
            ] {
                if let Ok(b) = std::fs::read(&m)
                    && b.len() % 48 != 0
                {
                    run.add(Issue::warning(format!(
                        "{what}: merges.bin ends in a partial record (the next open cuts it)"
                    )));
                }
            }
        }
        let summary = format!(
            "{n} branch{} besides main, {linked} linked{}; ordinals below {next}",
            if n == 1 { "" } else { "es" },
            match retired.len() {
                0 => String::new(),
                r => format!(", {r} retired"),
            }
        );
        self.checks.push(run.done(summary));
    }

    fn reasoning(&mut self) {
        #[derive(serde::Deserialize)]
        #[serde(rename_all = "camelCase")]
        #[allow(dead_code)]
        struct Reasoning {
            profile: String,
            inferred: u64,
            at: String,
            #[serde(default)]
            commit: Option<u64>,
            #[serde(default)]
            dataset_id: Option<String>,
        }
        let mut run = Run::new("reasoning");
        let file = "reasoning.json";
        let b = match std::fs::read(self.root.join(file)) {
            Ok(b) => b,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                self.checks.push(run.done("no materialized inferences"));
                return;
            }
            Err(e) => {
                run.add(Issue::error(e.to_string()).file(file));
                self.checks.push(run.done("unreadable"));
                return;
            }
        };
        let r: Reasoning = match serde_json::from_slice(&b) {
            Ok(r) => r,
            Err(e) => {
                run.add(
                    Issue::error(format!(
                        "does not parse ({e}): the reasoning status is lost"
                    ))
                    .file(file),
                );
                self.checks.push(run.done("unreadable"));
                return;
            }
        };
        if let (Some(d), Some(ds)) = (&r.dataset_id, self.dataset_id)
            && *d != ds.to_string()
        {
            run.add(Issue::warning(format!("recorded for dataset {d}, not {ds}")).file(file));
        }
        let head = self
            .wal
            .as_ref()
            .map(|w| w.head)
            .or(self.base.map(|b| b.seq));
        if let (Some(c), Some(h)) = (r.commit, head)
            && c > h
        {
            run.add(
                Issue::warning(format!(
                    "names commit {c}, which does not exist (the head is {h})"
                ))
                .file(file)
                .seq(c),
            );
        }
        let at = r
            .commit
            .map_or(String::new(), |c| format!(" at commit {c}"));
        self.checks
            .push(run.done(format!("{}: {} inferred{at}", r.profile, r.inferred)));
    }
}

/// The result of checking one permutation.
struct PermScan {
    perm: Perm,
    check: Check,
    /// total rows per the block metadata
    rows: Option<u64>,
    /// hash of its quads (full mode, when every block decoded)
    hash: Option<(u64, u64)>,
    ids_checked: u64,
    id_issues: Vec<Issue>,
}

/// One decoded block.
struct BlockScan {
    issues: Vec<Issue>,
    id_issues: Vec<Issue>,
    /// decoded first and last keys (`None`: a column did not decode)
    ends: Option<(Key, Key)>,
    hash: (u64, u64),
}

fn scan_perm(
    dir: &Path,
    gen_name: &str,
    perm: Perm,
    quads: Option<u64>,
    vocab_len: Option<u64>,
    full: bool,
) -> PermScan {
    let name = perm.name();
    let mut run = Run::new(format!("perm.{name}"));
    let meta_file = format!("{gen_name}/{name}.meta");
    let dat_file = format!("{gen_name}/{name}.dat");
    let fail = |run: Run, summary: &str| PermScan {
        perm,
        check: run.done(summary),
        rows: None,
        hash: None,
        ids_checked: 0,
        id_issues: Vec::new(),
    };
    let meta = match std::fs::read(dir.join(format!("{name}.meta"))) {
        Ok(m) => m,
        Err(e) => {
            run.add(Issue::error(e.to_string()).file(&meta_file));
            return fail(run, "unreadable");
        }
    };
    let data = match map(&dir.join(format!("{name}.dat"))) {
        Ok(d) => d,
        Err(e) => {
            run.add(Issue::error(e.to_string()).file(&dat_file));
            return fail(run, "unreadable");
        }
    };
    let data: &[u8] = data.as_deref().unwrap_or(&[]);
    if meta.len() % META_BYTES != 0 {
        run.add(
            Issue::error(format!(
                "{} bytes, not a whole number of {META_BYTES}-byte block records: open fails",
                meta.len()
            ))
            .file(&meta_file),
        );
    }
    let blocks: Vec<BlockMeta> = meta
        .as_chunks::<META_BYTES>()
        .0
        .iter()
        .map(|c| BlockMeta::read(c))
        .collect();
    // block metadata: contiguous, sorted, within the file
    let (mut rows, mut end) = (0u64, 0u64);
    let mut in_file = vec![true; blocks.len()];
    for (b, m) in blocks.iter().enumerate() {
        let rec = (b * META_BYTES) as u64;
        if m.rows == 0 || m.rows as usize > BLOCK_ROWS {
            run.add(
                Issue::error(format!("{} rows (1 to {BLOCK_ROWS} expected)", m.rows))
                    .file(&meta_file)
                    .block(b)
                    .offset(rec),
            );
        }
        if m.row_start != rows {
            run.add(
                Issue::error(format!("starts at row {}, expected {rows}", m.row_start))
                    .file(&meta_file)
                    .block(b)
                    .offset(rec),
            );
        }
        if m.offset != end {
            run.add(
                Issue::error(format!("starts at byte {}, expected {end}", m.offset))
                    .file(&meta_file)
                    .block(b)
                    .offset(rec),
            );
        }
        if m.first > m.last {
            run.add(
                Issue::error(format!(
                    "first key {:?} is after its last key {:?}",
                    m.first, m.last
                ))
                .file(&meta_file)
                .block(b)
                .offset(rec),
            );
        }
        if b > 0 && blocks[b - 1].last >= m.first {
            run.add(
                Issue::error(format!(
                    "first key {:?} does not follow the last key {:?} of block {}",
                    m.first,
                    blocks[b - 1].last,
                    b - 1
                ))
                .file(&meta_file)
                .block(b)
                .offset(rec),
            );
        }
        let len: u64 = m.col_len.iter().map(|&l| l as u64).sum();
        if m.offset + len > data.len() as u64 {
            in_file[b] = false;
        }
        rows += m.rows as u64;
        end = m.offset.max(end) + len;
    }
    if let Some(b) = in_file.iter().position(|ok| !ok) {
        run.add(
            Issue::error(format!(
                "{} bytes, but its blocks need {end}: truncated from block {b} on ({} blocks cannot be read)",
                data.len(),
                in_file.iter().filter(|ok| !**ok).count()
            ))
            .file(&dat_file)
            .block(b)
            .offset(data.len() as u64),
        );
    } else if end < data.len() as u64 {
        run.add(
            Issue::error(format!(
                "{} bytes after the last block (the blocks end at byte {end})",
                data.len() as u64 - end
            ))
            .file(&dat_file)
            .offset(end),
        );
    }
    if let Some(q) = quads
        && q != rows
    {
        run.add(
            Issue::error(format!("{rows} rows, but meta.json counts {q} quads")).file(&meta_file),
        );
    }
    if !full {
        let summary = format!("{} blocks, {rows} rows (metadata)", blocks.len());
        return PermScan {
            perm,
            check: run.done(summary),
            rows: Some(rows),
            hash: None,
            ids_checked: 0,
            id_issues: Vec::new(),
        };
    }
    // decode every block
    let scans: Vec<Option<BlockScan>> = blocks
        .par_iter()
        .enumerate()
        .map(|(b, m)| in_file[b].then(|| scan_block(data, perm, b, m, vocab_len, &dat_file)))
        .collect();
    let mut hash = Some((0u64, 0u64));
    let mut id_issues = Vec::new();
    let mut prev: Option<(usize, Key)> = None;
    for (b, s) in scans.into_iter().enumerate() {
        let Some(s) = s else {
            hash = None;
            continue;
        };
        run.extend(s.issues);
        id_issues.extend(s.id_issues);
        match s.ends {
            None => hash = None,
            Some((first, last)) => {
                if let Some((pb, pl)) = prev
                    && pb + 1 == b
                    && pl >= first
                {
                    run.add(
                        Issue::error(format!(
                            "block {b} starts with {first:?}, not after the last key {pl:?} of block {pb}"
                        ))
                        .file(&dat_file)
                        .block(b)
                        .offset(blocks[b].offset),
                    );
                }
                prev = Some((b, last));
                if let Some(h) = hash.as_mut() {
                    h.0 = h.0.wrapping_add(s.hash.0);
                    h.1 = h.1.wrapping_add(s.hash.1);
                }
            }
        }
    }
    let summary = format!("{} blocks, {rows} rows decoded and checked", blocks.len());
    PermScan {
        perm,
        check: run.done(summary),
        rows: Some(rows),
        hash,
        ids_checked: rows * 4,
        id_issues,
    }
}

fn scan_block(
    data: &[u8],
    perm: Perm,
    b: usize,
    m: &BlockMeta,
    vocab_len: Option<u64>,
    file: &str,
) -> BlockScan {
    let mut out = BlockScan {
        issues: Vec::new(),
        id_issues: Vec::new(),
        ends: None,
        hash: (0, 0),
    };
    let n = m.rows as usize;
    let mut cols: Vec<std::sync::Arc<[u64]>> = Vec::with_capacity(4);
    let mut off = m.offset as usize;
    for c in 0..4 {
        let len = m.col_len[c] as usize;
        match decode_column(&data[off..off + len], n) {
            Ok(v) => cols.push(v),
            Err(e) => out.issues.push(
                Issue::error(format!("column {c} does not decode to {n} rows: {e}"))
                    .file(file)
                    .block(b)
                    .offset(off as u64),
            ),
        }
        off += len;
    }
    if cols.len() < 4 || n == 0 {
        return out;
    }
    let key = |i: usize| -> Key { [cols[0][i], cols[1][i], cols[2][i], cols[3][i]] };
    let (first, last) = (key(0), key(n - 1));
    if first != m.first || last != m.last {
        out.issues.push(
            Issue::error(format!(
                "decoded keys {first:?}..{last:?} differ from the block metadata {:?}..{:?}",
                m.first, m.last
            ))
            .file(file)
            .block(b)
            .offset(m.offset),
        );
    }
    let (mut disorder, mut first_bad) = (0usize, None);
    let mut prev = first;
    let (mut ha, mut hb) = (0u64, 0u64);
    let mut id_bad = 0usize;
    for i in 0..n {
        let k = key(i);
        if i > 0 && k <= prev {
            disorder += 1;
            first_bad.get_or_insert((i, prev, k));
        }
        prev = k;
        let q = perm.to_quad(&k);
        let (a, h) = quad_hash(&q);
        ha = ha.wrapping_add(a);
        hb = hb.wrapping_add(h);
        for (pos, id) in q.iter().enumerate() {
            if let Some(p) = id_problem(*id, pos, vocab_len, None) {
                id_bad += 1;
                if out.id_issues.len() < 4 {
                    out.id_issues.push(
                        Issue::error(p)
                            .file(file)
                            .block(b)
                            .row(i as u64)
                            .id(id_str(*id)),
                    );
                }
            }
        }
    }
    if id_bad > out.id_issues.len() {
        out.id_issues.push(
            Issue::error(format!(
                "{} more invalid ids in this block",
                id_bad - out.id_issues.len()
            ))
            .file(file)
            .block(b),
        );
    }
    if let Some((i, p, k)) = first_bad {
        let what = if p == k { "repeats" } else { "sorts before" };
        out.issues.push(
            Issue::error(format!(
                "key {k:?} {what} the previous key {p:?} ({disorder} row{} out of order in the block)",
                if disorder == 1 { "" } else { "s" }
            ))
            .file(file)
            .block(b)
            .row(i as u64)
            .offset(m.offset),
        );
    }
    out.ends = Some((first, last));
    out.hash = (ha, hb);
    out
}
