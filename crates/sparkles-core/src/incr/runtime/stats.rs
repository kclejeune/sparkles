//! The runtime's counters for the metrics (spec F13 §8.2).

use super::*;

#[derive(Default)]
pub(super) struct Counters {
    pub(super) passes: AtomicU64,
    pub(super) skipped: AtomicU64,
    pub(super) resets: AtomicU64,
    pub(super) live_events: AtomicU64,
    pub(super) recompute_ns: AtomicU64,
    pub(super) pass_ns: AtomicU64,
    pub(super) pass_hist: [AtomicU64; PASS_BUCKETS.len() + 1],
    pub(super) checkpoints: AtomicU64,
    pub(super) loaded: AtomicU64,
}

/// A view's numbers for the metrics.
#[derive(Clone, Debug)]
pub struct ViewStat {
    pub name: String,
    pub lag_commits: u64,
    pub state_bytes: u64,
}

/// The runtime's counters (spec F13 §8.2).
#[derive(Clone, Debug, Default)]
pub struct RuntimeStats {
    pub views: Vec<ViewStat>,
    pub live_queries: usize,
    pub circuits: usize,
    /// the internal circuits server code attached (spec F13 §3.6), which `views` and
    /// `circuits` leave out
    pub internal_circuits: usize,
    pub passes: u64,
    pub skipped: u64,
    pub resets: u64,
    pub live_events: u64,
    pub recompute_seconds: f64,
    pub pass_seconds_sum: f64,
    /// the count of passes at or under each bound of [`PASS_BUCKETS`], then all
    pub pass_buckets: Vec<u64>,
    pub checkpoints: u64,
    /// views whose state was loaded from a checkpoint
    pub loaded: u64,
}

impl Runtime {
    /// The live queries running.
    pub fn live_count(&self) -> usize {
        self.live_total.load(Ordering::Acquire)
    }

    /// The counters for the metrics.
    pub fn stats(&self, store: &Store) -> RuntimeStats {
        let head = store.snapshot().commit;
        let reg = self.reg.lock();
        let views = reg
            .views
            .iter()
            .map(|(n, e)| {
                let (applied, bytes) = e.cell.as_ref().map_or((0, 0), |c| {
                    let i = c.info.lock();
                    (i.applied, i.bytes)
                });
                ViewStat {
                    name: n.clone(),
                    lag_commits: head.saturating_sub(applied),
                    state_bytes: bytes,
                }
            })
            .collect();
        let c = &self.counters;
        RuntimeStats {
            views,
            live_queries: self.live_count(),
            circuits: reg.cells.len(),
            internal_circuits: reg.internal.len(),
            passes: c.passes.load(Ordering::Relaxed),
            skipped: c.skipped.load(Ordering::Relaxed),
            resets: c.resets.load(Ordering::Relaxed),
            live_events: c.live_events.load(Ordering::Relaxed),
            recompute_seconds: c.recompute_ns.load(Ordering::Relaxed) as f64 / 1e9,
            pass_seconds_sum: c.pass_ns.load(Ordering::Relaxed) as f64 / 1e9,
            pass_buckets: c
                .pass_hist
                .iter()
                .map(|b| b.load(Ordering::Relaxed))
                .collect(),
            checkpoints: c.checkpoints.load(Ordering::Relaxed),
            loaded: c.loaded.load(Ordering::Relaxed),
        }
    }
}
