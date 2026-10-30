//! Runtime-family prerequisites. Exclusive admission is a test consumer here;
//! the production family close and revocable alias protocol are separate work.
use super::*;
use crate::guard::WriteOptions;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::mpsc;
use std::time::{Duration, Instant};

fn no_wait() -> WriteOptions {
    WriteOptions {
        no_wait: true,
        ..Default::default()
    }
}

fn grouped_options() -> StoreOptions {
    StoreOptions {
        group_commit: true,
        group_commit_idle_direct: false,
        ..Default::default()
    }
}

fn insert(store: &Store) -> Result<()> {
    crate::sparql::update::update(
        store,
        "INSERT DATA { <urn:s> <urn:p> <urn:o> }",
        &Default::default(),
    )?;
    Ok(())
}

#[test]
fn memory_members_share_one_opening_without_a_family_cycle() {
    let main = Store::in_memory(Default::default());
    let separate = Store::in_memory(Default::default());
    main.create_branch("member", &Default::default()).unwrap();
    let member = main.branch("member").unwrap().shared().unwrap();
    assert!(Arc::ptr_eq(&main.family, &member.family));
    assert!(!Arc::ptr_eq(&main.family, &separate.family));
    let family = Arc::downgrade(&main.family);
    drop(main);
    assert!(member.branching.set().is_none());
    insert(&member).unwrap();
    drop(member);
    assert!(
        family.upgrade().is_none(),
        "no family-to-store or set cycle"
    );
}

#[test]
fn source_identity_rejects_another_family_with_identical_stored_uuids() {
    use crate::access::source::{ArcSourceOwner, SampledSource, Target};
    let main = Arc::new(Store::in_memory(Default::default()));
    let mut foreign = Store::in_memory(Default::default());
    foreign.dataset_id = main.dataset_id();
    let owner = ArcSourceOwner::new(main.clone(), Arc::new(foreign));
    let target = Target {
        dataset: main.dataset_id(),
        branch: main.branch_id(),
    };
    assert!(SampledSource::sample(&owner, target).is_err());
    main.create_branch("member", &Default::default()).unwrap();
    let member = main.branch("member").unwrap().shared().unwrap();
    let owner = ArcSourceOwner::new(main.clone(), member.clone());
    let target = Target {
        dataset: main.dataset_id(),
        branch: member.branch_id(),
    };
    let sampled = SampledSource::sample(&owner, target).unwrap();
    assert!(sampled.observes(main.as_ref(), member.as_ref()));
    let mut foreign_member = Store::in_memory(Default::default());
    foreign_member.dataset_id = member.dataset_id();
    foreign_member.branching.ident = member.branching.ident.clone();
    let foreign_owner = ArcSourceOwner::new(main.clone(), Arc::new(foreign_member));
    assert!(SampledSource::sample(&foreign_owner, target).is_err());
    let reversed = ArcSourceOwner::new(member.clone(), main.clone());
    assert!(SampledSource::sample(&reversed, target).is_err());
}

#[test]
fn lazy_persistent_members_reuse_only_the_current_opening_identity() {
    let directory = tempfile::tempdir().unwrap();
    let main = Store::open(directory.path(), Default::default()).unwrap();
    let dataset_id = main.dataset_id();
    main.create_branch("member", &Default::default()).unwrap();
    let family = Arc::downgrade(&main.family);
    drop(main);
    assert!(family.upgrade().is_none());
    let reopened = Store::open(directory.path(), Default::default()).unwrap();
    assert_eq!(reopened.dataset_id(), dataset_id);
    assert!(!family.ptr_eq(&Arc::downgrade(&reopened.family)));
    let member = reopened.branch("member").unwrap().shared().unwrap();
    assert!(Arc::ptr_eq(&reopened.family, &member.family));
}

