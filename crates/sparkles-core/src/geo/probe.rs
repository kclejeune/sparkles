//! `sparkles check` of the spatial index's own files: those of the generation `CURRENT`
//! names, checked as opening the store would check them (header, footer, identity, index
//! checksums, and with `checksums` the data checksums too). A damaged file is only a
//! warning: the store rebuilds it when it opens.

use super::GeoProbe;
use super::persist::{self, FileKind, Identity, Mapped, Problem};
use std::path::Path;

/// Check the persisted index files under `root` (with `checksums`, their data too).
pub(crate) fn files(root: &Path, checksums: bool, p: &mut GeoProbe) {
    let Some(cfg) = &p.config else {
        return;
    };
    let Ok(current) = std::fs::read_to_string(root.join("CURRENT")) else {
        return;
    };
    let name = current.trim();
    let gdir = root.join(name);
    let Ok(meta) = std::fs::read(gdir.join("meta.json")) else {
        return;
    };
    let Ok(meta) = serde_json::from_slice::<crate::builder::IndexMeta>(&meta) else {
        return;
    };
    let ident = Identity::of(&gdir, meta.quads, meta.terms, cfg.index_hash());
    let dir = persist::dir_of(&gdir);
    let mut found = Vec::new();
    let mut missing = Vec::new();
    for kind in [FileKind::Rtree, FileKind::Column] {
        let rel = format!("{name}/{}/{}", persist::DIR, kind.file_name());
        match Mapped::open(&dir.join(kind.file_name()), kind, Some(&ident), checksums) {
            Ok(m) => {
                p.file_bytes += m.len();
                found.push(rel);
            }
            Err(Problem::Missing) => missing.push(rel),
            Err(Problem::Unusable(m)) => p.damaged.push((rel, m)),
        }
    }
    // both missing: the index was never written for this generation (it is built when
    // the store opens); one of the two alone is as good as damaged
    if !found.is_empty() || !p.damaged.is_empty() {
        for f in missing {
            p.damaged.push((f, "missing".into()));
        }
    }
    p.files = found;
}
