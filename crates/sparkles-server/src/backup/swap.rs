//! HTTP request draining around the catalog's restore publication.
use crate::state::{AppState, Dataset, Reservation};
use sparkles_backup::{BackupError, Code};
use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};

pub fn publish_new(
    st: &Arc<AppState>,
    reservation: Reservation,
    restored: &Path,
) -> Result<Arc<Dataset>, BackupError> {
    let name = reservation.name().to_string();
    st.catalog
        .publish_restored(reservation, restored)
        .map_err(super::ops::library_error)?;
    Ok(st.get(&name).expect("published"))
}

pub fn replace_in_place(
    st: &Arc<AppState>,
    name: &str,
    restored: &Path,
    task: &str,
    keep: bool,
) -> Result<Arc<Dataset>, BackupError> {
    let r = st
        .catalog
        .reserve(name, sparkles::catalog::ReservationKind::Restore, task)
        .map_err(|e| BackupError::new(Code::DatasetBusy, e.to_string()))?;
    crate::compaction::cancel(st, name);
    let start = Instant::now();
    loop {
        let busy = st.get(name).is_some_and(|ds| Arc::strong_count(&ds) > 2);
        if !busy {
            break;
        }
        if start.elapsed() >= Duration::from_secs(30) {
            return Err(BackupError::new(
                Code::DatasetBusy,
                format!("requests to /{name} did not finish within 30 s; the dataset is unchanged"),
            ));
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    st.forget_routing(name);
    let result = st
        .catalog
        .replace_restored_reserved(r, restored, task, keep);
    // Recreate the HTTP observers on success and on rollback.
    let ds = st.get(name);
    result.map_err(super::ops::library_error)?;
    Ok(ds.expect("replaced"))
}
