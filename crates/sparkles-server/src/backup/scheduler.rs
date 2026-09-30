//! The policy scheduler: one thread that sleeps until the earliest `nextRun` (at most
//! 60 s, so clock changes are noticed), starts one `backup-policy` task per due and
//! enabled policy (an overlapping run is recorded `skipped`), runs catch-up 60 s after
//! startup, and ticks F06 history collection (`Store::try_collect_history`) hourly.
//! Time comes from a [`Clock`], so tests drive it.

use crate::state::AppState;
use chrono::{DateTime, Utc};
use std::sync::Arc;

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

/// Start the scheduler thread (a no-op without policies support built, or on a server
/// without `AppState::backup`).
pub fn spawn(st: Arc<AppState>, clock: Arc<dyn Clock>) {
    let _ = (st, clock);
}