#[test]
fn retained_set_can_open_members_without_losing_root_exclusion() {
    let directory = tempfile::tempdir().unwrap();
    let main = Store::open(directory.path(), Default::default()).unwrap();
    let info = main.create_branch("member", &Default::default()).unwrap();
    let set = main.branching.set().unwrap();
    let family = Arc::downgrade(&main.family);
    drop(main);
    assert!(matches!(
        lock_database(directory.path()),
        Err(Error::Locked { .. })
    ));
    let member = set.open_store(info.id).unwrap();
    assert!(family.ptr_eq(&Arc::downgrade(&member.family)));
    drop(set);
    assert!(matches!(
        lock_database(directory.path()),
        Err(Error::Locked { .. })
    ));
    drop(member);
    assert!(family.upgrade().is_none());
    drop(lock_database(directory.path()).unwrap());
}

/// Port of the managed prototype's actual process-exclusion control, using only
/// main's ordinary branch and write APIs.
#[test]
fn retained_member_keeps_root_exclusion_until_its_last_owner_closes() {
    let directory = tempfile::tempdir().unwrap();
    let main = Store::open(directory.path(), Default::default()).unwrap();
    main.create_branch("member", &Default::default()).unwrap();
    let member = main.branch("member").unwrap().shared().unwrap();
    drop(main);
    assert!(matches!(
        Store::open(directory.path(), Default::default()),
        Err(Error::Locked { .. })
    ));
    insert(&member).unwrap();
    drop(member);
    let reopened = Store::open(directory.path(), Default::default()).unwrap();
    assert_eq!(reopened.branch("member").unwrap().snapshot().len(), 1);
}

#[test]
fn removed_and_retired_members_keep_exclusion_through_field_destruction() {
    struct Probe {
        root: PathBuf,
        observed: Arc<AtomicBool>,
    }
    impl Drop for Probe {
        fn drop(&mut self) {
            self.observed.store(
                matches!(lock_database(&self.root), Err(Error::Locked { .. })),
                Ordering::SeqCst,
            );
        }
    }
    for retire in [false, true] {
        let directory = tempfile::tempdir().unwrap();
        let main = Store::open(directory.path(), Default::default()).unwrap();
        main.create_branch("member", &Default::default()).unwrap();
        let member = main.branch("member").unwrap().shared().unwrap();
        if retire {
            main.create_branch(
                "child",
                &crate::branch::BranchOptions {
                    from: "member".into(),
                    ..Default::default()
                },
            )
            .unwrap();
        }
        main.delete_branch_with(
            "member",
            &crate::branch::DeleteOptions {
                force: true,
                reparent: retire,
            },
        )
        .unwrap();
        assert_eq!(member.branching.retired.load(Ordering::Relaxed), retire);
        let family = Arc::downgrade(&main.family);
        let observed = Arc::new(AtomicBool::new(false));
        let probe = Probe {
            root: directory.path().to_owned(),
            observed: observed.clone(),
        };
        *member.clock.lock() = Some(Arc::new(move || {
            let _keep_until_clock_drop = &probe;
            0
        }));
        drop(main);
        assert!(matches!(
            lock_database(directory.path()),
            Err(Error::Locked { .. })
        ));
        assert!(!observed.load(Ordering::SeqCst));
        drop(member);
        assert!(observed.load(Ordering::SeqCst));
        assert!(family.upgrade().is_none());
        drop(lock_database(directory.path()).unwrap());
    }
}

