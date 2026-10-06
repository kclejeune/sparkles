//! Compaction in the background, and the policy that decides when it is due.
//!
//! [`Store::compact_with`] builds a new generation from a snapshot without the writer
//! lock. Writes go on meanwhile. While a compaction runs, each commit also leaves its
//! changes in a tap (`WriterState::tap`), which the compaction drains and carries into
//! the new generation: the ids are translated into the new generation's, the changes are
//! applied to its delta, and each commit is written to its log under its own number. The
//! writer lock is held only for the last, short round of that catch-up and the switch of
//! `CURRENT`.
//!
//! [`CompactionPolicy`] says from [`CompactionMeasures`] whether a compaction is due. A
//! dataset's own settings ([`CompactionSettings`]) live in `compaction.json`. The server
//! runs the policy; the C13 spec has the design.

use super::link::{LINK_FILE, LinkFile, OVERLAY_FILE, Overlay, Segment};
use super::partial::{self, PartialMode};
use super::*;
use serde::{Deserialize, Serialize};
use std::sync::atomic::{AtomicI64, AtomicU32, AtomicU64};
use std::time::{Duration, Instant};

/// The file in a dataset directory that holds the dataset's own compaction settings.
pub const COMPACTION_FILE: &str = "compaction.json";

/// The file that marks a generation directory as a compaction's unfinished build.
const BUILDING_FILE: &str = "compacting";

/// A catch-up round that finds fewer commits than this is the last one, and runs under
/// the writer lock.
const LAST_ROUND: usize = 64;
/// Catch-up rounds without the writer lock, at most.
const MAX_ROUNDS: usize = 16;
/// Builder temporary files per quad, for the free-space estimate.
const TEMP_BYTES_PER_QUAD: u64 = 32;
/// No timestamp yet.
const NONE: i64 = i64::MIN;

// ------------------------------------------------------------------- policy ------

/// When a dataset is compacted automatically. `0` turns off the size, idle and age
/// triggers it is the limit of.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CompactionPolicy {
    pub enabled: bool,
    /// neither the relative trigger nor the idle trigger fires with a smaller delta (the
    /// age trigger does)
    pub min_delta_quads: u64,
    /// the relative trigger: `min_delta_quads + delta_ratio × base quads`
    pub delta_ratio: f64,
    /// the absolute trigger, whatever the base
    pub max_delta_quads: u64,
    /// the estimated memory of the delta and its new terms, in MiB
    pub max_delta_mb: u64,
    /// the write-ahead log of the current generation, in MiB
    pub max_wal_mb: u64,
    /// no commit for this long, with at least `min_delta_quads` in the delta
    pub idle_seconds: u64,
    /// the oldest commit not yet compacted is older than this
    pub max_age_seconds: u64,
    /// no automatic compaction starts sooner than this after the previous one ended
    pub min_interval_seconds: u64,
    /// whether a compaction may rewrite only the blocks the delta touches
    #[serde(default)]
    pub partial: PartialMode,
}

impl Default for CompactionPolicy {
    fn default() -> Self {
        CompactionPolicy {
            enabled: true,
            min_delta_quads: 10_000,
            delta_ratio: 0.05,
            max_delta_quads: 1_000_000,
            max_delta_mb: 512,
            max_wal_mb: 1024,
            idle_seconds: 300,
            max_age_seconds: 86_400,
            min_interval_seconds: 60,
            partial: PartialMode::Auto,
        }
    }
}

/// A dataset's own compaction settings: each one set overrides the server's.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CompactionSettings {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub enabled: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min_delta_quads: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub delta_ratio: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_delta_quads: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_delta_mb: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_wal_mb: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub idle_seconds: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_age_seconds: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min_interval_seconds: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub partial: Option<PartialMode>,
}

/// The setting names, as JSON keys and `key=value` arguments.
pub const SETTING_NAMES: [&str; 10] = [
    "enabled",
    "minDeltaQuads",
    "deltaRatio",
    "maxDeltaQuads",
    "maxDeltaMb",
    "maxWalMb",
    "idleSeconds",
    "maxAgeSeconds",
    "minIntervalSeconds",
    "partial",
];

impl CompactionSettings {
    pub fn is_empty(&self) -> bool {
        *self == CompactionSettings::default()
    }

    /// Settings from a JSON object of setting names (a `format` member is ignored).
    pub fn from_json(v: &serde_json::Value) -> Result<CompactionSettings> {
        let mut v = v.clone();
        let Some(o) = v.as_object_mut() else {
            return Err(Error::invalid(
                "compaction settings must be a JSON object of settings",
            ));
        };
        o.remove("format");
        let s: CompactionSettings = serde_json::from_value(v).map_err(|e| {
            Error::invalid(format!(
                "compaction settings: {e} (the settings are {})",
                SETTING_NAMES.join(", ")
            ))
        })?;
        s.validate()?;
        Ok(s)
    }

    /// Set one setting from `key=value` text (`sparkles compaction --set`). A value
    /// that is not valid leaves the settings as they were.
    pub fn set(&mut self, key: &str, value: &str) -> Result<()> {
        let mut next = self.clone();
        next.set_unchecked(key, value)?;
        next.validate()?;
        *self = next;
        Ok(())
    }

    fn set_unchecked(&mut self, key: &str, value: &str) -> Result<()> {
        let n = || {
            value.trim().replace('_', "").parse::<u64>().map_err(|_| {
                Error::invalid(format!("{key}: expected a whole number, not {value:?}"))
            })
        };
        match key {
            "enabled" => {
                self.enabled = Some(match value.trim() {
                    "true" | "on" | "yes" | "1" => true,
                    "false" | "off" | "no" | "0" => false,
                    _ => {
                        return Err(Error::invalid(format!(
                            "enabled: expected true or false, not {value:?}"
                        )));
                    }
                })
            }
            "minDeltaQuads" => self.min_delta_quads = Some(n()?),
            "deltaRatio" => {
                self.delta_ratio = Some(value.trim().parse::<f64>().map_err(|_| {
                    Error::invalid(format!("deltaRatio: expected a number, not {value:?}"))
                })?)
            }
            "maxDeltaQuads" => self.max_delta_quads = Some(n()?),
            "maxDeltaMb" => self.max_delta_mb = Some(n()?),
            "maxWalMb" => self.max_wal_mb = Some(n()?),
            "idleSeconds" => self.idle_seconds = Some(n()?),
            "maxAgeSeconds" => self.max_age_seconds = Some(n()?),
            "minIntervalSeconds" => self.min_interval_seconds = Some(n()?),
            "partial" => {
                self.partial = Some(PartialMode::parse(value).ok_or_else(|| {
                    Error::invalid(format!(
                        "partial: expected auto, off or always, not {value:?}"
                    ))
                })?)
            }
            _ => {
                return Err(Error::invalid(format!(
                    "unknown compaction setting {key:?} (the settings are {})",
                    SETTING_NAMES.join(", ")
                )));
            }
        }
        Ok(())
    }

    pub fn validate(&self) -> Result<()> {
        if let Some(r) = self.delta_ratio
            && !(r.is_finite() && (0.0..=1000.0).contains(&r))
        {
            return Err(Error::invalid(format!(
                "deltaRatio must be a number from 0 to 1000, not {r}"
            )));
        }
        Ok(())
    }
}

impl CompactionPolicy {
    /// This policy with a dataset's own settings applied.
    pub fn with(&self, own: &CompactionSettings) -> CompactionPolicy {
        CompactionPolicy {
            enabled: own.enabled.unwrap_or(self.enabled),
            min_delta_quads: own.min_delta_quads.unwrap_or(self.min_delta_quads),
            delta_ratio: own.delta_ratio.unwrap_or(self.delta_ratio),
            max_delta_quads: own.max_delta_quads.unwrap_or(self.max_delta_quads),
            max_delta_mb: own.max_delta_mb.unwrap_or(self.max_delta_mb),
            max_wal_mb: own.max_wal_mb.unwrap_or(self.max_wal_mb),
            idle_seconds: own.idle_seconds.unwrap_or(self.idle_seconds),
            max_age_seconds: own.max_age_seconds.unwrap_or(self.max_age_seconds),
            min_interval_seconds: own
                .min_interval_seconds
                .unwrap_or(self.min_interval_seconds),
            partial: own.partial.unwrap_or(self.partial),
        }
    }

