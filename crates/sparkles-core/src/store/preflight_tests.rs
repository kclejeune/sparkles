use super::*;

fn files(root: &Path) -> BTreeMap<PathBuf, Vec<u8>> {
    let mut out = BTreeMap::new();
    for entry in std::fs::read_dir(root).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            out.extend(files(&path));
        } else if path.file_name().unwrap() != "sparkles.lock" {
            out.insert(path.clone(), std::fs::read(path).unwrap());
        }
    }
    out
}

fn tree(root: &Path) -> BTreeMap<PathBuf, Option<Vec<u8>>> {
    let mut out = BTreeMap::new();
    for entry in std::fs::read_dir(root).unwrap() {
        let entry = entry.unwrap();
        let path = entry.path();
        if entry.file_type().unwrap().is_dir() {
            out.insert(path.clone(), None);
            out.extend(tree(&path));
        } else {
            out.insert(path.clone(), Some(std::fs::read(path).unwrap()));
        }
    }
    out
}

fn refusal_before_writes(retained: bool) {
    for settings in [true, false] {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("db");
        let store = Store::open(&root, StoreOptions::default()).unwrap();
        crate::sparql::update::update(
            &store,
            "INSERT DATA { <urn:s> <urn:p> 1 }",
            &Default::default(),
        )
        .unwrap();
        let bad = store.create_branch("bad", &Default::default()).unwrap();
        store.create_branch("healthy", &Default::default()).unwrap();
        if retained {
            store
                .create_branch(
                    "child",
                    &crate::branch::BranchOptions {
                        from: "bad".into(),
                        ..Default::default()
                    },
                )
                .unwrap();
            store
                .delete_branch_with(
                    "bad",
                    &crate::branch::DeleteOptions {
                        force: true,
                        reparent: true,
                    },
                )
                .unwrap();
        }
        drop(store);
        let branch = root.join("branches").join(bad.id.to_string());
        assert!(branch.is_dir());
        let root_meta = root.join("dataset.json");
        let mut meta: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&root_meta).unwrap()).unwrap();
        meta["writtenBy"] = "older-release".into();
        std::fs::write(&root_meta, serde_json::to_vec(&meta).unwrap()).unwrap();
        let path = branch.join(if settings {
            "compaction.json"
        } else {
            "dataset.json"
        });
        let original = std::fs::read(&path).ok();
        if settings {
            std::fs::write(&path, br#"{"format":2}"#).unwrap();
        } else {
            let mut m: serde_json::Value = serde_json::from_slice(
                original
                    .as_deref()
                    .unwrap_or(&std::fs::read(&root_meta).unwrap()),
            )
            .unwrap();
            m["minimumReader"] = 99.into();
            std::fs::write(&path, serde_json::to_vec(&m).unwrap()).unwrap();
        }
        let staging = root
            .join("branches")
            .join(format!(".create-{}", uuid::Uuid::new_v4()));
        let deleting = root
            .join("branches")
            .join(format!("{}.deleting", uuid::Uuid::new_v4()));
        for scratch in [&staging, &deleting] {
            std::fs::create_dir(scratch).unwrap();
            std::fs::write(scratch.join("dataset.json"), b"").unwrap();
        }
        let planning_before = tree(&root);
        let plan = crate::migrate::plan(&root).unwrap();
        assert!(!plan.in_window());
        assert!(
            plan.refused
                .iter()
                .all(|r| !r.contains(".create-") && !r.contains(".deleting"))
        );
        assert_eq!(tree(&root), planning_before);

        let before = files(&root);
        let error = Store::open(&root, StoreOptions::default())
            .err()
            .expect("incompatible branch must refuse the root");
        assert!(matches!(error, Error::Unsupported(_)), "{error}");
        assert!(error.to_string().contains(&bad.id.to_string()), "{error}");
        assert_eq!(files(&root), before, "a refused open changed metadata");
        assert!(
            staging.is_dir() && deleting.is_dir(),
            "preflight cleaned scratch before a real refusal"
        );
        match original {
            Some(bytes) => std::fs::write(&path, bytes).unwrap(),
            None => std::fs::remove_file(&path).unwrap(),
        }
        let repaired = Store::open(&root, StoreOptions::default()).unwrap();
        assert!(!staging.exists() && !deleting.exists());
        assert_eq!(repaired.snapshot().len(), 1);
        assert_eq!(repaired.head_commit().seq, 1);
        assert_eq!(repaired.branch("healthy").unwrap().snapshot().len(), 1);
        if retained {
            assert!(repaired.branch("child").is_ok());
        }
    }
}

