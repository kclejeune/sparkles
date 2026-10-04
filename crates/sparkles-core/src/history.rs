//! Point-in-time reads and named snapshots.
//!
//! A past commit is readable while some *retained* generation covers it: the current
//! one, or an older `gen-NNNN` kept because a named snapshot (a pin) or the retention
//! window needs it. Its state is that generation's base index plus its write-ahead log
//! replayed through the commit. Nothing is added to the write path: history is the
//! durable state a store already has, kept a little longer.
//!
//! `<root>/history.json` holds the pins (by commit, never by generation) and the
//! retention window. Garbage collection renames an unneeded generation to
//! `gen-NNNN.deleting` before removing it, so an interrupted collection is finished at
//! the next open, and a directory it cannot attribute to this dataset is left alone.

use crate::commit::{self, CommitInfo};
use crate::error::{Error, Result};
use crate::store::{Generation, Snapshot};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// Which state of a dataset to read.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum At {
    Head,
    Commit(u64),
    /// the last commit at or before this instant (milliseconds since the epoch)
    Time(i64),
    Snapshot(String),
}

impl std::str::FromStr for At {
    type Err = Error;

    /// `head`, `42`, `commit:42`, `time:<RFC 3339>` or `snapshot:<name>`.
    fn from_str(s: &str) -> Result<At> {
        let bad = || {
            Error::invalid(format!(
                "invalid at '{s}': expected head, N, commit:N, time:<RFC 3339> or snapshot:<name>"
            ))
        };
        let seq = |n: &str| {
            (!n.is_empty() && n.len() <= 20 && n.bytes().all(|b| b.is_ascii_digit()))
                .then(|| n.parse::<u64>().ok())
                .flatten()
                .ok_or_else(bad)
        };
        if s == "head" {
            Ok(At::Head)
        } else if let Some(n) = s.strip_prefix("commit:") {
            Ok(At::Commit(seq(n)?))
        } else if let Some(t) = s.strip_prefix("time:") {
            commit::parse_rfc3339_offset(t)
                .map(At::Time)
                .ok_or_else(bad)
        } else if let Some(n) = s.strip_prefix("snapshot:") {
            valid_name(n)
                .then(|| At::Snapshot(n.to_string()))
                .ok_or_else(bad)
        } else {
            Ok(At::Commit(seq(s)?))
        }
    }
}

impl std::fmt::Display for At {
    /// The canonical form: `head`, `commit:N`, `time:<RFC 3339 ms Z>`, `snapshot:NAME`.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            At::Head => write!(f, "head"),
            At::Commit(n) => write!(f, "commit:{n}"),
            At::Time(ms) => write!(f, "time:{}", commit::rfc3339_ms(*ms)),
            At::Snapshot(n) => write!(f, "snapshot:{n}"),
        }
    }
}

/// A snapshot name: `[A-Za-z0-9][A-Za-z0-9._-]{0,63}`.
pub fn valid_name(n: &str) -> bool {
    let b = n.as_bytes();
    !b.is_empty()
        && b.len() <= 64
        && b[0].is_ascii_alphanumeric()
        && b.iter()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, b'.' | b'_' | b'-'))
}

/// A selector resolved against the commit catalog.
#[derive(Clone, Debug)]
pub struct Resolved {
    pub at: At,
    pub commit: CommitInfo,
    pub head: u64,
    /// not the live state
    pub historical: bool,
}

/// A named snapshot.
#[derive(Clone, Debug)]
pub struct NamedSnapshot {
    pub name: String,
    /// the pinned commit (metadata `None` if the catalog no longer has it)
    pub seq: u64,
    pub commit: Option<CommitInfo>,
    pub created_ms: i64,
    pub note: Option<String>,
    /// when the pin lapses (milliseconds since the epoch)
    pub expires_ms: Option<i64>,
    /// the generation that would serve it now
    pub generation: Option<String>,
    /// false only after external damage (its generation is gone)
    pub reconstructable: bool,
    /// kept materialized (see [`SnapshotOptions::warm`])
    pub warm: bool,
}

/// Options of a named snapshot ([`Store::create_snapshot_opts`](crate::store::Store::create_snapshot_opts)).
#[derive(Clone, Debug, Default)]
pub struct SnapshotOptions {
    pub note: Option<String>,
    /// when the pin lapses (milliseconds since the epoch)
    pub expires_ms: Option<i64>,
    /// Keep the pinned state materialized: it is built when the pin is made and by the
    /// history upkeep (after a restart, say), and the history cache evicts it only when
    /// warm states alone pass its budget. An in-memory dataset keeps every pinned state
    /// in memory anyway.
    pub warm: bool,
}

/// The retention window: the states that were the head within the last `keep_commits`
/// commits or `keep_age_ms` milliseconds stay readable. `max_bytes` caps the disk the
/// generations kept only by the window may use: the oldest go first.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Retention {
    pub keep_commits: Option<u64>,
    pub keep_age_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_bytes: Option<u64>,
}

impl Retention {
    /// Whether the window keeps anything.
    pub fn is_on(&self) -> bool {
        self.keep_commits.is_some_and(|n| n > 0) || self.keep_age_ms.is_some()
    }
}

