use super::*;
use crate::store::{Store, StoreOptions};

fn cross_version_cleanup(at: &str) {
    for keep in [false, true] {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("db");
        super::tests::legacy_database(&root);
        assert!(
            migrate_with(&root, keep, &mut |stage| {
                if stage == at {
                    Err(Error::Cancelled)
                } else {
                    Ok(())
                }
            })
            .is_err()
        );
        let original = dir.path().join(".pre-migrate-0.0.9-db");
        std::fs::rename(old_dir(&root), &original).unwrap();
        let unrelated = old_dir(&root);
        std::fs::create_dir(&unrelated).unwrap();
        std::fs::write(unrelated.join("sentinel"), b"unrelated kept copy").unwrap();
        let marked = if at == "swapped" {
            root.clone()
        } else {
            work_dir(&root)
        };
        let marker = serde_json::json!({
            "format": 1,
            "mode": if keep { "keep-old" } else { "remove-old" },
            "oldDirectory": original.file_name().unwrap().to_str().unwrap(),
        });
        std::fs::write(
            marked.join(CHECKED_MARK),
            serde_json::to_vec(&marker).unwrap(),
        )
        .unwrap();
        recover(&root).unwrap();
        assert_eq!(
            std::fs::read(unrelated.join("sentinel")).unwrap(),
            b"unrelated kept copy"
        );
        assert_eq!(original.exists(), keep);
        assert!(!work_dir(&root).exists());
        assert!(!root.join(CHECKED_MARK).exists());
        let store = Store::open(&root, StoreOptions::default()).unwrap();
        assert_eq!(store.snapshot().len(), 1);
        assert_eq!(store.head_commit().seq, 1);
    }
}

#[test]
fn moved_aside_recovery_preserves_an_unrelated_versions_kept_copy() {
    cross_version_cleanup("moved-aside");
}

#[test]
fn swapped_recovery_preserves_an_unrelated_versions_kept_copy() {
    cross_version_cleanup("swapped");
}

/// Include directory entries as well as bytes, so refusal cannot create a lock file.
fn tree(dir: &Path) -> std::collections::BTreeMap<PathBuf, Option<Vec<u8>>> {
    fn visit(
        base: &Path,
        dir: &Path,
        out: &mut std::collections::BTreeMap<PathBuf, Option<Vec<u8>>>,
    ) {
        for entry in std::fs::read_dir(dir).unwrap() {
            let entry = entry.unwrap();
            let path = entry.path();
            let relative = path.strip_prefix(base).unwrap().to_path_buf();
            if entry.file_type().unwrap().is_dir() {
                out.insert(relative, None);
                visit(base, &path, out);
            } else {
                out.insert(relative, Some(std::fs::read(path).unwrap()));
            }
        }
    }
    let mut out = std::collections::BTreeMap::new();
    visit(dir, dir, &mut out);
    out
}

#[test]
fn invalid_checked_state_preserves_every_directory_and_byte() {
    let valid = serde_json::json!({"format": 1, "mode": "remove-old", "oldDirectory": ".pre-migrate-0.0.9-db"});
    let mut invalid = vec![
        b"{".to_vec(),
        b"keep-old".to_vec(),
        b"remove-old\n".to_vec(),
    ];
    for value in [
        serde_json::json!({"format": 2}),
        serde_json::json!({"mode": "remove-old", "oldDirectory": ".pre-migrate-0.0.9-db"}),
        serde_json::json!({"format": 1, "mode": "unknown", "oldDirectory": ".pre-migrate-0.0.9-db"}),
        serde_json::json!({"format": 1, "mode": "remove-old"}),
    ] {
        invalid.push(serde_json::to_vec(&value).unwrap());
    }
    for name in [
        "../other",
        "/tmp/other",
        "C:\\other",
        ".",
        "..",
        "db",
        ".migrate-db",
        ".migrate.lock-db",
        ".pre-migrate-0.0.9-other",
        ".pre-migrate--db",
        ".pre-migrate-0.0.9-../db",
    ] {
        let mut value = valid.clone();
        value["oldDirectory"] = name.into();
        invalid.push(serde_json::to_vec(&value).unwrap());
    }
    for at in ["checked", "moved-aside", "swapped"] {
        for bytes in &invalid {
            let dir = tempfile::tempdir().unwrap();
            let root = dir.path().join("db");
            super::tests::legacy_database(&root);
            assert!(
                migrate_with(&root, false, &mut |stage| {
                    if stage == at {
                        Err(Error::Cancelled)
                    } else {
                        Ok(())
                    }
                })
                .is_err()
            );
            let marked = if at == "swapped" {
                root.clone()
            } else {
                work_dir(&root)
            };
            std::fs::write(marked.join(CHECKED_MARK), bytes).unwrap();
            let before = tree(dir.path());
            let e = recover(&root).unwrap_err();
            assert!(
                matches!(e, Error::Corrupt(_) | Error::Unsupported(_)),
                "{e}"
            );
            assert_eq!(
                tree(dir.path()),
                before,
                "recover at {at}: {}",
                String::from_utf8_lossy(bytes)
            );
            assert!(migrate(&root, false).is_err());
            assert_eq!(
                tree(dir.path()),
                before,
                "migrate at {at}: {}",
                String::from_utf8_lossy(bytes)
            );
        }
    }
}

#[test]
fn cleanup_retries_after_the_recorded_old_copy_is_already_gone() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("db");
    super::tests::legacy_database(&root);
    assert!(
        migrate_with(&root, false, &mut |stage| {
            if stage == "swapped" {
                Err(Error::Cancelled)
            } else {
                Ok(())
            }
        })
        .is_err()
    );
    let mark = read_mark(&root, &root).unwrap().unwrap();
    std::fs::remove_dir_all(mark.old_path(&root).unwrap()).unwrap();
    recover(&root).unwrap();
    assert!(!root.join(CHECKED_MARK).exists());
    assert!(recover(&root).is_ok());
    assert_eq!(
        Store::open(&root, StoreOptions::default())
            .unwrap()
            .snapshot()
            .len(),
        1
    );
}

#[test]
fn checked_state_format_is_registered_and_checks_version_before_fields() {
    assert_eq!(formats::kind("migration-state").unwrap().writes, 1);
    assert_eq!(formats::MIGRATION_STATE.reads, &[1]);
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("db");
    std::fs::create_dir(&root).unwrap();
    std::fs::write(root.join(CHECKED_MARK), br#"{"format":99,"mode":false}"#).unwrap();
    assert!(matches!(
        read_mark(&root, &root),
        Err(Error::Unsupported(_))
    ));
}
