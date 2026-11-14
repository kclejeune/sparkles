//! The on-disk format fixtures of `testsuite/formats/` (X12 §4.6) at the engine level:
//! every fixture inside the window opens in this build without an upgrade, `check`
//! finds no errors, its quad counts match what the release that wrote it recorded (at
//! the head and at past commits), and a write, a compaction and a second open succeed.
//! Every pre-release fixture is refused with the dump-and-reload message, and the
//! refusal changes no file. The server test `cli_formats.rs` covers the catalog, the
//! queries and the backups.

use sparkles_core::check::{CheckOptions, Status, check};
use sparkles_core::history::{At, HistoryOptions};
use sparkles_core::sparql::QueryOptions;
use sparkles_core::sparql::update::update;
use sparkles_core::store::{Store, StoreOptions};
use std::path::{Path, PathBuf};
use std::process::Command;

/// The `format` of the dataset's `dataset.json` that a fixture's `facts.txt` records,
/// absent in the pre-release fixtures, whose datasets are format 1.
fn dataset_format(fixture: &Path) -> u32 {
    std::fs::read_to_string(fixture.join("facts.txt"))
        .unwrap()
        .lines()
        .find_map(|l| l.strip_prefix("dataset-format "))
        .map_or(1, |v| v.trim().parse().unwrap())
}

/// Every fixture, in label order, with whether this build reads its dataset format.
fn all_fixtures() -> Vec<(PathBuf, bool)> {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../testsuite/formats");
    let mut out: Vec<PathBuf> = std::fs::read_dir(&dir)
        .unwrap()
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.join("fixture.tar.gz").exists())
        .collect();
    out.sort();
    assert!(!out.is_empty(), "no fixtures in {}", dir.display());
    out.into_iter()
        .map(|p| {
            let inside = dataset_format(&p) == sparkles_core::commit::DATASET_FORMAT;
            (p, inside)
        })
        .collect()
}

/// The fixtures inside the window, which open and answer as recorded.
fn fixtures() -> Vec<PathBuf> {
    let out: Vec<PathBuf> = all_fixtures()
        .into_iter()
        .filter_map(|(p, inside)| inside.then_some(p))
        .collect();
    assert!(!out.is_empty(), "no fixture inside the window");
    out
}

/// The pre-release fixtures, outside the window, whose refusal the tests check.
fn refused_fixtures() -> Vec<PathBuf> {
    let out: Vec<PathBuf> = all_fixtures()
        .into_iter()
        .filter_map(|(p, inside)| (!inside).then_some(p))
        .collect();
    assert!(!out.is_empty(), "no fixture outside the window");
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

/// Every file under `root` with its content, in path order.
fn files_under(root: &Path) -> Vec<(PathBuf, Vec<u8>)> {
    let mut out = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        for entry in std::fs::read_dir(&dir).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                stack.push(path);
            } else {
                out.push((path.clone(), std::fs::read(&path).unwrap()));
            }
        }
    }
    out.sort();
    out
}