/// How long the commit catalog keeps the metadata of commits whose state can no longer
/// be read: the last `keep_commits` commits, and the commits made in the last
/// `keep_age_ms` milliseconds. Records older than both the readable history and the
/// horizon are pruned. With neither set, nothing is pruned.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CatalogHorizon {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub keep_commits: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub keep_age_ms: Option<u64>,
}

impl CatalogHorizon {
    /// Whether the catalog is pruned at all.
    pub fn is_on(&self) -> bool {
        self.keep_commits.is_some() || self.keep_age_ms.is_some()
    }

    fn is_off(&self) -> bool {
        !self.is_on()
    }

    /// The oldest commit the horizon keeps (`head` is the newest commit, and
    /// `made_since(ms)` the first commit made at or after `ms`). A commit is kept if
    /// either limit keeps it; `None` when the horizon is off.
    pub fn cutoff(
        &self,
        head: u64,
        now_ms: i64,
        made_since: &dyn Fn(i64) -> Option<u64>,
    ) -> Option<u64> {
        let by_count = self
            .keep_commits
            .map(|n| head.saturating_sub(n.saturating_sub(1)));
        let by_age = self.keep_age_ms.map(|age| {
            made_since(now_ms.saturating_sub(age.min(i64::MAX as u64) as i64)).unwrap_or(head)
        });
        match (by_count, by_age) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (a, b) => a.or(b),
        }
    }
}

/// A scheduled pin: every `every_ms`, the head is pinned as `<prefix><UTC time>`
/// (unless the newest such pin already holds it), and only the newest `keep_last` pins
/// of the prefix are kept.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Schedule {
    pub prefix: String,
    pub every_ms: u64,
    pub keep_last: u32,
}

impl Schedule {
    /// Check a schedule: a prefix that starts a valid snapshot name (at most 40 bytes),
    /// an interval of at least a minute, and at least one pin to keep.
    pub fn validate(&self) -> Result<()> {
        let p = &self.prefix;
        if p.len() > 40 || !valid_name(&format!("{p}0")) {
            return Err(Error::invalid(format!(
                "invalid schedule prefix {p:?}: letters, digits, '.', '_' and '-', at most 40, starting with a letter or digit"
            )));
        }
        if self.every_ms < 60_000 {
            return Err(Error::invalid(
                "a schedule's interval is at least 60 seconds",
            ));
        }
        if self.keep_last == 0 {
            return Err(Error::invalid("a schedule keeps at least one snapshot"));
        }
        Ok(())
    }

    /// The name of the pin made at `ms`: the prefix and a compact UTC time.
    pub fn name_at(&self, ms: i64) -> String {
        let t: String = commit::rfc3339_ms(ms)
            .chars()
            .filter(|c| c.is_ascii_digit() || *c == 'T')
            .take(15)
            .collect();
        format!("{}{t}Z", self.prefix)
    }
}

/// What a history tick did ([`Store::history_tick`](crate::store::Store::history_tick)).
#[derive(Clone, Debug, Default)]
pub struct TickReport {
    pub created: Vec<String>,
    pub expired: Vec<String>,
    pub rotated: Vec<String>,
    /// warm pins materialized
    pub warmed: usize,
    /// commit records pruned from the catalog
    pub pruned: u64,
}

/// Why a generation is kept.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Hold {
    Head,
    Snapshot(String),
    Retention,
    /// a backup reading the generation (a lease, by backup name)
    Lease(String),
    /// a clone copying the generation's files (a lease, by the clone's name)
    Clone(String),
    /// a branch whose linked generation reads the generation's files (by branch name)
    Branch(String),
    /// a branch's starting commit, kept readable for merges (by branch name)
    BranchBase(String),
}

impl std::fmt::Display for Hold {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Hold::Head => write!(f, "head"),
            Hold::Snapshot(n) => write!(f, "snapshot:{n}"),
            Hold::Retention => write!(f, "retention"),
            Hold::Lease(n) => write!(f, "backup:{n}"),
            Hold::Clone(n) => write!(f, "clone:{n}"),
            Hold::Branch(n) => write!(f, "branch:{n}"),
            Hold::BranchBase(n) => write!(f, "branch-base:{n}"),
        }
    }
}

#[derive(Clone, Debug)]
pub struct HistoryGeneration {
    pub name: String,
    pub base_seq: u64,
    pub end_seq: u64,
    pub bytes: u64,
    pub current: bool,
    pub held_by: Vec<Hold>,
}

#[derive(Clone, Debug)]
pub struct HistoryStatus {
    pub head: u64,
    /// ascending, disjoint, inclusive
    pub reconstructable: Vec<(u64, u64)>,
    pub generations: Vec<HistoryGeneration>,
    /// bytes of retained non-current generations
    pub bytes: u64,
    pub retention: Retention,
    pub snapshots: usize,
    /// how long the commit catalog keeps metadata
    pub catalog: CatalogHorizon,
    /// the oldest commit whose metadata the catalog keeps
    pub first_commit: u64,
    pub cache_entries: usize,
    pub cache_bytes: u64,
    pub hits: u64,
    pub misses: u64,
    pub materializations: u64,
    /// time spent materializing past states
    pub materialize_seconds: f64,
}

impl HistoryStatus {
    pub fn oldest_reconstructable(&self) -> Option<u64> {
        self.reconstructable.first().map(|r| r.0)
    }
}

