//! The policy scheduler: one thread that sleeps until the earliest `nextRun` (at most
//! 60 s, so clock changes are noticed), starts one `backup-policy` task per due and
//! enabled policy (an overlapping run is recorded `skipped`), runs catch-up 60 s after
//! startup, and ticks history collection (`Store::try_collect_history`) hourly.
//! Time comes from a [`Clock`], so tests drive it.

use crate::state::AppState;
use chrono::{DateTime, TimeDelta, Utc};
use std::sync::{Arc, Weak};
use std::time::Duration;

/// The longest the scheduler sleeps between evaluations.
pub const MAX_SLEEP: Duration = Duration::from_secs(60);
/// The scheduler's first evaluation (and so any catch-up run) waits this long after
/// startup, so the server is ready first.
pub const STARTUP_DELAY: Duration = Duration::from_secs(60);
/// How often every dataset's unneeded history generations are collected.
const HISTORY_EVERY: TimeDelta = TimeDelta::hours(1);

/// The scheduler's time source.
pub trait Clock: Send + Sync {
    fn now(&self) -> DateTime<Utc>;
    /// Sleep for `d` (a test clock may advance instead).
    fn sleep(&self, d: std::time::Duration) {
        std::thread::sleep(d);
    }
}

/// The wall clock.
pub struct SystemClock;

impl Clock for SystemClock {
    fn now(&self) -> DateTime<Utc> {
        Utc::now()
    }
}

/// Start the scheduler thread (a no-op on a server without `AppState::backup`). The
/// thread holds the state weakly and ends once it is dropped. `clock` also becomes the
/// time source of the policy routes (`nextRun`, previews, run times).
pub fn spawn(st: Arc<AppState>, clock: Arc<dyn Clock>) {
    let Some(b) = &st.backup else { return };
    b.policies.set_clock(clock.clone());
    let weak = Arc::downgrade(&st);
    drop(st);
    let spawned = std::thread::Builder::new()
        .name("backup-scheduler".into())
        .spawn(move || run(weak, clock));
    if let Err(e) = spawned {
        tracing::error!(target: "sparkles::backup", "cannot start the policy scheduler: {e}");
    }
}

fn run(weak: Weak<AppState>, clock: Arc<dyn Clock>) {
    clock.sleep(STARTUP_DELAY);
    let mut s = Scheduler::default();
    loop {
        let Some(st) = weak.upgrade() else { return };
        let now = clock.now();
        let wait = s.step(&st, now);
        drop(st);
        clock.sleep(wait);
    }
}

/// The scheduler's loop state.
#[derive(Default)]
pub struct Scheduler {
    last_history: Option<DateTime<Utc>>,
}

impl Scheduler {
    /// One evaluation at `now`: start the due policy runs, collect history when an hour
    /// has passed since the last collection (or the clock went back), and return how
    /// long to sleep.
    pub fn step(&mut self, st: &Arc<AppState>, now: DateTime<Utc>) -> Duration {
        let next = st
            .backup
            .as_ref()
            .and_then(|b| b.policies.evaluate_all(st, now));
        if self
            .last_history
            .is_none_or(|t| now - t >= HISTORY_EVERY || now < t)
        {
            self.last_history = Some(now);
            collect_history(st);
        }
        next.and_then(|t| (t - now).to_std().ok())
            .unwrap_or(MAX_SLEEP)
            .clamp(Duration::from_millis(10), MAX_SLEEP)
    }
}

/// Best-effort collection of every dataset's unneeded history generations (skipped for
/// a dataset whose writer is busy).
fn collect_history(st: &AppState) {
    let datasets: Vec<_> = st.datasets().values().cloned().collect();
    for d in datasets {
        d.store.try_collect_history();
    }
}