    /// The delta size at which the relative trigger fires for a base of `base_quads`
    /// (capped by the absolute trigger).
    pub fn threshold(&self, base_quads: u64) -> u64 {
        let rel = self
            .min_delta_quads
            .saturating_add((self.delta_ratio * base_quads as f64) as u64)
            .max(1);
        match self.max_delta_quads {
            0 => rel,
            max => rel.min(max),
        }
    }

    /// Whether a compaction is due, and why (the first trigger that fires). It does not
    /// look at `enabled`.
    pub fn verdict(&self, m: &CompactionMeasures) -> Option<Trigger> {
        let delta = m.delta_quads;
        let pending = delta > 0 || m.head > m.base_seq;
        if !pending {
            return None;
        }
        let t = |kind, detail: String| Some(Trigger { kind, detail });
        if self.max_delta_quads > 0 && delta >= self.max_delta_quads {
            return t(
                TriggerKind::MaxDelta,
                format!(
                    "delta of {delta} quads reached maxDeltaQuads {}",
                    self.max_delta_quads
                ),
            );
        }
        let threshold = self.threshold(m.base_quads);
        if delta >= threshold {
            return t(
                TriggerKind::Ratio,
                format!(
                    "delta of {delta} quads reached {threshold} ({} + {} x {} base quads)",
                    self.min_delta_quads, self.delta_ratio, m.base_quads
                ),
            );
        }
        if self.max_delta_mb > 0 && m.delta_bytes >= self.max_delta_mb << 20 {
            return t(
                TriggerKind::DeltaBytes,
                format!(
                    "delta of about {} reached maxDeltaMb {}",
                    crate::error::human_bytes(m.delta_bytes),
                    self.max_delta_mb
                ),
            );
        }
        if self.max_wal_mb > 0 && m.wal_bytes >= self.max_wal_mb << 20 {
            return t(
                TriggerKind::WalBytes,
                format!(
                    "write-ahead log of {} reached maxWalMb {}",
                    crate::error::human_bytes(m.wal_bytes),
                    self.max_wal_mb
                ),
            );
        }
        let floor = delta >= self.min_delta_quads.max(1);
        if self.idle_seconds > 0
            && floor
            && m.idle_ms.is_some_and(|i| i >= self.idle_seconds * 1000)
        {
            return t(
                TriggerKind::Idle,
                format!(
                    "no commit for {} s, with a delta of {delta} quads",
                    m.idle_ms.unwrap_or(0) / 1000
                ),
            );
        }
        if self.max_age_seconds > 0
            && m.head > m.base_seq
            && m.oldest_change_ms
                .is_some_and(|a| a >= self.max_age_seconds * 1000)
        {
            return t(
                TriggerKind::Age,
                format!(
                    "the oldest change not compacted is {} s old",
                    m.oldest_change_ms.unwrap_or(0) / 1000
                ),
            );
        }
        None
    }
}

/// What made a compaction due.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Trigger {
    pub kind: TriggerKind,
    pub detail: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum TriggerKind {
    MaxDelta,
    Ratio,
    DeltaBytes,
    WalBytes,
    Idle,
    Age,
}

impl TriggerKind {
    pub fn as_str(self) -> &'static str {
        match self {
            TriggerKind::MaxDelta => "max-delta",
            TriggerKind::Ratio => "ratio",
            TriggerKind::DeltaBytes => "delta-bytes",
            TriggerKind::WalBytes => "wal-bytes",
            TriggerKind::Idle => "idle",
            TriggerKind::Age => "age",
        }
    }
}

/// What the policy looks at, measured without the writer lock.
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CompactionMeasures {
    /// the current generation
    pub generation: String,
    /// the commit its base index holds
    pub base_seq: u64,
    pub head: u64,
    pub base_quads: u64,
    /// inserted plus deleted quads in the delta
    pub delta_quads: u64,
    pub delta_inserts: u64,
    pub delta_deletes: u64,
    /// estimated memory of the delta and the terms it added
    pub delta_bytes: u64,
    /// bytes of the current generation's write-ahead log (0 in memory)
    pub wal_bytes: u64,
    /// milliseconds since the last commit (`None`: not known)
    pub idle_ms: Option<u64>,
    /// age of the oldest commit not yet compacted, in milliseconds
    pub oldest_change_ms: Option<u64>,
    /// a compaction of the store is running
    pub compacting: bool,
}

/// Why a due compaction should wait, as far as the store can tell.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Blocker {
    /// `running`, `bulk-load`, `backup`, `history` or `disk`
    pub reason: &'static str,
    pub detail: String,
}

// -------------------------------------------------------------- tracking ------

/// A commit's changes, kept for a compaction that runs while it is made.
pub(crate) struct TapCommit {
    pub info: CommitInfo,
    /// the blank-node counter after the commit
    pub next_bnode: u64,
    pub changes: Vec<(u8, [Id; 4])>,
}

/// A run owns its tap through an identity token. Cancellation can disable it
/// without waiting for a transaction that retained the writer mutex.
pub(crate) struct Tap {
    pub active: Arc<AtomicBool>,
    pub commits: Vec<TapCommit>,
}

/// What the store tracks for the compaction policy, readable without the writer lock.
pub(crate) struct Track {
    /// the commit the current generation's base holds
    base_seq: AtomicU64,
    /// when the oldest commit not yet compacted was made (`NONE`: no such commit)
    oldest_change_ms: AtomicI64,
    /// when the last commit was made (`NONE`: not known)
    last_commit_ms: AtomicI64,
    /// a bulk commit is rebuilding the generation
    pub rebuilding: AtomicBool,
    /// a compaction is running
    pub running: AtomicBool,
    /// the generation number a background compaction builds (0: none)
    pub reserved: AtomicU32,
    /// the dataset's own settings
    pub settings: Mutex<CompactionSettings>,
    /// the published rebuilds since the store was opened
    pub rebuilds: RebuildStats,
}

impl Track {
    pub fn new(base_seq: u64, oldest: Option<i64>, last: Option<i64>) -> Track {
        Track {
            base_seq: AtomicU64::new(base_seq),
            oldest_change_ms: AtomicI64::new(oldest.unwrap_or(NONE)),
            last_commit_ms: AtomicI64::new(last.unwrap_or(NONE)),
            rebuilding: AtomicBool::new(false),
            running: AtomicBool::new(false),
            reserved: AtomicU32::new(0),
            settings: Mutex::new(CompactionSettings::default()),
            rebuilds: RebuildStats::default(),
        }
    }

    /// A commit was published to the delta at `ts`.
    pub fn committed(&self, ts: i64) {
        self.last_commit_ms.store(ts, Ordering::Relaxed);
        let _ =
            self.oldest_change_ms
                .compare_exchange(NONE, ts, Ordering::Relaxed, Ordering::Relaxed);
    }

    /// A new generation holds commit `base_seq`; `oldest` is the first commit after it,
    /// if there is one.
    pub fn rebased(&self, base_seq: u64, oldest: Option<i64>) {
        self.base_seq.store(base_seq, Ordering::Relaxed);
        self.oldest_change_ms
            .store(oldest.unwrap_or(NONE), Ordering::Relaxed);
    }

    /// A bulk commit was made at `ts` (its generation holds it), after a rebuild of
    /// `took`.
    pub fn bulk_committed(&self, base_seq: u64, ts: i64, took: Duration) {
        self.last_commit_ms.store(ts, Ordering::Relaxed);
        self.rebased(base_seq, None);
        self.rebuilds.record(RebuildReason::Bulk, took);
    }
}