/// The detail of a `410 history-gone`.
#[derive(Clone, Debug)]
pub struct HistoryGone {
    pub message: String,
    pub seq: u64,
    pub head: u64,
    pub snapshot: Option<String>,
    pub reconstructable: Vec<(u64, u64)>,
    pub metadata: Option<CommitInfo>,
}

impl std::fmt::Display for HistoryGone {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

/// Options of a past-state read.
#[derive(Clone, Default)]
pub struct HistoryOptions {
    pub cancel: Option<Arc<std::sync::atomic::AtomicBool>>,
    pub deadline: Option<std::time::Instant>,
}

// -------------------------------------------------------------- history.json ------

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct HistoryFile {
    format: u32,
    dataset_id: uuid::Uuid,
    #[serde(default)]
    retention: Retention,
    #[serde(default)]
    snapshots: Vec<PinFile>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    schedules: Vec<Schedule>,
    #[serde(default, skip_serializing_if = "CatalogHorizon::is_off")]
    catalog: CatalogHorizon,
}

#[derive(Serialize, Deserialize)]
struct PinFile {
    name: String,
    seq: u64,
    created: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    note: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    expires: Option<String>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    warm: bool,
}

#[derive(Clone, Debug)]
pub(crate) struct Pin {
    pub seq: u64,
    pub created_ms: i64,
    pub note: Option<String>,
    pub expires_ms: Option<i64>,
    /// kept materialized in the history cache
    pub warm: bool,
}

/// What `history.json` holds.
#[derive(Default)]
pub(crate) struct HistoryConfig {
    pub pins: BTreeMap<String, Pin>,
    pub retention: Retention,
    pub schedules: Vec<Schedule>,
    pub catalog: CatalogHorizon,
}

/// Pins, retention, schedules and the catalog horizon from `history.json` (none if the
/// file is missing).
pub(crate) fn read_file(root: &Path, dataset_id: uuid::Uuid) -> Result<HistoryConfig> {
    let bytes = match std::fs::read(root.join("history.json")) {
        Ok(b) => b,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Ok(HistoryConfig::default());
        }
        Err(e) => return Err(e.into()),
    };
    let f: HistoryFile =
        serde_json::from_slice(&bytes).map_err(|e| Error::Corrupt(format!("history.json: {e}")))?;
    if f.format != 1 {
        return Err(Error::Corrupt(format!(
            "history.json has format {}, this build reads 1",
            f.format
        )));
    }
    if f.dataset_id != dataset_id {
        return Err(Error::Corrupt(format!(
            "history.json belongs to dataset {}, not {dataset_id}",
            f.dataset_id
        )));
    }
    let pins = f
        .snapshots
        .into_iter()
        .map(|p| {
            let created_ms = commit::parse_rfc3339_ms(&p.created).unwrap_or(0);
            (
                p.name,
                Pin {
                    seq: p.seq,
                    created_ms,
                    note: p.note,
                    expires_ms: p.expires.as_deref().and_then(commit::parse_rfc3339_ms),
                    warm: p.warm,
                },
            )
        })
        .collect();
    Ok(HistoryConfig {
        pins,
        retention: f.retention,
        schedules: f.schedules,
        catalog: f.catalog,
    })
}

/// Write `history.json` durably (before anything relies on it).
pub(crate) fn write_file(
    root: &Path,
    dataset_id: uuid::Uuid,
    pins: &BTreeMap<String, Pin>,
    retention: Retention,
    schedules: &[Schedule],
    catalog: CatalogHorizon,
) -> Result<()> {
    let f = HistoryFile {
        format: 1,
        dataset_id,
        retention,
        snapshots: pins
            .iter()
            .map(|(name, p)| PinFile {
                name: name.clone(),
                seq: p.seq,
                created: commit::rfc3339_ms(p.created_ms),
                note: p.note.clone(),
                expires: p.expires_ms.map(commit::rfc3339_ms),
                warm: p.warm,
            })
            .collect(),
        schedules: schedules.to_vec(),
        catalog,
    };
    crate::store::write_atomic(
        &root.join("history.json"),
        &serde_json::to_vec_pretty(&f).unwrap(),
    )
}

/// Give `history.json` (if any) the dataset id `new` in place of `old` (a restore that
/// takes a new identity; pins are by commit, which it keeps).
pub(crate) fn reidentify_file(root: &Path, old: uuid::Uuid, new: uuid::Uuid) -> Result<()> {
    if !root.join("history.json").exists() {
        return Ok(());
    }
    let c = read_file(root, old)?;
    write_file(root, new, &c.pins, c.retention, &c.schedules, c.catalog)
}

// ------------------------------------------------------------ generation table ------

/// One generation directory that belongs to this dataset.
#[derive(Clone, Debug)]
pub(crate) struct GenEntry {
    pub name: String,
    pub dir: PathBuf,
    /// the commit its base index holds
    pub base: CommitInfo,
    /// the last commit in its WAL (for the current generation: kept at the head)
    pub end: u64,
    /// legacy WAL commits before the first one with metadata fold into the base
    pub fold_legacy: bool,
    pub bytes: u64,
}

