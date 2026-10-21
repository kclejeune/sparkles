use super::*;
use std::cell::RefCell;

type AfterAcquire = Box<dyn FnOnce(&File)>;

thread_local! {
    static AFTER_OPEN: RefCell<Option<Box<dyn FnOnce()>>> = const { RefCell::new(None) };
    static AFTER_ACQUIRE: RefCell<Option<AfterAcquire>> = const { RefCell::new(None) };
}

/// Consumed before invocation so a nested migration lock does not call it again.
pub(super) fn after_open() {
    let hook = AFTER_OPEN.with(|h| h.borrow_mut().take());
    if let Some(hook) = hook {
        hook();
    }
}

pub(super) fn after_acquire(file: &File) {
    let hook = AFTER_ACQUIRE.with(|h| h.borrow_mut().take());
    if let Some(hook) = hook {
        hook(file);
    }
}

#[test]
fn closing_store_unlocks_while_an_inherited_descriptor_lives() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path(), Default::default()).unwrap();
    let inherited = store._lock.as_ref().unwrap().try_clone().unwrap();
    assert!(matches!(
        Store::open(dir.path(), Default::default()),
        Err(Error::Locked { .. })
    ));
    drop(store);
    let reopened = Store::open(dir.path(), Default::default()).unwrap();
    // Closing an inherited descriptor must not release the new owner's lock.
    drop(inherited);
    assert!(matches!(
        Store::open(dir.path(), Default::default()),
        Err(Error::Locked { .. })
    ));
    drop(reopened);
    assert!(Store::open(dir.path(), Default::default()).is_ok());
}

#[test]
fn offline_guard_unlocks_while_an_inherited_descriptor_lives() {
    let dir = tempfile::tempdir().unwrap();
    let guard = lock_database(dir.path()).unwrap();
    let inherited = guard.try_clone().unwrap();
    drop(guard);
    let next = lock_database(dir.path()).unwrap();
    drop(inherited);
    assert!(matches!(
        lock_database(dir.path()),
        Err(Error::Locked { .. })
    ));
    drop(next);
    assert!(lock_database(dir.path()).is_ok());
}

#[test]
fn database_lock_outlives_owned_field_destructors() {
    struct CheckLock {
        root: PathBuf,
        observed: Arc<AtomicBool>,
    }
    impl Drop for CheckLock {
        fn drop(&mut self) {
            self.observed.store(
                matches!(lock_database(&self.root), Err(Error::Locked { .. })),
                Ordering::SeqCst,
            );
        }
    }
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path(), Default::default()).unwrap();
    let observed = Arc::new(AtomicBool::new(false));
    let check = CheckLock {
        root: dir.path().to_path_buf(),
        observed: observed.clone(),
    };
    *store.clock.lock() = Some(Arc::new(move || {
        let _keep_until_clock_drop = &check;
        0
    }));
    drop(store);
    assert!(observed.load(Ordering::SeqCst));
    assert!(lock_database(dir.path()).is_ok());
}

#[test]
fn failed_startup_unlocks_while_an_inherited_descriptor_lives() {
    use std::rc::Rc;
    let dir = tempfile::tempdir().unwrap();
    drop(Store::open(dir.path(), Default::default()).unwrap());
    let current = dir.path().join("CURRENT");
    let original = std::fs::read(&current).unwrap();
    let inherited = Rc::new(RefCell::new(None));
    let capture = inherited.clone();
    let corrupt = current.clone();
    AFTER_ACQUIRE.with(|h| {
        *h.borrow_mut() = Some(Box::new(move |file| {
            *capture.borrow_mut() = Some(file.try_clone().unwrap());
            std::fs::write(corrupt, b"missing-generation\n").unwrap();
        }));
    });
    let failed = Store::open(dir.path(), Default::default());
    std::fs::write(current, original).unwrap();
    assert!(failed.is_err());
    assert!(inherited.borrow().is_some());
    let reopened = Store::open(dir.path(), Default::default()).unwrap();
    drop(inherited);
    assert!(matches!(
        Store::open(dir.path(), Default::default()),
        Err(Error::Locked { .. })
    ));
    drop(reopened);
}

#[cfg(unix)]
#[test]
fn closing_store_unlocks_during_an_unrelated_pre_exec_child() {
    use std::io::Read;
    use std::os::fd::AsRawFd;
    use std::os::unix::{net::UnixStream, process::CommandExt};
    use std::time::{Duration, Instant};

    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path(), Default::default()).unwrap();
    let (mut ready_parent, ready_child) = UnixStream::pair().unwrap();
    let (resume_parent, resume_child) = UnixStream::pair().unwrap();
    ready_parent
        .set_read_timeout(Some(Duration::from_secs(10)))
        .unwrap();
    let close_ready = ready_parent.as_raw_fd();
    let close_resume = resume_parent.as_raw_fd();
    let worker = std::thread::spawn(move || {
        let mut command = std::process::Command::new(std::env::current_exe().unwrap());
        command
            .args(["--list", "--exact", "store::lock_tests"])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null());
        let ready = ready_child.as_raw_fd();
        let resume = resume_child.as_raw_fd();
        // SAFETY: only async-signal-safe close/write/poll/read syscalls run after
        // fork. The parent holds both pairs until the spawn has completed, and the
        // poll timeout bounds cleanup even if the parent fails before releasing it.
        unsafe {
            command.pre_exec(move || {
                libc::close(close_ready);
                libc::close(close_resume);
                if libc::write(ready, b"x".as_ptr().cast(), 1) != 1 {
                    return Err(std::io::Error::last_os_error());
                }
                let mut poll = libc::pollfd {
                    fd: resume,
                    events: libc::POLLIN,
                    revents: 0,
                };
                if libc::poll(&mut poll, 1, 10_000) <= 0 {
                    return Err(std::io::Error::from_raw_os_error(libc::ETIMEDOUT));
                }
                let mut byte = 0u8;
                if libc::read(resume, (&mut byte as *mut u8).cast(), 1) < 0 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }
        let spawned = command.spawn();
        drop(ready_child);
        drop(resume_child);
        let mut child = spawned.unwrap();
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            if let Some(status) = child.try_wait().unwrap() {
                return status;
            }
            if Instant::now() >= deadline {
                let _ = child.kill();
                let _ = child.wait();
                panic!("inherited-descriptor child failed to exit");
            }
            std::thread::sleep(Duration::from_millis(5));
        }
    });
    let ready = ready_parent.read_exact(&mut [0u8]);
    // Keep the child strictly before exec while the logical Store owner closes.
    drop(store);
    let reopened = Store::open(dir.path(), Default::default());
    // Reap before asserting, so the expected fail-before result also cleans up.
    drop(resume_parent);
    let status = worker.join().unwrap();
    ready.unwrap();
    assert!(status.success());
    let reopened = reopened.unwrap();
    assert!(matches!(
        Store::open(dir.path(), Default::default()),
        Err(Error::Locked { .. })
    ));
    drop(reopened);
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
        legacy.extend_from_slice(key);
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
