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
            assert!(
                bytes
                    .file_name()
                    .unwrap()
                    .to_string_lossy()
                    .starts_with(&format!("{KEPT_PREFIX}source-")),
                "{bytes:?}"
            );
            // Boot refuses only the missing registered root. It neither invents it
            // nor garbage-collects the kept source bytes.
            let booted = Catalog::open(dir.path(), Default::default()).unwrap();
            assert!(booted.get("source").is_none());
            let refused = booted.refused();
            assert_eq!(refused.len(), 1);
            assert_eq!(refused[0].0, "source");
            assert!(
                refused[0].1.contains(&root.display().to_string()),
                "{refused:?}"
            );
            assert!(matches!(
                booted.create("source", &Default::default()),
                Err(Error::Conflict(_))
            ));
            drop(booted);
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

/// A close callback that panics after the runtime close commits.
struct PanickingClose;
impl crate::guard::CommitGuard for PanickingClose {
    fn check(
        &self,
        _: &crate::guard::Candidate<'_>,
    ) -> crate::Result<crate::guard::ValidationSummary> {
        unreachable!("installed after the final write")
    }
    fn describe(&self) -> String {
        "post-commit close panic".into()
    }
    fn close(&self, _: &crate::commit::CommitInfo, _: &crate::store::Snapshot) {
        std::panic::panic_any("post-commit-close-panic");
    }
}

#[test]
fn post_commit_close_panic_keeps_the_dataset_registered_across_later_saves() {
    for rename in [false, true] {
        let dir = tempfile::tempdir().unwrap();
        let cat = Catalog::open(dir.path(), Default::default()).unwrap();
        let ds = cat.create("source", &Default::default()).unwrap();
        ds.update("INSERT DATA { <urn:a> <urn:p> 1 }").unwrap();
        let id = ds.dataset_id();
        ds.store().set_guard(Some(Arc::new(PanickingClose)));
        drop(ds);
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            if rename {
                cat.rename("source", "target").map(|_| true)
            } else {
                cat.delete("source")
            }
        }));
        let payload = result.unwrap_err();
        assert_eq!(
            payload.downcast_ref::<&str>(),
            Some(&"post-commit-close-panic")
        );
        // The catalog keeps serving, as a server does after a caught handler panic.
        assert!(cat.get("source").is_none());
        assert!(matches!(
            cat.create("source", &Default::default()),
            Err(Error::Conflict(_))
        ));
        cat.create("other", &Default::default()).unwrap();
        drop(cat);
        let reopened = Catalog::open(dir.path(), Default::default()).unwrap();
        let source = reopened
            .get("source")
            .unwrap_or_else(|| panic!("rename={rename}: dataset was unregistered"));
        assert_eq!(source.dataset_id(), id);
        assert_eq!(source.len(), 1);
        assert!(reopened.get("target").is_none());
        assert!(reopened.get("other").is_some());
    }
}

#[test]
fn refused_close_removes_the_closing_registration() {
    for rename in [false, true] {
        let dir = tempfile::tempdir().unwrap();
        let cat = Catalog::open(dir.path(), Default::default()).unwrap();
        let ds = cat.create("source", &Default::default()).unwrap();
        // A raw Weak alias makes the ownership proof refuse after the preflight.
        let weak = Arc::downgrade(ds.inner_state());
        drop(ds);
        let refused = if rename {
            cat.rename("source", "target").map(|_| true)
        } else {
            cat.delete("source")
        };
        assert!(
            matches!(refused, Err(Error::Conflict(_))),
            "{:?}",
            refused.err()
        );
        assert!(!cat.inner.detached.lock().contains_key("source"));
        assert!(cat.get("source").is_some());
        drop(weak);
        if rename {
            cat.rename("source", "target").unwrap();
            assert!(cat.get("target").is_some());
            assert!(cat.inner.detached.lock().is_empty());
        } else {
            assert!(cat.delete("source").unwrap());
            assert!(cat.inner.detached.lock().is_empty());
        }
        drop(cat);
        let reopened = Catalog::open(dir.path(), Default::default()).unwrap();
        assert_eq!(reopened.get("target").is_some(), rename);
        assert!(reopened.get("source").is_none());
    }
}

