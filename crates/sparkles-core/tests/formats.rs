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
        // a generation without predicate patterns answers predicate listings from the
        // permutations, and one with them from the patterns, alike
        same_predicate_listing(&head);
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
        // the compaction wrote the patterns this build reads
        assert!(s.snapshot().generation.patterns.file.is_some());
        same_predicate_listing(&s.snapshot());
    }
}

const LISTING: &str = "SELECT ?p (COUNT(DISTINCT ?s) AS ?n) WHERE { ?s ?p ?o } GROUP BY ?p";

fn listing(
    snap: &std::sync::Arc<sparkles_core::store::Snapshot>,
    opt: sparkles_core::sparql::ctx::Optimizations,
) -> (Vec<String>, bool) {
    let r = sparkles_core::sparql::query(
        snap.clone(),
        LISTING,
        &QueryOptions {
            optimizations: Some(opt),
            no_cache: true,
            ..Default::default()
        },
    )
    .unwrap();
    fn uses(p: &sparkles_core::sparql::exec::PlanInfo) -> bool {
        p.operator == "PatternCount" || p.children.iter().any(uses)
    }
    let mut rows: Vec<String> = r.rows().into_iter().map(|r| format!("{r:?}")).collect();
    rows.sort();
    (rows, uses(&r.plan))
}

fn same_predicate_listing(snap: &std::sync::Arc<sparkles_core::store::Snapshot>) {
    use sparkles_core::sparql::ctx::Optimizations;
    let (fast, used) = listing(snap, Optimizations::ALL);
    let (slow, _) = listing(snap, Optimizations::NONE);
    assert_eq!(fast, slow);
    assert_eq!(used, snap.generation.patterns.file.is_some());
}

/// A pattern file of a newer version is not used: the database opens, queries read the
/// permutations, `check` warns, and the next full compaction writes the current version.
#[test]
fn a_newer_pattern_file_is_not_used() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("db");
    two_generation_database(&root);
    let s = Store::open(&root, StoreOptions::default()).unwrap();
    s.compact().unwrap();
    let gen_dir = s.snapshot().generation.dir.clone().unwrap();
    drop(s);
    let path = gen_dir.join(sparkles_core::patterns::FILE);
    let mut bytes = std::fs::read(&path).unwrap();
    bytes[8..12].copy_from_slice(&99u32.to_le_bytes());
    std::fs::write(&path, &bytes).unwrap();
    let r = check(&root, &CheckOptions { quick: false }).unwrap();
    let c = r.get("patterns").unwrap();
    assert_eq!(c.status, Status::Warning, "{}", r.to_text());
    assert!(r.to_text().contains("version 99"), "{}", r.to_text());
    let s = Store::open(&root, StoreOptions::default()).unwrap();
    let snap = s.snapshot();
    assert!(snap.generation.patterns.file.is_none());
    assert!(
        snap.generation
            .patterns
            .missing
            .as_deref()
            .unwrap()
            .contains("version 99")
    );
    same_predicate_listing(&snap);
    drop(snap);
    s.compact().unwrap();
    assert!(s.snapshot().generation.patterns.file.is_some());
    same_predicate_listing(&s.snapshot());
}

// ---------------------------------------------------------- refused datasets ------

/// A closed database whose `dataset.json` an older release wrote: `minimumReader` 1 and
/// no `writtenBy`. A write-open by this build would change it.
fn old_stamped_database(root: &Path) {
    drop(Store::open(root, StoreOptions::default()).unwrap());
    let path = root.join("dataset.json");
    let mut v: serde_json::Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    v["minimumReader"] = 1.into();
    v.as_object_mut().unwrap().remove("writtenBy");
    std::fs::write(&path, serde_json::to_vec_pretty(&v).unwrap()).unwrap();
}

#[test]
fn a_refused_open_changes_no_files() {
    let newer: &[(&str, serde_json::Value)] = &[
        ("compaction.json", serde_json::json!({"format": 2})),
        ("describe.json", serde_json::json!({"format": 2})),
        ("history.json", serde_json::json!({"format": 2})),
        ("quota.json", serde_json::json!({"format": 2})),
        ("branches.json", serde_json::json!({"format": 3})),
        (
            "rdfs.json",
            serde_json::json!({"rdfsFormat": 2, "graph": "default"}),
        ),
        ("reasoning.json", serde_json::json!({"reasoningFormat": 3})),
    ];
    let mut failures = Vec::new();
    for (name, content) in newer {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("db");
        old_stamped_database(&root);
        let before = std::fs::read(root.join("dataset.json")).unwrap();
        std::fs::write(root.join(name), serde_json::to_vec(content).unwrap()).unwrap();
        match Store::open(&root, StoreOptions::default()) {
            Err(sparkles_core::error::Error::Unsupported(_)) => {}
            Err(e) => failures.push(format!("{name}: {e:?} is not Unsupported")),
            Ok(_) => failures.push(format!("{name}: the open succeeded")),
        }
        if std::fs::read(root.join("dataset.json")).unwrap() != before {
            failures.push(format!("{name}: dataset.json changed"));
        }
    }
    assert!(failures.is_empty(), "{failures:#?}");
}