#[test]
fn active_branch_preflight_refuses_before_root_writes() {
    refusal_before_writes(false);
}

#[test]
fn retained_branch_preflight_refuses_before_root_writes() {
    refusal_before_writes(true);
}

#[test]
fn branch_informational_files_do_not_refuse_the_dataset() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("db");
    let store = Store::open(&root, StoreOptions::default()).unwrap();
    let info = store.create_branch("dev", &Default::default()).unwrap();
    drop(store);
    let branch = root.join("branches").join(info.id.to_string());
    std::fs::write(branch.join("origin.json"), br#"{"originFormat":99}"#).unwrap();
    std::fs::write(branch.join("restore.json"), br#"{"restoreFormat":99}"#).unwrap();
    assert!(crate::migrate::plan(&root).unwrap().in_window());
    let store = Store::open(&root, StoreOptions::default()).unwrap();
    assert!(store.branch("dev").is_ok());
}

#[test]
fn branch_enumeration_errors_refuse_before_metadata_changes() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("db");
    drop(Store::open(&root, StoreOptions::default()).unwrap());
    // A normal root has no branches directory until a branch is created.
    if root.join("branches").is_dir() {
        std::fs::remove_dir(root.join("branches")).unwrap();
    }
    std::fs::write(root.join("branches"), b"not a directory").unwrap();
    let before = files(&root);
    assert!(Store::open(&root, StoreOptions::default()).is_err());
    assert!(crate::migrate::plan(&root).is_err());
    assert_eq!(files(&root), before);
}

#[test]
fn unpublished_branch_directories_do_not_block_startup_recovery() {
    for suffix in ["staging", "deleting"] {
        for bytes in [b"".as_slice(), b"{", br#"{"format":99,"minimumReader":99}"#] {
            let dir = tempfile::tempdir().unwrap();
            let root = dir.path().join("db");
            let store = Store::open(&root, StoreOptions::default()).unwrap();
            crate::sparql::update::update(
                &store,
                "INSERT DATA { <urn:s> <urn:p> 1 }",
                &Default::default(),
            )
            .unwrap();
            drop(store);
            let id = uuid::Uuid::new_v4();
            let name = if suffix == "staging" {
                format!(".create-{id}")
            } else {
                format!("{id}.deleting")
            };
            let scratch = root.join("branches").join(name);
            std::fs::create_dir_all(&scratch).unwrap();
            std::fs::write(scratch.join("dataset.json"), bytes).unwrap();
            std::fs::write(scratch.join("compaction.json"), br#"{"format":99}"#).unwrap();
            let before = tree(&root);
            let plan = crate::migrate::plan(&root).unwrap();
            assert!(
                plan.steps
                    .iter()
                    .all(|s| !s.file.contains(".create-") && !s.file.contains(".deleting"))
            );
            assert_eq!(tree(&root), before, "planning changed unpublished scratch");
            assert!(scratch.exists());
            let opened = Store::open(&root, StoreOptions::default()).unwrap();
            assert!(
                plan.in_window(),
                "unpublished scratch must not refuse migration planning: {:?}",
                plan.refused
            );
            assert_eq!(opened.snapshot().len(), 1);
            assert_eq!(opened.head_commit().seq, 1);
            assert!(
                !scratch.exists(),
                "startup did not recover unpublished branch scratch"
            );
        }
    }
}

#[test]
fn an_unrecognized_hidden_directory_is_not_treated_as_branch_scratch() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("db");
    drop(Store::open(&root, StoreOptions::default()).unwrap());
    let saved = root.join("branches/.saved");
    std::fs::create_dir_all(&saved).unwrap();
    let mut metadata: serde_json::Value =
        serde_json::from_slice(&std::fs::read(root.join("dataset.json")).unwrap()).unwrap();
    metadata["minimumReader"] = 99.into();
    std::fs::write(
        saved.join("dataset.json"),
        serde_json::to_vec(&metadata).unwrap(),
    )
    .unwrap();
    let before = tree(&root);
    let error = Store::open(&root, StoreOptions::default())
        .err()
        .expect("unrecognized hidden directories must still be checked");
    assert!(matches!(error, Error::Unsupported(_)), "{error}");
    assert!(!crate::migrate::plan(&root).unwrap().in_window());
    assert_eq!(tree(&root), before);
}