/// Upper bounds, in seconds, of the rebuild duration histogram, without `+Inf`.
pub const REBUILD_BUCKETS: [f64; 13] = [
    0.1, 0.25, 0.5, 1.0, 2.5, 5.0, 10.0, 30.0, 60.0, 300.0, 900.0, 1800.0, 3600.0,
];

/// What made a new generation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RebuildReason {
    /// a compaction, background or not
    Compact,
    /// a bulk commit, which writes its data into a new generation
    Bulk,
}

impl RebuildReason {
    pub const ALL: [RebuildReason; 2] = [RebuildReason::Compact, RebuildReason::Bulk];

    pub fn as_str(self) -> &'static str {
        match self {
            RebuildReason::Compact => "compact",
            RebuildReason::Bulk => "bulk",
        }
    }
}

/// The published rebuilds of one reason since the store was opened, by duration.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct RebuildHistogram {
    /// rebuilds per bucket of [`REBUILD_BUCKETS`], not cumulative; the last is `+Inf`
    pub buckets: [u64; REBUILD_BUCKETS.len() + 1],
    pub sum_seconds: f64,
}

impl RebuildHistogram {
    pub fn count(&self) -> u64 {
        self.buckets.iter().sum()
    }
}

#[derive(Default)]
pub(crate) struct RebuildCounter {
    buckets: [AtomicU64; REBUILD_BUCKETS.len() + 1],
    sum_micros: AtomicU64,
}

/// Rebuild counts and durations per [`RebuildReason`], kept in memory.
#[derive(Default)]
pub(crate) struct RebuildStats([RebuildCounter; 2]);

impl RebuildStats {
    pub fn record(&self, reason: RebuildReason, d: Duration) {
        let c = &self.0[reason as usize];
        let s = d.as_secs_f64();
        let i = REBUILD_BUCKETS
            .iter()
            .position(|b| s <= *b)
            .unwrap_or(REBUILD_BUCKETS.len());
        c.buckets[i].fetch_add(1, Ordering::Relaxed);
        c.sum_micros.fetch_add(
            u64::try_from(d.as_micros()).unwrap_or(u64::MAX),
            Ordering::Relaxed,
        );
    }

    pub fn get(&self, reason: RebuildReason) -> RebuildHistogram {
        let c = &self.0[reason as usize];
        RebuildHistogram {
            buckets: std::array::from_fn(|i| c.buckets[i].load(Ordering::Relaxed)),
            sum_seconds: c.sum_micros.load(Ordering::Relaxed) as f64 / 1e6,
        }
    }
}

/// Clears `Track::rebuilding` when a bulk rebuild ends, however it ends.
pub(crate) struct Rebuilding<'a>(pub &'a AtomicBool);

impl Drop for Rebuilding<'_> {
    fn drop(&mut self) {
        self.0.store(false, Ordering::Relaxed);
    }
}

// --------------------------------------------------------------- compact ------

/// How [`Store::compact_with`] builds.
#[derive(Clone, Default)]
pub struct CompactOptions {
    /// threads for the build (`None`: the global pool, every core)
    pub threads: Option<usize>,
    /// lower the build threads' priority (Linux: nice 10)
    pub low_priority: bool,
    /// the average rate at which the build may write, in bytes per second
    pub io_bytes_per_sec: Option<u64>,
    /// stops the build ([`Error::Cancelled`]) when set
    pub cancel: Option<Arc<AtomicBool>>,
    pub progress: Option<crate::builder::MessageFn>,
    /// whether it may rewrite only the blocks the delta touches (`None`: the dataset's
    /// own setting, else [`PartialMode::Auto`])
    pub partial: Option<PartialMode>,
    /// build the spatial index base under the writer lock at the switch, as before it
    /// was built with the generation (for measurements)
    #[doc(hidden)]
    pub geo_at_switch: bool,
}

/// What a compaction did.
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CompactReport {
    /// the generation it published (the current one when abandoned)
    pub generation: String,
    pub quads: u64,
    /// the commit the new generation's base holds
    pub base_commit: u64,
    /// commits made during the build and carried into the new generation
    pub caught_up_commits: u64,
    /// why it published nothing (a bulk commit rebuilt the dataset meanwhile)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub abandoned: Option<String>,
    /// `full`, or `partial` when it rewrote only the blocks the delta touched
    pub mode: String,
    /// why a compaction that could have been partial rebuilt everything
    #[serde(skip_serializing_if = "Option::is_none")]
    pub full_reason: Option<String>,
    /// blocks of the old permutations that a partial compaction rewrote, and those it
    /// copied as they were
    pub blocks_rewritten: u64,
    pub blocks_copied: u64,
    /// how long the switch held the writer lock
    pub lock_ms: f64,
    pub build_ms: f64,
    pub total_ms: f64,
}

/// Undoes what an unfinished compaction set up: the tap, the reserved number, the
/// quota's exclusion and the new generation's directory.
struct Run<'a> {
    store: &'a Store,
    dir: Option<PathBuf>,
    published: bool,
    started: bool,
    active: Arc<AtomicBool>,
}

impl Drop for Run<'_> {
    fn drop(&mut self) {
        let s = self.store;
        if !self.published && self.started {
            self.active.store(false, Ordering::Release);
            if let Some(mut w) = s.writer.try_lock()
                && w.tap
                    .as_ref()
                    .is_some_and(|tap| Arc::ptr_eq(&tap.active, &self.active))
            {
                w.tap = None;
            }
            if let Some(dir) = &self.dir {
                let _ = std::fs::remove_dir_all(dir);
            }
            s.release_link_if_rebuilt();
        }
        s.compaction.reserved.store(0, Ordering::Relaxed);
        if self.dir.is_some() {
            s.quota.exclude(None);
        }
        s.compaction.running.store(false, Ordering::Release);
    }
}

/// The commits made during a build, carried into the new generation.
struct CatchUp {
    gen_: Arc<Generation>,
    cache: Arc<BlockCache>,
    delta: Delta,
    wal: Option<BufWriter<File>>,
    wal_len: u64,
    index: wal::WalIndex,
    /// old ids to new ones
    ids: rustc_hash::FxHashMap<u64, Id>,
    last: u64,
    commits: u64,
    next_bnode: u64,
    /// the timestamp of the first commit carried over
    oldest_ms: Option<i64>,
}

struct RelinkSource {
    segment: Segment,
    generation: Arc<Generation>,
    _lease: LeaseGuard,
}

impl CatchUp {
    fn translate(&mut self, id: Id, view: &Snapshot) -> Result<Id> {
        if !matches!(id.tag(), Tag::Vocab | Tag::Delta) {
            return Ok(id);
        }
        if let Some(&n) = self.ids.get(&id.0) {
            return Ok(n);
        }
        let key = view
            .key(id)
            .ok_or_else(|| Error::Corrupt(format!("compaction: dangling id {id:?}")))?;
        let n = match self.gen_.vocab.find(&key) {
            Ok(i) => Id::vocab(i),
            Err(_) => Id::delta(self.gen_.dvocab.insert(&key)?),
        };
        self.ids.insert(id.0, n);
        Ok(n)
    }

