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

struct ColdFamily {
    dir: tempfile::TempDir,
    parent_id: uuid::Uuid,
    target: sparkles_core::commit::CommitInfo,
    expected: Vec<Vec<Option<oxrdf::Term>>>,
}
impl ColdFamily {
    fn new(compact: bool) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let main = Store::open(dir.path(), Default::default()).unwrap();
        #[cfg(feature = "failpoints")]
        let t0 = main.head_commit().timestamp_ms + 100_000;
        #[cfg(feature = "failpoints")]
        main.set_clock(Arc::new(move || t0));
        write(&main, "INSERT DATA { <urn:base> <urn:p> 1 }");
        main.create_branch("parent", &Default::default()).unwrap();
        let parent = main.branch("parent").unwrap();
        #[cfg(feature = "failpoints")]
        parent.set_clock(Arc::new(move || t0 + 1_000));
        write(
            &parent,
            "DELETE { <urn:base> <urn:p> 1 } INSERT { <urn:parent> <urn:p> _:b; <urn:q> 2 } WHERE {}",
        );
        let target = parent.head_commit();
        let expected = rows(parent.snapshot());
        #[cfg(feature = "failpoints")]
        parent.set_clock(Arc::new(move || t0 + 2_000));
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
        if compact {
            parent.compact().unwrap();
            write(&parent, "INSERT DATA { <urn:after-compaction> <urn:p> 5 }");
        }
        child.flush_change_log().unwrap();
        parent.flush_change_log().unwrap();
        main.flush_change_log().unwrap();
        Self {
            dir,
            parent_id: parent.branch_id(),
            target,
            expected,
        }
    }
    fn parent_root(&self) -> PathBuf {
        self.dir
            .path()
            .join("branches")
            .join(self.parent_id.to_string())
    }
    fn current(&self) -> PathBuf {
        let root = self.parent_root();
        root.join(
            std::fs::read_to_string(root.join("CURRENT"))
                .unwrap()
                .trim(),
        )
    }
    fn read(&self) -> (Store, Arc<Store>) {
        let main = Store::open(self.dir.path(), Default::default()).unwrap();
        let child = main.branch("child").unwrap().shared().unwrap();
        child.flush_change_log().unwrap();
        main.flush_change_log().unwrap();
        (main, child)
    }
    fn assert_exact(&self, main: &Store, child: &Store) {
        let before = durable_bytes(self.dir.path());
        let selectors = vec![At::Commit(self.target.seq)];
        #[cfg(feature = "failpoints")]
        let selectors = {
            let mut selectors = selectors;
            selectors.push(At::Time(self.target.timestamp_ms));
            selectors
        };
        for selector in selectors {
            let (snapshot, resolved, origin) = main
                .snapshot_at_with_branch_limited(child, &selector, &Default::default(), 32 << 20)
                .unwrap();
            assert_eq!(origin, self.parent_id);
            assert_eq!(resolved.commit, self.target);
            assert_eq!(rows(snapshot), self.expected);
            assert_eq!(durable_bytes(self.dir.path()), before);
        }
        let (snapshot, resolved, origin) = main
            .snapshot_at_with_branch(child, &At::Commit(self.target.seq), &Default::default())
            .unwrap();
        assert_eq!(origin, self.parent_id);
        assert_eq!(resolved.commit, self.target);
        assert_eq!(rows(snapshot), self.expected);
    }
}

#[test]
fn cold_catalog_reconciles_only_supported_tail_and_exact_selected_counts() {
    for mode in ["lag", "missing", "torn", "preallocated"] {
        let family = ColdFamily::new(false);
        let catalog = family.parent_root().join("commits.bin");
        let mut bytes = std::fs::read(&catalog).unwrap();
        match mode {
            // Keep the valid base record and lose the following complete records.
            "lag" => bytes.truncate(128),
            "missing" => {
                std::fs::remove_file(&catalog).unwrap();
            }
            "torn" => bytes.truncate(128 + 17),
            "preallocated" => {
                bytes.truncate(128);
                bytes.resize(64 << 10, 0);
            }
            _ => unreachable!(),
        }
        if mode != "missing" {
            std::fs::write(&catalog, bytes).unwrap();
        }
        let (main, child) = family.read();
        family.assert_exact(&main, &child);
    }
}

#[test]
fn cold_compaction_uses_covering_generation_and_preserves_original_metadata() {
    let family = ColdFamily::new(true);
    let (main, child) = family.read();
    family.assert_exact(&main, &child);
}