/// A backup's or a clone's hold on a generation directory: kept, whatever else needs
/// it, until the backup has read it or the clone has copied it. In memory only (never
/// in `history.json`).
#[derive(Clone, Debug)]
pub(crate) struct Lease {
    pub generation: u32,
    pub label: String,
    /// held by a clone rather than a backup
    pub clone: bool,
}

/// A materialized past state in the history cache.
pub(crate) struct Cached {
    /// the generation that served it
    pub generation: u32,
    pub seq: u64,
    pub snap: Arc<Snapshot>,
    /// the memory its delta is estimated to take
    pub bytes: u64,
    /// where its commit ends in the generation's log: a later read of a nearby commit
    /// starts from this state
    pub end: crate::store::wal::WalPoint,
}

/// The in-memory history state of a persistent store.
pub(crate) struct HistoryState {
    pub pins: BTreeMap<String, Pin>,
    pub retention: Retention,
    pub schedules: Vec<Schedule>,
    pub catalog: CatalogHorizon,
    /// backup leases by lease id
    pub leases: BTreeMap<u64, Lease>,
    /// the id of the next lease
    pub next_lease: u64,
    /// by generation number, the current one included
    pub gens: BTreeMap<u32, GenEntry>,
    /// sealed generations open for reading, most recent first
    pub open: Vec<(u32, Arc<Generation>)>,
    /// materialized past states, most recent first
    pub cache: Vec<Cached>,
    pub hits: u64,
    pub misses: u64,
    pub materializations: u64,
    pub materialize_nanos: u64,
    /// generations that branches' linked generations read (by generation number)
    pub branch_gens: Vec<(u32, Hold)>,
    /// commits branches keep readable (their starting commits)
    pub branch_pins: Vec<(u64, Hold)>,
}

impl HistoryState {
    pub fn new(pins: BTreeMap<String, Pin>, retention: Retention) -> HistoryState {
        HistoryState {
            pins,
            retention,
            schedules: Vec::new(),
            catalog: CatalogHorizon::default(),
            leases: BTreeMap::new(),
            next_lease: 1,
            gens: BTreeMap::new(),
            open: Vec::new(),
            cache: Vec::new(),
            hits: 0,
            misses: 0,
            materializations: 0,
            materialize_nanos: 0,
            branch_gens: Vec::new(),
            branch_pins: Vec::new(),
        }
    }

    /// A copy of what decides which generations are kept (pins, retention, leases and
    /// the generation table), without the open generations and cached states, for
    /// working out what a change would keep.
    pub fn clone_for_simulation(&self) -> HistoryState {
        let mut h = HistoryState::new(self.pins.clone(), self.retention);
        h.leases = self.leases.clone();
        h.gens = self.gens.clone();
        h.branch_gens = self.branch_gens.clone();
        h.branch_pins = self.branch_pins.clone();
        h
    }

    /// Generation ranges `(number, base, end)`; the current generation ends at `head`.
    fn ranges(&self, current: u32, head: u64) -> Vec<(u32, u64, u64)> {
        self.gens
            .iter()
            .map(|(&no, g)| (no, g.base.seq, if no == current { head } else { g.end }))
            .collect()
    }

    /// The generation that serves commit `seq`: the newest retained one covering it.
    pub fn owner(&self, seq: u64, current: u32, head: u64) -> Option<u32> {
        self.ranges(current, head)
            .into_iter()
            .rev()
            .find(|&(_, b, e)| b <= seq && seq <= e)
            .map(|(no, _, _)| no)
    }

    /// The readable commits, as ascending disjoint ranges.
    pub fn reconstructable(&self, current: u32, head: u64) -> Vec<(u64, u64)> {
        let mut r: Vec<(u64, u64)> = self
            .ranges(current, head)
            .into_iter()
            .map(|(_, b, e)| (b, e))
            .collect();
        r.sort_unstable();
        let mut out: Vec<(u64, u64)> = Vec::new();
        for (b, e) in r {
            match out.last_mut() {
                Some(last) if b <= last.1 + 1 => last.1 = last.1.max(e),
                _ => out.push((b, e)),
            }
        }
        out
    }

    /// The protected commits as inclusive ranges with what protects them: the head,
    /// every pin, and the retention window (`ts(s)` gives a commit's timestamp).
    fn protected(
        &self,
        head: u64,
        now_ms: i64,
        ts: &dyn Fn(u64) -> Option<i64>,
    ) -> Vec<(u64, u64, Hold)> {
        let mut p = vec![(head, head, Hold::Head)];
        for (name, pin) in &self.pins {
            p.push((pin.seq, pin.seq, Hold::Snapshot(name.clone())));
        }
        for (seq, hold) in &self.branch_pins {
            p.push((*seq, *seq, hold.clone()));
        }
        if let Some(n) = self.retention.keep_commits
            && n > 0
        {
            p.push((head.saturating_sub(n - 1), head, Hold::Retention));
        }
        if let Some(age) = self.retention.keep_age_ms {
            // s was the head until ts(s + 1): inside the window if that is recent
            let cutoff = now_ms.saturating_sub(age as i64);
            let mut s = head;
            while s > 0 && ts(s).is_some_and(|t| t > cutoff) {
                s -= 1;
            }
            p.push((s, head, Hold::Retention));
        }
        p
    }