/// A pre-release fixture is refused by its `dataset.json`, which is `format` 1, with the
/// release that wrote it and the dump-and-reload path named, by the open, by `check` and
/// by the migration plan, and the refusal changes no file. The cutoff of spec X16
/// Phase 1 keeps no reader of the pre-release forms, so there is no upgrade step.
#[test]
fn every_pre_release_fixture_is_refused_with_dump_and_reload() {
    for fixture in refused_fixtures() {
        let tmp = tempfile::tempdir().unwrap();
        unpack(&fixture, tmp.path());
        let db = tmp.path().join("data/databases/books");
        let before = files_under(&db);
        let written_by = sparkles_core::commit::read_written_by(&db);
        let e = Store::open(&db, StoreOptions::default()).err().unwrap();
        let message = e.to_string();
        assert!(
            matches!(e, sparkles_core::Error::Unsupported(_)),
            "{message}"
        );
        assert!(message.contains("dataset.json"), "{message}");
        assert!(message.contains("has format 1"), "{message}");
        assert!(message.contains("sparkles dump"), "{message}");
        assert!(message.contains("sparkles load"), "{message}");
        match written_by {
            Some(v) => assert!(message.contains(&format!("Sparkles {v}")), "{message}"),
            None => assert!(message.contains("the release that wrote it"), "{message}"),
        }
        let plan = sparkles_core::migrate::plan(&db).unwrap();
        assert!(!plan.in_window(), "{}", fixture.display());
        assert!(plan.steps.is_empty(), "{:?}", plan.steps);
        assert!(
            plan.refused.iter().any(|r| r.contains("sparkles dump")),
            "{:?}",
            plan.refused
        );
        let r = check(&db, &CheckOptions { quick: false }).unwrap();
        assert_eq!(r.status, Status::Error, "{}", r.to_text());
        assert!(r.to_text().contains("sparkles dump"), "{}", r.to_text());
        assert_eq!(files_under(&db), before, "the refusal changed a file");
    }
}

/// The removed forms, each refused from a minimal hand-written file so that the refusal
/// does not depend on a fixture: a `format` 1 dataset, a dataset that requires a reader
/// below 5, a format 4 generation, a format 5 generation without `vocab.idx`, a delta
/// vocabulary of the entries alone and a headerless log.
#[test]
fn each_removed_form_is_refused_from_a_minimal_file() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("db");
    {
        // a commit, so that the log holds records after its header
        let s = Store::open(&root, StoreOptions::default()).unwrap();
        update(
            &s,
            "INSERT DATA { <http://ex.org/s> <http://ex.org/p> 1 }",
            &QueryOptions::default(),
        )
        .unwrap();
    }
    let dataset = root.join("dataset.json");
    let original = std::fs::read(&dataset).unwrap();
    let refusal = |root: &Path| {
        Store::open(root, StoreOptions::default())
            .err()
            .expect("the removed form opened")
    };
    let edit_dataset = |f: &dyn Fn(&mut serde_json::Value)| {
        let mut v: serde_json::Value = serde_json::from_slice(&original).unwrap();
        f(&mut v);
        std::fs::write(&dataset, serde_json::to_vec_pretty(&v).unwrap()).unwrap();
    };
    // the pre-release dataset file, with and without the version members
    edit_dataset(&|v| v["format"] = 1.into());
    let e = refusal(&root).to_string();
    assert!(
        e.contains("has format 1") && e.contains("sparkles dump"),
        "{e}"
    );
    edit_dataset(&|v| {
        v.as_object_mut().unwrap().remove("format");
        v.as_object_mut().unwrap().remove("minimumReader");
    });
    let e = refusal(&root).to_string();
    assert!(e.contains("has format 1 and requires reader 1"), "{e}");
    assert!(e.contains("sparkles dump"), "{e}");
    // a dataset that requires a pre-release reader
    edit_dataset(&|v| v["minimumReader"] = 4.into());
    let e = refusal(&root).to_string();
    assert!(
        e.contains("requires reader 4") && e.contains("sparkles dump"),
        "{e}"
    );
    // a newer dataset is refused as newer, not with dump and reload
    edit_dataset(&|v| v["format"] = 3.into());
    let e = refusal(&root).to_string();
    assert!(
        e.contains("has format 3") && e.contains("newer release"),
        "{e}"
    );
    assert!(!e.contains("sparkles dump"), "{e}");
    std::fs::write(&dataset, &original).unwrap();
    drop(Store::open(&root, StoreOptions::default()).unwrap());

    // the pre-release index formats, and a format 5 generation without its vocab.idx
    let current = std::fs::read_to_string(root.join("CURRENT")).unwrap();
    let gen_dir = root.join(current.trim());
    let meta = gen_dir.join("meta.json");
    let original_meta = std::fs::read(&meta).unwrap();
    let mut m: serde_json::Value = serde_json::from_slice(&original_meta).unwrap();
    assert_eq!(m["format_version"], 5);
    assert_eq!(m["features"], serde_json::json!([]));
    for format in [2, 3, 4] {
        m["format_version"] = format.into();
        std::fs::write(&meta, serde_json::to_vec(&m).unwrap()).unwrap();
        let e = refusal(&root).to_string();
        assert!(e.contains(&format!("index version {format}")), "{e}");
        assert!(
            e.contains("reads version 5") && e.contains("sparkles dump"),
            "{e}"
        );
    }
    m["format_version"] = 5.into();
    m.as_object_mut().unwrap().remove("features");
    std::fs::write(&meta, serde_json::to_vec(&m).unwrap()).unwrap();
    let e = refusal(&root);
    assert!(matches!(e, sparkles_core::Error::Corrupt(_)), "{e}");
    assert!(e.to_string().contains("features is missing"), "{e}");
    std::fs::write(&meta, &original_meta).unwrap();
    let idx = gen_dir.join("vocab.idx");
    let original_idx = std::fs::read(&idx).unwrap();
    std::fs::remove_file(&idx).unwrap();
    let e = refusal(&root);
    assert!(matches!(e, sparkles_core::Error::Corrupt(_)), "{e}");
    assert!(
        e.to_string()
            .contains("requires the sparse vocabulary index"),
        "{e}"
    );
    std::fs::write(&idx, &original_idx).unwrap();
    drop(Store::open(&root, StoreOptions::default()).unwrap());

    // a delta vocabulary of the entries alone, without the framed header
    let delta = gen_dir.join("delta.vocab");
    let original_delta = std::fs::read(&delta).unwrap();
    std::fs::write(&delta, [&7u32.to_le_bytes()[..], b"<urn:a>"].concat()).unwrap();
    let e = refusal(&root).to_string();
    assert!(
        e.contains("delta-vocab version 1") && e.contains("sparkles dump"),
        "{e}"
    );
    std::fs::write(&delta, &original_delta).unwrap();
    drop(Store::open(&root, StoreOptions::default()).unwrap());

    // a headerless log: the records of a committed transaction without the header record
    let wal = gen_dir.join("wal.log");
    let original_wal = std::fs::read(&wal).unwrap();
    assert!(
        original_wal.len() > 33,
        "the log holds records after its header"
    );
    std::fs::write(&wal, &original_wal[33..]).unwrap();
    let e = refusal(&root).to_string();
    assert!(
        e.contains("wal version 0") && e.contains("sparkles dump"),
        "{e}"
    );
    std::fs::write(&wal, &original_wal).unwrap();
    drop(Store::open(&root, StoreOptions::default()).unwrap());
}

