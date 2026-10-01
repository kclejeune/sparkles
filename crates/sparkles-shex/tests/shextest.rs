//! The shexTest conformance suite (the copy vendored in Apache Jena's `jena-shex`, or an
//! upstream shexTest checkout).
//!
//! Set `SPARKLES_SHEX_TESTS` to the suite directory, otherwise
//! `../../../apache/jena/jena-shex/src/test/files/spec` relative to the workspace is
//! used; the tests are skipped when neither exists. Failures listed in
//! `tests/known-failures.txt` do not fail the run.

use std::path::{Path, PathBuf};

fn suite_dir() -> Option<PathBuf> {
    let p = std::env::var("SPARKLES_SHEX_TESTS")
        .map(PathBuf::from)
        .unwrap_or_else(|_| {
            Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../../../../apache/jena/jena-shex/src/test/files/spec")
        });
    p.exists().then_some(p)
}

#[test]
fn shextest() {
    let Some(dir) = suite_dir() else {
        eprintln!("shexTest suite not found (set SPARKLES_SHEX_TESTS); skipped");
        return;
    };
    eprintln!("shexTest suite at {}: no groups run yet", dir.display());
}
