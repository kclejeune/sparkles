//! Publishing a restored directory on the server.
//!
//! * **New dataset**: rename the restored directory into `databases/<target>` and
//!   `AppState::adopt` the reservation (the clone code path).
//! * **In place**: mark `<target>` restoring (`AppState::restoring`: requests get `503`
//!   with `Retry-After`), take it out of the map (`AppState::detach_for_swap`), wait up
//!   to 30 s for in-flight requests to release it (`Arc::strong_count == 1`; else put
//!   it back and fail `409 dataset-busy`), drop it (releasing `sparkles.lock`), rename
//!   `databases/<t>` → `databases/.replaced-<t>-<task>`, the restored directory →
//!   `databases/<t>`, fsync `databases/`, reopen (`AppState::reattach`), and remove the
//!   replaced copy unless kept. If the new store fails to open: rename back, reopen the
//!   old one, fail. The task is not cancellable from the first rename on.

use crate::state::{AppState, Dataset, Reservation};
use sparkles_backup::BackupError;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

/// How long an in-place restore waits for requests to release the dataset.
pub const DRAIN: Duration = Duration::from_secs(30);

/// Publish `restored` as the new dataset of `reservation` (renamed into
/// `databases/<name>`, then adopted). On failure nothing stays registered.
pub fn publish_new(
    st: &Arc<AppState>,
    reservation: Reservation,
    restored: &Path,
) -> Result<Arc<Dataset>, BackupError> {
    let _ = (st, reservation, restored);
    Err(BackupError::unsupported("publishing a restored dataset"))
}

/// Replace the registered persistent dataset `name` with `restored` (see the module
/// docs). `task` names the `.replaced-` directory; `keep_replaced` keeps it.
pub fn replace_in_place(
    st: &Arc<AppState>,
    name: &str,
    restored: &Path,
    task: &str,
    keep_replaced: bool,
) -> Result<Arc<Dataset>, BackupError> {
    let _ = (st, name, restored, task, keep_replaced);
    Err(BackupError::unsupported("in-place restores"))
}