    /// Carry `batch` (in commit order) over; `view` is a snapshot of the old generation
    /// that knows every term the batch names.
    fn apply(&mut self, batch: &[TapCommit], view: &Snapshot, o: &CompactOptions) -> Result<()> {
        let mut rec = [0u8; WAL_REC];
        for c in batch {
            if o.cancel
                .as_ref()
                .is_some_and(|cancel| cancel.load(Ordering::Relaxed))
            {
                return Err(Error::Cancelled);
            }
            if c.info.seq != self.last + 1 {
                return Err(Error::Corrupt(format!(
                    "compaction: commit {} follows commit {}",
                    c.info.seq, self.last
                )));
            }
            let mut data = Vec::with_capacity((c.changes.len() + 1) * WAL_REC);
            for (i, (op, q)) in c.changes.iter().enumerate() {
                if i % 1024 == 0
                    && o.cancel
                        .as_ref()
                        .is_some_and(|cancel| cancel.load(Ordering::Relaxed))
                {
                    return Err(Error::Cancelled);
                }
                let n = [
                    self.translate(q[0], view)?,
                    self.translate(q[1], view)?,
                    self.translate(q[2], view)?,
                    self.translate(q[3], view)?,
                ];
                let in_base = self
                    .gen_
                    .perm(Perm::Spo)
                    .contains(&self.cache, &Perm::Spo.to_key(&n))?;
                apply(&mut self.delta, &n, *op == WAL_INSERT, in_base);
                rec[0] = *op;
                for j in 0..4 {
                    rec[1 + j * 8..9 + j * 8].copy_from_slice(&n[j].0.to_le_bytes());
                }
                data.extend_from_slice(&rec);
            }
            rec[0] = WAL_COMMIT;
            rec[1..9].copy_from_slice(&c.next_bnode.to_le_bytes());
            let flags = if c.info.unvalidated {
                commit::WAL_FLAG_UNVALIDATED
            } else {
                0
            };
            commit::seal_wal_commit(
                &mut rec,
                c.info.seq,
                c.info.timestamp_ms,
                c.info.kind,
                flags,
                &data,
            );
            data.extend_from_slice(&rec);
            if let Some(w) = self.wal.as_mut() {
                w.write_all(&data)?;
                self.wal_len += data.len() as u64;
                self.index.note(wal::WalPoint {
                    seq: c.info.seq,
                    offset: self.wal_len,
                    folding: false,
                });
            }
            self.last = c.info.seq;
            self.commits += 1;
            self.next_bnode = self.next_bnode.max(c.next_bnode);
            self.oldest_ms.get_or_insert(c.info.timestamp_ms);
        }
        Ok(())
    }

    /// Make the carried commits and their new terms durable.
    fn sync(&mut self) -> Result<()> {
        match self.wal.as_mut() {
            Some(w) => {
                w.flush()?;
                if self.gen_.dvocab.needs_sync() {
                    self.gen_.dvocab.flush()?;
                }
                sync_commit(w.get_ref(), &self.gen_.dvocab)
            }
            None => Ok(()),
        }
    }
}

/// How a compaction wrote the new generation.
enum How {
    /// in full, and why not partially
    Full(String),
    Partial {
        rewritten: u64,
        copied: u64,
    },
    Relink,
}

impl How {
    fn mode(&self) -> &'static str {
        match self {
            How::Full(_) => "full",
            How::Partial { .. } => "partial",
            How::Relink => "relink",
        }
    }
    fn full_reason(&self) -> Option<String> {
        match self {
            How::Full(why) => Some(why.clone()),
            How::Partial { .. } | How::Relink => None,
        }
    }
    fn rewritten(&self) -> u64 {
        match self {
            How::Full(_) | How::Relink => 0,
            How::Partial { rewritten, .. } => *rewritten,
        }
    }
    fn copied(&self) -> u64 {
        match self {
            How::Full(_) | How::Relink => 0,
            How::Partial { copied, .. } => *copied,
        }
    }
}

/// Lower the calling thread's CPU priority (a build thread of a background compaction).
fn lower_priority() {
    #[cfg(target_os = "linux")]
    // SAFETY: gettid has no preconditions; setpriority only reads its arguments, and
    // its failure (an unprivileged caller may only lower priority) is ignored
    unsafe {
        let tid = libc::syscall(libc::SYS_gettid) as libc::id_t;
        libc::setpriority(libc::PRIO_PROCESS, tid, 10);
    }
}

impl Store {
    /// Move a persistent linked branch onto main's current immutable index, while
    /// preserving its state, identity, commits and historical pins. This explicit
    /// operation shares index files and stores only a translated sparse base overlay;
    /// ordinary compaction continues to build an independent index. Concurrent writes
    /// are caught up before publication, as in [`compact_with`](Self::compact_with).
    pub fn relink_branch(&self, name: &str, o: &CompactOptions) -> Result<CompactReport> {
        self.owned_set()?;
        if name == crate::branch::MAIN || self.root.is_none() {
            return Err(Error::Unsupported(
                "relinking requires a persistent linked branch".into(),
            ));
        }
        let target = self.branch(name)?;
        if target.snapshot().generation.linked().is_none() {
            return Err(Error::Unsupported(
                "the branch already owns its index".into(),
            ));
        }
        let source = {
            let _w = self.lock_writer(&crate::guard::WriteOptions {
                cancel: o.cancel.clone(),
                ..Default::default()
            })?;
            let snapshot = self.snapshot();
            let generation = snapshot.generation.clone();
            let dir = generation.dir.as_ref().ok_or_else(|| {
                Error::Unsupported("relinking requires a persistent upstream".into())
            })?;
            let (_, base, _) = commit::read_gen_commit(dir)?
                .ok_or_else(|| Error::Corrupt("upstream generation lacks a base commit".into()))?;
            let number = commit::generation_number(&generation.name);
            let history = self
                .history
                .as_ref()
                .expect("a persistent store has history");
            let id = history.lock().lease_for(number, "relink", true);
            let collector = self.collector();
            RelinkSource {
                segment: Segment {
                    branch_id: self.dataset_id,
                    generation: generation.name.clone(),
                    path: generation.name.clone(),
                    base_seq: base.seq,
                    end_seq: base.seq,
                    wal_end: 0,
                    dvocab_len: 0,
                },
                generation,
                _lease: LeaseGuard {
                    generation: number,
                    label: "relink".into(),
                    release: Some(Box::new(move || {
                        if let Some(c) = collector {
                            c.release(id);
                        }
                    })),
                },
            }
        };
        target.compact_inner(o, Some((self, source)))
    }

    /// The dataset's own compaction settings (`compaction.json` of a persistent store).
    pub fn compaction_settings(&self) -> CompactionSettings {
        self.compaction.settings.lock().clone()
    }

    /// Replace the dataset's own compaction settings (`None` or empty: remove them, so
    /// that the server's apply). A persistent store keeps them in `compaction.json`.
    pub fn set_compaction_settings(&self, s: Option<CompactionSettings>) -> Result<()> {
        let s = s.unwrap_or_default();
        s.validate()?;
        let mut cur = self.compaction.settings.lock();
        if let Some(root) = &self.root {
            let path = root.join(COMPACTION_FILE);
            if s.is_empty() {
                match std::fs::remove_file(&path) {
                    Ok(()) => sync_dir(root)?,
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                    Err(e) => return Err(e.into()),
                }
            } else {
                let mut v = serde_json::to_value(&s).expect("settings serialize");
                v.as_object_mut()
                    .expect("an object")
                    .insert("format".into(), 1.into());
                let mut bytes = serde_json::to_vec_pretty(&v).expect("json serializes");
                bytes.push(b'\n');
                write_atomic(&path, &bytes)?;
            }
        }
        *cur = s;
        Ok(())
    }

    /// What the compaction policy looks at, measured without the writer lock.
    /// The rebuilds this store published since it was opened, with their durations:
    /// compactions and bulk commits. They are counted in memory only.
    pub fn rebuilds(&self, reason: RebuildReason) -> RebuildHistogram {
        self.compaction.rebuilds.get(reason)
    }

    pub fn compaction_measures(&self) -> CompactionMeasures {
        let snap = self.snapshot();
        let now = self.now_ms();
        let t = &self.compaction;
        let since = |ms: i64| (ms != NONE).then(|| now.saturating_sub(ms).max(0) as u64);
        // a linked branch counts its own changes, those since its starting commit, for
        // the delta-size triggers: the delta it inherited is not its to compact away
        let inherited = snap.generation.linked().map_or(0, |l| {
            (l.base_delta.inserts() + l.base_delta.deletes()) as u64
        });
        CompactionMeasures {
            generation: snap.generation.name.clone(),
            base_seq: t.base_seq.load(Ordering::Relaxed),
            head: snap.commit,
            base_quads: snap.generation.meta.quads,
            delta_quads: ((snap.delta.inserts() + snap.delta.deletes()) as u64)
                .saturating_sub(inherited),
            delta_inserts: snap.delta.inserts() as u64,
            delta_deletes: snap.delta.deletes() as u64,
            delta_bytes: delta_bytes(&snap.delta)
                + snap.generation.dvocab.with(|v| v.bytes()) as u64,
            wal_bytes: self.wal_bytes(),
            idle_ms: since(t.last_commit_ms.load(Ordering::Relaxed)),
            oldest_change_ms: since(t.oldest_change_ms.load(Ordering::Relaxed)),
            compacting: t.running.load(Ordering::Relaxed),
        }
    }

