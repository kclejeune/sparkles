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