    /// Add a lease on generation `generation`; returns its id.
    pub fn lease(&mut self, generation: u32, label: &str) -> u64 {
        self.lease_for(generation, label, false)
    }

    /// Add a lease on generation `generation` for a backup, or for a clone (`clone`);
    /// returns its id.
    pub(crate) fn lease_for(&mut self, generation: u32, label: &str, clone: bool) -> u64 {
        let id = self.next_lease;
        self.next_lease += 1;
        self.leases.insert(
            id,
            Lease {
                generation,
                label: label.to_string(),
                clone,
            },
        );
        id
    }

    /// The leases on generation `no`, and the branches that read it, as holds.
    pub fn lease_holds(&self, no: u32) -> impl Iterator<Item = Hold> + '_ {
        self.leases
            .values()
            .filter(move |l| l.generation == no)
            .map(|l| {
                if l.clone {
                    Hold::Clone(l.label.clone())
                } else {
                    Hold::Lease(l.label.clone())
                }
            })
            .chain(
                self.branch_gens
                    .iter()
                    .filter(move |(g, _)| *g == no)
                    .map(|(_, h)| h.clone()),
            )
    }

    /// The non-current generations to keep, with what holds each: a generation is
    /// needed if it is the newest retained one covering some protected commit, or if a
    /// backup leases it. At most `max_gens` are kept for the retention window alone
    /// (oldest dropped first); pins and leases always win.
    pub fn needed(
        &self,
        current: u32,
        head: u64,
        now_ms: i64,
        ts: &dyn Fn(u64) -> Option<i64>,
        max_gens: usize,
    ) -> BTreeMap<u32, Vec<Hold>> {
        let protected = self.protected(head, now_ms, ts);
        let ranges = self.ranges(current, head);
        let mut covered: Vec<(u64, u64)> = ranges
            .iter()
            .filter(|r| r.0 == current)
            .map(|r| (r.1, r.2))
            .collect();
        let mut out: BTreeMap<u32, Vec<Hold>> = BTreeMap::new();
        for &(no, b, e) in ranges.iter().rev() {
            if no == current {
                continue;
            }
            let mut holds: Vec<Hold> = Vec::new();
            for (pb, pe, hold) in &protected {
                let (lo, hi) = ((*pb).max(b), (*pe).min(e));
                if lo <= hi && !fully_covered(lo, hi, &covered) && !holds.contains(hold) {
                    holds.push(hold.clone());
                }
            }
            if !holds.is_empty() {
                out.insert(no, holds);
                covered.push((b, e));
            }
        }
        // the window alone may hold at most `max_gens` generations
        let window_only: Vec<u32> = out
            .iter()
            .filter(|(_, h)| h.iter().all(|h| *h == Hold::Retention))
            .map(|(&no, _)| no)
            .collect();
        let pinned = out.len() - window_only.len();
        let allowed = max_gens.saturating_sub(pinned);
        let mut dropped = 0;
        if window_only.len() > allowed {
            dropped = window_only.len() - allowed;
            for no in &window_only[..dropped] {
                out.remove(no);
            }
        }
        // and at most `max_bytes` of disk, counting every kept generation
        if let Some(max) = self.retention.max_bytes {
            let size = |no: &u32| self.gens.get(no).map_or(0, |g| g.bytes);
            let mut total: u64 = out.keys().map(size).sum();
            for no in &window_only[dropped..] {
                if total <= max {
                    break;
                }
                total -= size(no);
                out.remove(no);
            }
        }
        // a leased generation is kept whatever covers its commits (a compaction's new
        // generation covers the leased head), and outside the limit
        for (&no, _) in self.gens.iter().filter(|(no, _)| **no != current) {
            for hold in self.lease_holds(no) {
                let holds = out.entry(no).or_default();
                if !holds.contains(&hold) {
                    holds.push(hold);
                }
            }
        }
        out
    }
}

/// The history of an in-memory store: the past states that pins and the retention
/// window keep, as snapshots. Snapshots share their structure, so a state costs only
/// the delta changes since the one before it, plus the old index while a compaction's
/// predecessor is kept.
#[derive(Default)]
pub(crate) struct MemHistory {
    pub pins: BTreeMap<String, (Pin, Arc<Snapshot>)>,
    pub retention: Retention,
    /// past states kept by the window, oldest first (never the head)
    pub window: std::collections::VecDeque<Arc<Snapshot>>,
    pub schedules: Vec<Schedule>,
    pub hits: u64,
}

impl MemHistory {
    /// The kept state of commit `seq`.
    pub fn get(&self, seq: u64) -> Option<Arc<Snapshot>> {
        self.window
            .iter()
            .find(|s| s.commit == seq)
            .or_else(|| self.pins.values().map(|p| &p.1).find(|s| s.commit == seq))
            .cloned()
    }

    /// The readable commits, the head included, as ascending disjoint ranges.
    pub fn reconstructable(&self, head: u64) -> Vec<(u64, u64)> {
        let mut seqs: Vec<u64> = self
            .window
            .iter()
            .map(|s| s.commit)
            .chain(self.pins.values().map(|p| p.1.commit))
            .chain(std::iter::once(head))
            .collect();
        seqs.sort_unstable();
        seqs.dedup();
        let mut out: Vec<(u64, u64)> = Vec::new();
        for s in seqs {
            match out.last_mut() {
                Some(last) if s == last.1 + 1 => last.1 = s,
                _ => out.push((s, s)),
            }
        }
        out
    }

