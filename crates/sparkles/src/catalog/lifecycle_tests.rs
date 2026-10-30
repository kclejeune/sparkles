//! Real before-close races and source-family disposal controls.
use super::*;
use std::cell::RefCell;
thread_local! {
    static BEFORE_CLOSE: RefCell<Option<Box<dyn FnMut()>>> = const { RefCell::new(None) };
}
pub(super) fn before_close_hook() {
    BEFORE_CLOSE.with_borrow_mut(|hook| {
        if let Some(hook) = hook {
            hook();
        }
    });
}

use crate::dataset::StoreHandle;
use std::sync::{Arc, Weak};

enum Alias {
    Store(Arc<Store>),
    StoreWeak(Weak<Store>),
    Set(Arc<crate::store::BranchSet>),
    SetWeak(Weak<crate::store::BranchSet>),
    StateWeak(Weak<crate::dataset::DatasetState>),
}
fn alias_refusal(kind: usize) {
    let dir = tempfile::tempdir().unwrap();
    let cat = Catalog::open(dir.path(), Default::default()).unwrap();
    let ds = cat.create("source", &Default::default()).unwrap();
    ds.create_branch("child", &Default::default()).unwrap();
    let child = ds.branch("child").unwrap();
    let state = Arc::as_ptr(ds.inner_state()) as usize;
    let child_state = Arc::as_ptr(child.inner_state()) as usize;
    let child_store = match &child.state().store {
        StoreHandle::Branch(store) => store.clone(),
        StoreHandle::Own(_) => panic!("real branch must stay Branch"),
    };
    let alias = match kind {
        0 => Alias::Store(child_store.clone()),
        1 => Alias::StoreWeak(Arc::downgrade(&child_store)),
        2 => Alias::Set(ds.store().branch_set().unwrap()),
        3 => Alias::SetWeak(Arc::downgrade(&ds.store().branch_set().unwrap())),
        4 => Alias::StateWeak(Arc::downgrade(ds.inner_state())),
        _ => unreachable!(),
    };
    let root = ds.store().root().unwrap().to_path_buf();
    let id = ds.dataset_id();
    let config = std::fs::read(dir.path().join("config.json")).unwrap();
    let current = std::fs::read(root.join("CURRENT")).unwrap();
    drop(child_store);
    drop(child);
    drop(ds);
    assert!(
        matches!(cat.delete("source"), Err(Error::Conflict(_))),
        "an actual raw alias must refuse before deleting files"
    );
    assert!(matches!(
        cat.rename("source", "moved"),
        Err(Error::Conflict(_))
    ));
    assert_eq!(
        std::fs::read(dir.path().join("config.json")).unwrap(),
        config
    );
    assert_eq!(std::fs::read(root.join("CURRENT")).unwrap(), current);
    assert!(!dir.path().join("rename.json").exists());
    let restored = cat.get("source").unwrap();
    assert_eq!(Arc::as_ptr(restored.inner_state()) as usize, state);
    assert_eq!(
        Arc::as_ptr(restored.branch("child").unwrap().inner_state()) as usize,
        child_state
    );
    match &alias {
        Alias::Store(store) => assert_eq!(store.owner_dataset_id(), id),
        Alias::StoreWeak(weak) => assert_eq!(weak.upgrade().unwrap().owner_dataset_id(), id),
        Alias::Set(set) => assert!(Arc::ptr_eq(set, &restored.store().branch_set().unwrap())),
        Alias::SetWeak(weak) => assert!(Arc::ptr_eq(
            &weak.upgrade().unwrap(),
            &restored.store().branch_set().unwrap()
        )),
        Alias::StateWeak(weak) => assert!(Arc::ptr_eq(
            &weak.upgrade().unwrap(),
            restored.inner_state()
        )),
    }
    drop(restored);
    drop(alias);
    let moved = cat.rename("source", "moved").unwrap();
    assert_eq!(moved.dataset_id(), id);
    drop(moved);
    assert!(cat.delete("moved").unwrap());
    assert!(!root.exists());
}
#[test]
fn raw_member_strong_refuses_without_replacing_any_owner() {
    alias_refusal(0);
}
#[test]
fn raw_member_weak_refuses_without_replacing_any_owner() {
    alias_refusal(1);
}
#[test]
fn raw_set_strong_refuses_without_replacing_any_owner() {
    alias_refusal(2);
}
#[test]
fn raw_set_weak_refuses_without_replacing_any_owner() {
    alias_refusal(3);
}
#[test]
fn raw_state_weak_refuses_without_replacing_any_owner() {
    alias_refusal(4);
}

