//! Counters of the commit path, kept in memory: how long the synchronizations of commits
//! take, how many commits a group-commit fence holds, how long a grouped commit waits for
//! its acknowledgement, and why commits did not join a group (spec X09 §8).
use super::*;
use std::time::Duration;

/// Upper bounds, in seconds, of the synchronization and wait histograms, without `+Inf`.
pub const SYNC_BUCKETS: [f64; 13] = [
    0.0001, 0.00025, 0.0005, 0.001, 0.0025, 0.005, 0.01, 0.025, 0.05, 0.1, 0.25, 1.0, 5.0,
];

/// Upper bounds of the group size histogram (commits per fence), without `+Inf`.
pub const SIZE_BUCKETS: [f64; 7] = [1.0, 2.0, 4.0, 8.0, 16.0, 32.0, 64.0];

/// How a commit was made durable.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SyncPath {
    /// write-through writes (`O_DSYNC`) of the WAL records and the new vocabulary entries
    Direct,
    /// buffered writes followed by `fdatasync`, for one commit
    Buffered,
    /// one `fdatasync` for the commits of a group
    Group,
}

impl SyncPath {
    pub const ALL: [SyncPath; 3] = [SyncPath::Direct, SyncPath::Buffered, SyncPath::Group];

    pub fn as_str(self) -> &'static str {
        match self {
            SyncPath::Direct => "direct",
            SyncPath::Buffered => "buffered",
            SyncPath::Group => "group",
        }
    }
}

/// Why a commit of a store with group commit on took the ordinary path: the first
/// condition of admission that failed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Fallback {
    Message,
    Precondition,
    Guard,
    Text,
    Geo,
    Embed,
    Compaction,
    Kind,
    Bulk,
    Size,
    Noop,
    Digests,
    Bypass,
    Graphs,
    Poisoned,
}

impl Fallback {
    pub const ALL: [Fallback; 15] = [
        Fallback::Message,
        Fallback::Precondition,
        Fallback::Guard,
        Fallback::Text,
        Fallback::Geo,
        Fallback::Embed,
        Fallback::Compaction,
        Fallback::Kind,
        Fallback::Bulk,
        Fallback::Size,
        Fallback::Noop,
        Fallback::Digests,
        Fallback::Bypass,
        Fallback::Graphs,
        Fallback::Poisoned,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Fallback::Message => "message",
            Fallback::Precondition => "precondition",
            Fallback::Guard => "guard",
            Fallback::Text => "text",
            Fallback::Geo => "geo",
            Fallback::Embed => "embed",
            Fallback::Compaction => "compaction",
            Fallback::Kind => "kind",
            Fallback::Bulk => "bulk",
            Fallback::Size => "size",
            Fallback::Noop => "noop",
            Fallback::Digests => "digests",
            Fallback::Bypass => "bypass",
            Fallback::Graphs => "graphs",
            Fallback::Poisoned => "poisoned",
        }
    }
}

/// A histogram as a reader sees it: counts per bucket (not cumulative; the last bucket
/// is `+Inf`) and the sum of the observed values.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct HistogramSnapshot {
    pub buckets: Vec<u64>,
    pub sum: f64,
}

impl HistogramSnapshot {
    pub fn count(&self) -> u64 {
        self.buckets.iter().sum()
    }

    /// Add `other`'s counts to this one (a dataset's branches share a label).
    pub fn merge(&mut self, other: &HistogramSnapshot) {
        if self.buckets.len() < other.buckets.len() {
            self.buckets.resize(other.buckets.len(), 0);
        }
        for (a, b) in self.buckets.iter_mut().zip(&other.buckets) {
            *a += b;
        }
        self.sum += other.sum;
    }
}

struct Histogram {
    bounds: &'static [f64],
    buckets: Vec<AtomicU64>,
    /// the sum in millionths, so seconds count microseconds and sizes count commits
    sum_micro: AtomicU64,
}

impl Histogram {
    fn new(bounds: &'static [f64]) -> Histogram {
        Histogram {
            bounds,
            buckets: (0..=bounds.len()).map(|_| AtomicU64::new(0)).collect(),
            sum_micro: AtomicU64::new(0),
        }
    }