/// A header record that a crash at the log's creation tore, an empty file, zeros or a
/// few bytes of the header, reads as an empty log that the open creates again, and the
/// next commit survives a reopen. A zeroed header with records after it is damage, not a
/// torn creation, and is refused as corrupt. A headerless log with records is the
/// pre-release form, which the migration plan refuses with dump and reload.
#[test]
fn a_torn_log_header_is_recreated_and_a_zeroed_one_with_records_is_corrupt() {
    for torn in [&[][..], &[0u8; 33][..], &b"\x89SPKWAL\n\x01"[..]] {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("db");
        drop(Store::open(&root, StoreOptions::default()).unwrap());
        let current = std::fs::read_to_string(root.join("CURRENT")).unwrap();
        let wal = root.join(current.trim()).join("wal.log");
        std::fs::write(&wal, torn).unwrap();
        let s = Store::open(&root, StoreOptions::default()).unwrap();
        assert_eq!(s.head_commit().seq, 0);
        update(
            &s,
            "INSERT DATA { <http://ex.org/s> <http://ex.org/p> 1 }",
            &QueryOptions::default(),
        )
        .unwrap();
        drop(s);
        let bytes = std::fs::read(&wal).unwrap();
        assert_eq!(
            &bytes[..8],
            b"\x89SPKWAL\n",
            "the log starts with its header again"
        );
        let s = Store::open(&root, StoreOptions::default()).unwrap();
        assert_eq!(s.head_commit().seq, 1);
        assert_eq!(s.snapshot().len(), 1);
        drop(s);
        assert_clean(&root);
    }
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("db");
    {
        let s = Store::open(&root, StoreOptions::default()).unwrap();
        update(
            &s,
            "INSERT DATA { <http://ex.org/s> <http://ex.org/p> 1 }",
            &QueryOptions::default(),
        )
        .unwrap();
    }
    let current = std::fs::read_to_string(root.join("CURRENT")).unwrap();
    let wal = root.join(current.trim()).join("wal.log");
    let good = std::fs::read(&wal).unwrap();
    let mut zeroed = good.clone();
    zeroed[..33].fill(0);
    std::fs::write(&wal, &zeroed).unwrap();
    let e = Store::open(&root, StoreOptions::default()).err().unwrap();
    assert!(matches!(e, sparkles_core::Error::Corrupt(_)), "{e}");
    assert!(e.to_string().contains("header record is zeros"), "{e}");
    assert_eq!(
        std::fs::read(&wal).unwrap(),
        zeroed,
        "the refusal changed the log"
    );
    std::fs::write(&wal, &good[33..]).unwrap();
    let plan = sparkles_core::migrate::plan(&root).unwrap();
    assert!(!plan.in_window(), "{plan:?}");
    assert!(
        plan.refused
            .iter()
            .any(|r| r.contains("wal version 0") && r.contains("sparkles dump")),
        "{:?}",
        plan.refused
    );
    assert!(plan.steps.is_empty());
    std::fs::write(&wal, &good).unwrap();
    assert_eq!(
        Store::open(&root, StoreOptions::default())
            .unwrap()
            .head_commit()
            .seq,
        1
    );
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
    let mut rows: Vec<String> = r
        .rows()
        .unwrap()
        .into_iter()
        .map(|r| format!("{r:?}"))
        .collect();
    rows.sort();
    (rows, uses(r.plan()))
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

/// A closed database whose `dataset.json` another release of this format wrote, without
/// a `writtenBy`. A write-open by this build would change it.
fn old_stamped_database(root: &Path) {
    drop(Store::open(root, StoreOptions::default()).unwrap());
    let path = root.join("dataset.json");
    let mut v: serde_json::Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
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
        v["format_version"] = 6.into();
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

/// A managed-graph registry this build cannot enforce does not refuse the open: the
/// dataset stays readable, and every write fails closed (spec C25 §7).
#[test]
fn a_newer_managed_registry_refuses_writes_not_the_open() {
    use sparkles_core::managed::{OwnerEntry, OwnerKind};
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("db");
    let s = Store::open(&root, StoreOptions::default()).unwrap();
    let settings = br#"{"format":1,"profile":1}"#;
    s.enable_managed(
        OwnerKind::Evidence,
        OwnerEntry::for_settings(1, vec!["urn:m".into()], settings),
        settings,
    )
    .unwrap();
    drop(s);
    let v = sparkles_core::commit::read_dataset_versions(&root)
        .unwrap()
        .unwrap();
    assert_eq!(v.minimum_reader, 6);
    std::fs::write(
        root.join("managed.json"),
        r#"{"format": 3, "revision": 9, "owners": {}}"#,
    )
    .unwrap();
    assert!(sparkles_core::formats::check_dataset_dir(&root).is_ok());
    let s = Store::open(&root, StoreOptions::default()).unwrap();
    sparkles_core::sparql::query(s.snapshot(), "ASK { ?s ?p ?o }", &QueryOptions::default())
        .unwrap();
    match update(
        &s,
        "INSERT DATA { <urn:a> <urn:p> 1 }",
        &QueryOptions::default(),
    ) {
        Err(sparkles_core::error::Error::Managed(m)) => assert_eq!(m.code, "managed-unavailable"),
        r => panic!("{r:?}"),
    }
}
