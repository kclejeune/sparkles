//! Linked bases, inherited overlays and blank-node identities use exact bounded replay.
use sparkles_core::{
    Error,
    branch::BranchOptions,
    history::{At, HistoryOptions},
    io::{RdfFormat, Source},
    sparql::{QueryOptions, query, update::update},
    store::{Snapshot, Store, StoreOptions},
};
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    sync::Arc,
};

fn write(store: &Store, text: &str) {
    update(store, text, &QueryOptions::default()).unwrap();
}
fn rows(snapshot: Arc<Snapshot>) -> Vec<Vec<Option<oxrdf::Term>>> {
    query(
        snapshot,
        "SELECT ?s ?p ?o WHERE { ?s ?p ?o } ORDER BY ?s ?p ?o",
        &QueryOptions::default(),
    )
    .unwrap()
    .rows()
}
fn durable_bytes(root: &Path) -> BTreeMap<PathBuf, Vec<u8>> {
    let mut pending = vec![root.to_owned()];
    let mut result = BTreeMap::new();
    while let Some(dir) = pending.pop() {
        for entry in std::fs::read_dir(dir).unwrap() {
            let entry = entry.unwrap();
            if entry.file_type().unwrap().is_dir() {
                pending.push(entry.path());
            } else {
                result.insert(entry.path(), std::fs::read(entry.path()).unwrap());
            }
        }
    }
    result
}
fn options() -> StoreOptions {
    StoreOptions {
        build: sparkles_core::builder::BuildOptions {
            threads: 1,
            ..Default::default()
        },
        ..Default::default()
    }
}

#[test]
fn cold_nested_linked_bases_preserve_state_and_parent_overlays_without_writes() {
    for parent_overlay in [false, true] {
        let directory = tempfile::tempdir().unwrap();
        let main = Store::open(directory.path(), options()).unwrap();
        main.load(&[Source::from_bytes(
            b"<urn:base> <urn:p> \"v\" .".to_vec(),
            RdfFormat::NTriples,
            None,
        )])
        .unwrap();
        write(&main, "INSERT DATA { <urn:main> <urn:p> 1 }");
        main.create_branch("dev", &Default::default()).unwrap();
        let expected;
        let target;
        {
            let dev = main.branch("dev").unwrap();
            write(&dev, "INSERT DATA { <urn:dev> <urn:p> _:dev }");
            if parent_overlay {
                write(&main, "INSERT DATA { <urn:main-later> <urn:p> 9 }");
                main.relink_branch("dev", &Default::default()).unwrap();
            }
            main.create_branch(
                "leaf",
                &BranchOptions {
                    from: "dev".into(),
                    ..Default::default()
                },
            )
            .unwrap();
            let leaf = main.branch("leaf").unwrap();
            write(&leaf, "INSERT DATA { <urn:leaf> <urn:p> _:leaf }");
            target = leaf.head_commit().seq;
            expected = rows(
                leaf.snapshot_at(&At::Commit(target), &Default::default())
                    .unwrap()
                    .0,
            );
            assert_eq!(expected.len(), 4);
            leaf.create_snapshot("retained", &At::Commit(target), None)
                .unwrap();
            write(&leaf, "INSERT DATA { <urn:future> <urn:p> 5 }");
            leaf.compact().unwrap();
            leaf.flush_change_log().unwrap();
            dev.flush_change_log().unwrap();
        }
        main.flush_change_log().unwrap();
        drop(main);
        let main = Store::open(directory.path(), options()).unwrap();
        let leaf = main.branch("leaf").unwrap();
        main.flush_change_log().unwrap();
        leaf.flush_change_log().unwrap();
        let before = durable_bytes(directory.path());
        let options = HistoryOptions::default();
        assert!(matches!(
            leaf.snapshot_at_limited(&At::Commit(target), &options, 32 << 10),
            Err(Error::BudgetExceeded(_))
        ));
        let (snapshot, resolved) = leaf
            .snapshot_at_limited(&At::Commit(target), &options, 32 << 20)
            .unwrap();
        assert_eq!(resolved.commit.seq, target);
        assert_eq!(snapshot.commit, target);
        assert_eq!(rows(snapshot), expected);
        assert_eq!(before, durable_bytes(directory.path()));
        let (cached, _) = leaf
            .snapshot_at_limited(&At::Commit(target), &options, 32 << 20)
            .unwrap();
        assert_eq!(rows(cached), expected);
        assert_eq!(before, durable_bytes(directory.path()));
    }
}

#[test]
fn cold_relinked_generation_preserves_its_own_overlay() {
    let directory = tempfile::tempdir().unwrap();
    let main = Store::open(directory.path(), options()).unwrap();
    write(&main, "INSERT DATA { <urn:base> <urn:p> 1 }");
    main.create_branch("dev", &Default::default()).unwrap();
    let target;
    let expected;
    {
        let dev = main.branch("dev").unwrap();
        write(&dev, "INSERT DATA { <urn:dev> <urn:p> _:dev }");
        write(&main, "INSERT DATA { <urn:main-only> <urn:p> 2 }");
        main.relink_branch("dev", &Default::default()).unwrap();
        write(&dev, "INSERT DATA { <urn:after-relink> <urn:p> 3 }");
        target = dev.head_commit().seq;
        expected = rows(dev.snapshot());
        assert_eq!(expected.len(), 3);
        dev.create_snapshot("retained", &At::Commit(target), None)
            .unwrap();
        write(&dev, "INSERT DATA { <urn:future> <urn:p> 4 }");
        dev.compact().unwrap();
        dev.flush_change_log().unwrap();
    }
    main.flush_change_log().unwrap();
    drop(main);
    let main = Store::open(directory.path(), options()).unwrap();
    let dev = main.branch("dev").unwrap();
    main.flush_change_log().unwrap();
    dev.flush_change_log().unwrap();
    let before = durable_bytes(directory.path());
    let (view, _) = dev
        .snapshot_at_limited(&At::Commit(target), &HistoryOptions::default(), 32 << 20)
        .unwrap();
    assert_eq!(view.commit, target);
    assert_eq!(rows(view), expected);
    assert_eq!(before, durable_bytes(directory.path()));
}
