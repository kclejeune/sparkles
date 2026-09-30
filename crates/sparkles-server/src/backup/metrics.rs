//! Backup metrics (Prometheus at `/$/metrics`, and OTel observable instruments). Label
//! sets are closed: `repository` is capped like `dataset` (`--metrics-max-datasets`,
//! overflow `$other`).
//!
//! | Name | Type | Labels |
//! |---|---|---|
//! | `sparkles_backup_operations_total` | counter | `repository`, `operation`, `result` |
//! | `sparkles_backup_operation_duration_seconds` | histogram | `operation` |
//! | `sparkles_backup_bytes_uploaded_total`, `…_downloaded_total` | counter | `repository` |
//! | `sparkles_backup_blobs_uploaded_total`, `…_blobs_reused_total` | counter | `repository` |
//! | `sparkles_backup_object_requests_total` | counter | `repository`, `op`, `result` |
//! | `sparkles_backup_last_success_timestamp_seconds` | gauge | `dataset`, `repository` |
//! | `sparkles_backup_capture_lock_seconds` | histogram | |
//! | `sparkles_backup_repository_stored_bytes`, `…_logical_bytes`, `…_backups` | gauge | `repository` |
//! | `sparkles_backup_lock_conflicts_total` | counter | `repository` |
//! | `sparkles_backup_policy_runs_total` | counter | `policy`, `result` |
//! | `sparkles_backup_policy_last_success_timestamp_seconds`, `…_next_run_timestamp_seconds`, `…_consecutive_failures` | gauge | `policy` |

use crate::state::AppState;

/// Append the backup metric families to a Prometheus exposition (called by
/// `obs::render_prometheus`). Writes nothing without `AppState::backup`.
pub fn render(st: &AppState, out: &mut String) {
    super::policies::render_metrics(st, out);
}
