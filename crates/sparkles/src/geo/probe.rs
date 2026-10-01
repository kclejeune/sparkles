//! `sparkles check` of the spatial index's own files.
//!
//! The index is built in memory, so there are no files to check yet.

use super::GeoProbe;
use std::path::Path;

/// Check the persisted index files under `root` (with `checksums`, their data too).
pub(crate) fn files(root: &Path, checksums: bool, p: &mut GeoProbe) {
    let _ = (root, checksums, p);
}