    /// Why a compaction should not start now, as far as the store can tell: one is
    /// running, a bulk commit is rebuilding, a backup leases the current generation, the
    /// retention window would lose a generation it covers, or (with `disk`) the file
    /// system lacks room for the new generation and the free-space reserve.
    pub fn compaction_blocker(&self, disk: bool) -> Option<Blocker> {
        let b = |reason, detail: String| Some(Blocker { reason, detail });
        if self.compaction.running.load(Ordering::Relaxed) {
            return b("running", "a compaction is running".into());
        }
        if self.compaction.rebuilding.load(Ordering::Relaxed) {
            return b(
                "bulk-load",
                "a bulk commit is rebuilding the dataset".into(),
            );
        }
        let (Some(root), Some(hist)) = (&self.root, &self.history) else {
            return None;
        };
        let snap = self.snapshot();
        let current = commit::generation_number(&snap.generation.name);
        // the first rebuild of a linked branch adds a full index to the dataset
        if snap.generation.link.is_some()
            && let Some(limit) = self.quota.limit()
        {
            let projected = self.quota.used() + self.compaction_disk_need(&snap);
            if projected > limit {
                return b(
                    "quota",
                    format!(
                        "the branch's own index would take the dataset to about {}, over its quota of {}",
                        crate::error::human_bytes(projected),
                        crate::error::human_bytes(limit)
                    ),
                );
            }
        }
        {
            let h = hist.lock();
            if let Some(l) = h.leases.values().find(|l| l.generation == current) {
                return b(
                    "backup",
                    format!("{} is reading {}", l.label, snap.generation.name),
                );
            }
            if let Some(no) = self.window_would_drop(&h, current, snap.commit) {
                return b(
                    "history",
                    format!(
                        "the retention window would drop gen-{no:04}: it keeps at most {} generations{}",
                        self.opts.history_max_generations,
                        h.retention
                            .max_bytes
                            .map(|m| format!(" and {}", crate::error::human_bytes(m)))
                            .unwrap_or_default()
                    ),
                );
            }
        }
        if disk {
            let need = self.compaction_disk_need(&snap);
            let reserve = self.opts.min_free_disk_bytes.unwrap_or(0);
            if let Err(e) = crate::disk::check_reserve(root, reserve, need, true) {
                return b("disk", e.to_string());
            }
        }
        None
    }

    /// The disk a compaction of `snap` needs at its peak, estimated: the current base
    /// scaled to the quads it will hold, plus the builder's temporary files.
    fn compaction_disk_need(&self, snap: &Snapshot) -> u64 {
        let quads = snap.len();
        let base = snap.generation.meta.quads.max(1);
        let scaled = (snap.generation.disk_bytes() as f64 * quads as f64 / base as f64) as u64;
        scaled + quads * TEMP_BYTES_PER_QUAD
    }

    /// A generation that the retention window keeps now but would drop if the current
    /// generation were compacted at `head`, because of its generation or byte limit.
    fn window_would_drop(
        &self,
        h: &crate::history::HistoryState,
        current: u32,
        head: u64,
    ) -> Option<u32> {
        let r = h.retention;
        if r.keep_commits.is_none_or(|n| n == 0) && r.keep_age_ms.is_none() {
            return None;
        }
        let cat = self.catalog.lock();
        let ts = |s: u64| cat.get(s).map(|c| c.timestamp_ms);
        let now = self.now_ms();
        let max = self.opts.history_max_generations;
        let lost = |s: &crate::history::HistoryState, cur: u32| {
            let kept = s.needed(cur, head, now, &ts, max);
            let mut all = s.clone_for_simulation();
            all.retention.max_bytes = None;
            let wanted = all.needed(cur, head, now, &ts, usize::MAX);
            wanted
                .into_keys()
                .filter(|no| !kept.contains_key(no))
                .collect::<Vec<u32>>()
        };
        let before = lost(h, current);
        let mut after = h.clone_for_simulation();
        let next = current + 1;
        if let Some(g) = after.gens.get_mut(&current) {
            g.end = head;
            g.bytes = dir_size(&g.dir);
            let base = CommitInfo {
                seq: head,
                ..g.base
            };
            let entry = crate::history::GenEntry {
                name: format!("gen-{next:04}"),
                dir: g.dir.clone(),
                base,
                end: head,
                fold_legacy: false,
                bytes: 0,
            };
            after.gens.insert(next, entry);
        }
        lost(&after, next)
            .into_iter()
            .find(|no| !before.contains(no))
    }

    /// Compact: merge the base and the delta into a new generation, without stopping
    /// writes. The build reads a snapshot without the writer lock. The commits made
    /// meanwhile are carried into the new generation, and the writer lock is held only
    /// for the last of them and the switch. The data and the head do not change.
    ///
    /// A bulk commit during the build makes the compaction moot: it publishes nothing
    /// and says why in [`CompactReport::abandoned`]. Only one compaction of a store runs
    /// at a time ([`Error::Conflict`] otherwise).
    pub fn compact_with(&self, o: &CompactOptions) -> Result<CompactReport> {
        self.compact_inner(o, None)
    }