mod algo_catalog_admission {
    use super::*;

    #[derive(Clone, Debug, PartialEq, Eq, serde::Serialize)]
    struct Entry {
        kind: &'static str,
        mode: u32,
        bytes: Vec<u8>,
    }

    fn inventory(root: &Path) -> BTreeMap<PathBuf, Entry> {
        fn visit(root: &Path, path: &Path, out: &mut BTreeMap<PathBuf, Entry>) {
            let meta = std::fs::symlink_metadata(path).unwrap();
            #[cfg(unix)]
            let mode = {
                use std::os::unix::fs::PermissionsExt;
                meta.permissions().mode() & 0o7777
            };
            #[cfg(not(unix))]
            let mode = u32::from(meta.permissions().readonly());
            let (kind, bytes) = if meta.is_dir() {
                ("directory", Vec::new())
            } else if meta.file_type().is_symlink() {
                (
                    "symlink",
                    std::fs::read_link(path)
                        .unwrap()
                        .to_string_lossy()
                        .as_bytes()
                        .to_vec(),
                )
            } else {
                ("file", std::fs::read(path).unwrap())
            };
            out.insert(
                path.strip_prefix(root).unwrap().to_path_buf(),
                Entry { kind, mode, bytes },
            );
            if meta.is_dir() {
                for entry in std::fs::read_dir(path).unwrap() {
                    visit(root, &entry.unwrap().path(), out);
                }
            }
        }
        let mut out = BTreeMap::new();
        visit(root, root, &mut out);
        out
    }

    fn record(
        label: &str,
        before: &BTreeMap<PathBuf, Entry>,
        after: &BTreeMap<PathBuf, Entry>,
        result: &str,
    ) {
        if let Some(root) = std::env::var_os("SPARKLES_F3_EVIDENCE_DIR") {
            let root = PathBuf::from(root);
            std::fs::create_dir_all(&root).unwrap();
            let path = root.join(format!("{label}.json"));
            assert!(!path.exists(), "never overwrite earlier observations");
            std::fs::write(path, serde_json::to_vec_pretty(&serde_json::json!({
                "label": label, "before": before, "after": after, "result": result,
                "openException": "Only the root sparkles.lock holder bytes may change; type/mode/path remain exact. Planning has no exception.",
            })).unwrap()).unwrap();
        }
    }

