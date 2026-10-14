use super::*;
use std::cell::RefCell;

thread_local! {
    static AFTER_OPEN: RefCell<Option<Box<dyn FnOnce()>>> = const { RefCell::new(None) };
}

/// Consumed before invocation so a nested migration lock does not call it again.
pub(super) fn after_open() {
    let hook = AFTER_OPEN.with(|h| h.borrow_mut().take());
    if let Some(hook) = hook {
        hook();
    }
}

#[cfg(unix)]
#[test]
fn startup_rejects_a_lock_from_the_previous_root() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("db");
    let store = Store::open(&root, StoreOptions::default()).unwrap();
    crate::sparql::update::update(
        &store,
        "INSERT DATA { <urn:s> <urn:p> \"legacy term\" }",
        &Default::default(),
    )
    .unwrap();
    let vocab = root
        .join(&store.snapshot().generation.name)
        .join("delta.vocab");
    drop(store);
    let bytes = std::fs::read(&vocab).unwrap();
    let parsed = crate::vocab::delta::parse_delta(&bytes).unwrap();
    let mut legacy = Vec::new();
    for key in parsed.keys {
        legacy.extend_from_slice(&(key.len() as u32).to_le_bytes());
        legacy.extend_from_slice(&key);
    }
    std::fs::write(vocab, legacy).unwrap();
    let migrate_root = root.clone();
    AFTER_OPEN.with(|h| {
        *h.borrow_mut() = Some(Box::new(move || {
            let migrated = crate::migrate::migrate(&migrate_root, true).unwrap();
            assert_eq!(migrated.steps.len(), 1);
            assert!(crate::migrate::pending(&migrate_root).unwrap().is_none());
        }));
    });
    let late = Store::open(&root, StoreOptions::default());
    assert!(
        matches!(late, Err(Error::Locked { .. })),
        "stale lock authorized startup"
    );
    let current = Store::open(&root, StoreOptions::default()).unwrap();
    assert_eq!(current.snapshot().len(), 1);
    assert!(matches!(
        Store::open(&root, StoreOptions::default()),
        Err(Error::Locked { .. })
    ));
    drop(current);
    assert!(Store::open(&root, StoreOptions::default()).is_ok());
}

#[test]
fn identity_accepts_current_handles_and_relative_paths() {
    // A relative path without changing the process-wide working directory.
    let cwd = std::env::current_dir().unwrap();
    let dir = tempfile::tempdir_in(&cwd).unwrap();
    let root = dir.path().join("db");
    std::fs::create_dir(&root).unwrap();
    let locked = lock_database(&root).unwrap();
    verify_database_lock(&root, &locked).unwrap();
    let second = File::open(root.join("sparkles.lock")).unwrap();
    verify_database_lock(&root, &second).unwrap();
    verify_database_lock(root.strip_prefix(cwd).unwrap(), &locked).unwrap();
    assert!(matches!(lock_database(&root), Err(Error::Locked { .. })));
    drop(second);
    drop(locked);
    assert!(lock_database(&root).is_ok());
}

#[cfg(unix)]
#[test]
fn identity_refuses_old_and_missing_current_lock_files() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("db");
    let old = dir.path().join("old");
    std::fs::create_dir(&root).unwrap();
    let locked = lock_database(&root).unwrap();
    std::fs::rename(&root, &old).unwrap();
    std::fs::create_dir(&root).unwrap();
    assert!(matches!(
        verify_database_lock(&root, &locked),
        Err(Error::Locked { .. })
    ));
    assert!(!root.join("sparkles.lock").exists());
    let current = lock_database(&root).unwrap();
    verify_database_lock(&root, &current).unwrap();
    assert!(matches!(
        verify_database_lock(&root, &locked),
        Err(Error::Locked { .. })
    ));
    // Rejecting the old descriptor does not release the current root's real lock.
    assert!(matches!(lock_database(&root), Err(Error::Locked { .. })));
    verify_database_lock(&old, &locked).unwrap();
}

#[test]
fn startup_locks_a_new_nested_root_and_its_branch() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("new/nested/db");
    let store = Store::open(&root, StoreOptions::default()).unwrap();
    store.create_branch("dev", &Default::default()).unwrap();
    let branch = store.branch("dev").unwrap();
    verify_database_lock(branch.root().unwrap(), branch._lock.as_ref().unwrap()).unwrap();
    assert!(matches!(
        Store::open(&root, StoreOptions::default()),
        Err(Error::Locked { .. })
    ));
    drop(branch);
    drop(store);
    let reopened = Store::open(&root, StoreOptions::default()).unwrap();
    assert!(reopened.branch("dev").is_ok());
}

#[cfg(unix)]
#[test]
fn startup_needs_no_write_permission_on_the_parent() {
    use std::os::unix::fs::PermissionsExt;
    let dir = tempfile::tempdir().unwrap();
    let parent = dir.path().join("parent");
    let root = parent.join("db");
    drop(Store::open(&root, StoreOptions::default()).unwrap());
    let original = std::fs::metadata(&parent).unwrap().permissions();
    std::fs::set_permissions(&parent, std::fs::Permissions::from_mode(0o555)).unwrap();
    let opened = Store::open(&root, StoreOptions::default());
    std::fs::set_permissions(&parent, original).unwrap();
    drop(opened.unwrap());
}