    fn compact_inner(
        &self,
        o: &CompactOptions,
        relink: Option<(&Store, RelinkSource)>,
    ) -> Result<CompactReport> {
        let t0 = Instant::now();
        if self.compaction.running.swap(true, Ordering::Acquire) {
            return Err(Error::Conflict(
                "a compaction of this dataset is already running".into(),
            ));
        }
        let mut run = Run {
            store: self,
            dir: None,
            published: false,
            started: false,
            active: Arc::new(AtomicBool::new(true)),
        };
        let cancelled = || o.cancel.as_ref().is_some_and(|c| c.load(Ordering::Relaxed));
        // start: the snapshot to build, with the tap on from its commit
        let (snap0, base, next_bnode, name, dir, tmp) = {
            let mut w = self.lock_writer(&crate::guard::WriteOptions {
                cancel: o.cancel.clone(),
                ..Default::default()
            })?;
            if w.poisoned {
                return Err(Error::Poisoned);
            }
            // the commits up to here are durable in the catalog, and the full-text index
            // at its position, before the old generation's log can go
            self.catalog.lock().sync()?;
            #[cfg(feature = "text")]
            if let Some(ti) = self.text.load_full() {
                ti.checkpoint()?;
            }
            let snap = self.snapshot();
            let (dir, name, tmp) = match &self.root {
                Some(root) => {
                    let cur = commit::generation_number(&snap.generation.name);
                    let n = cur.max(self.compaction.reserved.load(Ordering::Relaxed)) + 1;
                    self.compaction.reserved.store(n, Ordering::Relaxed);
                    let name = format!("gen-{n:04}");
                    let dir = root.join(&name);
                    if dir.exists() {
                        std::fs::remove_dir_all(&dir)?;
                    }
                    (dir, name, None)
                }
                None => {
                    let t = tempfile::Builder::new().prefix("sparkles-mem-").tempdir()?;
                    (t.path().to_path_buf(), "mem".to_string(), Some(t))
                }
            };
            w.tap = Some(Tap {
                active: run.active.clone(),
                commits: Vec::new(),
            });
            run.started = true;
            (snap, w.head, w.next_bnode, name, dir, tmp)
        };
        if self.root.is_some() {
            run.dir = Some(dir.clone());
            self.quota.exclude(Some(&dir));
            // marks the directory as this dataset's unfinished build, which the next open
            // removes after a crash
            std::fs::create_dir_all(&dir)?;
            std::fs::write(dir.join(BUILDING_FILE), self.dataset_id.to_string())?;
        }
        self.failpoint("compact-started");
        let tb = Instant::now();
        let pool = build_pool(o, self.opts.build.threads)?;
        let mode = o
            .partial
            .unwrap_or_else(|| self.compaction.settings.lock().partial.unwrap_or_default());
        let (meta, how) = match &relink {
            Some((main, source)) => self
                .build_relinked(main, source, &snap0, &dir, &name, next_bnode, o)
                .map(|meta| (meta, How::Relink)),
            None => match &pool {
                Some(p) => p.install(|| self.build_new(&snap0, &dir, next_bnode, o, mode)),
                None => self.build_new(&snap0, &dir, next_bnode, o, mode),
            },
        }?;
        let mut build = tb.elapsed();
        self.failpoint("compact-built");
        // the first rebuild of a linked branch adds a full index: refused over the quota
        if snap0.generation.link.is_some()
            && let (Some(limit), Some(root)) = (self.quota.limit(), &self.root)
        {
            let ds_root = super::link::dataset_root_of(root)?;
            let projected = dir_size(&ds_root);
            if projected > limit {
                return Err(Error::BudgetExceeded(crate::Budget {
                    kind: crate::BudgetKind::DatasetBytes,
                    limit,
                    requested: projected,
                }));
            }
        }
        let persistent = self.root.is_some();
        let mut gen_ = match super::link::read_link(&dir)? {
            Some(file) => Generation::open_linked(
                &dir,
                &name,
                self.root.as_ref().expect("relinking is persistent"),
                file,
                false,
                &self.cache,
            )?,
            None => Generation::open(&dir, &name, persistent)?,
        };
        if let Some((_, source)) = &relink {
            gen_.share_blocks_with(&source.generation);
        }
        gen_._tmp = tmp;
        let gen_ = Arc::new(gen_);
        // the spatial index base of the new generation, built now rather than at the
        // switch: it depends on the generation alone, and the switch adds the overlay of
        // the commits carried over
        let geo = if o.geo_at_switch {
            None
        } else {
            let tg = Instant::now();
            let base_only = self.base_snapshot(&gen_, base.seq);
            let stop = || cancelled();
            let pre = match &pool {
                Some(p) => p.install(|| self.prebuild_geo(&base_only, &snap0, &stop)),
                None => self.prebuild_geo(&base_only, &snap0, &stop),
            };
            build += tg.elapsed();
            pre
        };
        self.failpoint("compact-indexed");
        if cancelled() {
            return Err(Error::Cancelled);
        }
        let mut cu = CatchUp {
            gen_: gen_.clone(),
            cache: self.cache.clone(),
            delta: gen_.base_delta(),
            wal: if persistent {
                Some(BufWriter::new(wal::open_for_append(&dir.join("wal.log"))?))
            } else {
                None
            },
            wal_len: 0,
            index: wal::WalIndex::new(base.seq, false),
            ids: Default::default(),
            last: base.seq,
            commits: 0,
            next_bnode: meta.next_bnode,
            oldest_ms: None,
        };
        let superseded = |w: &WriterState| -> Option<String> {
            if self.snapshot().generation.uid != snap0.generation.uid
                || !w
                    .tap
                    .as_ref()
                    .is_some_and(|tap| Arc::ptr_eq(&tap.active, &run.active))
            {
                Some("a bulk commit rebuilt the dataset during the build".into())
            } else if w.poisoned {
                Some("the store stopped taking writes".into())
            } else {
                None
            }
        };
        let abandoned = |why: String| CompactReport {
            generation: self.snapshot().generation.name.clone(),
            abandoned: Some(why),
            mode: how.mode().into(),
            build_ms: build.as_secs_f64() * 1e3,
            total_ms: t0.elapsed().as_secs_f64() * 1e3,
            ..Default::default()
        };
        // catch-up rounds without the writer lock
        for _ in 0..MAX_ROUNDS {
            if cancelled() {
                return Err(Error::Cancelled);
            }
            self.failpoint("compact-catching-up");
            let (batch, view) = {
                let mut w = self.lock_writer(&crate::guard::WriteOptions {
                    cancel: o.cancel.clone(),
                    ..Default::default()
                })?;
                if let Some(why) = superseded(&w) {
                    return Ok(abandoned(why));
                }
                let batch = std::mem::take(&mut w.tap.as_mut().expect("checked above").commits);
                (batch, self.snapshot())
            };
            cu.apply(&batch, &view, o)?;
            if batch.len() < LAST_ROUND {
                break;
            }
        }
        self.failpoint("compact-caught-up");
        // the commits up to the new base are in the old generation's log only: the change
        // log must hold them durably before that log can go
        self.sync_change_log()?;
        // the switch, under the writer lock
        let mut w = self.lock_writer(&crate::guard::WriteOptions {
            cancel: o.cancel.clone(),
            ..Default::default()
        })?;
        let tl = Instant::now();
        if let Some(why) = superseded(&w) {
            return Ok(abandoned(why));
        }
        if cancelled() {
            return Err(Error::Cancelled);
        }
        let rest = std::mem::take(&mut w.tap.as_mut().expect("checked above").commits);
        let view = self.snapshot();
        cu.apply(&rest, &view, o)?;
        if cancelled() {
            return Err(Error::Cancelled);
        }
        cu.sync()?;
        w.next_bnode = w.next_bnode.max(cu.next_bnode);
        let new_no = commit::generation_number(&name);
        if let Some(root) = &self.root {
            // the new generation's files, its log and its base commit are durable before
            // CURRENT names it; a crash before the switch leaves the old one current
            std::fs::remove_file(dir.join(BUILDING_FILE))?;
            write_synced(
                &dir.join("commit.json"),
                &commit::gen_commit_bytes(self.dataset_id, "compaction", &base),
            )?;
            sync_dir(&dir)?;
            sync_dir(root)?;
            self.failpoint("compact-before-current");
            if let Some((main, source)) = &relink {
                commit::require_reader(main.root.as_ref().expect("persistent main"), 3)?;
                commit::require_reader(root, 3)?;
                main.add_relink_hold(self.dataset_id, &source.segment)?;
                self.failpoint("relink-held");
                if cancelled() {
                    return Err(Error::Cancelled);
                }
            }
            if let Err(e) = write_atomic(&root.join("CURRENT"), name.as_bytes()) {
                let switched =
                    std::fs::read_to_string(root.join("CURRENT")).is_ok_and(|c| c.trim() == name);
                if switched {
                    // the next open uses the new generation, which lacks the commits
                    // this process would make to the old one: take no more writes
                    w.tap = None;
                    w.poisoned = true;
                    run.published = true;
                }
                return Err(e);
            }
            run.published = true;
            if relink.is_some() {
                self.failpoint("relink-current");
            }
            let wal = cu.wal.take().expect("a persistent store has a log");
            w.trim_wal();
            w.wal = Some(wal);
            w.wal_len = cu.wal_len;
            w.wal_alloc = cu.wal_len;
            self.wal_end.store(cu.wal_len, Ordering::Relaxed);
            self.quota.set_preallocated(0);
            *gen_.wal_index.lock() = Some(std::mem::replace(
                &mut cu.index,
                wal::WalIndex::new(base.seq, false),
            ));
        }
        w.tap = None;
        run.published = true;
        if let Err(e) = self.add_prefixes(meta.prefixes.clone()) {
            w.poisoned = true;
            return Err(e);
        }
        let dvocab_len = gen_.dvocab.len();
        let mut new_snap = Snapshot {
            generation: gen_,
            delta: std::mem::take(&mut cu.delta),
            version: view.version + 1,
            cache: self.cache.clone(),
            results: self.results.clone(),
            dvocab_len,
            commit: view.commit,
            text: view.text.clone(),
            geo: None,
            union_default_graph: self.opts.union_default_graph,
            geo_op_vertices: self.opts.geo_op_vertices,
            delta_stats: Default::default(),
            counts: Default::default(),
            historical: false,
            mask: None,
            change_log: self.changelog.clone(),
        };
        self.switch_geo_locked(&mut new_snap, &view, geo);
        let quads = new_snap.len();
        self.current.store(Arc::new(new_snap));
        self.commits.send_replace(view.commit);
        self.vectors_switched(&view);
        self.compaction.rebased(base.seq, cu.oldest_ms);
        self.compaction
            .rebuilds
            .record(RebuildReason::Compact, t0.elapsed());
        let mut retired = Vec::new();
        if let (Some(root), Some(h), Some(old)) = (&self.root, &self.history, &view.generation.dir)
            && old.starts_with(root)
        {
            let mut h = h.lock();
            let old_no = commit::generation_number(&view.generation.name);
            if let Some(g) = h.gens.get_mut(&old_no) {
                g.end = view.commit;
                g.bytes = dir_size(&g.dir);
            }
            h.gens.insert(
                new_no,
                crate::history::GenEntry {
                    name: name.clone(),
                    dir: dir.clone(),
                    base,
                    end: view.commit,
                    fold_legacy: false,
                    bytes: 0,
                },
            );
            retired = self.retire_locked(&mut h, new_no, view.commit);
        }
        let lock = tl.elapsed();
        drop(w);
        drop(run);
        // the old generation's files go after the writer lock is released (an open
        // finishes the deletion after a crash)
        for d in retired {
            let _ = std::fs::remove_dir_all(d);
        }
        self.quota.invalidate();
        self.release_link_if_rebuilt();
        Ok(CompactReport {
            generation: name,
            quads,
            base_commit: base.seq,
            caught_up_commits: cu.commits,
            abandoned: None,
            mode: how.mode().into(),
            full_reason: how.full_reason(),
            blocks_rewritten: how.rewritten(),
            blocks_copied: how.copied(),
            lock_ms: lock.as_secs_f64() * 1e3,
            build_ms: build.as_secs_f64() * 1e3,
            total_ms: t0.elapsed().as_secs_f64() * 1e3,
        })
    }

