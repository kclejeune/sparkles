//! Durable source holds (spec C25 §3 and §7).
//!
//! An evidence reference names a dataset, a branch and a commit by their identities.
//! A source hold keeps such a commit readable: history trimming, the retention window,
//! compaction and generation retirement keep every generation that serves a held
//! commit, and an in-memory store keeps the held state. A branch is not deleted while a
//! hold names it, unless the deletion releases its holds.
//!
//! The holds of a dataset and all its branches are recorded in `holds.json` at the
//! dataset root:
//!
//! ```json
//! {"format": 1, "datasetId": "…",
//!  "holds": [{"id": "…", "owner": "evidence", "created": "2026-11-01T00:00:00.000Z",
//!             "commits": [{"branchId": "…", "seq": 12, "storeId": "…"}]}]}
//! ```
//!
//! `branchId` is the branch the caller named. `storeId`, present when it differs, is the
//! branch that made the commit: a commit at or below a branch's starting point belongs
//! to its upstream, whose history the hold keeps. Together the holds name at most
//! [`MAX_COMMITS`] commits. A dataset with `holds.json` needs dataset reader 7
//! ([`READER`]), so a build that ignores the file refuses to open the dataset instead of
//! trimming held history.
//!
//! A `holds.json` of a newer format refuses the open. One that cannot be read or does not
//! belong to the dataset makes the holds unavailable. Reads and writes continue, no
//! generation or in-memory state is removed until the file is repaired, and acquiring or
//! releasing a hold fails with `holds-unavailable`.
//!
//! A hold is acquired before the commit that refers to it and is durable when the
//! acquisition returns. An owner whose referencing commit never published leaves an
//! orphan, which [`crate::store::Store::reconcile_holds`] releases after a grace period.
//! Backups and clones do not carry holds, as they do not carry past states.

use crate::branch::CommitRef;
use crate::error::{Error, Result};
use crate::managed::{ManagedError, ManagedErrorKind, OwnerKind};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;

/// The holds file at the dataset root.
pub const FILE: &str = "holds.json";

/// The `holds.json` format this build reads and writes.
pub const FORMAT: u32 = 1;

/// The dataset reader capability that honors holds (`minimumReader` of `dataset.json`).
pub const READER: u32 = 7;

/// The most commits all the holds of a dataset may name together.
pub const MAX_COMMITS: usize = 65_536;

/// The largest `holds.json` this build reads, in bytes.
pub const MAX_FILE_BYTES: u64 = 32 << 20;

/// The default age, in milliseconds, after which an unreferenced hold is an orphan
/// ([`crate::store::Store::reconcile_holds`]).
pub const DEFAULT_GRACE_MS: i64 = 3_600_000;

/// One durable source hold: the commits its owner keeps readable.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SourceHold {
    /// The hold's identity, which its owner chooses, such as an operation's id.
    pub id: uuid::Uuid,
    pub owner: OwnerKind,
    /// When the hold was acquired, in milliseconds since the epoch.
    pub created_ms: i64,
    /// The held commits as the owner named them, sorted and distinct.
    pub commits: Vec<CommitRef>,
}

/// A hold and, for each of its commits, the branch store whose history serves it.
#[derive(Clone, Debug)]
pub(crate) struct Record {
    pub hold: SourceHold,
    pub stores: Vec<uuid::Uuid>,
    /// Raised by every acquisition of the hold, the repeated ones too, so that a
    /// reconciliation releases only a hold nobody acquired since it looked. In memory
    /// only.
    pub touch: u64,
    /// When the hold was last acquired, its creation or a repeated acquisition, which
    /// starts its grace period again. In memory only: a reopened hold starts from its
    /// creation.
    pub acquired_ms: i64,
}

/// The holds of a dataset family, or why they cannot be read.
#[derive(Clone, Debug)]
pub(crate) enum HoldsState {
    Ready(BTreeMap<uuid::Uuid, Record>),
    Unavailable(String),
}

impl Default for HoldsState {
    fn default() -> Self {
        HoldsState::Ready(BTreeMap::new())
    }
}