#[test]
fn boot_refuses_only_a_registered_root_without_directory_or_current() {
    for lose_current in [false, true] {
        let dir = tempfile::tempdir().unwrap();
        let cat = Catalog::open(dir.path(), Default::default()).unwrap();
        cat.create("a", &Default::default()).unwrap();
        cat.create("b", &Default::default()).unwrap();
        drop(cat);
        let root = dir.path().join("databases").join("a");
        if lose_current {
            std::fs::remove_file(root.join("CURRENT")).unwrap();
        } else {
            std::fs::rename(&root, dir.path().join("databases/.kept-a-x")).unwrap();
        }
        let cat = Catalog::open(dir.path(), Default::default()).unwrap();
        assert!(cat.get("b").is_some());
        assert!(cat.get("a").is_none());
        let refused = cat.refused();
        assert_eq!(refused.len(), 1);
        assert_eq!(refused[0].0, "a");
        assert!(
            refused[0].1.contains(&root.display().to_string()),
            "{refused:?}"
        );
        assert!(matches!(
            cat.create("a", &Default::default()),
            Err(Error::Conflict(_))
        ));
        // Later saves keep the refused entry registered.
        cat.create("c", &Default::default()).unwrap();
        drop(cat);
        assert_eq!(root.exists(), lose_current, "boot must not recreate a root");
        let again = Catalog::open(dir.path(), Default::default()).unwrap();
        assert_eq!(again.refused().len(), 1);
    }
}

/// Close callbacks that manage the catalog from inside a disposal.
mod callback_management {
    use super::*;
    use crate::guard::{Candidate, CommitGuard, ValidationSummary};
    use std::sync::mpsc;
    use std::time::Duration;