#[test]
fn cold_wal_torn_and_preallocated_suffixes_preserve_the_proven_prefix() {
    for tail in [vec![0; 128 << 10], vec![1, 2, 3, 4, 5]] {
        let family = ColdFamily::new(false);
        let wal = family.current().join("wal.log");
        let mut bytes = std::fs::read(&wal).unwrap();
        bytes.extend(tail);
        std::fs::write(&wal, bytes).unwrap();
        let (main, child) = family.read();
        family.assert_exact(&main, &child);
    }
}

#[test]
fn cold_origin_refuses_foreign_future_and_missing_durable_metadata_without_writes() {
    for mode in [
        "foreign-catalog",
        "future-catalog",
        "future-dataset",
        "future-generation",
        "strict-wal",
    ] {
        let family = ColdFamily::new(false);
        // Open only the supported child first. Ordinary family opening itself
        // rejects a future dataset version before any bounded request can run.
        let (main, child) = family.read();
        let root = family.parent_root();
        match mode {
            "future-catalog" => {
                let path = root.join("commits.bin");
                let mut bytes = std::fs::read(&path).unwrap();
                bytes[8..12].copy_from_slice(&99u32.to_le_bytes());
                std::fs::write(path, bytes).unwrap();
            }
            "foreign-catalog" => {
                let path = root.join("commits.bin");
                let mut bytes = std::fs::read(&path).unwrap();
                bytes[16..32].copy_from_slice(uuid::Uuid::new_v4().as_bytes());
                let mut crc = flate2::Crc::new();
                crc.update(&bytes[..60]);
                bytes[60..64].copy_from_slice(&crc.sum().to_le_bytes());
                std::fs::write(path, bytes).unwrap();
            }
            "future-dataset" => {
                let path = root.join("dataset.json");
                let mut file: serde_json::Value =
                    serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
                file["minimumReader"] = 99.into();
                std::fs::write(path, serde_json::to_vec(&file).unwrap()).unwrap();
            }
            "future-generation" => {
                let path = family.current().join("commit.json");
                let mut file: serde_json::Value =
                    serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
                file["format"] = 99.into();
                // A newer format may change the shape of its supported payload.
                file["commit"] = false.into();
                std::fs::write(path, serde_json::to_vec(&file).unwrap()).unwrap();
            }
            "strict-wal" => {
                let current = family.current();
                let path = current.join("commit.json");
                let mut file: serde_json::Value =
                    serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
                file["walStrictFrom"] = 0.into();
                std::fs::write(path, serde_json::to_vec(&file).unwrap()).unwrap();
                std::fs::remove_file(current.join("wal.log")).unwrap();
            }
            _ => unreachable!(),
        }
        let before = durable_bytes(family.dir.path());
        let result = main.snapshot_at_with_branch_limited(
            &child,
            &At::Commit(family.target.seq),
            &Default::default(),
            32 << 20,
        );
        let error = result
            .err()
            .unwrap_or_else(|| panic!("{mode} was accepted"));
        match mode {
            "future-catalog" | "future-dataset" | "future-generation" => {
                assert!(matches!(&error, Error::Unsupported(_)), "{mode}: {error}");
            }
            _ => assert!(matches!(&error, Error::Corrupt(_)), "{mode}: {error}"),
        }
        assert_eq!(durable_bytes(family.dir.path()), before, "{mode}");
    }
}

#[test]
fn cold_historical_providers_match_ordinary_configuration_without_rebuilding() {
    let family = ColdFamily::new(false);
    let root = family.parent_root();
    let cfg = sparkles_core::geo::GeoConfig {
        query_rewrite: true,
        distance: sparkles_core::geo::DistanceModel::Haversine,
        ..Default::default()
    };
    std::fs::write(root.join("geo.json"), serde_json::to_vec(&cfg).unwrap()).unwrap();
    // Past snapshots omit the configured live full-text provider.
    std::fs::write(
        root.join("text.json"),
        serde_json::to_vec(&sparkles_core::text::TextConfig::default()).unwrap(),
    )
    .unwrap();
    let (main, child) = family.read();
    let before = durable_bytes(family.dir.path());
    let (snapshot, _, _) = main
        .snapshot_at_with_branch_limited(
            &child,
            &At::Commit(family.target.seq),
            &Default::default(),
            32 << 20,
        )
        .unwrap();
    assert!(snapshot.text.is_none());
    #[cfg(feature = "geo")]
    {
        assert_eq!(*snapshot.geo.as_ref().unwrap().config, cfg);
        assert_eq!(
            snapshot.geo.as_ref().unwrap().state(),
            sparkles_core::geo::IndexState::Historical
        );
    }
    #[cfg(not(feature = "geo"))]
    assert!(snapshot.geo.is_none());
    assert_eq!(durable_bytes(family.dir.path()), before);
    let (legacy, _, _) = main
        .snapshot_at_with_branch(&child, &At::Commit(family.target.seq), &Default::default())
        .unwrap();
    assert!(legacy.text.is_none());
    assert_eq!(
        legacy.geo.as_ref().map(|g| &*g.config),
        snapshot.geo.as_ref().map(|g| &*g.config)
    );
    assert_eq!(rows(legacy), rows(snapshot));
}