    fn fixture(location: &str, payload: &[u8]) -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("db");
        let store = Store::open(&root, StoreOptions::default()).unwrap();
        crate::sparql::update::update(
            &store,
            "INSERT DATA { <urn:s> <urn:p> 1 }",
            &Default::default(),
        )
        .unwrap();
        store.create_branch("healthy", &Default::default()).unwrap();
        let target = if location == "root" {
            root.clone()
        } else {
            let info = store.create_branch("catalog", &Default::default()).unwrap();
            if location == "retained" {
                store
                    .create_branch(
                        "child",
                        &crate::branch::BranchOptions {
                            from: "catalog".into(),
                            ..Default::default()
                        },
                    )
                    .unwrap();
                store
                    .delete_branch_with(
                        "catalog",
                        &crate::branch::DeleteOptions {
                            force: true,
                            reparent: true,
                        },
                    )
                    .unwrap();
            }
            root.join("branches").join(info.id.to_string())
        };
        drop(store);
        let path = root.join("dataset.json");
        let mut meta: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        meta["writtenBy"] = "older-format-fixture-release".into();
        std::fs::write(path, serde_json::to_vec(&meta).unwrap()).unwrap();
        std::fs::write(target.join(crate::algo::catalog::FILE), payload).unwrap();
        for name in [
            format!(".create-{}", uuid::Uuid::new_v4()),
            format!("{}.deleting", uuid::Uuid::new_v4()),
        ] {
            let p = root.join("branches").join(name);
            std::fs::create_dir(&p).unwrap();
            std::fs::write(p.join("dataset.json"), b"{").unwrap();
        }
        (dir, root)
    }

    fn open_refuses(location: &str, payload: &[u8], label: &str, future: bool) -> bool {
        let (_dir, root) = fixture(location, payload);
        let before = inventory(&root);
        let result = Store::open(&root, StoreOptions::default());
        let description = result
            .as_ref()
            .err()
            .map(ToString::to_string)
            .unwrap_or_else(|| "OPENED (unexpected)".into());
        let expected = if future {
            matches!(&result, Err(Error::Unsupported(_)))
        } else {
            matches!(&result, Err(Error::Corrupt(_)))
        };
        drop(result);
        let after = inventory(&root);
        record(label, &before, &after, &description);
        if expected {
            let lock = PathBuf::from("sparkles.lock");
            assert_eq!(
                before.keys().collect::<Vec<_>>(),
                after.keys().collect::<Vec<_>>()
            );
            let mut normalized = after.clone();
            let old = before.get(&lock).unwrap();
            let current = normalized.get_mut(&lock).unwrap();
            assert_eq!((current.kind, current.mode), (old.kind, old.mode));
            current.bytes.clone_from(&old.bytes);
            assert_eq!(
                before, normalized,
                "a refused catalog changed entries beyond holder text"
            );
            assert!(description.contains("algo.json"), "{description}");
        }
        expected
    }

    #[test]
    fn algo_catalog_root_future_refuses_before_writers() {
        assert!(open_refuses(
            "root",
            br#"{"format":3,"projections":{}}"#,
            "root-future",
            true
        ));
    }
    #[test]
    fn algo_catalog_active_future_refuses_before_writers() {
        assert!(open_refuses(
            "active",
            br#"{"format":3,"projections":{}}"#,
            "active-future",
            true
        ));
    }
    #[test]
    fn algo_catalog_retained_future_refuses_before_writers() {
        assert!(open_refuses(
            "retained",
            br#"{"format":3,"projections":{}}"#,
            "retained-future",
            true
        ));
    }
    #[test]
    fn algo_catalog_unknown_future_shape_refuses_before_writers() {
        assert!(open_refuses(
            "root",
            br#"{"format":3,"futureSchema":["not", "a", "FileDoc"]}"#,
            "unknown-future-shape",
            true
        ));
    }
    #[test]
    fn algo_catalog_invalid_versions_refuse_before_writers() {
        let mut failures = Vec::new();
        for (name, payload) in [
            ("zero", br#"{"format":0,"projections":{}}"#.as_slice()),
            ("nonnumeric", br#"{"format":"next","projections":{}}"#),
            ("overflow", br#"{"format":4294967296,"projections":{}}"#),
        ] {
            if !open_refuses("root", payload, name, false) {
                failures.push(name);
            }
        }
        assert!(
            failures.is_empty(),
            "invalid versions were admitted: {failures:?}"
        );
    }
    #[test]
    fn algo_catalog_migration_plans_are_complete_and_read_only() {
        let mut admitted = Vec::new();
        for location in ["root", "active", "retained"] {
            let (_dir, root) = fixture(location, br#"{"format":3,"futureShape":true}"#);
            let before = inventory(&root);
            let plan = crate::migrate::plan(&root).unwrap();
            let after = inventory(&root);
            record(
                &format!("plan-{location}"),
                &before,
                &after,
                &format!("in_window={} refused={:?}", plan.in_window(), plan.refused),
            );
            assert_eq!(before, after, "planning has no lock-holder exception");
            if plan.in_window() || !plan.refused.iter().any(|m| m.contains("algo.json")) {
                admitted.push(location);
            }
        }
        assert!(
            admitted.is_empty(),
            "future catalogs omitted from plans: {admitted:?}"
        );
    }
}