impl HoldsState {
    /// The commits the holds name together.
    pub(crate) fn commits(records: &BTreeMap<uuid::Uuid, Record>) -> usize {
        records.values().map(|r| r.hold.commits.len()).sum()
    }

    /// The protection these holds give each branch store.
    pub(crate) fn index(&self) -> HeldIndex {
        match self {
            HoldsState::Unavailable(reason) => HeldIndex {
                by_store: HashMap::new(),
                unavailable: Some(reason.clone()),
            },
            HoldsState::Ready(records) => HeldIndex::of(records.values()),
        }
    }
}

/// The commits held on each branch store, read without a lock by the collectors.
#[derive(Clone, Debug, Default)]
pub(crate) struct HeldIndex {
    /// sorted, distinct commits by the store whose history serves them
    by_store: HashMap<uuid::Uuid, Arc<[u64]>>,
    /// Set when the holds cannot be read: every past state is then kept.
    pub unavailable: Option<String>,
}

impl HeldIndex {
    pub(crate) fn of<'a>(records: impl Iterator<Item = &'a Record>) -> HeldIndex {
        let mut by: HashMap<uuid::Uuid, Vec<u64>> = HashMap::new();
        for r in records {
            for (c, store) in r.hold.commits.iter().zip(&r.stores) {
                by.entry(*store).or_default().push(c.seq);
            }
        }
        HeldIndex {
            by_store: by
                .into_iter()
                .map(|(k, mut v)| {
                    v.sort_unstable();
                    v.dedup();
                    (k, Arc::from(v))
                })
                .collect(),
            unavailable: None,
        }
    }

    /// An index of the given commits by store.
    pub(crate) fn with(by_store: HashMap<uuid::Uuid, Arc<[u64]>>) -> HeldIndex {
        HeldIndex {
            by_store,
            unavailable: None,
        }
    }

    /// The index of holds that cannot be read, which keeps every past state.
    pub(crate) fn unreadable(reason: String) -> HeldIndex {
        HeldIndex {
            by_store: HashMap::new(),
            unavailable: Some(reason),
        }
    }

    /// The commits held on store `store`, ascending.
    pub(crate) fn on(&self, store: uuid::Uuid) -> Arc<[u64]> {
        self.by_store
            .get(&store)
            .cloned()
            .unwrap_or_else(|| Arc::from(Vec::new()))
    }

    /// Whether commit `seq` of store `store` is held.
    pub(crate) fn holds(&self, store: uuid::Uuid, seq: u64) -> bool {
        self.by_store
            .get(&store)
            .is_some_and(|v| v.binary_search(&seq).is_ok())
    }
}

/// The family's holds: the recorded state behind its mutation lock, and the index the
/// collectors read.
pub(crate) struct FamilyHolds {
    /// Held by every acquisition, release and branch deletion that changes the holds.
    /// Lock order: this before a branch table or a store's history.
    pub state: parking_lot::Mutex<HoldsState>,
    /// Shared by a commit that publishes a reference to a hold, from its check of the
    /// hold until the commit has published or been refused, and taken exclusively by
    /// every release before it takes `state`. So a release either happens before the
    /// check, which then refuses the commit, or waits until the commit is published.
    /// Lock order: a writer, then this, then `state`.
    pub publishing: Arc<parking_lot::RwLock<()>>,
    pub index: Arc<arc_swap::ArcSwap<HeldIndex>>,
    /// the last [`Record::touch`] given out
    pub touches: std::sync::atomic::AtomicU64,
    /// Set, under the holds' lock, when a write of `holds.json` failed after it may
    /// have replaced the file, so that the file may differ from the installed holds.
    /// The next change or repeated acquisition writes the installed holds again before
    /// it answers.
    pub uncertain: std::sync::atomic::AtomicBool,
}

impl Default for FamilyHolds {
    fn default() -> Self {
        FamilyHolds {
            state: parking_lot::Mutex::new(HoldsState::default()),
            publishing: Default::default(),
            index: Arc::new(arc_swap::ArcSwap::from_pointee(HeldIndex::default())),
            touches: Default::default(),
            uncertain: Default::default(),
        }
    }
}

