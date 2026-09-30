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
    /// the generation that would serve it now
    pub generation: Option<String>,
    /// false only after external damage (its generation is gone)
    pub reconstructable: bool,
}

/// The retention window: the states that were the head within the last `keep_commits`
/// commits or `keep_age_ms` milliseconds stay readable.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Retention {
    pub keep_commits: Option<u64>,
    pub keep_age_ms: Option<u64>,
}

/// Why a generation is kept.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Hold {
    Head,
    Snapshot(String),
    Retention,
}

impl std::fmt::Display for Hold {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Hold::Head => write!(f, "head"),
            Hold::Snapshot(n) => write!(f, "snapshot:{n}"),
            Hold::Retention => write!(f, "retention"),
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
    pub cache_entries: usize,
    pub cache_bytes: u64,
    pub hits: u64,
    pub misses: u64,
    pub materializations: u64,
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
}

#[derive(Serialize, Deserialize)]
struct PinFile {
    name: String,
    seq: u64,
    created: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    note: Option<String>,
}

#[derive(Clone, Debug)]
pub(crate) struct Pin {
    pub seq: u64,
    pub created_ms: i64,
    pub note: Option<String>,
}

/// Pins and retention from `history.json` (none if the file is missing).
pub(crate) fn read_file(
    root: &Path,
    dataset_id: uuid::Uuid,
) -> Result<(BTreeMap<String, Pin>, Retention)> {
    let bytes = match std::fs::read(root.join("history.json")) {
        Ok(b) => b,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Ok((BTreeMap::new(), Retention::default()));
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
                },
            )
        })
        .collect();
    Ok((pins, f.retention))
}

/// Write `history.json` durably (before anything relies on it).
pub(crate) fn write_file(
    root: &Path,
    dataset_id: uuid::Uuid,
    pins: &BTreeMap<String, Pin>,
    retention: Retention,
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
            })
            .collect(),
    };
    crate::store::write_atomic(
        &root.join("history.json"),
        &serde_json::to_vec_pretty(&f).unwrap(),
    )
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

/// The in-memory history state of a persistent store.
pub(crate) struct HistoryState {
    pub pins: BTreeMap<String, Pin>,
    pub retention: Retention,
    /// by generation number, the current one included
    pub gens: BTreeMap<u32, GenEntry>,
    /// sealed generations open for reading, most recent first
    pub open: Vec<(u32, Arc<Generation>)>,
    /// materialized past states by (generation, commit), most recent first
    pub cache: Vec<((u32, u64), Arc<Snapshot>, u64)>,
    pub hits: u64,
    pub misses: u64,
    pub materializations: u64,
}

impl HistoryState {
    pub fn new(pins: BTreeMap<String, Pin>, retention: Retention) -> HistoryState {
        HistoryState {
            pins,
            retention,
            gens: BTreeMap::new(),
            open: Vec::new(),
            cache: Vec::new(),
            hits: 0,
            misses: 0,
            materializations: 0,
        }
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

    /// The non-current generations to keep, with what holds each: a generation is
    /// needed if it is the newest retained one covering some protected commit. At most
    /// `max_gens` are kept for the retention window alone (oldest dropped first); pins
    /// always win.
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
        if window_only.len() > allowed {
            for no in &window_only[..window_only.len() - allowed] {
                out.remove(no);
            }
        }
        out
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
    let Some(name) = dir.file_name() else {
        return Ok(());
    };
    let doomed = root.join(format!("{}.deleting", name.to_string_lossy()));
    std::fs::rename(dir, &doomed)?;
    crate::store::sync_dir(root)?;
    std::fs::remove_dir_all(&doomed)?;
    Ok(())
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
            },
        );
        h.pins.insert(
            "b".into(),
            Pin {
                seq: 5,
                created_ms: 0,
                note: None,
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
    }
}
