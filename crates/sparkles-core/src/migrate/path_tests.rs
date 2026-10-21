use super::*;
use crate::store::{Store, StoreOptions};

#[test]
fn migration_guard_unlocks_while_an_inherited_descriptor_lives() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("db");
    let guard = lock_migration(&root).unwrap();
    let inherited = guard.try_clone().unwrap();
    drop(guard);
    let next = lock_migration(&root).unwrap();
    drop(inherited);
    assert!(matches!(lock_migration(&root), Err(Error::Locked { .. })));
    drop(next);
    assert!(lock_migration(&root).is_ok());
}

fn assert_data(root: &Path) {
    let store = Store::open(root, StoreOptions::default()).unwrap();
    assert_eq!(store.snapshot().len(), 1);
    assert_eq!(store.head_commit().seq, 1);
}

#[test]
fn parent_component_migration_uses_a_stable_root() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("db");
    super::tests::legacy_database(&root);
    let alias = root.join("../db");
    let outcome = migrate(&alias, false).unwrap();
    assert_eq!(outcome.root, root);
    assert!(pending(&root).unwrap().is_none());
    assert!(!old_dir(&root).exists());
    assert_data(&root);
}

#[test]
fn an_unresolvable_interrupted_alias_refuses_without_changing_any_copy() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("db");
    super::tests::legacy_database(&root);
    let alias = root.join("../db");
    let stable = plan(&alias).unwrap().root;
    assert_eq!(stable, root);
    assert!(matches!(
        migrate_with(&alias, false, &mut |stage| {
            if stage == "moved-aside" {
                Err(Error::Cancelled)
            } else {
                Ok(())
            }
        }),
        Err(Error::Cancelled)
    ));
    assert!(!root.exists());
    assert!(pending(&stable).unwrap().is_some());
    let before = super::recovery_tests::tree(dir.path());
    for error in [
        pending(&alias).unwrap_err(),
        recover(&alias).unwrap_err(),
        migrate(&alias, false).unwrap_err(),
        Store::open(&alias, StoreOptions::default()).err().unwrap(),
    ] {
        assert!(matches!(error, Error::Invalid(_)), "{error}");
        assert!(
            error.to_string().contains("stable existing parent"),
            "{error}"
        );
        assert_eq!(super::recovery_tests::tree(dir.path()), before);
    }
    recover(&stable).unwrap();
    assert!(pending(&stable).unwrap().is_none());
    assert_data(&stable);
}

#[test]
fn a_missing_root_recovers_through_a_parent_that_still_exists() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir(dir.path().join("sub")).unwrap();
    let root = dir.path().join("db");
    super::tests::legacy_database(&root);
    assert!(matches!(
        migrate_with(&root, false, &mut |stage| {
            if stage == "moved-aside" {
                Err(Error::Cancelled)
            } else {
                Ok(())
            }
        }),
        Err(Error::Cancelled)
    ));
    let alias = dir.path().join("sub/../db");
    assert_eq!(absolute(&alias).unwrap(), root);
    assert_eq!(pending(&alias).unwrap(), Some(work_dir(&root)));
    recover(&alias).unwrap();
    assert!(pending(&alias).unwrap().is_none());
    assert_data(&root);
}

#[cfg(unix)]
#[test]
fn parent_symlinks_resolve_physically_before_dotdot() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("physical/child")).unwrap();
    std::os::unix::fs::symlink(dir.path().join("physical/child"), dir.path().join("alias"))
        .unwrap();
    let physical = dir.path().join("physical/db");
    super::tests::legacy_database(&physical);
    let lexical = dir.path().join("db");
    drop(Store::open(&lexical, StoreOptions::default()).unwrap());
    let before = super::recovery_tests::tree(&lexical);
    let input = dir.path().join("alias/../db");
    let migrated = migrate(&input, false).unwrap();
    assert_eq!(migrated.root, physical);
    assert_eq!(super::recovery_tests::tree(&lexical), before);
    assert_data(&physical);
}

#[cfg(unix)]
#[test]
fn parent_resolution_does_not_follow_the_final_symlink() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir(dir.path().join("sub")).unwrap();
    std::fs::create_dir(dir.path().join("real")).unwrap();
    std::os::unix::fs::symlink(dir.path().join("real"), dir.path().join("link")).unwrap();
    assert_eq!(
        absolute(&dir.path().join("sub/../link")).unwrap(),
        dir.path().join("link")
    );
    assert_eq!(
        absolute(&dir.path().join("link")).unwrap(),
        dir.path().join("link")
    );
}

#[test]
fn ordinary_nested_roots_do_not_require_existing_parents() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("new/nested/db");
    assert_eq!(absolute(&root).unwrap(), root);
    assert!(pending(&root).unwrap().is_none());
    assert!(!root.parent().unwrap().exists());
    drop(Store::open(&root, StoreOptions::default()).unwrap());
}