    /// Write the generation of `snap` to `dir`: partially when `mode` allows it and the
    /// delta suits it, else in full.
    fn build_new(
        &self,
        snap: &Snapshot,
        dir: &Path,
        next_bnode: u64,
        o: &CompactOptions,
        mode: PartialMode,
    ) -> Result<(IndexMeta, How)> {
        let why = match mode {
            PartialMode::Off => "partial compaction is off".to_string(),
            _ => match partial::plan(snap) {
                Err(why) => why,
                Ok(plan)
                    if let Some(why) = (mode == PartialMode::Auto)
                        .then(|| plan.auto_refusal())
                        .flatten() =>
                {
                    why
                }
                Ok(plan) => {
                    if let Some(p) = &o.progress {
                        p(&format!(
                            "partial compaction: rewriting {} of {} blocks",
                            plan.rewritten, plan.blocks
                        ));
                    }
                    let interrupt = self.compaction_interrupt(o, dir);
                    let meta =
                        partial::write(snap, &plan, dir, next_bnode, self.prefixes(), &interrupt)?;
                    interrupt()?;
                    return Ok((
                        meta,
                        How::Partial {
                            rewritten: plan.rewritten,
                            copied: plan.blocks - plan.rewritten,
                        },
                    ));
                }
            },
        };
        let meta = self.build_compacted(snap, dir, next_bnode, o)?;
        Ok((meta, How::Full(why)))
    }

    #[allow(clippy::too_many_arguments)]
    fn build_relinked(
        &self,
        main: &Store,
        source: &RelinkSource,
        snapshot: &Snapshot,
        dir: &Path,
        name: &str,
        next_bnode: u64,
        o: &CompactOptions,
    ) -> Result<IndexMeta> {
        let interrupt = self.compaction_interrupt(o, dir);
        interrupt()?;
        let changes = main.toggles(
            main.owned_set()?,
            crate::branch::CommitRef {
                branch_id: source.segment.branch_id,
                seq: source.segment.base_seq,
            },
            crate::branch::CommitRef {
                branch_id: self.dataset_id,
                seq: snapshot.commit,
            },
            &super::diff::DiffOptions {
                cancel: o.cancel.clone(),
                ..Default::default()
            },
        )?;
        write_synced(&dir.join("delta.vocab"), &[])?;
        let mut file = LinkFile {
            format: 1,
            base_seq: snapshot.commit,
            segments: vec![source.segment.clone()],
            overlay: None,
        };
        let generation = Generation::open_linked(
            dir,
            name,
            self.root.as_ref().expect("persistent branch"),
            file.clone(),
            false,
            &self.cache,
        )?;
        let mut changes: Vec<_> = changes.into_iter().collect();
        changes.sort_unstable_by(|a, b| (a.1, &a.0).cmp(&(b.1, &b.0)));
        let mut bytes = Vec::with_capacity(changes.len() * WAL_REC);
        for (i, (key, insert)) in changes.into_iter().enumerate() {
            if i % 4096 == 0 {
                interrupt()?;
            }
            let ids = [
                relink_id(&generation, &key[1])?,
                relink_id(&generation, &key[2])?,
                relink_id(&generation, &key[3])?,
                relink_id(&generation, &key[0])?,
            ];
            bytes.push(if insert { WAL_INSERT } else { WAL_DELETE });
            for id in ids {
                bytes.extend_from_slice(&id.0.to_le_bytes());
            }
        }
        generation.dvocab.sync()?;
        file.format = 2;
        file.overlay = Some(Overlay {
            bytes: bytes.len() as u64,
            sha256: super::link::overlay_checksum(&bytes),
            next_bnode,
        });
        write_synced(&dir.join(OVERLAY_FILE), &bytes)?;
        write_synced(
            &dir.join(LINK_FILE),
            &serde_json::to_vec_pretty(&file).expect("serializes"),
        )?;
        interrupt()?;
        let mut meta = generation.meta.clone();
        meta.next_bnode = next_bnode;
        meta.prefixes = self.prefixes();
        Ok(meta)
    }

    /// Build the generation of `snap` in `dir`, under the build limits of `o`.
    fn build_compacted(
        &self,
        snap: &Snapshot,
        dir: &Path,
        next_bnode: u64,
        o: &CompactOptions,
    ) -> Result<IndexMeta> {
        let mut bopts = self.opts.build.clone();
        bopts.first_bnode = next_bnode;
        if let Some(t) = o.threads {
            bopts.threads = t.max(1);
        }
        let interrupt = self.compaction_interrupt(o, dir);
        let mut builder = Builder::new(dir, bopts)?.with_interrupt(interrupt.clone());
        if let Some(p) = &o.progress {
            builder = builder.with_progress(p.clone());
        }
        write_snapshot(&builder, snap, None, |_| Ok(true), &[])?;
        builder.add_prefixes(self.prefixes());
        let meta = builder.finish()?;
        interrupt()?;
        Ok(meta)
    }

    /// A snapshot of `gen_` alone, with an empty delta, at commit `seq` (what a
    /// compaction's new generation holds before the commits carried over).
    fn base_snapshot(&self, gen_: &Arc<Generation>, seq: u64) -> Snapshot {
        Snapshot {
            generation: gen_.clone(),
            delta: gen_.base_delta(),
            version: 0,
            cache: self.cache.clone(),
            results: self.results.clone(),
            dvocab_len: gen_.dvocab.len(),
            commit: seq,
            text: None,
            geo: None,
            union_default_graph: self.opts.union_default_graph,
            geo_op_vertices: self.opts.geo_op_vertices,
            delta_stats: Default::default(),
            counts: Default::default(),
            historical: false,
            mask: None,
            change_log: None,
        }
    }