#[test]
fn post_preflight_raw_weak_refusal_restores_exact_state_and_files() {
    let dir = tempfile::tempdir().unwrap();
    let cat = Catalog::open(dir.path(), Default::default()).unwrap();
    let ds = cat.create("source", &Default::default()).unwrap();
    let reference = ds.state_reference();
    let pointer = Arc::as_ptr(ds.inner_state());
    let weak = Arc::new(parking_lot::Mutex::new(None));
    let copy = weak.clone();
    let config = std::fs::read(dir.path().join("config.json")).unwrap();
    drop(ds);
    BEFORE_CLOSE.with_borrow_mut(|hook| {
        *hook = Some(Box::new(move || {
            let state = reference.upgrade().unwrap();
            *copy.lock() = Some(Arc::downgrade(&state));
        }))
    });
    let result = cat.delete("source");
    BEFORE_CLOSE.with_borrow_mut(|hook| *hook = None);
    assert!(matches!(result, Err(Error::Conflict(_))));
    assert_eq!(
        std::fs::read(dir.path().join("config.json")).unwrap(),
        config
    );
    assert_eq!(
        Arc::as_ptr(&weak.lock().as_ref().unwrap().upgrade().unwrap()),
        pointer
    );
    weak.lock().take();
    assert!(cat.delete("source").unwrap());
}

#[test]
fn idle_duplicate_cache_edges_and_distinct_states_around_one_store_close() {
    let dir = tempfile::tempdir().unwrap();
    let cat = Catalog::open(dir.path(), Default::default()).unwrap();
    let ds = cat.create("source", &Default::default()).unwrap();
    ds.create_branch("child", &Default::default()).unwrap();
    let child = ds.branch("child").unwrap();
    ds.inner
        .branches
        .lock()
        .insert("duplicate".into(), child.clone());
    let store = match &child.state().store {
        StoreHandle::Branch(store) => store.clone(),
        StoreHandle::Own(_) => unreachable!(),
    };
    let other = Dataset::from_shared(store, Default::default());
    assert!(!Arc::ptr_eq(child.inner_state(), other.inner_state()));
    ds.inner.branches.lock().insert("other-state".into(), other);
    drop(child);
    drop(ds);
    assert!(cat.delete("source").unwrap());
}

#[test]
fn duplicate_cached_edges_do_not_hide_an_actual_external_facade_handle() {
    let dir = tempfile::tempdir().unwrap();
    let cat = Catalog::open(dir.path(), Default::default()).unwrap();
    let ds = cat.create("source", &Default::default()).unwrap();
    ds.create_branch("child", &Default::default()).unwrap();
    let child = ds.branch("child").unwrap();
    ds.inner
        .branches
        .lock()
        .insert("duplicate".into(), child.clone());
    let pointer = Arc::as_ptr(child.inner_state());
    let root = ds.store().root().unwrap().to_path_buf();
    let current = std::fs::read(root.join("CURRENT")).unwrap();
    let config = std::fs::read(dir.path().join("config.json")).unwrap();
    drop(ds);
    assert!(matches!(cat.delete("source"), Err(Error::Conflict(_))));
    assert!(matches!(
        cat.rename("source", "moved"),
        Err(Error::Conflict(_))
    ));
    assert_eq!(Arc::as_ptr(child.inner_state()), pointer);
    assert_eq!(std::fs::read(root.join("CURRENT")).unwrap(), current);
    assert_eq!(
        std::fs::read(dir.path().join("config.json")).unwrap(),
        config
    );
    child
        .update("INSERT DATA { <urn:still> <urn:p> 1 }")
        .unwrap();
    drop(child);
    assert!(cat.delete("source").unwrap());
}