    /// Drop the window's states that it no longer covers (`ts(s)` gives a commit's
    /// timestamp).
    pub fn trim(&mut self, head: u64, now_ms: i64, ts: &dyn Fn(u64) -> Option<i64>) {
        let r = self.retention;
        let keep = |s: u64| {
            let by_count = r.keep_commits.is_some_and(|n| n > 0 && s + n > head);
            // s was the head until ts(s + 1)
            let by_age = r.keep_age_ms.is_some_and(|age| {
                ts(s + 1).is_some_and(|t| t > now_ms.saturating_sub(age as i64))
            });
            by_count || by_age
        };
        self.window.retain(|s| s.commit < head && keep(s.commit));
    }
}

/// Whether `[lo, hi]` lies inside the union of `covered`.
fn fully_covered(lo: u64, hi: u64, covered: &[(u64, u64)]) -> bool {
    let mut c: Vec<(u64, u64)> = covered.to_vec();
    c.sort_unstable();
    let mut next = lo;
    for (b, e) in c {
        if b > next {
            break;
        }
        if e >= next {
            if e >= hi {
                return true;
            }
            next = e + 1;
        }
    }
    false
}

/// Generation directories of `root` that belong to `dataset_id` (by their
/// `commit.json`), with the ones that cannot be attributed reported as warnings.
pub(crate) fn scan_generations(
    root: &Path,
    dataset_id: uuid::Uuid,
) -> Result<Vec<(u32, String, CommitInfo, bool)>> {
    let mut out = Vec::new();
    for e in std::fs::read_dir(root)? {
        let e = e?;
        let name = e.file_name().to_string_lossy().into_owned();
        let Some(num) = name.strip_prefix("gen-") else {
            continue;
        };
        if !e.file_type()?.is_dir() || !num.bytes().all(|b| b.is_ascii_digit()) {
            continue;
        }
        let no = commit::generation_number(&name);
        match commit::read_gen_commit(&e.path()) {
            Ok(Some((id, c, origin))) if id == dataset_id => {
                out.push((no, name, c, origin == "baseline"))
            }
            _ => tracing::warn!(
                "{}: not a generation of this dataset; left alone",
                e.path().display()
            ),
        }
    }
    out.sort_by_key(|g| g.0);
    Ok(out)
}

/// The non-current generations that named snapshots or the retention window keep,
/// with what holds each, computed from the files alone (for read-only tools such as
/// `sparkles check`), with the default generation limit.
pub fn retained_offline(
    root: &Path,
    dataset_id: uuid::Uuid,
    current: u32,
) -> Result<BTreeMap<u32, Vec<Hold>>> {
    let HistoryConfig {
        pins, retention, ..
    } = read_file(root, dataset_id)?;
    if pins.is_empty() && retention == Retention::default() {
        return Ok(BTreeMap::new());
    }
    let recs = commit::read_catalog(&root.join("commits.bin"))?
        .map(|(_, r)| r)
        .unwrap_or_default();
    let head = recs.last().map_or(0, |c| c.seq);
    let mut h = HistoryState::new(pins, retention);
    for (no, name, base, fold_legacy) in scan_generations(root, dataset_id)? {
        if no > current {
            continue;
        }
        let dir = root.join(&name);
        let end = if no == current {
            head
        } else {
            recs.iter()
                .rev()
                .find(|c| c.generation == no)
                .map(|c| c.seq.max(base.seq))
                .unwrap_or_else(|| crate::store::wal_end(&dir, &base, fold_legacy))
        };
        h.gens.insert(
            no,
            GenEntry {
                name,
                dir,
                base,
                end,
                fold_legacy,
                bytes: 0,
            },
        );
    }
    let first = recs.first().map_or(0, |c| c.seq);
    let ts = |s: u64| {
        s.checked_sub(first)
            .and_then(|i| recs.get(i as usize))
            .map(|c| c.timestamp_ms)
    };
    let max_gens = crate::store::StoreOptions::default().history_max_generations;
    Ok(h.needed(current, head, commit::now_ms(), &ts, max_gens))
}

/// The readable commits of a database, from its files alone (no lock): the ranges of
/// the current generation and of the older ones still on disk. A generation that the
/// next collection would remove still counts.
pub fn reconstructable_offline(root: &Path, dataset_id: uuid::Uuid) -> Result<Vec<(u64, u64)>> {
    let current = std::fs::read_to_string(root.join("CURRENT"))
        .map(|s| commit::generation_number(s.trim()))
        .unwrap_or(0);
    let recs = commit::read_catalog(&root.join("commits.bin"))?
        .map(|(_, r)| r)
        .unwrap_or_default();
    let head = recs.last().map_or(0, |c| c.seq);
    let mut h = HistoryState::new(BTreeMap::new(), Retention::default());
    for (no, name, base, fold_legacy) in scan_generations(root, dataset_id)? {
        if no > current {
            continue;
        }
        let dir = root.join(&name);
        // a generation whose commits a compaction carried over has none of its own
        let end = recs
            .iter()
            .rev()
            .find(|c| c.generation == no)
            .map(|c| c.seq.max(base.seq))
            .unwrap_or_else(|| crate::store::wal_end(&dir, &base, fold_legacy));
        h.gens.insert(
            no,
            GenEntry {
                dir,
                name,
                base,
                end,
                fold_legacy,
                bytes: 0,
            },
        );
    }
    Ok(h.reconstructable(current, head))
}

