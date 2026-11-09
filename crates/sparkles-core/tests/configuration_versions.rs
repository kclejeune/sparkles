//! The file versions a store writes depend on whether its configuration history is
//! activated. A store that never activates keeps the versions that older releases read,
//! and an activated store records versions that they refuse cleanly.

use sparkles_core::access::Preparation;
use sparkles_core::formats::{DATASET, FileKind, WAL};
use sparkles_core::sparql::QueryOptions;
use sparkles_core::sparql::update::update;
use sparkles_core::store::{Store, StoreOptions};
use sparkles_core::task::Control;
use std::path::{Path, PathBuf};

/// The versions of the release before configuration history, which an ordinary store
/// must keep writing.
const PLAIN_WAL: u32 = 1;
const PLAIN_READER: u64 = 5;
const PLAIN_GENERATION_COMMIT: u64 = 1;
const PLAIN_MANAGED: u64 = 2;

/// What an older release reads: logs up to version 1 and datasets up to reader 7.
const OLDER_WAL: FileKind = FileKind {
    reads: &[0, 1],
    writes: 1,
    ..WAL
};
const OLDER_DATASET: FileKind = FileKind {
    reads: &[1, 2, 3, 4, 5, 6, 7],
    writes: 7,
    ..DATASET
};

fn insert(store: &Store, n: u32) {
    update(
        store,
        &format!("INSERT DATA {{ <urn:s{n}> <urn:p> {n} }}"),
        &QueryOptions::default(),
    )
    .unwrap();
}

fn json(path: &Path) -> serde_json::Value {
    serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap()
}

fn current(root: &Path) -> PathBuf {
    root.join(
        std::fs::read_to_string(root.join("CURRENT"))
            .unwrap()
            .trim(),
    )
}

fn wal_version(generation: &Path) -> u32 {
    let bytes = std::fs::read(generation.join("wal.log")).unwrap();
    assert_eq!(&bytes[..8], b"\x89SPKWAL\n");
    u32::from_le_bytes(bytes[8..12].try_into().unwrap())
}

fn reader(root: &Path) -> u64 {
    json(&root.join("dataset.json"))["minimumReader"]
        .as_u64()
        .unwrap()
}

fn commit_format(generation: &Path) -> u64 {
    json(&generation.join("commit.json"))["format"]
        .as_u64()
        .unwrap()
}

fn enable_evidence(store: &Store) {
    let settings = sparkles_core::evidence::Settings::default();
    let canonical = settings.canonical(&Control::none()).unwrap();
    store
        .enable_managed(
            sparkles_core::managed::OwnerKind::Evidence,
            sparkles_core::managed::OwnerEntry::for_settings(
                1,
                settings.managed_graphs(),
                canonical.bytes(),
            ),
            canonical.bytes(),
        )
        .unwrap();
}

/// A store with managed graphs, two commits and a compaction, never activated.
fn ordinary(root: &Path) -> Store {
    let store = Store::open(root, StoreOptions::default()).unwrap();
    enable_evidence(&store);
    insert(&store, 1);
    store.compact().unwrap();
    insert(&store, 2);
    store
}

fn activate(root: &Path) {
    let preparation = Preparation::new(Control::none(), 64 << 20).unwrap();
    sparkles_core::migrate::activate_configuration_history(root, false, &preparation).unwrap();
}

#[test]
fn a_store_that_never_activates_keeps_the_older_versions() {
    let temporary = tempfile::tempdir().unwrap();
    let root = temporary.path().join("db");
    let store = ordinary(&root);
    store.compact().unwrap();
    drop(store);
    let store = Store::open(&root, StoreOptions::default()).unwrap();
    insert(&store, 3);
    drop(store);
    let generation = current(&root);
    assert_eq!(wal_version(&generation), PLAIN_WAL);
    assert_eq!(commit_format(&generation), PLAIN_GENERATION_COMMIT);
    assert!(
        json(&generation.join("commit.json"))
            .get("configurationBinding")
            .is_none()
    );
    // the managed reader stays what managed graphs need, below configuration history
    assert!(
        reader(&root) >= PLAIN_READER && reader(&root) < 8,
        "{}",
        reader(&root)
    );
    let managed = json(&root.join("managed.json"));
    assert_eq!(managed["format"].as_u64(), Some(PLAIN_MANAGED));
    assert!(managed.get("configurationHistory").is_none());
    assert!(!root.join("evidence-history").exists());
    // an older release reads every version this store records
    OLDER_WAL
        .check("wal", wal_version(&generation), None)
        .unwrap();
    OLDER_DATASET
        .check("dataset", reader(&root) as u32, None)
        .unwrap();
}