#[test]
fn rename_intent_failure_reopens_without_reviving_a_closed_reference() {
    let dir = tempfile::tempdir().unwrap();
    let cat = Catalog::open(dir.path(), Default::default()).unwrap();
    let ds = cat.create("source", &Default::default()).unwrap();
    let reference = ds.state_reference();
    let id = ds.dataset_id();
    drop(ds);
    rename::FAIL_AT.set(1 << rename::FAIL_PREPARE_WRITE);
    let result = cat.rename("source", "moved");
    rename::FAIL_AT.set(0);
    assert!(result.is_err());
    assert!(reference.upgrade().is_none());
    assert!(!dir.path().join("rename.json").exists());
    let reopened = cat.get("source").unwrap();
    assert_eq!(reopened.dataset_id(), id);
    assert!(!Arc::ptr_eq(
        &reference.link,
        &reopened.state_reference().link
    ));
    assert!(cat.get("moved").is_none());
    drop(reopened);
    assert!(cat.delete("source").unwrap());
}

#[test]
fn delete_registry_failure_reopens_actual_root_without_reviving_old_links() {
    let dir = tempfile::tempdir().unwrap();
    let cat = Catalog::open(dir.path(), Default::default()).unwrap();
    let ds = cat.create("source", &Default::default()).unwrap();
    let reference = ds.state_reference();
    let id = ds.dataset_id();
    drop(ds);
    let path = dir.path().join("config.json");
    let config = std::fs::read(&path).unwrap();
    std::fs::remove_file(&path).unwrap();
    std::fs::create_dir(&path).unwrap();
    let result = cat.delete("source");
    std::fs::remove_dir(&path).unwrap();
    std::fs::write(&path, config).unwrap();
    assert!(result.is_err());
    assert!(reference.upgrade().is_none());
    let reopened = cat.get("source").unwrap();
    assert_eq!(reopened.dataset_id(), id);
    drop(reopened);
    assert!(cat.delete("source").unwrap());
}

#[test]
fn close_callbacks_run_after_entries_release_and_panic_never_reopens_old_links() {
    use crate::guard::{Candidate, CommitGuard, ValidationSummary};
    use std::sync::atomic::{AtomicUsize, Ordering};
    struct ClosingGuard {
        catalog: Catalog,
        id: Uuid,
        calls: Arc<AtomicUsize>,
        panic: bool,
    }
    impl CommitGuard for ClosingGuard {
        fn check(&self, _: &Candidate<'_>) -> crate::Result<ValidationSummary> {
            unreachable!("installed after final write")
        }
        fn describe(&self) -> String {
            "actual close ordering fault control".into()
        }
        fn close(&self, _: &crate::commit::CommitInfo, _: &crate::store::Snapshot) {
            let result = self.catalog.try_get_by_id(
                self.id,
                &crate::guard::WriteOptions {
                    no_wait: true,
                    ..Default::default()
                },
            );
            assert!(
                matches!(result, Ok(None)),
                "entries exclusion remained held during callback"
            );
            self.calls.fetch_add(1, Ordering::SeqCst);
            if self.panic {
                std::panic::panic_any("actual-close-callback-payload");
            }
        }
    }
    for panic in [false, true] {
        let dir = tempfile::tempdir().unwrap();
        let cat = Catalog::open(dir.path(), Default::default()).unwrap();
        let ds = cat.create("source", &Default::default()).unwrap();
        let calls = Arc::new(AtomicUsize::new(0));
        let id = ds.dataset_id();
        let reference = ds.state_reference();
        let root = ds.store().root().unwrap().to_path_buf();
        ds.store().set_guard(Some(Arc::new(ClosingGuard {
            catalog: cat.clone(),
            id,
            calls: calls.clone(),
            panic,
        })));
        drop(ds);
        let result =
            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| cat.delete("source")));
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        assert!(reference.upgrade().is_none());
        if panic {
            let payload = result.unwrap_err();
            assert_eq!(
                payload.downcast_ref::<&str>(),
                Some(&"actual-close-callback-payload")
            );
            // The old registry/path remain recoverable, but the old link stays
            // terminal. A fresh real opening owns that path after full shutdown.
            let opened = Store::open(&root, Default::default()).unwrap();
            assert_eq!(opened.dataset_id(), id);
            drop(opened);
            drop(cat);
            let reopened = Catalog::open(dir.path(), Default::default()).unwrap();
            assert_eq!(reopened.get("source").unwrap().dataset_id(), id);
            assert!(reopened.delete("source").unwrap());
        } else {
            assert!(result.unwrap().unwrap());
        }
    }
}