/// Finish interrupted collections: remove `gen-*.deleting` directories.
pub(crate) fn remove_deleting(root: &Path) -> Result<()> {
    for e in std::fs::read_dir(root)? {
        let e = e?;
        let name = e.file_name().to_string_lossy().into_owned();
        if name.starts_with("gen-") && name.ends_with(".deleting") {
            std::fs::remove_dir_all(e.path())?;
        }
    }
    Ok(())
}

/// Remove a generation directory crash-safely: rename, sync the parent, delete.
pub(crate) fn delete_generation(root: &Path, dir: &Path) -> Result<()> {
    if let Some(doomed) = retire_generation(root, dir)? {
        std::fs::remove_dir_all(&doomed)?;
    }
    Ok(())
}

/// The first half of [`delete_generation`]: rename the directory to `*.deleting` and
/// sync the parent. The caller deletes the returned directory, perhaps later, and an
/// open finishes the deletion after a crash.
pub(crate) fn retire_generation(root: &Path, dir: &Path) -> Result<Option<PathBuf>> {
    let Some(name) = dir.file_name() else {
        return Ok(None);
    };
    let doomed = root.join(format!("{}.deleting", name.to_string_lossy()));
    std::fs::rename(dir, &doomed)?;
    crate::store::sync_dir(root)?;
    Ok(Some(doomed))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commit::CommitKind;

    fn c(seq: u64) -> CommitInfo {
        CommitInfo {
            seq,
            timestamp_ms: seq as i64 * 1000,
            kind: CommitKind::Update,
            inserted: 0,
            deleted: 0,
            quads: 0,
            generation: 0,
            bulk: false,
            exact: true,
            reconstructed: false,
            default_graph: true,
            unvalidated: false,
        }
    }

    fn state(gens: &[(u32, u64, u64)]) -> HistoryState {
        let mut h = HistoryState::new(BTreeMap::new(), Retention::default());
        for &(no, b, e) in gens {
            h.gens.insert(
                no,
                GenEntry {
                    name: commit::generation_name(no),
                    dir: PathBuf::new(),
                    base: c(b),
                    end: e,
                    fold_legacy: false,
                    bytes: 0,
                },
            );
        }
        h
    }

    #[test]
    fn selectors_parse_and_print() {
        for (s, want) in [
            ("head", At::Head),
            ("42", At::Commit(42)),
            ("commit:7", At::Commit(7)),
            ("snapshot:rel-1.0", At::Snapshot("rel-1.0".into())),
            ("time:1970-01-01T00:00:02.500Z", At::Time(2500)),
            ("time:1970-01-01T01:00:02+01:00", At::Time(2000)),
            ("time:1970-01-01T01:00:02 01:00", At::Time(2000)),
            ("time:1969-12-31T23:00:02-01:00", At::Time(2000)),
        ] {
            assert_eq!(s.parse::<At>().unwrap(), want, "{s}");
        }
        for s in [
            "",
            "abc",
            "commit:-1",
            "commit:",
            "snapshot:a/b",
            "time:yesterday",
        ] {
            assert!(s.parse::<At>().is_err(), "{s}");
        }
        assert_eq!(At::Time(2500).to_string(), "time:1970-01-01T00:00:02.500Z");
    }

    #[test]
    fn owners_and_ranges() {
        // gen 1: 0..=5 (sealed), gen 2: 5..=9 (compaction), gen 3: 10.. (bulk), current
        let h = state(&[(1, 0, 5), (2, 5, 9), (3, 10, 10)]);
        assert_eq!(h.owner(3, 3, 12), Some(1));
        assert_eq!(h.owner(5, 3, 12), Some(2));
        assert_eq!(h.owner(12, 3, 12), Some(3));
        assert_eq!(h.owner(13, 3, 12), None);
        assert_eq!(h.reconstructable(3, 12), [(0, 12)]);
        let h = state(&[(1, 0, 3), (3, 10, 10)]);
        assert_eq!(h.reconstructable(3, 12), [(0, 3), (10, 12)]);
    }

    #[test]
    fn needed_generations() {
        let ts = |s: u64| Some(s as i64 * 1000);
        let mut h = state(&[(1, 0, 5), (2, 5, 9), (3, 9, 9)]);
        // nothing pinned: only the current generation stays
        assert!(h.needed(3, 12, 0, &ts, 8).is_empty());
        // a pin at 3 holds gen 1; a pin at 5 is served by gen 2
        h.pins.insert(
            "a".into(),
            Pin {
                seq: 3,
                created_ms: 0,
                note: None,
                expires_ms: None,
                warm: false,
            },
        );
        h.pins.insert(
            "b".into(),
            Pin {
                seq: 5,
                created_ms: 0,
                note: None,
                expires_ms: None,
                warm: false,
            },
        );
        let n = h.needed(3, 12, 0, &ts, 8);
        assert_eq!(n.keys().copied().collect::<Vec<_>>(), [1, 2]);
        assert_eq!(n[&1], [Hold::Snapshot("a".into())]);
        // a pin at the head of a compacted generation needs nothing more
        h.pins.clear();
        h.pins.insert(
            "c".into(),
            Pin {
                seq: 9,
                created_ms: 0,
                note: None,
                expires_ms: None,
                warm: false,
            },
        );
        assert!(h.needed(3, 12, 0, &ts, 8).is_empty());
        // the commit window
        h.pins.clear();
        h.retention.keep_commits = Some(5); // 8..=12
        assert_eq!(
            h.needed(3, 12, 0, &ts, 8)
                .keys()
                .copied()
                .collect::<Vec<_>>(),
            [2]
        );
        // the age window: commits whose successor came after the cutoff
        h.retention = Retention {
            keep_commits: None,
            keep_age_ms: Some(7_500),
            max_bytes: None,
        };
        // now 12 000, cutoff 4 500: s with ts(s) > 4500 → s ≥ 5, so s from 4 is kept
        assert_eq!(
            h.needed(3, 12, 12_000, &ts, 8)
                .keys()
                .copied()
                .collect::<Vec<_>>(),
            [1, 2]
        );
        // at most one window-only generation: the oldest is dropped
        assert_eq!(
            h.needed(3, 12, 12_000, &ts, 1)
                .keys()
                .copied()
                .collect::<Vec<_>>(),
            [2]
        );
        // at most `max_bytes` of kept generations: the oldest window-only ones go first
        h.gens.get_mut(&1).unwrap().bytes = 100;
        h.gens.get_mut(&2).unwrap().bytes = 50;
        let keep = |h: &HistoryState| {
            h.needed(3, 12, 12_000, &ts, 8)
                .keys()
                .copied()
                .collect::<Vec<_>>()
        };
        h.retention.max_bytes = Some(150);
        assert_eq!(keep(&h), [1, 2]);
        h.retention.max_bytes = Some(149);
        assert_eq!(keep(&h), [2]);
        h.retention.max_bytes = Some(10);
        assert!(keep(&h).is_empty());
        // a pin wins over the byte limit
        h.pins.insert(
            "p".into(),
            Pin {
                seq: 2,
                created_ms: 0,
                note: None,
                expires_ms: None,
                warm: false,
            },
        );
        assert_eq!(keep(&h), [1]);
        h.pins.clear();
        h.retention.max_bytes = None;
    }

    #[test]
    fn the_catalog_horizon_keeps_either_limit() {
        // commit s was made at 1000·s
        let made_since = |ms: i64| Some(((ms.max(0) + 999) / 1000) as u64).filter(|s| *s <= 50);
        let h = |n: Option<u64>, a: Option<u64>| CatalogHorizon {
            keep_commits: n,
            keep_age_ms: a,
        };
        assert_eq!(h(None, None).cutoff(50, 50_000, &made_since), None);
        assert_eq!(h(Some(5), None).cutoff(50, 50_000, &made_since), Some(46));
        assert_eq!(h(Some(500), None).cutoff(50, 50_000, &made_since), Some(0));
        assert_eq!(
            h(None, Some(10_000)).cutoff(50, 50_000, &made_since),
            Some(40)
        );
        // nothing made recently: only the head
        assert_eq!(h(None, Some(10)).cutoff(50, 90_000, &made_since), Some(50));
        // both: what either keeps
        assert_eq!(
            h(Some(5), Some(10_000)).cutoff(50, 50_000, &made_since),
            Some(40)
        );
        assert_eq!(
            h(Some(20), Some(1_000)).cutoff(50, 50_000, &made_since),
            Some(31)
        );
    }

    #[test]
    fn leases_keep_generations() {
        let ts = |s: u64| Some(s as i64 * 1000);
        // gen 2 is compacted into gen 3 at its head 9: nothing needs it by commits
        let mut h = state(&[(1, 0, 5), (2, 5, 9), (3, 9, 9)]);
        assert!(h.needed(3, 12, 0, &ts, 8).is_empty());
        let a = h.lease(2, "nightly");
        let b = h.lease(2, "hourly");
        h.lease(3, "now");
        let n = h.needed(3, 12, 0, &ts, 8);
        assert_eq!(n.keys().copied().collect::<Vec<_>>(), [2]);
        assert_eq!(
            n[&2],
            [Hold::Lease("nightly".into()), Hold::Lease("hourly".into())]
        );
        assert_eq!(n[&2][0].to_string(), "backup:nightly");
        // next to the retention window, and outside the generation limit
        h.retention.keep_commits = Some(20);
        let n = h.needed(3, 12, 0, &ts, 8);
        assert_eq!(n.keys().copied().collect::<Vec<_>>(), [1, 2]);
        assert_eq!(n[&2].len(), 3);
        assert_eq!(n[&2][0], Hold::Retention);
        let n = h.needed(3, 12, 0, &ts, 0);
        assert_eq!(n.keys().copied().collect::<Vec<_>>(), [2]);
        assert_eq!(n[&2].len(), 2);
        h.leases.remove(&a);
        h.leases.remove(&b);
        assert!(h.needed(3, 12, 0, &ts, 0).is_empty());
        assert_eq!(h.lease_holds(3).count(), 1);
    }
}