#[cfg(feature = "failpoints")]
#[test]
fn equal_and_backward_clamped_child_times_are_not_parent_origins_after_pruning() {
    use sparkles_core::history::CatalogHorizon;
    for backward in [false, true] {
        let dir = tempfile::tempdir().unwrap();
        let main = Store::open(dir.path(), Default::default()).unwrap();
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
        let now = if backward { t0 - 1_000 } else { t0 + 1_000 };
        child.set_clock(Arc::new(move || now));
        write(&child, "INSERT DATA { <urn:equal-time> <urn:p> 3 }");
        let equal = child.head_commit();
        assert_eq!(
            equal.timestamp_ms,
            t0 + 1_000,
            "fixture must clamp to the fork clock"
        );
        let (_, resolved, origin) = main
            .snapshot_at_with_branch_limited(
                &child,
                &At::Time(equal.timestamp_ms),
                &Default::default(),
                32 << 20,
            )
            .unwrap();
        assert_eq!(resolved.commit, equal);
        assert_eq!(origin, child.branch_id());
        child.set_clock(Arc::new(move || t0 + 2_000));
        write(&child, "INSERT DATA { <urn:later> <urn:p> 4 }");
        child.compact().unwrap();
        child
            .set_catalog_horizon(CatalogHorizon {
                keep_commits: Some(1),
                keep_age_ms: None,
            })
            .unwrap();
        assert!(child.history().first_commit > equal.seq);
        drop(child);
        drop(parent);
        drop(main);
        let main = Store::open(dir.path(), Default::default()).unwrap();
        let child = main.branch("child").unwrap();
        child.flush_change_log().unwrap();
        main.flush_change_log().unwrap();
        let before = durable_bytes(dir.path());
        let at = At::Time(equal.timestamp_ms);
        for result in [
            main.snapshot_at_with_branch_limited(&child, &at, &Default::default(), 32 << 20)
                .map(|_| ()),
            main.snapshot_at_with_branch(&child, &at, &Default::default())
                .map(|_| ()),
            main.branch_resolve("child", &at).map(|_| ()),
        ] {
            let Err(Error::HistoryGone(gone)) = result else {
                panic!("equal fork time inherited parent state");
            };
            assert_eq!(gone.seq, None);
            assert_eq!(gone.at, Some(at.clone()));
            assert!(gone.metadata.is_none());
        }
        // Before the fork remains a legitimate inherited time, including a cold
        // metadata-only fork-clock lookup before any parent Store is opened.
        let (snapshot, resolved, _) = main
            .snapshot_at_with_branch_limited(&child, &At::Time(t0), &Default::default(), 32 << 20)
            .unwrap();
        assert_eq!(resolved.commit.timestamp_ms, t0);
        assert_eq!(rows(snapshot).len(), 1);
        assert_eq!(durable_bytes(dir.path()), before);
    }
}

#[test]
fn cold_nested_origins_follow_renamed_retired_parent_by_identity() {
    let family = ColdFamily::new(false);
    let main = Store::open(family.dir.path(), Default::default()).unwrap();
    main.create_branch(
        "grandchild",
        &BranchOptions {
            from: "child".into(),
            ..Default::default()
        },
    )
    .unwrap();
    let grandchild = main.branch("grandchild").unwrap();
    write(&grandchild, "INSERT DATA { <urn:grandchild> <urn:p> 5 }");
    main.rename_branch("parent", "renamed").unwrap();
    main.delete_branch_with(
        "renamed",
        &sparkles_core::branch::DeleteOptions {
            force: true,
            reparent: true,
        },
    )
    .unwrap();
    grandchild.flush_change_log().unwrap();
    main.flush_change_log().unwrap();
    drop(grandchild);
    drop(main);
    let main = Store::open(family.dir.path(), Default::default()).unwrap();
    let grandchild = main.branch("grandchild").unwrap();
    grandchild.flush_change_log().unwrap();
    main.flush_change_log().unwrap();
    family.assert_exact(&main, &grandchild);
}