#[cfg(feature = "failpoints")]
fn detached_refusal_callbacks_outside_entries(force_set_refusal: bool) {
    use crate::guard::{Candidate, CommitGuard, ValidationSummary};
    use std::sync::{
        atomic::{AtomicBool, AtomicUsize, Ordering},
        mpsc,
    };
    use std::time::{Duration, Instant};
    struct DetachedGuard {
        catalog: Catalog,
        id: Uuid,
        calls: Arc<AtomicUsize>,
        busy: Arc<AtomicBool>,
        found: Arc<AtomicBool>,
    }
    impl CommitGuard for DetachedGuard {
        fn check(&self, _: &Candidate<'_>) -> crate::Result<ValidationSummary> {
            unreachable!()
        }
        fn describe(&self) -> String {
            "real detached rollback callback".into()
        }
        fn close(&self, _: &crate::commit::CommitInfo, _: &crate::store::Snapshot) {
            let result = self.catalog.try_get_by_id(
                self.id,
                &crate::guard::WriteOptions {
                    no_wait: true,
                    ..Default::default()
                },
            );
            self.busy
                .store(matches!(result, Err(Error::WriterBusy)), Ordering::SeqCst);
            self.found
                .store(matches!(result, Ok(Some(_))), Ordering::SeqCst);
            self.calls.fetch_add(1, Ordering::SeqCst);
        }
    }
    let dir = tempfile::tempdir().unwrap();
    let cat = Catalog::open(dir.path(), Default::default()).unwrap();
    let ds = cat.create("source", &Default::default()).unwrap();
    ds.update("INSERT DATA { <urn:original> <urn:p> 1 }")
        .unwrap();
    ds.create_branch("detached", &Default::default()).unwrap();
    let mut detached = Some(ds.store().branch("detached").unwrap().shared().unwrap());
    ds.store().delete_branch("detached", true).unwrap();
    let set_weak = force_set_refusal.then(|| Arc::downgrade(&ds.store().branch_set().unwrap()));
    let calls = Arc::new(AtomicUsize::new(0));
    let busy = Arc::new(AtomicBool::new(false));
    let found = Arc::new(AtomicBool::new(false));
    let id = ds.dataset_id();
    let pointer = Arc::as_ptr(ds.inner_state());
    let reference = ds.state_reference();
    let root = ds.store().root().unwrap().to_path_buf();
    let config = std::fs::read(dir.path().join("config.json")).unwrap();
    let current = std::fs::read(root.join("CURRENT")).unwrap();
    detached
        .as_ref()
        .unwrap()
        .set_guard(Some(Arc::new(DetachedGuard {
            catalog: cat.clone(),
            id,
            calls: calls.clone(),
            busy: busy.clone(),
            found: found.clone(),
        })));
    let (hit, ready) = mpsc::channel();
    let (resume, go) = mpsc::channel();
    let go = parking_lot::Mutex::new(go);
    ds.store().set_failpoint(
        "family-close-inventoried",
        Some(Arc::new(move |_| {
            hit.send(()).unwrap();
            go.lock().recv_timeout(Duration::from_secs(10)).unwrap();
        })),
    );
    drop(ds);
    let (done, result) = mpsc::channel();
    let worker_catalog = cat.clone();
    let worker = std::thread::spawn(move || {
        done.send(worker_catalog.delete("source")).unwrap();
    });
    let until = Instant::now() + Duration::from_secs(5);
    let mut outcome = None;
    let mut latched = false;
    while Instant::now() < until {
        if ready.try_recv().is_ok() {
            latched = true;
            drop(detached.take());
            let _ = resume.send(());
            break;
        }
        if let Ok(done) = result.try_recv() {
            outcome = Some(done);
            break;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    if outcome.is_none() {
        outcome = result.recv_timeout(Duration::from_secs(5)).ok();
    }
    let _ = resume.send(());
    worker.join().unwrap();
    // The repaired conservative refusal intentionally never reaches inventory's
    // latch. Its unrelated outside owner drops only after the refusal unlocks.
    drop(detached.take());
    let outcome = outcome.expect("neither inventory progress nor completed refusal");
    assert!(
        matches!(outcome, Err(Error::Conflict(_))),
        "unmatched detached owner must refuse before inventory capture"
    );
    assert!(
        !busy.load(Ordering::SeqCst),
        "rollback ran detached callback under entries exclusion"
    );
    assert!(found.load(Ordering::SeqCst));
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert!(
        !latched,
        "fixed close must not acquire unmatched detached ownership"
    );
    assert_eq!(
        std::fs::read(dir.path().join("config.json")).unwrap(),
        config
    );
    assert_eq!(std::fs::read(root.join("CURRENT")).unwrap(), current);
    let restored = cat.get("source").unwrap();
    assert_eq!(restored.dataset_id(), id);
    assert_eq!(restored.len(), 1);
    assert_eq!(Arc::as_ptr(restored.inner_state()), pointer);
    assert!(Arc::ptr_eq(
        &reference.link,
        &restored.state_reference().link
    ));
    restored
        .store()
        .set_failpoint("family-close-inventoried", None);
    drop(restored);
    drop(set_weak);
    assert!(cat.delete("source").unwrap());
}

#[cfg(feature = "failpoints")]
#[test]
fn unmatched_detached_owner_refuses_then_releases_outside_entries() {
    detached_refusal_callbacks_outside_entries(false);
}

#[cfg(feature = "failpoints")]
#[test]
fn detached_rollback_callback_runs_only_after_original_entry_is_restored() {
    detached_refusal_callbacks_outside_entries(true);
}

#[test]
fn delete_sync_failure_reopens_only_the_original_dataset_and_bytes() {
    for point in [DELETE_BEFORE_MOVE, DELETE_STAGE_SYNC] {
        let dir = tempfile::tempdir().unwrap();
        let cat = Catalog::open(dir.path(), Default::default()).unwrap();
        let ds = cat.create("source", &Default::default()).unwrap();
        ds.update("INSERT DATA { <urn:retained> <urn:p> 7 }")
            .unwrap();
        let id = ds.dataset_id();
        let reference = ds.state_reference();
        let root = ds.store().root().unwrap().to_path_buf();
        let config = std::fs::read(dir.path().join("config.json")).unwrap();
        drop(ds);
        DELETE_FAIL.set(1 << point);
        let failed = cat.delete("source");
        DELETE_FAIL.set(0);
        assert!(failed.is_err());
        assert!(reference.upgrade().is_none());
        let reopened = cat.get("source").expect("original source must be reopened");
        assert_eq!(
            reopened.dataset_id(),
            id,
            "recovery fabricated a different dataset identity"
        );
        assert_eq!(reopened.len(), 1, "recovery lost original source data");
        assert_eq!(
            std::fs::read(dir.path().join("config.json")).unwrap(),
            config
        );
        drop(reopened);
        drop(cat);
        let store = Store::open(&root, Default::default()).unwrap();
        assert_eq!(store.dataset_id(), id);
        assert_eq!(store.snapshot().len(), 1);
        drop(store);
        let cat = Catalog::open(dir.path(), Default::default()).unwrap();
        assert_eq!(cat.get("source").unwrap().dataset_id(), id);
        assert!(cat.delete("source").unwrap());
    }
}

#[test]
fn failed_delete_rollback_keeps_original_bytes_unavailable_without_a_new_root() {
    for point in [DELETE_ROLLBACK_MOVE, DELETE_ROLLBACK_SYNC] {
        let dir = tempfile::tempdir().unwrap();
        let cat = Catalog::open(dir.path(), Default::default()).unwrap();
        let ds = cat.create("source", &Default::default()).unwrap();
        ds.update("INSERT DATA { <urn:retained> <urn:p> 7 }")
            .unwrap();
        let id = ds.dataset_id();
        let reference = ds.state_reference();
        let root = ds.store().root().unwrap().to_path_buf();
        drop(ds);
        DELETE_FAIL.set((1 << DELETE_STAGE_SYNC) | (1 << point));
        let failed = cat.delete("source");
        DELETE_FAIL.set(0);
        let error = failed.unwrap_err().to_string();
        assert!(error.contains(&id.to_string()));
        assert!(cat.get("source").is_none());
        assert!(reference.upgrade().is_none());
        let bytes = if point == DELETE_ROLLBACK_MOVE {
            assert!(
                !root.exists(),
                "failed rollback must not fabricate an original root"
            );
            let paths: Vec<_> = std::fs::read_dir(dir.path().join("databases"))
                .unwrap()
                .map(|e| e.unwrap().path())
                .filter(|p| {
                    p.file_name()
                        .unwrap()
                        .to_string_lossy()
                        .starts_with(KEPT_PREFIX)
                })
                .collect();
            assert_eq!(paths.len(), 1);
            paths[0].clone()
        } else {
            assert!(root.exists());
            root.clone()
        };
        drop(cat);
        let store = Store::open(&bytes, Default::default()).unwrap();
        assert_eq!(store.dataset_id(), id);
        assert_eq!(store.snapshot().len(), 1);
        drop(store);
        if point == DELETE_ROLLBACK_MOVE {
            // Catalog boot may refuse the missing registered root, but it must
            // neither invent it nor garbage-collect existing kept source bytes.
            assert!(Catalog::open(dir.path(), Default::default()).is_err());
            assert!(!root.exists());
            assert!(bytes.exists());
            std::fs::rename(&bytes, &root).unwrap();
        }
        let cat = Catalog::open(dir.path(), Default::default()).unwrap();
        assert_eq!(cat.get("source").unwrap().dataset_id(), id);
        assert!(cat.delete("source").unwrap());
    }
}

#[test]
fn recovery_refuses_replaced_or_missing_existing_root_without_publishing_empty_data() {
    for replace in [false, true] {
        let dir = tempfile::tempdir().unwrap();
        let cat = Catalog::open(dir.path(), Default::default()).unwrap();
        let ds = cat.create("source", &Default::default()).unwrap();
        let id = ds.dataset_id();
        let reference = ds.state_reference();
        let root = ds.store().root().unwrap().to_path_buf();
        drop(ds);
        // Close through the real catalog boundary before changing its recovery path.
        DELETE_FAIL.set((1 << DELETE_STAGE_SYNC) | (1 << DELETE_ROLLBACK_MOVE));
        assert!(cat.delete("source").is_err());
        DELETE_FAIL.set(0);
        assert!(!root.exists());
        let replacement = replace.then(|| {
            let store = Store::open(&root, Default::default()).unwrap();
            let replacement = store.dataset_id();
            drop(store);
            replacement
        });
        let error = cat.reattach_locked("source", Some(id)).unwrap_err();
        assert!(reference.upgrade().is_none());
        assert!(cat.get("source").is_none());
        if let Some(replacement) = replacement {
            let store = Store::open(&root, Default::default()).unwrap();
            assert_eq!(store.dataset_id(), replacement);
            assert_ne!(replacement, id);
            drop(store);
        } else {
            assert!(
                !root.exists(),
                "recovery created an empty missing path: {error}"
            );
        }
    }
}

#[cfg(feature = "failpoints")]
#[test]
fn precommit_panic_restores_original_graph_before_propagation() {
    use crate::guard::{Candidate, CommitGuard, ValidationSummary};
    use std::sync::atomic::{AtomicUsize, Ordering};
    if std::env::var_os("C25_PRECOMMIT_CHILD").is_none() {
        use std::{
            process::{Command, Stdio},
            time::{Duration, Instant},
        };
        let dir = tempfile::tempdir().unwrap();
        let mut child = Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "catalog::lifecycle_tests::precommit_panic_restores_original_graph_before_propagation", "--nocapture"])
            .env("C25_PRECOMMIT_CHILD", "1").env("C25_PRECOMMIT_DIRECTORY", dir.path())
            .stdout(Stdio::piped()).stderr(Stdio::piped()).spawn().unwrap();
        use std::io::Read;
        let stdout = child.stdout.take().unwrap();
        let stderr = child.stderr.take().unwrap();
        let drain = |mut pipe: Box<dyn Read + Send>| {
            let mut bytes = Vec::new();
            pipe.read_to_end(&mut bytes).unwrap();
            bytes
        };
        let out = std::thread::spawn(move || drain(Box::new(stdout)));
        let err = std::thread::spawn(move || drain(Box::new(stderr)));
        let until = Instant::now() + Duration::from_secs(15);
        let mut timed_out = false;
        while child.try_wait().unwrap().is_none() {
            if Instant::now() >= until {
                timed_out = true;
                child.kill().unwrap();
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        let status = child.wait().unwrap();
        let stdout = out.join().unwrap();
        let stderr = err.join().unwrap();
        assert!(
            !timed_out && status.success(),
            "precommit child failed: {status:?}\n{}\n{}",
            String::from_utf8_lossy(&stdout),
            String::from_utf8_lossy(&stderr)
        );
        return;
    }
    struct Guard {
        catalog: Catalog,
        id: Uuid,
        calls: Arc<AtomicUsize>,
    }
    impl CommitGuard for Guard {
        fn check(&self, _: &Candidate<'_>) -> crate::Result<ValidationSummary> {
            unreachable!()
        }
        fn describe(&self) -> String {
            "real cached detached close guard".into()
        }
        fn close(&self, _: &crate::commit::CommitInfo, _: &crate::store::Snapshot) {
            let _ = self.catalog.try_get_by_id(
                self.id,
                &crate::guard::WriteOptions {
                    no_wait: true,
                    ..Default::default()
                },
            );
            self.calls.fetch_add(1, Ordering::SeqCst);
        }
    }
    let path = std::path::PathBuf::from(std::env::var_os("C25_PRECOMMIT_DIRECTORY").unwrap());
    let cat = Catalog::open(&path, Default::default()).unwrap();
    let ds = cat.create("source", &Default::default()).unwrap();
    ds.create_branch("child", &Default::default()).unwrap();
    let child = ds.branch("child").unwrap();
    ds.store().delete_branch("child", true).unwrap();
    let child_state = Arc::as_ptr(child.inner_state());
    let child_store = child.store() as *const crate::store::Store;
    let calls = Arc::new(AtomicUsize::new(0));
    let id = ds.dataset_id();
    let pointer = Arc::as_ptr(ds.inner_state());
    let reference = ds.state_reference();
    child.store().set_guard(Some(Arc::new(Guard {
        catalog: cat.clone(),
        id,
        calls: calls.clone(),
    })));
    ds.store().set_failpoint(
        "family-close-inventoried",
        Some(Arc::new(|_| {
            std::panic::panic_any("precommit-original-payload")
        })),
    );
    let config = std::fs::read(path.join("config.json")).unwrap();
    drop(child);
    drop(ds);
    let failure = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| cat.delete("source")))
        .unwrap_err();
    assert_eq!(
        failure.downcast_ref::<&str>(),
        Some(&"precommit-original-payload")
    );
    assert_eq!(
        calls.load(Ordering::SeqCst),
        0,
        "refused close destroyed a cached detached Store"
    );
    let restored = cat.get("source").unwrap();
    assert_eq!(Arc::as_ptr(restored.inner_state()), pointer);
    assert!(Arc::ptr_eq(
        &reference.link,
        &restored.state_reference().link
    ));
    assert_eq!(std::fs::read(path.join("config.json")).unwrap(), config);
    restored
        .store()
        .set_failpoint("family-close-inventoried", None);
    let child = restored.inner.branches.lock().get("child").unwrap().clone();
    assert_eq!(Arc::as_ptr(child.inner_state()), child_state);
    assert_eq!(child.store() as *const crate::store::Store, child_store);
    child.store().set_guard(None);
    drop(child);
    drop(restored);
    assert!(cat.delete("source").unwrap());
}