impl FamilyHolds {
    /// Replace the recorded state and publish its index.
    pub(crate) fn install(&self, state: HoldsState) {
        self.index.store(Arc::new(state.index()));
        *self.state.lock() = state;
    }
}

/// What the holds protect on one store, as its history state sees them.
#[derive(Clone)]
pub(crate) struct HeldView {
    pub index: Arc<arc_swap::ArcSwap<HeldIndex>>,
    pub store: uuid::Uuid,
}

impl HeldView {
    /// The commits held on this store now, or `None` when the holds cannot be read and
    /// every past state is kept.
    pub(crate) fn held(&self) -> Option<Arc<[u64]>> {
        let index = self.index.load();
        match index.unavailable {
            Some(_) => None,
            None => Some(index.on(self.store)),
        }
    }
}

// ------------------------------------------------------------------ errors ------

pub(crate) fn unavailable(reason: &str) -> Error {
    ManagedError::error(
        ManagedErrorKind::HoldsUnavailable,
        format!("the source holds cannot be read: {reason}"),
    )
}

pub(crate) fn conflict(message: impl Into<String>) -> Error {
    ManagedError::error(ManagedErrorKind::HoldConflict, message)
}

pub(crate) fn limit(held: usize, more: usize) -> Error {
    ManagedError::error(
        ManagedErrorKind::HoldsLimit,
        format!(
            "the holds of this dataset name {held} commits; {more} more would pass the limit of {MAX_COMMITS}"
        ),
    )
}