    type Action = Box<dyn Fn(&Catalog) + Send + Sync>;
    struct CallbackGuard {
        catalog: Catalog,
        action: Action,
    }
    impl CommitGuard for CallbackGuard {
        fn check(&self, _: &Candidate<'_>) -> crate::Result<ValidationSummary> {
            unreachable!("installed after the final write")
        }
        fn describe(&self) -> String {
            "close callback that manages the catalog".into()
        }
        fn close(&self, _: &crate::commit::CommitInfo, _: &crate::store::Snapshot) {
            (self.action)(&self.catalog);
        }
    }
    fn install(ds: &Dataset, cat: &Catalog, action: impl Fn(&Catalog) + Send + Sync + 'static) {
        ds.store().set_guard(Some(Arc::new(CallbackGuard {
            catalog: cat.clone(),
            action: Box::new(action),
        })));
    }
    /// Run `f` on another thread so that a deadlock fails as a timeout.
    fn bounded<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> T {
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            let _ = tx.send(f());
        });
        rx.recv_timeout(Duration::from_secs(5))
            .expect("a catalog call from a close callback must not wait on its own disposal")
    }
    type Seen = Arc<parking_lot::Mutex<Option<crate::Result<()>>>>;
    fn create_other(seen: &Seen) -> impl Fn(&Catalog) + Send + Sync + 'static {
        let seen = seen.clone();
        move |c: &Catalog| {
            *seen.lock() = Some(c.create("other", &Default::default()).map(drop));
        }
    }
    fn assert_disposing(seen: &Seen) {
        let seen = seen.lock().take().expect("the close callback ran");
        assert!(
            matches!(&seen, Err(Error::Conflict(m)) if m.starts_with("catalog-disposing")),
            "{seen:?}"
        );
    }

    #[test]
    fn delete_refuses_management_from_its_close_callback() {
        let dir = tempfile::tempdir().unwrap();
        let cat = Catalog::open(dir.path(), Default::default()).unwrap();
        let ds = cat.create("source", &Default::default()).unwrap();
        let seen = Seen::default();
        install(&ds, &cat, create_other(&seen));
        drop(ds);
        let worker = cat.clone();
        assert!(bounded(move || worker.delete("source")).unwrap());
        assert_disposing(&seen);
        assert!(cat.get("other").is_none());
        assert!(cat.get("source").is_none());
    }

    #[test]
    fn rename_refuses_management_from_its_close_callback() {
        let dir = tempfile::tempdir().unwrap();
        let cat = Catalog::open(dir.path(), Default::default()).unwrap();
        let ds = cat.create("source", &Default::default()).unwrap();
        let seen = Seen::default();
        install(&ds, &cat, create_other(&seen));
        drop(ds);
        let worker = cat.clone();
        bounded(move || worker.rename("source", "moved").map(drop)).unwrap();
        assert_disposing(&seen);
        assert!(cat.get("other").is_none());
        assert!(cat.get("moved").is_some());
    }

    #[cfg(feature = "backup")]
    #[test]
    fn restore_swap_refuses_management_from_its_close_callback() {
        let dir = tempfile::tempdir().unwrap();
        let cat = Catalog::open(dir.path(), Default::default()).unwrap();
        let ds = cat.create("source", &Default::default()).unwrap();
        let seen = Seen::default();
        install(&ds, &cat, create_other(&seen));
        drop(ds);
        let restored = dir.path().join("databases").join(".restore-source-1");
        let id = Store::open(&restored, Default::default())
            .unwrap()
            .dataset_id();
        let worker = cat.clone();
        let swapped = bounded(move || {
            super::super::swap::replace_with(
                &worker,
                "source",
                &restored,
                "1",
                false,
                Duration::from_secs(5),
            )
            .map(|ds| ds.dataset_id())
            .map_err(|e| e.to_string())
        });
        assert_eq!(swapped.unwrap(), id);
        assert_disposing(&seen);
        assert!(cat.get("other").is_none());
    }

    #[test]
    fn management_from_another_thread_waits_for_the_disposal() {
        let dir = tempfile::tempdir().unwrap();
        let cat = Catalog::open(dir.path(), Default::default()).unwrap();
        let ds = cat.create("source", &Default::default()).unwrap();
        let (tx, rx) = mpsc::channel();
        let tx = parking_lot::Mutex::new(tx);
        install(&ds, &cat, move |c| {
            let (started, waiting) = mpsc::channel();
            let other = c.clone();
            let tx = tx.lock().clone();
            std::thread::spawn(move || {
                started.send(()).unwrap();
                let _ = tx.send(other.create("other", &Default::default()).map(drop));
            });
            waiting.recv().unwrap();
            // The other thread now waits for the management lock that this
            // disposal holds.
            std::thread::sleep(Duration::from_millis(50));
        });
        drop(ds);
        let worker = cat.clone();
        assert!(bounded(move || worker.delete("source")).unwrap());
        let created = rx.recv_timeout(Duration::from_secs(5)).unwrap();
        assert!(created.is_ok(), "{created:?}");
        assert!(cat.get("other").is_some());
    }

    #[test]
    fn deferred_management_runs_after_the_disposal_returns_its_lock() {
        for rename in [false, true] {
            let dir = tempfile::tempdir().unwrap();
            let cat = Catalog::open(dir.path(), Default::default()).unwrap();
            let ds = cat.create("source", &Default::default()).unwrap();
            let gone = Arc::new(std::sync::atomic::AtomicBool::new(false));
            let seen = gone.clone();
            install(&ds, &cat, move |c| {
                let seen = seen.clone();
                c.defer(move |c| {
                    seen.store(
                        c.get("source").is_none(),
                        std::sync::atomic::Ordering::SeqCst,
                    );
                    c.create("other", &Default::default()).unwrap();
                });
            });
            drop(ds);
            let worker = cat.clone();
            bounded(move || {
                if rename {
                    worker.rename("source", "moved").map(drop)
                } else {
                    worker.delete("source").map(drop)
                }
            })
            .unwrap();
            assert!(cat.get("other").is_some(), "rename={rename}");
            assert!(gone.load(std::sync::atomic::Ordering::SeqCst));
            drop(cat);
            let reopened = Catalog::open(dir.path(), Default::default()).unwrap();
            assert!(reopened.get("other").is_some());
            assert_eq!(reopened.get("moved").is_some(), rename);
        }
    }

    #[cfg(feature = "backup")]
    #[test]
    fn deferred_management_runs_after_a_restore_swap() {
        let dir = tempfile::tempdir().unwrap();
        let cat = Catalog::open(dir.path(), Default::default()).unwrap();
        let ds = cat.create("source", &Default::default()).unwrap();
        install(&ds, &cat, |c| {
            c.defer(|c| {
                // The swap has published the restored dataset by now.
                assert!(c.get("source").is_some());
                c.create("other", &Default::default()).unwrap();
            });
        });
        drop(ds);
        let restored = dir.path().join("databases").join(".restore-source-1");
        drop(Store::open(&restored, Default::default()).unwrap());
        let worker = cat.clone();
        bounded(move || {
            super::super::swap::replace_with(
                &worker,
                "source",
                &restored,
                "1",
                false,
                Duration::from_secs(5),
            )
            .map(drop)
            .map_err(|e| e.to_string())
        })
        .unwrap();
        assert!(cat.get("other").is_some());
    }

    #[test]
    fn a_panicking_deferred_operation_does_not_stop_later_ones() {
        let dir = tempfile::tempdir().unwrap();
        let cat = Catalog::open(dir.path(), Default::default()).unwrap();
        let ds = cat.create("source", &Default::default()).unwrap();
        install(&ds, &cat, |c| {
            c.defer(|_| std::panic::panic_any("deferred-op-payload"));
            c.defer(|c| {
                c.create("other", &Default::default()).unwrap();
            });
        });
        drop(ds);
        let worker = cat.clone();
        assert!(bounded(move || worker.delete("source")).unwrap());
        assert!(cat.get("other").is_some());
        assert!(cat.get("source").is_none());
    }

    #[test]
    fn deferral_outside_management_runs_at_once() {
        let cat = Catalog::memory(Default::default());
        let ran = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let flag = ran.clone();
        cat.defer(move |c| {
            let memory = CreateDataset {
                kind: DatasetKind::Memory,
                geo: None,
            };
            c.create("other", &memory).unwrap();
            flag.store(true, std::sync::atomic::Ordering::SeqCst);
        });
        assert!(ran.load(std::sync::atomic::Ordering::SeqCst));
        assert!(cat.get("other").is_some());
    }

    #[test]
    fn a_long_deferral_chain_drains_in_constant_stack() {
        const LINKS: usize = 20_000;
        fn link(
            left: usize,
            ran: Arc<std::sync::atomic::AtomicUsize>,
        ) -> impl FnOnce(&Catalog) + Send {
            move |c: &Catalog| {
                ran.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                if left > 0 {
                    c.defer(link(left - 1, ran));
                }
                // A management call whose guard drops inside the drain.
                c.save().unwrap();
            }
        }
        let ran = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let counted = ran.clone();
        let worker = std::thread::Builder::new()
            .stack_size(512 * 1024)
            .spawn(move || {
                let cat = Catalog::memory(Default::default());
                let batch = cat.batch_scope();
                cat.defer(link(LINKS - 1, counted));
                drop(batch);
            })
            .unwrap();
        worker.join().unwrap();
        assert_eq!(ran.load(std::sync::atomic::Ordering::SeqCst), LINKS);
    }

    #[test]
    fn a_panic_payload_whose_destructor_panics_stays_contained() {
        struct Bomb;
        impl Drop for Bomb {
            fn drop(&mut self) {
                panic!("payload destructor");
            }
        }
        let dir = tempfile::tempdir().unwrap();
        let cat = Catalog::open(dir.path(), Default::default()).unwrap();
        let ds = cat.create("source", &Default::default()).unwrap();
        install(&ds, &cat, |c| {
            c.defer(|_| std::panic::panic_any(Bomb));
            c.defer(|c| {
                c.create("other", &Default::default()).unwrap();
            });
        });
        drop(ds);
        let worker = cat.clone();
        assert!(bounded(move || worker.delete("source")).unwrap());
        assert!(cat.get("other").is_some());
    }

    /// A reservation that a close callback uses once.
    fn take_once(r: Reservation) -> impl Fn() -> Option<Reservation> + Send + Sync + 'static {
        let slot = parking_lot::Mutex::new(Some(r));
        move || slot.lock().take()
    }

    #[test]
    fn a_refused_reserved_clone_publishes_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let cat = Catalog::open(dir.path(), Default::default()).unwrap();
        cat.create("src", &Default::default()).unwrap();
        let ds = cat.create("source", &Default::default()).unwrap();
        let reservation = take_once(cat.reserve("copy", ReservationKind::Clone, "t").unwrap());
        let seen = Seen::default();
        let s = seen.clone();
        install(&ds, &cat, move |c| {
            let r = reservation().expect("one close");
            *s.lock() = Some(
                c.clone_reserved("src", r, &Default::default(), &Default::default())
                    .map(drop),
            );
        });
        drop(ds);
        let worker = cat.clone();
        assert!(bounded(move || worker.delete("source")).unwrap());
        assert_disposing(&seen);
        let databases = dir.path().join("databases");
        assert!(!databases.join("copy").exists());
        assert!(
            std::fs::read_dir(&databases).unwrap().all(|e| !e
                .unwrap()
                .file_name()
                .to_string_lossy()
                .starts_with(".clone-")),
            "no staged copy remains"
        );
        assert!(cat.reserved_by("copy").is_none());
        cat.clone_dataset("src", "copy", &Default::default(), &Default::default())
            .unwrap();
    }

    #[cfg(feature = "backup")]
    #[test]
    fn a_refused_restore_publication_keeps_its_staging_directory() {
        let dir = tempfile::tempdir().unwrap();
        let cat = Catalog::open(dir.path(), Default::default()).unwrap();
        let ds = cat.create("source", &Default::default()).unwrap();
        let staged = dir.path().join("databases").join(".restore-x-1");
        let id = Store::open(&staged, Default::default())
            .unwrap()
            .dataset_id();
        let reservation = take_once(cat.reserve("x", ReservationKind::Restore, "1").unwrap());
        let seen = Seen::default();
        let s = seen.clone();
        let path = staged.clone();
        install(&ds, &cat, move |c| {
            let r = reservation().expect("one close");
            *s.lock() = Some(c.publish_restored(r, &path).map(drop));
        });
        drop(ds);
        let worker = cat.clone();
        assert!(bounded(move || worker.delete("source")).unwrap());
        let seen = seen.lock().take().expect("the close callback ran");
        assert!(
            matches!(&seen, Err(Error::Conflict(m)) if m.contains("catalog-disposing")),
            "{seen:?}"
        );
        assert!(
            staged.join("CURRENT").exists(),
            "the staging directory stays"
        );
        assert!(!dir.path().join("databases").join("x").exists());
        assert!(cat.reserved_by("x").is_none());
        let r = cat.reserve("x", ReservationKind::Restore, "2").unwrap();
        assert_eq!(cat.publish_restored(r, &staged).unwrap().dataset_id(), id);
    }
}