    fn observe(&self, v: f64) {
        let i = self
            .bounds
            .iter()
            .position(|b| v <= *b)
            .unwrap_or(self.bounds.len());
        self.buckets[i].fetch_add(1, Ordering::Relaxed);
        self.sum_micro
            .fetch_add((v * 1e6).round() as u64, Ordering::Relaxed);
    }

    fn get(&self) -> HistogramSnapshot {
        HistogramSnapshot {
            buckets: self
                .buckets
                .iter()
                .map(|b| b.load(Ordering::Relaxed))
                .collect(),
            sum: self.sum_micro.load(Ordering::Relaxed) as f64 / 1e6,
        }
    }
}

pub(crate) struct CommitStats {
    sync: [Histogram; 3],
    size: Histogram,
    wait: Histogram,
    fallbacks: [AtomicU64; Fallback::ALL.len()],
    failures: AtomicU64,
}

impl Default for CommitStats {
    fn default() -> Self {
        CommitStats {
            sync: std::array::from_fn(|_| Histogram::new(&SYNC_BUCKETS)),
            size: Histogram::new(&SIZE_BUCKETS),
            wait: Histogram::new(&SYNC_BUCKETS),
            fallbacks: Default::default(),
            failures: AtomicU64::new(0),
        }
    }
}

impl CommitStats {
    pub fn synced(&self, path: SyncPath, took: Duration) {
        self.sync[path as usize].observe(took.as_secs_f64());
    }

    pub fn fenced(&self, commits: usize) {
        self.size.observe(commits as f64);
    }

    pub fn waited(&self, took: Duration) {
        self.wait.observe(took.as_secs_f64());
    }

    pub fn fell_back(&self, why: Fallback) {
        self.fallbacks[why as usize].fetch_add(1, Ordering::Relaxed);
    }

    pub fn failed(&self) {
        self.failures.fetch_add(1, Ordering::Relaxed);
    }

    pub fn get(&self, enabled: bool) -> CommitMetrics {
        CommitMetrics {
            group_commit: enabled,
            sync: std::array::from_fn(|i| self.sync[i].get()),
            size: self.size.get(),
            wait: self.wait.get(),
            fallbacks: std::array::from_fn(|i| self.fallbacks[i].load(Ordering::Relaxed)),
            failures: self.failures.load(Ordering::Relaxed),
        }
    }
}

/// The commit path's counters since the store was opened (see [`Store::commit_metrics`]).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct CommitMetrics {
    /// whether the store groups commits
    pub group_commit: bool,
    /// synchronization time per [`SyncPath::ALL`], in seconds, over [`SYNC_BUCKETS`]
    pub sync: [HistogramSnapshot; 3],
    /// commits per fence, over [`SIZE_BUCKETS`]
    pub size: HistogramSnapshot,
    /// seconds from a grouped commit's enqueue to its acknowledgement
    pub wait: HistogramSnapshot,
    /// commits that took the ordinary path, per [`Fallback::ALL`]
    pub fallbacks: [u64; Fallback::ALL.len()],
    /// fences whose synchronization failed
    pub failures: u64,
}

impl CommitMetrics {
    pub fn merge(&mut self, other: &CommitMetrics) {
        self.group_commit |= other.group_commit;
        for (a, b) in self.sync.iter_mut().zip(&other.sync) {
            a.merge(b);
        }
        self.size.merge(&other.size);
        self.wait.merge(&other.wait);
        for (a, b) in self.fallbacks.iter_mut().zip(other.fallbacks) {
            *a += b;
        }
        self.failures += other.failures;
    }
}

impl Store {
    /// The synchronization, group and fallback counters of the commit path.
    pub fn commit_metrics(&self) -> CommitMetrics {
        self.commit_stats.get(self.group_commit_enabled())
    }

    /// Whether commits of this store may share a synchronization.
    pub fn group_commit_enabled(&self) -> bool {
        self.group.is_some() && self.root.is_some()
    }
}
