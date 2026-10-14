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
        let plan = crate::migrate::plan(&root).unwrap();
        assert!(!plan.in_window());
        let before = files(&root);
        let error = Store::open(&root, StoreOptions::default())
            .err()
            .expect("incompatible branch must refuse the root");
        assert!(matches!(error, Error::Unsupported(_)), "{error}");
        assert!(error.to_string().contains(&bad.id.to_string()), "{error}");
        assert_eq!(files(&root), before, "a refused open changed metadata");
        match original {
            Some(bytes) => std::fs::write(&path, bytes).unwrap(),
            None => std::fs::remove_file(&path).unwrap(),
        }
        let repaired = Store::open(&root, StoreOptions::default()).unwrap();
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