// -------------------------------------------------------------- holds.json ------

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct File {
    format: u32,
    dataset_id: uuid::Uuid,
    #[serde(default)]
    holds: Vec<HoldFile>,
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct HoldFile {
    id: uuid::Uuid,
    owner: String,
    created: String,
    commits: Vec<CommitFile>,
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct CommitFile {
    branch_id: uuid::Uuid,
    seq: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    store_id: Option<uuid::Uuid>,
}

/// The bytes of `holds.json` for `records` of dataset `dataset_id`.
pub(crate) fn encode(dataset_id: uuid::Uuid, records: &BTreeMap<uuid::Uuid, Record>) -> Vec<u8> {
    let f = File {
        format: FORMAT,
        dataset_id,
        holds: records
            .values()
            .map(|r| HoldFile {
                id: r.hold.id,
                owner: r.hold.owner.as_str().to_string(),
                created: crate::commit::rfc3339_ms(r.hold.created_ms),
                commits: r
                    .hold
                    .commits
                    .iter()
                    .zip(&r.stores)
                    .map(|(c, s)| CommitFile {
                        branch_id: c.branch_id,
                        seq: c.seq,
                        store_id: (*s != c.branch_id).then_some(*s),
                    })
                    .collect(),
            })
            .collect(),
    };
    serde_json::to_vec(&f).expect("holds serialize")
}

/// The holds `bytes` record for dataset `dataset_id`. A newer format is
/// [`Error::Unsupported`]; anything else this build cannot accept is
/// [`Error::Corrupt`].
pub(crate) fn decode(bytes: &[u8], dataset_id: uuid::Uuid) -> Result<BTreeMap<uuid::Uuid, Record>> {
    let corrupt = |m: String| Error::Corrupt(format!("{FILE}: {m}"));
    let v: serde_json::Value = serde_json::from_slice(bytes).map_err(|e| corrupt(e.to_string()))?;
    crate::formats::check_settings(FILE, &v)?;
    let f: File = serde_json::from_value(v).map_err(|e| corrupt(e.to_string()))?;
    if f.format != FORMAT {
        return Err(corrupt(format!("format {} is not {FORMAT}", f.format)));
    }
    if f.dataset_id != dataset_id {
        return Err(corrupt(format!(
            "belongs to dataset {}, not {dataset_id}",
            f.dataset_id
        )));
    }
    let mut out = BTreeMap::new();
    let mut total = 0usize;
    for h in f.holds {
        let owner = OwnerKind::parse(&h.owner)
            .ok_or_else(|| corrupt(format!("hold {} has an unknown owner {:?}", h.id, h.owner)))?;
        let created_ms = crate::commit::parse_rfc3339_ms(&h.created)
            .ok_or_else(|| corrupt(format!("hold {} has an invalid created time", h.id)))?;
        if h.commits.is_empty() {
            return Err(corrupt(format!("hold {} names no commit", h.id)));
        }
        total += h.commits.len();
        if total > MAX_COMMITS {
            return Err(corrupt(format!(
                "the holds name more than {MAX_COMMITS} commits"
            )));
        }
        let mut commits = Vec::with_capacity(h.commits.len());
        let mut stores = Vec::with_capacity(h.commits.len());
        for c in &h.commits {
            commits.push(CommitRef {
                branch_id: c.branch_id,
                seq: c.seq,
            });
            stores.push(c.store_id.unwrap_or(c.branch_id));
        }
        if commits.windows(2).any(|w| w[0] >= w[1]) {
            return Err(corrupt(format!(
                "the commits of hold {} are not sorted and distinct",
                h.id
            )));
        }
        let record = Record {
            hold: SourceHold {
                id: h.id,
                owner,
                created_ms,
                commits,
            },
            stores,
            touch: 0,
            acquired_ms: created_ms,
        };
        if out.insert(h.id, record).is_some() {
            return Err(corrupt(format!("hold {} is listed twice", h.id)));
        }
    }
    Ok(out)
}

/// The holds recorded at `root` for dataset `dataset_id`: none without the file, and
/// unavailable when the file cannot be read or accepted.
pub(crate) fn load_dir(root: &std::path::Path, dataset_id: uuid::Uuid) -> HoldsState {
    let path = root.join(FILE);
    match std::fs::metadata(&path) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return HoldsState::default(),
        Ok(m) if m.len() > MAX_FILE_BYTES => {
            return held_back(
                root,
                format!("{FILE} is larger than {MAX_FILE_BYTES} bytes"),
            );
        }
        _ => {}
    }
    match std::fs::read(&path)
        .map_err(Error::from)
        .and_then(|b| decode(&b, dataset_id))
    {
        Ok(records) => HoldsState::Ready(records),
        Err(e) => held_back(root, e.to_string()),
    }
}

fn held_back(root: &std::path::Path, reason: String) -> HoldsState {
    tracing::error!(target: "sparkles::store", "{}: no past state is removed until the source holds can be read: {reason}", root.display());
    HoldsState::Unavailable(reason)
}

/// The commits held on store `store` of the dataset `dataset_id` whose holds are
/// recorded at `root`, for read-only tools: `None` when the holds cannot be read.
pub(crate) fn held_offline(
    root: &std::path::Path,
    dataset_id: uuid::Uuid,
    store: uuid::Uuid,
) -> Option<Arc<[u64]>> {
    match load_dir(root, dataset_id) {
        HoldsState::Ready(r) => Some(HeldIndex::of(r.values()).on(store)),
        HoldsState::Unavailable(_) => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn record(id: u128, commits: &[(u128, u64, u128)]) -> Record {
        Record {
            hold: SourceHold {
                id: uuid::Uuid::from_u128(id),
                owner: OwnerKind::Evidence,
                created_ms: 1_700_000_000_000,
                commits: commits
                    .iter()
                    .map(|(b, s, _)| CommitRef {
                        branch_id: uuid::Uuid::from_u128(*b),
                        seq: *s,
                    })
                    .collect(),
            },
            stores: commits
                .iter()
                .map(|(_, _, s)| uuid::Uuid::from_u128(*s))
                .collect(),
            touch: 0,
            acquired_ms: 1_700_000_000_000,
        }
    }

    #[test]
    fn round_trip_and_store_ids() {
        let ds = uuid::Uuid::from_u128(9);
        let mut m = BTreeMap::new();
        // sorted by branch, then seq; the first is a commit of its upstream
        m.insert(uuid::Uuid::from_u128(1), record(1, &[(7, 2, 9), (9, 3, 9)]));
        let bytes = encode(ds, &m);
        let v: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(v["format"], 1);
        let c = &v["holds"][0]["commits"];
        assert_eq!(c[0]["storeId"], uuid::Uuid::from_u128(9).to_string());
        assert!(c[1].get("storeId").is_none(), "same as the branch");
        let back = decode(&bytes, ds).unwrap();
        assert_eq!(
            back[&uuid::Uuid::from_u128(1)].hold,
            m[&uuid::Uuid::from_u128(1)].hold
        );
        assert_eq!(
            back[&uuid::Uuid::from_u128(1)].stores,
            m[&uuid::Uuid::from_u128(1)].stores
        );
        let index = HeldIndex::of(back.values());
        assert_eq!(&*index.on(uuid::Uuid::from_u128(9)), &[2, 3]);
        assert!(index.holds(uuid::Uuid::from_u128(9), 2));
        assert!(!index.holds(uuid::Uuid::from_u128(7), 2));
    }

    #[test]
    fn refusals() {
        let ds = uuid::Uuid::from_u128(9);
        let other = uuid::Uuid::from_u128(8);
        let bytes = encode(ds, &BTreeMap::new());
        assert!(matches!(decode(&bytes, other), Err(Error::Corrupt(_))));
        let newer = format!(r#"{{"format":2,"datasetId":"{ds}","holds":[]}}"#);
        assert!(matches!(
            decode(newer.as_bytes(), ds),
            Err(Error::Unsupported(_))
        ));
        let unknown = format!(r#"{{"format":1,"datasetId":"{ds}","holds":[],"x":1}}"#);
        assert!(matches!(
            decode(unknown.as_bytes(), ds),
            Err(Error::Corrupt(_))
        ));
        let hold = |owner: &str, commits: &str| {
            format!(
                r#"{{"format":1,"datasetId":"{ds}","holds":[{{"id":"{}","owner":"{owner}","created":"2026-11-01T00:00:00.000Z","commits":{commits}}}]}}"#,
                uuid::Uuid::from_u128(1)
            )
        };
        let c = |s: u64| format!(r#"{{"branchId":"{ds}","seq":{s}}}"#);
        assert!(decode(hold("evidence", &format!("[{}]", c(1))).as_bytes(), ds).is_ok());
        assert!(decode(hold("jobs", &format!("[{}]", c(1))).as_bytes(), ds).is_ok());
        assert!(decode(hold("unknown", &format!("[{}]", c(1))).as_bytes(), ds).is_err());
        assert!(decode(hold("evidence", "[]").as_bytes(), ds).is_err());
        assert!(
            decode(
                hold("evidence", &format!("[{},{}]", c(2), c(1))).as_bytes(),
                ds
            )
            .is_err(),
            "unsorted"
        );
        assert!(
            decode(
                hold("evidence", &format!("[{},{}]", c(1), c(1))).as_bytes(),
                ds
            )
            .is_err(),
            "duplicate"
        );
    }

    #[test]
    fn the_bound_is_checked_when_read() {
        let ds = uuid::Uuid::from_u128(9);
        let mut m = BTreeMap::new();
        let per = 4096u64;
        let holds = MAX_COMMITS as u64 / per + 1;
        for h in 0..holds {
            let commits: Vec<(u128, u64, u128)> = (0..per).map(|s| (9, h * per + s, 9)).collect();
            m.insert(
                uuid::Uuid::from_u128(h as u128 + 1),
                record(h as u128 + 1, &commits),
            );
        }
        let bytes = encode(ds, &m);
        assert!(matches!(decode(&bytes, ds), Err(Error::Corrupt(_))));
        m.remove(&uuid::Uuid::from_u128(holds as u128));
        assert_eq!(HoldsState::commits(&m), MAX_COMMITS);
        assert!(decode(&encode(ds, &m), ds).is_ok());
    }
}
