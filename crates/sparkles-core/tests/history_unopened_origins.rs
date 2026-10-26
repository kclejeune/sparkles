//! Exact inherited reads must prepare a never-opened parent without recovery writes.
use sparkles_core::{
    Error,
    branch::BranchOptions,
    history::{At, HistoryOptions},
    sparql::{QueryOptions, query, update::update},
    store::{Snapshot, Store, StoreOptions},
};
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    sync::Arc,
};

fn write(store: &Store, body: &str) {
    update(store, body, &QueryOptions::default()).unwrap();
}
fn rows(snapshot: Arc<Snapshot>) -> Vec<Vec<Option<oxrdf::Term>>> {
    query(
        snapshot,
        "SELECT ?s ?p ?o WHERE { ?s ?p ?o } ORDER BY ?s ?p ?o",
        &Default::default(),
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

#[test]
fn unopened_parent_preserves_exact_commit_origin_and_durable_bytes() {
    let dir = tempfile::tempdir().unwrap();
    let options = StoreOptions::default();
    let main = Store::open(dir.path(), options.clone()).unwrap();
    write(&main, "INSERT DATA { <urn:main> <urn:p> 1 }");
    main.create_branch("parent", &Default::default()).unwrap();
    let parent = main.branch("parent").unwrap();
    write(&parent, "INSERT DATA { <urn:parent> <urn:p> _:parent }");
    let target = parent.head_commit();
    let expected = rows(parent.snapshot());
    let parent_id = parent.branch_id();
    write(&parent, "INSERT DATA { <urn:later> <urn:p> 3 }");
    main.create_branch(
        "child",
        &BranchOptions {
            from: "parent".into(),
            ..Default::default()
        },
    )
    .unwrap();
    let child = main.branch("child").unwrap();
    write(&child, "INSERT DATA { <urn:child> <urn:p> 4 }");
    child.flush_change_log().unwrap();
    parent.flush_change_log().unwrap();
    main.flush_change_log().unwrap();
    drop(child);
    drop(parent);
    drop(main);

    let main = Store::open(dir.path(), options).unwrap();
    // Opening this child reads linked files, but does not open its parent Store.
    let child = main.branch("child").unwrap();
    child.flush_change_log().unwrap();
    main.flush_change_log().unwrap();
    let before = durable_bytes(dir.path());
    let (snapshot, resolved, origin) = main
        .snapshot_at_with_branch_limited(
            &child,
            &At::Commit(target.seq),
            &HistoryOptions::default(),
            32 << 20,
        )
        .expect("bounded history must serve an unopened retained parent");
    assert_eq!(origin, parent_id);
    assert_eq!(resolved.commit, target);
    assert_eq!(snapshot.commit, target.seq);
    assert_eq!(rows(snapshot), expected);
    assert_eq!(durable_bytes(dir.path()), before);
    assert!(matches!(
        main.snapshot_at_with_branch_limited(
            &child,
            &At::Commit(target.seq),
            &HistoryOptions::default(),
            32 << 10
        ),
        Err(Error::BudgetExceeded(_))
    ));
    assert_eq!(durable_bytes(dir.path()), before);
    // Ordinary origin-aware reads are an independent sufficient-cap oracle.
    let (legacy, legacy_resolved, legacy_origin) = main
        .snapshot_at_with_branch(&child, &At::Commit(target.seq), &HistoryOptions::default())
        .unwrap();
    assert_eq!(legacy_origin, origin);
    assert_eq!(legacy_resolved.commit, target);
    assert_eq!(rows(legacy), expected);
}

#[cfg(feature = "failpoints")]
#[test]
fn pruned_branch_time_cannot_substitute_its_parents_later_state() {
    use sparkles_core::history::CatalogHorizon;
    let dir = tempfile::tempdir().unwrap();
    let main = Store::open(dir.path(), StoreOptions::default()).unwrap();
    let t0 = main.head_commit().timestamp_ms + 100_000;
    main.set_clock(Arc::new(move || t0));
    write(&main, "INSERT DATA { <urn:base> <urn:p> 1 }");
    main.create_branch("parent", &Default::default()).unwrap();
    let parent = main.branch("parent").unwrap();
    parent.set_clock(Arc::new(move || t0 + 1_000));
    write(&parent, "INSERT DATA { <urn:parent> <urn:p> 2 }");
    main.create_branch(
        "child",
        &BranchOptions {
            from: "parent".into(),
            ..Default::default()
        },
    )
    .unwrap();
    let child = main.branch("child").unwrap();
    child.set_clock(Arc::new(move || t0 + 2_000));
    write(&child, "INSERT DATA { <urn:child-own> <urn:p> 3 }");
    let pruned = child.head_commit();
    child.set_clock(Arc::new(move || t0 + 3_000));
    write(&child, "INSERT DATA { <urn:child-later> <urn:p> 4 }");
    child.compact().unwrap();
    child
        .set_catalog_horizon(CatalogHorizon {
            keep_commits: Some(1),
            keep_age_ms: None,
        })
        .unwrap();
    assert!(
        child.history().first_commit > pruned.seq,
        "fixture did not prune the requested branch metadata"
    );
    for bounded in [false, true] {
        let result = if bounded {
            main.snapshot_at_with_branch_limited(
                &child,
                &At::Time(pruned.timestamp_ms),
                &HistoryOptions::default(),
                32 << 20,
            )
        } else {
            main.snapshot_at_with_branch(
                &child,
                &At::Time(pruned.timestamp_ms),
                &HistoryOptions::default(),
            )
        };
        let Err(Error::HistoryGone(detail)) = result else {
            panic!("a pruned branch-time selector substituted a different branch's state");
        };
        assert_eq!(detail.seq, None);
        assert_eq!(detail.at, Some(At::Time(pruned.timestamp_ms)));
        assert_eq!(detail.head, child.head_commit().seq);
        assert!(detail.metadata.is_none());
        assert_eq!(detail.reconstructable, child.history().reconstructable);
    }
}