#[test]
fn a_new_store_without_managed_graphs_records_the_older_reader() {
    let temporary = tempfile::tempdir().unwrap();
    let root = temporary.path().join("db");
    let store = Store::open(&root, StoreOptions::default()).unwrap();
    insert(&store, 1);
    drop(store);
    assert_eq!(reader(&root), PLAIN_READER);
    assert_eq!(wal_version(&current(&root)), PLAIN_WAL);
}

#[test]
fn activation_records_newer_versions_that_an_older_reader_refuses() {
    let temporary = tempfile::tempdir().unwrap();
    let root = temporary.path().join("db");
    drop(ordinary(&root));
    activate(&root);
    assert!(reader(&root) >= 8, "{}", reader(&root));
    // the current log was upgraded in place, and later generations keep the version
    assert_eq!(
        wal_version(&current(&root)),
        sparkles_core::formats::WAL_VERSION
    );
    let store = Store::open(&root, StoreOptions::default()).unwrap();
    insert(&store, 3);
    store.compact().unwrap();
    insert(&store, 4);
    drop(store);
    let generation = current(&root);
    assert_eq!(
        wal_version(&generation),
        sparkles_core::formats::WAL_VERSION
    );
    assert_eq!(commit_format(&generation), 2);
    assert!(
        json(&generation.join("commit.json"))
            .get("configurationBinding")
            .is_some()
    );
    let managed = json(&root.join("managed.json"));
    assert_eq!(managed["format"].as_u64(), Some(3));
    assert!(managed.get("configurationHistory").is_some());
    // an older release refuses the dataset by its reader, before it reads anything else
    for refused in [
        OLDER_DATASET.check("dataset", reader(&root) as u32, None),
        OLDER_WAL.check("wal", wal_version(&generation), None),
    ] {
        assert!(
            matches!(refused, Err(sparkles_core::Error::Unsupported(_))),
            "{refused:?}"
        );
    }
}

#[test]
fn an_activated_store_without_its_registry_root_refuses_to_open() {
    let temporary = tempfile::tempdir().unwrap();
    let root = temporary.path().join("db");
    drop(ordinary(&root));
    activate(&root);
    drop(Store::open(&root, StoreOptions::default()).unwrap());
    let managed = std::fs::read(root.join("managed.json")).unwrap();
    let dataset = std::fs::read(root.join("dataset.json")).unwrap();
    // a missing registry, and a registry that no longer names the history root
    let mut legacy = json(&root.join("managed.json"));
    legacy
        .as_object_mut()
        .unwrap()
        .remove("configurationHistory");
    legacy["format"] = PLAIN_MANAGED.into();
    // and a registry this build cannot read at all, which names no root either
    for replacement in [
        None,
        Some(serde_json::to_vec_pretty(&legacy).unwrap()),
        Some(b"{}".to_vec()),
    ] {
        match &replacement {
            None => std::fs::remove_file(root.join("managed.json")).unwrap(),
            Some(bytes) => std::fs::write(root.join("managed.json"), bytes).unwrap(),
        }
        let refused = Store::open(&root, StoreOptions::default());
        let error = refused
            .err()
            .expect("an activated store must not open as legacy");
        assert_eq!(error.code(), "evidence-unavailable", "{error}");
        // the refusal writes nothing
        assert_eq!(std::fs::read(root.join("dataset.json")).unwrap(), dataset);
        std::fs::write(root.join("managed.json"), &managed).unwrap();
    }
    // with its reader lowered, the store still refuses: its version 2 files are not
    // admitted, and its history directory records activation
    let mut versions = json(&root.join("dataset.json"));
    versions["minimumReader"] = PLAIN_READER.into();
    std::fs::write(
        root.join("dataset.json"),
        serde_json::to_vec_pretty(&versions).unwrap(),
    )
    .unwrap();
    std::fs::remove_file(root.join("managed.json")).unwrap();
    let error = Store::open(&root, StoreOptions::default())
        .err()
        .expect("a lowered reader must not open the store as legacy");
    assert!(
        matches!(error.code(), "evidence-unavailable" | "unsupported"),
        "{error}"
    );
    std::fs::write(root.join("dataset.json"), &dataset).unwrap();
    std::fs::write(root.join("managed.json"), &managed).unwrap();
    drop(Store::open(&root, StoreOptions::default()).unwrap());
}

#[test]
fn an_ordinary_store_without_a_registry_still_opens() {
    let temporary = tempfile::tempdir().unwrap();
    let root = temporary.path().join("db");
    let store = Store::open(&root, StoreOptions::default()).unwrap();
    insert(&store, 1);
    drop(store);
    assert!(!root.join("managed.json").exists());
    let store = Store::open(&root, StoreOptions::default()).unwrap();
    insert(&store, 2);
}