    /// What stops or paces a compaction's build: its cancel flag, the free-space reserve,
    /// and its write rate.
    fn compaction_interrupt(&self, o: &CompactOptions, dir: &Path) -> crate::builder::InterruptFn {
        let cancel = o.cancel.clone();
        let reserve = self.root.as_ref().and(self.opts.min_free_disk_bytes);
        let rate = o.io_bytes_per_sec.filter(|r| *r > 0);
        let dir = dir.to_path_buf();
        let start = Instant::now();
        Arc::new(move || {
            let stop = || cancel.as_ref().is_some_and(|c| c.load(Ordering::Relaxed));
            if stop() {
                return Err(Error::Cancelled);
            }
            if let Some(r) = reserve {
                crate::disk::check_reserve(&dir, r, 0, false)?;
            }
            if let Some(rate) = rate {
                // ahead of the rate: wait until the bytes written so far are due
                let due = Duration::from_secs_f64(dir_size(&dir) as f64 / rate as f64);
                while start.elapsed() < due {
                    if stop() {
                        return Err(Error::Cancelled);
                    }
                    std::thread::sleep((due - start.elapsed()).min(Duration::from_millis(100)));
                }
            }
            Ok(())
        })
    }
}

/// The thread pool a compaction builds in: its own when `o` limits the threads or
/// lowers their priority (`None`: the global pool). `threads` is the default count.
fn build_pool(o: &CompactOptions, threads: usize) -> Result<Option<rayon::ThreadPool>> {
    if o.threads.is_none() && !o.low_priority {
        return Ok(None);
    }
    let low = o.low_priority;
    rayon::ThreadPoolBuilder::new()
        .num_threads(o.threads.unwrap_or(threads).max(1))
        .thread_name(|i| format!("compact-{i}"))
        .start_handler(move |_| {
            if low {
                lower_priority();
            }
        })
        .build()
        .map(Some)
        .map_err(|e| Error::Io(std::io::Error::other(e.to_string())))
}

/// Translate a vocabulary key into the relinked generation without renumbering
/// stored blank nodes or canonical inline literals.
fn relink_id(generation: &Generation, key: &[u8]) -> Result<Id> {
    if key.is_empty() {
        return Ok(Id::DEFAULT_GRAPH);
    }
    if key[0] == b'_' {
        let bytes: [u8; 8] = key
            .get(1..9)
            .and_then(|bytes| bytes.try_into().ok())
            .ok_or_else(|| Error::Corrupt("invalid stored blank-node key".into()))?;
        return Ok(Id::new(Tag::BNode, u64::from_be_bytes(bytes)));
    }
    if key[0] == b'"'
        && let Some(id) = crate::id::inline_id(&crate::id::key_to_term(key))
    {
        return Ok(id);
    }
    if let Ok(id) = generation.vocab.find(key) {
        return Ok(Id::vocab(id));
    }
    Ok(Id::delta(generation.dvocab.insert(key)?))
}

/// Remove the unfinished builds of compactions that a crash interrupted: directories
/// `gen-N` above the current generation that hold this dataset's build marker.
pub(crate) fn remove_interrupted(root: &Path, dataset_id: uuid::Uuid, current: u32) {
    let Ok(dir) = std::fs::read_dir(root) else {
        return;
    };
    for e in dir.flatten() {
        let name = e.file_name().to_string_lossy().into_owned();
        let Some(num) = name.strip_prefix("gen-") else {
            continue;
        };
        if !num.bytes().all(|b| b.is_ascii_digit()) || commit::generation_number(&name) <= current {
            continue;
        }
        let ours = std::fs::read_to_string(e.path().join(BUILDING_FILE))
            .is_ok_and(|id| id.trim() == dataset_id.to_string());
        if ours && let Err(err) = crate::history::delete_generation(root, &e.path()) {
            tracing::warn!(target: "sparkles::store::compaction", "could not remove the interrupted compaction {name}: {err}");
        }
    }
}

/// Read a dataset's `compaction.json` (`None` without one).
pub(crate) fn read_settings(root: &Path) -> Result<CompactionSettings> {
    match std::fs::read(root.join(COMPACTION_FILE)) {
        Ok(b) => {
            let v: serde_json::Value = serde_json::from_slice(&b)
                .map_err(|e| Error::Corrupt(format!("{COMPACTION_FILE}: {e}")))?;
            CompactionSettings::from_json(&v)
                .map_err(|e| Error::Corrupt(format!("{COMPACTION_FILE}: {e}")))
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(CompactionSettings::default()),
        Err(e) => Err(e.into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn m(base: u64, delta: u64) -> CompactionMeasures {
        CompactionMeasures {
            base_quads: base,
            delta_quads: delta,
            head: if delta > 0 { 1 } else { 0 },
            ..Default::default()
        }
    }

    #[test]
    fn the_relative_trigger_fires_at_its_threshold() {
        let p = CompactionPolicy::default();
        assert_eq!(p.threshold(100_000), 15_000);
        assert_eq!(p.verdict(&m(100_000, 14_999)), None);
        let t = p.verdict(&m(100_000, 15_000)).unwrap();
        assert_eq!(t.kind, TriggerKind::Ratio, "{t:?}");
        // a huge base is capped by the absolute trigger
        assert_eq!(p.threshold(1_000_000_000), 1_000_000);
        let t = p.verdict(&m(1_000_000_000, 1_000_000)).unwrap();
        assert_eq!(t.kind, TriggerKind::MaxDelta);
        // nothing to compact
        assert_eq!(p.verdict(&m(0, 0)), None);
    }

    #[test]
    fn idle_needs_the_floor_and_age_does_not() {
        let p = CompactionPolicy::default();
        let mut x = m(1_000_000, 9_999);
        x.idle_ms = Some(3_600_000);
        assert_eq!(p.verdict(&x), None);
        x.delta_quads = 10_000;
        assert_eq!(p.verdict(&x).unwrap().kind, TriggerKind::Idle);
        let mut y = m(1_000_000, 1);
        y.oldest_change_ms = Some(86_400_000);
        assert_eq!(p.verdict(&y).unwrap().kind, TriggerKind::Age);
        y.oldest_change_ms = Some(86_399_999);
        assert_eq!(p.verdict(&y), None);
        // turned off
        let off = p.with(&CompactionSettings {
            max_age_seconds: Some(0),
            idle_seconds: Some(0),
            ..Default::default()
        });
        y.oldest_change_ms = Some(u64::MAX / 2);
        x.idle_ms = Some(u64::MAX / 2);
        assert_eq!(off.verdict(&y), None);
        assert_eq!(off.verdict(&x), None);
    }

    #[test]
    fn size_triggers() {
        let p = CompactionPolicy::default();
        let mut x = m(1_000_000_000, 100);
        x.wal_bytes = 1024 << 20;
        assert_eq!(p.verdict(&x).unwrap().kind, TriggerKind::WalBytes);
        x.wal_bytes = 0;
        x.delta_bytes = 512 << 20;
        assert_eq!(p.verdict(&x).unwrap().kind, TriggerKind::DeltaBytes);
    }

    #[test]
    fn settings_parse_validate_and_override() {
        let mut s = CompactionSettings::default();
        s.set("deltaRatio", "0.02").unwrap();
        s.set("enabled", "off").unwrap();
        s.set("minDeltaQuads", "5_000").unwrap();
        assert!(s.set("deltaRatio", "-1").is_err());
        assert!(s.set("nope", "1").is_err());
        let p = CompactionPolicy::default().with(&s);
        assert!(!p.enabled);
        assert_eq!((p.delta_ratio, p.min_delta_quads), (0.02, 5_000));
        let j = serde_json::json!({"format": 1, "deltaRatio": 0.5});
        let t = CompactionSettings::from_json(&j).unwrap();
        assert_eq!(t.delta_ratio, Some(0.5));
        assert!(CompactionSettings::from_json(&serde_json::json!({"ratio": 1})).is_err());
        assert!(CompactionSettings::from_json(&serde_json::json!([1])).is_err());
    }
}
