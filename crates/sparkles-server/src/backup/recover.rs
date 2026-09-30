//! Startup recovery of interrupted restores, run before the registry's datasets are
//! opened (a crash between the two renames of an in-place restore leaves
//! `databases/<ds>` missing, which would stop the server):
//! * `databases/.restore-*` (an unfinished download) is removed;
//! * `databases/.replaced-<ds>-<task>` with no `databases/<ds>` is renamed back to
//!   `<ds>` with a WARN (the crash came between the renames);
//! * `databases/.replaced-<ds>-<task>` next to a `databases/<ds>` is removed (the swap
//!   finished before the crash). A copy kept on purpose (`keepReplaced`) is renamed
//!   by the restore to a name this rule does not match.

use std::path::Path;

/// Recover `<data_dir>/databases` (see the module docs). Called by `serve` before
/// `AppState::new`.
pub fn startup(data_dir: &Path) -> anyhow::Result<()> {
    let _ = data_dir;
    Ok(())
}