#[test]
fn a_newer_informational_file_does_not_refuse() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("db");
    old_stamped_database(&root);
    for (name, body) in [
        ("origin.json", r#"{"originFormat": 9}"#),
        ("restore.json", r#"{"restoreFormat": 9}"#),
    ] {
        std::fs::write(root.join(name), body).unwrap();
        drop(Store::open(&root, StoreOptions::default()).unwrap());
    }
}

fn find_files(root: &Path, name: &str) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(d) = stack.pop() {
        for e in std::fs::read_dir(&d).unwrap().flatten() {
            let p = e.path();
            if p.is_dir() {
                stack.push(p)
            } else if p.file_name().unwrap() == name {
                out.push(p)
            }
        }
    }
    out
}

/// A database with a sealed generation and a current one, each with a headered log.
fn two_generation_database(root: &Path) {
    let s = Store::open(root, StoreOptions::default()).unwrap();
    update(
        &s,
        "INSERT DATA { <http://e/a> <http://e/p> 1 }",
        &QueryOptions::default(),
    )
    .unwrap();
    s.compact().unwrap();
    update(
        &s,
        "INSERT DATA { <http://e/a> <http://e/p> 2 }",
        &QueryOptions::default(),
    )
    .unwrap();
}

fn open_err(root: &Path) -> String {
    match Store::open(root, StoreOptions::default()) {
        Ok(_) => "opened".into(),
        Err(sparkles_core::error::Error::Unsupported(m)) => format!("Unsupported({m})"),
        Err(e) => format!("other: {e:?}"),
    }
}

/// A newer log header may use the bytes after its version: the version is read first,
/// so the log is refused as newer, not reported as damaged.
#[test]
fn a_newer_wal_header_with_a_used_tail_is_unsupported() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("db");
    two_generation_database(&root);
    let wals: Vec<_> = find_files(&root, "wal.log")
        .into_iter()
        .filter(|p| std::fs::read(p).unwrap().first() == Some(&0x89))
        .collect();
    assert!(!wals.is_empty(), "no headered log");
    for p in &wals {
        let mut b = std::fs::read(p).unwrap();
        b[8..12].copy_from_slice(&2u32.to_le_bytes());
        b[12] = 1; // a flag a version 2 header might define
        std::fs::write(p, b).unwrap();
    }
    let r = open_err(&root);
    let plan = sparkles_core::migrate::plan(&root);
    assert!(r.starts_with("Unsupported"), "open: {r}; plan: {plan:?}");
    let plan = plan.expect("migrate --check plan failed");
    assert!(!plan.refused.is_empty(), "plan refuses nothing");
}

/// A newer `meta.json` may drop members this build needs: the version is read first, so
/// the generation is refused as newer, not reported as damaged.
#[test]
fn a_newer_index_meta_in_a_new_shape_is_unsupported() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("db");
    two_generation_database(&root);
    for p in &find_files(&root, "meta.json") {
        let mut v: serde_json::Value = serde_json::from_slice(&std::fs::read(p).unwrap()).unwrap();
        v["format_version"] = 3.into();
        v.as_object_mut().unwrap().remove("created");
        v.as_object_mut().unwrap().remove("quads");
        std::fs::write(p, serde_json::to_vec(&v).unwrap()).unwrap();
    }
    let r = open_err(&root);
    let plan = sparkles_core::migrate::plan(&root);
    assert!(r.starts_with("Unsupported"), "open: {r}; plan: {plan:?}");
    let plan = plan.expect("migrate --check plan failed");
    assert!(!plan.refused.is_empty(), "plan refuses nothing");
}

/// A `meta.json` without a usable version is damaged, and `migrate --check` reports it
/// as a refusal instead of failing.
#[test]
fn an_index_meta_without_a_version_is_corrupt() {
    for bad in [
        serde_json::json!("2"),
        serde_json::json!(-1),
        serde_json::Value::Null,
    ] {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("db");
        two_generation_database(&root);
        for p in &find_files(&root, "meta.json") {
            let mut v: serde_json::Value =
                serde_json::from_slice(&std::fs::read(p).unwrap()).unwrap();
            if bad.is_null() {
                v.as_object_mut().unwrap().remove("format_version");
            } else {
                v["format_version"] = bad.clone();
            }
            std::fs::write(p, serde_json::to_vec(&v).unwrap()).unwrap();
        }
        match Store::open(&root, StoreOptions::default()) {
            Err(sparkles_core::error::Error::Corrupt(_)) => {}
            Err(e) => panic!("{bad}: {e:?}"),
            Ok(_) => panic!("{bad}: opened"),
        }
        let plan = sparkles_core::migrate::plan(&root).expect("plan failed");
        assert!(!plan.refused.is_empty(), "{bad}: plan refuses nothing");
    }
}