#[test]
fn exclusion_covers_existing_members_and_waiting_writers() {
    let main = Arc::new(Store::in_memory(Default::default()));
    main.create_branch("member", &Default::default()).unwrap();
    let member = main.branch("member").unwrap().shared().unwrap();
    let exclusive = main.family.gate.admit(&no_wait()).unwrap();
    assert!(exclusive.belongs_to(&member.family.gate));
    assert!(!exclusive.belongs_to(&family::Family::new(None).gate));
    for store in [&*main, &*member] {
        assert!(matches!(
            store.try_write_with(CommitKind::Update, no_wait()),
            Err(Error::WriterBusy)
        ));
        assert!(matches!(
            store.try_write_with(
                CommitKind::Update,
                WriteOptions {
                    deadline: Some(Instant::now() + Duration::from_millis(20)),
                    ..Default::default()
                },
            ),
            Err(Error::Timeout)
        ));
        assert!(matches!(
            store.try_write_with(
                CommitKind::Update,
                WriteOptions {
                    cancellation: Some(crate::task::Cancellation::from_flags([Arc::new(
                        AtomicBool::new(true)
                    ),])),
                    ..Default::default()
                },
            ),
            Err(Error::Cancelled)
        ));
    }
    drop(exclusive);
    let active = main.write();
    assert!(matches!(
        main.family.gate.admit(&no_wait()),
        Err(Error::WriterBusy)
    ));
    drop(active);

    let raw = main.writer.lock();
    let (entered_tx, entered_rx) = mpsc::channel();
    main.set_failpoint(
        "writer-acquiring",
        Some(Arc::new(move |_| entered_tx.send(()).unwrap())),
    );
    let writer = std::thread::spawn({
        let main = main.clone();
        move || {
            drop(
                main.try_write_with(CommitKind::Update, Default::default())
                    .unwrap(),
            );
        }
    });
    entered_rx.recv_timeout(Duration::from_secs(5)).unwrap();
    assert!(matches!(
        main.family.gate.admit(&no_wait()),
        Err(Error::WriterBusy)
    ));
    drop(raw);
    writer.join().unwrap();
    main.set_failpoint("writer-acquiring", None);
    drop(main.family.gate.admit(&no_wait()).unwrap());
}

#[test]
fn blocking_write_keeps_its_existing_admission_behavior() {
    let main = Arc::new(Store::in_memory(Default::default()));
    let exclusive = main.family.gate.admit(&no_wait()).unwrap();
    let (started_tx, started_rx) = mpsc::channel();
    let (entered_tx, entered_rx) = mpsc::channel();
    let writer = std::thread::spawn({
        let main = main.clone();
        move || {
            started_tx.send(()).unwrap();
            // write_with has always acquired its mutex unconditionally. These
            // controls are checked by the transaction, not by blocking admission.
            let txn = main.write_with(
                CommitKind::Update,
                WriteOptions {
                    no_wait: true,
                    cancel: Some(Arc::new(AtomicBool::new(true))),
                    deadline: Some(Instant::now()),
                    ..Default::default()
                },
            );
            entered_tx.send(()).unwrap();
            drop(txn);
        }
    });
    started_rx.recv_timeout(Duration::from_secs(5)).unwrap();
    assert!(entered_rx.recv_timeout(Duration::from_millis(20)).is_err());
    drop(exclusive);
    entered_rx.recv_timeout(Duration::from_secs(5)).unwrap();
    writer.join().unwrap();
}