#[cfg(feature = "geo")]
#[path = "support/caller_allocations.rs"]
mod caller_allocations;

#[cfg(feature = "geo")]
#[test]
fn cold_provider_low_cap_refuses_before_allocating_unbounded_config() {
    let family = ColdFamily::new(false);
    let (main, child) = family.read();
    let cap = 2 << 20;
    main.snapshot_at_with_branch_limited(
        &child,
        &At::Commit(family.target.seq),
        &Default::default(),
        cap,
    )
    .unwrap();
    let root = family.parent_root();
    let config = serde_json::json!({"featureLinks": [format!("urn:{}", "x".repeat(6 << 20))]});
    std::fs::write(root.join("geo.json"), serde_json::to_vec(&config).unwrap()).unwrap();
    let before = durable_bytes(family.dir.path());
    let (result, counts) = caller_allocations::measure(|| {
        main.snapshot_at_with_branch_limited(
            &child,
            &At::Commit(family.target.seq),
            &Default::default(),
            cap,
        )
    });
    assert!(matches!(result, Err(Error::BudgetExceeded(_))));
    assert!(
        counts.largest <= cap as usize,
        "allocation {} exceeded cap {cap}",
        counts.largest
    );
    assert!(
        counts.peak <= cap as usize,
        "caller peak {} exceeded cap {cap}",
        counts.peak
    );
    assert_eq!(durable_bytes(family.dir.path()), before);
}

#[test]
fn cold_pruned_or_gapped_catalog_does_not_resurrect_surviving_generation_metadata() {
    for gap in [false, true] {
        let family = ColdFamily::new(true);
        let catalog = family.parent_root().join("commits.bin");
        let original = std::fs::read(&catalog).unwrap();
        let bytes = if gap {
            // The valid prefix ends before CURRENT's compacted base with a gap.
            original[..128].to_vec()
        } else {
            let mut header = original[..64].to_vec();
            let first = family.target.seq + 1;
            header[32..40].copy_from_slice(&first.to_le_bytes());
            let mut crc = flate2::Crc::new();
            crc.update(&header[..60]);
            header[60..64].copy_from_slice(&crc.sum().to_le_bytes());
            for record in original[64..].as_chunks::<64>().0 {
                let seq = u64::from_le_bytes(record[..8].try_into().unwrap());
                if seq >= first {
                    header.extend_from_slice(record);
                }
            }
            header
        };
        std::fs::write(&catalog, bytes).unwrap();
        let (main, child) = family.read();
        let before = durable_bytes(family.dir.path());
        let result = main.snapshot_at_with_branch_limited(
            &child,
            &At::Commit(family.target.seq),
            &Default::default(),
            32 << 20,
        );
        let Err(Error::HistoryGone(detail)) = result else {
            panic!("surviving physical bytes resurrected pruned catalog metadata");
        };
        assert_eq!(detail.seq, Some(family.target.seq));
        assert!(detail.metadata.is_none());
        assert_eq!(durable_bytes(family.dir.path()), before);
        assert!(matches!(
            main.snapshot_at_with_branch(
                &child,
                &At::Commit(family.target.seq),
                &Default::default()
            ),
            Err(Error::HistoryGone(_))
        ));
    }
}

#[test]
fn cold_lost_generation_returns_known_gone_metadata_without_substitution() {
    let family = ColdFamily::new(true);
    let (main, child) = family.read();
    let old = family
        .parent_root()
        .join(format!("gen-{:04}", family.target.generation));
    assert_ne!(old, family.current());
    assert!(old.is_dir());
    std::fs::remove_dir_all(old).unwrap();
    let before = durable_bytes(family.dir.path());
    let result = main.snapshot_at_with_branch_limited(
        &child,
        &At::Commit(family.target.seq),
        &Default::default(),
        32 << 20,
    );
    let Err(Error::HistoryGone(detail)) = result else {
        panic!("missing backing generation did not refuse the exact requested state");
    };
    assert_eq!(detail.seq, Some(family.target.seq));
    assert_eq!(detail.metadata, Some(family.target));
    assert_eq!(durable_bytes(family.dir.path()), before);
}
