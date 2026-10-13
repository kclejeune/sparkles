//! The on-disk format fixtures of `testsuite/formats/` (X12 §4.6) at the engine level:
//! every fixture's dataset opens in this build without an upgrade, `check` finds no
//! errors, its quad counts match what the release that wrote it recorded (at the head
//! and at past commits), and a write, a compaction and a second open succeed. The
//! server test `cli_formats.rs` covers the catalog, the queries and the backups.

use sparkles_core::check::{CheckOptions, Status, check};
use sparkles_core::history::{At, HistoryOptions};
use sparkles_core::sparql::QueryOptions;
use sparkles_core::sparql::update::update;
use sparkles_core::store::{Store, StoreOptions};
use std::path::{Path, PathBuf};
use std::process::Command;

fn fixtures() -> Vec<PathBuf> {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../testsuite/formats");
    let mut out: Vec<PathBuf> = std::fs::read_dir(&dir)
        .unwrap()
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.join("fixture.tar.gz").exists())
        .collect();
    out.sort();
    assert!(!out.is_empty(), "no fixtures in {}", dir.display());
    out
}

fn unpack(fixture: &Path, into: &Path) {
    let s = Command::new("tar")
        .arg("-xzf")
        .arg(fixture.join("fixture.tar.gz"))
        .arg("-C")
        .arg(into)
        .status()
        .unwrap();
    assert!(s.success());
}

/// The recorded count of all quads (`expected/<case>.json` of the `count` query).
fn recorded_count(fixture: &Path, case: &str) -> u64 {
    let v: serde_json::Value = serde_json::from_slice(
        &std::fs::read(fixture.join("expected").join(format!("{case}.json"))).unwrap(),
    )
    .unwrap();
    v["results"]["bindings"][0]["n"]["value"]
        .as_str()
        .unwrap()
        .parse()
        .unwrap()
}

fn assert_clean(db: &Path) {
    let r = check(db, &CheckOptions { quick: false }).unwrap();
    assert_ne!(r.status, Status::Error, "{}", r.to_text());
}

#[test]
fn every_fixture_opens_in_place() {
    for fixture in fixtures() {
        let tmp = tempfile::tempdir().unwrap();
        unpack(&fixture, tmp.path());
        let db = tmp.path().join("data/databases/books");
        assert_clean(&db);
        let plan = sparkles_core::migrate::plan(&db).unwrap();
        assert!(plan.in_window(), "{:?}", plan.refused);

        let s = Store::open(&db, StoreOptions::default()).unwrap();
        let head = s.snapshot();
        assert_eq!(head.len(), recorded_count(&fixture, "count"));
        // a past state in a retained generation, kept by the named snapshot
        let (then, _) = s
            .snapshot_at(
                &At::Snapshot("release-1".into()),
                &HistoryOptions::default(),
            )
            .unwrap();
        assert_eq!(then.len(), recorded_count(&fixture, "count-at-snapshot"));
        drop((head, then));

        update(
            &s,
            "INSERT DATA { <http://ex.org/new> <http://ex.org/p> 1 }",
            &QueryOptions::default(),
        )
        .unwrap();
        s.compact().unwrap();
        let n = s.snapshot().len();
        drop(s);
        assert_clean(&db);
        let s = Store::open(&db, StoreOptions::default()).unwrap();
        assert_eq!(s.snapshot().len(), n);
        assert_eq!(n, recorded_count(&fixture, "count") + 1);
    }
}