#[test]
fn writer_mutex_drops_before_the_last_permit_on_normal_and_unwind_paths() {
    for unwind in [false, true] {
        let main = Arc::new(Store::in_memory(Default::default()));
        let (held_tx, held_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let worker = std::thread::spawn({
            let main = main.clone();
            move || {
                let result = catch_unwind(AssertUnwindSafe(|| {
                    let _writer = main.raw_writer(&Default::default()).unwrap();
                    held_tx.send(()).unwrap();
                    release_rx.recv_timeout(Duration::from_secs(5)).unwrap();
                    assert!(!unwind, "release writer on unwind");
                }));
                assert_eq!(result.is_err(), unwind);
            }
        });
        held_rx.recv_timeout(Duration::from_secs(5)).unwrap();
        // Permit destruction must wait for this actual mutex. Its branch writer
        // must already be unlocked while that destruction is waiting.
        main.family.gate.with_locked_state(|| {
            release_tx.send(()).unwrap();
            let deadline = Instant::now() + Duration::from_secs(5);
            loop {
                if let Some(writer) = main.writer.try_lock() {
                    drop(writer);
                    break;
                }
                assert!(
                    Instant::now() < deadline,
                    "permit dropped before writer mutex"
                );
                std::thread::yield_now();
            }
        });
        worker.join().unwrap();
        drop(main.family.gate.admit(&no_wait()).unwrap());
    }
}

#[test]
fn writer_lock_refusal_and_panic_release_the_family_permit() {
    let main = Store::in_memory(Default::default());
    let raw = main.writer.lock();
    assert!(matches!(
        main.try_write_with(CommitKind::Update, no_wait()),
        Err(Error::WriterBusy)
    ));
    assert!(matches!(
        main.try_write_with(
            CommitKind::Update,
            WriteOptions {
                deadline: Some(Instant::now() + Duration::from_millis(20)),
                ..Default::default()
            },
        ),
        Err(Error::Timeout)
    ));
    drop(raw);
    drop(main.family.gate.admit(&no_wait()).unwrap());
    main.set_failpoint(
        "writer-acquiring",
        Some(Arc::new(|_| panic!("before writer lock"))),
    );
    assert!(
        catch_unwind(AssertUnwindSafe(|| {
            main.try_write_with(CommitKind::Update, Default::default())
                .ok();
        }))
        .is_err()
    );
    main.set_failpoint("writer-acquiring", None);
    drop(main.family.gate.admit(&no_wait()).unwrap());
}

#[test]
fn pending_and_writer_share_one_permit_until_both_finish() {
    for fail_sync in [false, true] {
        let directory = tempfile::tempdir().unwrap();
        let main = Arc::new(Store::open(directory.path(), grouped_options()).unwrap());
        let (queued_tx, queued_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let release_rx = std::sync::Mutex::new(release_rx);
        main.set_failpoint(
            "group-enqueued-writer-held",
            Some(Arc::new(move |_| {
                queued_tx.send(()).unwrap();
                release_rx
                    .lock()
                    .unwrap()
                    .recv_timeout(Duration::from_secs(5))
                    .unwrap();
            })),
        );
        if fail_sync {
            main.fail_next_group_sync();
        }
        let writer = std::thread::spawn({
            let main = main.clone();
            move || insert(&main)
        });
        queued_rx.recv_timeout(Duration::from_secs(5)).unwrap();
        let drained = main.group.as_ref().unwrap().drain(&main, None);
        assert_eq!(drained.is_err(), fail_sync);
        assert!(matches!(
            main.family.gate.admit(&no_wait()),
            Err(Error::WriterBusy)
        ));
        assert!(main.writer.try_lock().is_none());
        release_tx.send(()).unwrap();
        assert_eq!(writer.join().unwrap().is_err(), fail_sync);
        main.set_failpoint("group-enqueued-writer-held", None);
        drop(main.family.gate.admit(&no_wait()).unwrap());
        assert_eq!(main.snapshot().commit, if fail_sync { 0 } else { 1 });
    }
}

#[test]
fn orphaned_pending_refuses_exclusion_without_writes_until_drain_or_refusal() {
    for fail_sync in [false, true] {
        for point in ["group-enqueued-writer-held", "group-appended"] {
            let directory = tempfile::tempdir().unwrap();
            let main = Store::open(directory.path(), grouped_options()).unwrap();
            main.set_failpoint(point, Some(Arc::new(|_| panic!("abandon queued commit"))));
            assert!(catch_unwind(AssertUnwindSafe(|| insert(&main))).is_err());
            main.set_failpoint(point, None);
            assert!(main.writer.try_lock().is_some());
            let current = std::fs::read(directory.path().join("CURRENT")).unwrap();
            let wal = directory
                .path()
                .join(main.snapshot().generation.name.clone())
                .join("wal.log");
            let wal_before = std::fs::read(&wal).unwrap();
            assert!(matches!(
                main.family.gate.admit(&no_wait()),
                Err(Error::WriterBusy)
            ));
            assert_eq!(
                std::fs::read(directory.path().join("CURRENT")).unwrap(),
                current
            );
            assert_eq!(std::fs::read(&wal).unwrap(), wal_before);
            assert_eq!(main.snapshot().commit, 0);
            if fail_sync {
                main.fail_next_group_sync();
            }
            let drained = main.group.as_ref().unwrap().drain(&main, None);
            assert_eq!(drained.is_err(), fail_sync);
            drop(main.family.gate.admit(&no_wait()).unwrap());
            assert_eq!(main.snapshot().commit, if fail_sync { 0 } else { 1 });
        }
    }
}

#[test]
fn admission_queue_cleans_up_on_cancel_error_and_unwind() {
    let family = family::Family::new(None);
    let shared = family.gate.enter(&Default::default()).unwrap();
    let cancel = Arc::new(AtomicBool::new(false));
    let (queued_tx, queued_rx) = mpsc::channel();
    let admission = std::thread::spawn({
        let gate = family.gate.clone();
        let cancel = cancel.clone();
        move || {
            let mut queued_tx = Some(queued_tx);
            gate.admit_draining(
                &WriteOptions {
                    cancel: Some(cancel),
                    ..Default::default()
                },
                || {
                    if let Some(tx) = queued_tx.take() {
                        tx.send(()).unwrap();
                    }
                    Ok(())
                },
            )
            .map(|_| ())
        }
    });
    queued_rx.recv_timeout(Duration::from_secs(5)).unwrap();
    assert_eq!(family.gate.waiting_admissions(), 1);
    assert!(matches!(
        family.gate.enter(&no_wait()),
        Err(Error::WriterBusy)
    ));
    cancel.store(true, Ordering::Release);
    assert!(matches!(admission.join().unwrap(), Err(Error::Cancelled)));
    assert_eq!(family.gate.waiting_admissions(), 0);
    assert!(matches!(
        family
            .gate
            .admit_draining(&Default::default(), || Err(Error::Cancelled)),
        Err(Error::Cancelled)
    ));
    assert_eq!(family.gate.waiting_admissions(), 0);
    assert!(
        catch_unwind(AssertUnwindSafe(|| {
            family
                .gate
                .admit_draining(&Default::default(), || panic!("admission progress"))
                .ok();
        }))
        .is_err()
    );
    assert_eq!(family.gate.waiting_admissions(), 0);
    drop(shared);
    drop(family.gate.admit(&no_wait()).unwrap());
}

#[test]
fn gate_mutex_contention_honors_no_wait_deadline_and_cancel() {
    let family = family::Family::new(None);
    family.gate.with_locked_state(|| {
        std::thread::scope(|scope| {
            scope.spawn(|| {
                assert!(matches!(
                    family.gate.enter(&no_wait()),
                    Err(Error::WriterBusy)
                ));
                assert!(matches!(
                    family.gate.enter(&WriteOptions {
                        deadline: Some(Instant::now() + Duration::from_millis(20)),
                        ..Default::default()
                    }),
                    Err(Error::Timeout)
                ));
                assert!(matches!(
                    family.gate.enter(&WriteOptions {
                        cancel: Some(Arc::new(AtomicBool::new(true))),
                        ..Default::default()
                    }),
                    Err(Error::Cancelled)
                ));
            });
        });
    });
    drop(family.gate.admit(&no_wait()).unwrap());
}

#[test]
fn branch_lifecycle_waits_outside_table_locks() {
    let main = Arc::new(Store::in_memory(Default::default()));
    main.create_branch("member", &Default::default()).unwrap();
    for operation in 0..5 {
        let exclusive = main.family.gate.admit(&no_wait()).unwrap();
        let (started_tx, started_rx) = mpsc::channel();
        let (done_tx, done_rx) = mpsc::channel();
        let worker = std::thread::spawn({
            let main = main.clone();
            move || {
                started_tx.send(()).unwrap();
                match operation {
                    0 => {
                        main.create_branch("new", &Default::default()).unwrap();
                    }
                    1 => {
                        main.rename_branch("new", "renamed").unwrap();
                    }
                    2 => {
                        main.set_branch_protected("member", true).unwrap();
                    }
                    3 => {
                        main.set_merge_exempt(&[]).unwrap();
                    }
                    _ => main.delete_branch("renamed", true).unwrap(),
                }
                done_tx.send(()).unwrap();
            }
        });
        started_rx.recv_timeout(Duration::from_secs(5)).unwrap();
        assert!(done_rx.recv_timeout(Duration::from_millis(20)).is_err());
        // Listing takes the same table mutex: admission waiters must not hold it.
        main.branches().unwrap();
        drop(exclusive);
        done_rx.recv_timeout(Duration::from_secs(5)).unwrap();
        worker.join().unwrap();
    }
}
