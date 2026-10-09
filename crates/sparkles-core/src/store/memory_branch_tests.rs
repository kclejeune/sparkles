use super::branch_tests::{apply, code, dump, has, merge_historical, merged};
use super::*;
use crate::branch::{BranchOptions, DeleteOptions};
use crate::history::At;
use std::collections::BTreeSet;

#[test]
fn memory_branches_share_only_immutable_base_and_survive_rollback() {
    let s = Store::in_memory(StoreOptions::default());
    apply(&s, "+<urn:a> <urn:p> \"base\" .");
    let fork = s.create_branch("work", &BranchOptions::default()).unwrap();
    let dev = s.branch("work").unwrap();
    let (a, b) = (s.snapshot(), dev.snapshot());
    assert!(Arc::ptr_eq(&a.generation.vocab, &b.generation.vocab));
    assert_ne!(a.generation.uid, b.generation.uid);
    assert_eq!(dev.head_commit().seq, 1);
    assert_eq!(dump(&s), dump(&dev));
    let mut tx = s.write();
    let q = oxrdf::Quad::new(
        oxrdf::NamedNode::new("urn:pending").unwrap(),
        oxrdf::NamedNode::new("urn:new-predicate").unwrap(),
        oxrdf::Literal::new_simple_literal("pending"),
        oxrdf::GraphName::DefaultGraph,
    );
    let ids = tx.encode_quad(&q, &mut Default::default()).unwrap();
    tx.insert(ids).unwrap();
    apply(&dev, "+<urn:branch> <urn:branch-predicate> \"committed\" .");
    drop(tx);
    assert!(has(&dev, "committed"));
    assert!(!has(&s, "pending"));
    apply(&s, "+<urn:main> <urn:main-predicate> \"committed-main\" .");
    assert_eq!(s.head_commit().seq, dev.head_commit().seq);
    assert_ne!(dump(&s), dump(&dev));
    let r = merged(s.merge("work", "main", &Default::default()).unwrap());
    assert_eq!(r.inserted, 1);
    assert!(has(&s, "committed"));
    assert_eq!(s.branch_info("work").unwrap().id, fork.id);
}

#[test]
fn memory_branch_bases_outlive_history_rings_and_retired_parents() {
    let s = Store::in_memory(StoreOptions {
        memory_commit_ring: 2,
        ..Default::default()
    });
    apply(&s, "+<urn:a> <urn:p> \"1\" .");
    s.create_branch("work", &Default::default()).unwrap();
    let work = s.branch("work").unwrap();
    apply(&work, "+<urn:b> <urn:p> \"2\" .");
    s.create_branch(
        "child",
        &BranchOptions {
            from: "work".into(),
            ..Default::default()
        },
    )
    .unwrap();
    for i in 0..6 {
        apply(&s, &format!("+<urn:main{i}> <urn:p> \"{i}\" ."));
    }
    s.delete_branch_with(
        "work",
        &DeleteOptions {
            force: true,
            reparent: true,
        },
    )
    .unwrap();
    let child = s.branch("child").unwrap();
    apply(&child, "+<urn:c> <urn:p> \"3\" .");
    merged(s.merge("child", "main", &Default::default()).unwrap());
    assert!(has(&s, "urn:b") && has(&s, "urn:c"));
    assert!(s.branch("work").is_err());
    let before = child.dataset_id();
    s.create_branch("work", &Default::default()).unwrap();
    assert_ne!(s.branch("work").unwrap().dataset_id(), before);
    s.rename_branch("child", "renamed").unwrap();
    assert_eq!(s.branch("renamed").unwrap().dataset_id(), before);
    assert!(
        crate::sparql::update::update(
            &work,
            "INSERT DATA { <urn:retired> <urn:p> 1 }",
            &Default::default()
        )
        .is_err()
    );
}

#[test]
fn memory_branch_can_fork_a_pin_and_backup_as_a_standalone_store() {
    let s = Store::in_memory(StoreOptions::default());
    apply(&s, "+_:original <urn:p> \"1\" .");
    s.create_snapshot("first", &At::Head, None).unwrap();
    apply(&s, "+<urn:later> <urn:p> \"2\" .");
    s.create_branch(
        "work",
        &BranchOptions {
            at: At::Snapshot("first".into()),
            ..Default::default()
        },
    )
    .unwrap();
    let work = s.branch("work").unwrap();
    assert!(!has(&work, "urn:later"));
    apply(&work, "+_:new <urn:p> \"3\" .");
    let tmp = tempfile::tempdir().unwrap();
    let cap = work
        .materialized_backup_capture(
            "branch",
            &MemoryCaptureOptions {
                tmp_dir: tmp.path().into(),
                ..Default::default()
            },
        )
        .unwrap();
    assert!(cap.in_memory && cap.materialized);
    assert_eq!(cap.branch.as_ref().unwrap().id, work.dataset_id());
    let out = tmp.path().join("copy");
    cap.write_to(&out).unwrap();
    let restored = Store::open(&out, Default::default()).unwrap();
    assert_eq!(dump(&restored), dump(&work));
    assert_eq!(restored.head_commit().seq, work.head_commit().seq);
    restored.create_branch("next", &Default::default()).unwrap();
    assert_eq!(
        restored.branch_info("next").unwrap().ordinal as u32,
        cap.next_ordinal
    );
}

#[test]
fn unsupported_dataset_reader_fails_before_recovery_changes_files() {
    let tmp = tempfile::tempdir().unwrap();
    let s = Store::open(tmp.path(), Default::default()).unwrap();
    apply(&s, "+<urn:a> <urn:p> \"1\" .");
    drop(s);
    let file = tmp.path().join("dataset.json");
    let mut ds: serde_json::Value = serde_json::from_slice(&std::fs::read(&file).unwrap()).unwrap();
    ds["minimumReader"] = serde_json::json!(crate::commit::DATASET_READER + 1);
    std::fs::write(&file, serde_json::to_vec(&ds).unwrap()).unwrap();
    let current = std::fs::read(tmp.path().join("CURRENT")).unwrap();
    let wal = tmp.path().join("gen-0001/wal.log");
    let before = std::fs::read(&wal).unwrap();
    assert!(matches!(
        Store::open(tmp.path(), Default::default()),
        Err(Error::Unsupported(_))
    ));
    assert_eq!(std::fs::read(&wal).unwrap(), before);
    assert_eq!(std::fs::read(tmp.path().join("CURRENT")).unwrap(), current);
}

#[test]
fn linked_branch_capture_is_independent_and_preserves_persistent_kind() {
    let (tmp, s) = super::branch_tests::setup();
    s.create_branch("work", &Default::default()).unwrap();
    let work = s.branch("work").unwrap();
    apply(&work, "+<urn:new> <urn:p> \"3\" .");
    let cap = work
        .materialized_backup_capture(
            "backup",
            &MemoryCaptureOptions {
                tmp_dir: tmp.path().join("tmp"),
                ..Default::default()
            },
        )
        .unwrap();
    assert!(!cap.in_memory);
    assert!(cap.materialized);
    assert!(!cap.files.iter().any(|f| f.path.ends_with("link.json")));
    cap.write_to(&tmp.path().join("copy")).unwrap();
    let restored = Store::open(&tmp.path().join("copy"), Default::default()).unwrap();
    assert_eq!(dump(&restored), dump(&work));
    assert!(work.snapshot().generation.linked().is_some());
}

#[test]
fn deleted_memory_branches_release_obsolete_snapshot_holds() {
    let s = Store::in_memory(StoreOptions {
        memory_commit_ring: 2,
        ..Default::default()
    });
    apply(&s, "+<urn:a> <urn:p> \"base\" .");
    let base = Arc::downgrade(&s.snapshot());
    s.create_branch("work", &Default::default()).unwrap();
    let work = s.branch("work").unwrap();
    for i in 0..6 {
        apply(&s, &format!("+<urn:m{i}> <urn:p> \"{i}\" ."));
    }
    assert!(
        base.upgrade().is_some(),
        "live fork retains its source snapshot"
    );
    s.delete_branch("work", true).unwrap();
    assert!(
        base.upgrade().is_none(),
        "removed fork releases its source snapshot"
    );
    assert!(
        work.mem_history
            .as_ref()
            .unwrap()
            .lock()
            .branch_bases
            .is_empty()
    );
}

#[test]
fn capture_admission_honors_deadline_and_cancellation_while_writer_is_held() {
    use std::sync::mpsc;
    use std::time::{Duration, Instant};
    for memory in [false, true] {
        for backup in [false, true] {
            for timeout in [false, true] {
                let dir = tempfile::tempdir().unwrap();
                let s = Arc::new(if memory {
                    Store::in_memory(Default::default())
                } else {
                    Store::open(&dir.path().join("source"), Default::default()).unwrap()
                });
                let writer = s.write();
                let busy = if backup && memory {
                    s.materialized_backup_capture(
                        "busy",
                        &MemoryCaptureOptions {
                            tmp_dir: dir.path().join("busy"),
                            no_wait: true,
                            ..Default::default()
                        },
                    )
                    .map(|_| ())
                } else if backup {
                    s.backup_capture_with(
                        "busy",
                        &crate::guard::WriteOptions {
                            no_wait: true,
                            ..Default::default()
                        },
                    )
                    .map(|_| ())
                } else {
                    s.clone_to_memory(
                        &CloneOptions {
                            no_wait: true,
                            ..Default::default()
                        },
                        Default::default(),
                    )
                    .map(|_| ())
                };
                assert!(matches!(busy, Err(Error::WriterBusy)));
                let flag = Arc::new(AtomicBool::new(false));
                let (send, receive) = mpsc::channel();
                let source = s.clone();
                let cancel = flag.clone();
                let scratch = dir.path().join("scratch");
                let deadline = timeout.then(|| Instant::now() + Duration::from_millis(50));
                let task = std::thread::spawn(move || {
                    let result = if backup && memory {
                        source
                            .materialized_backup_capture(
                                "test",
                                &MemoryCaptureOptions {
                                    tmp_dir: scratch,
                                    cancel: Some(cancel),
                                    deadline,
                                    ..Default::default()
                                },
                            )
                            .map(|_| ())
                    } else if backup {
                        source
                            .backup_capture_with(
                                "test",
                                &crate::guard::WriteOptions {
                                    cancel: Some(cancel),
                                    deadline,
                                    ..Default::default()
                                },
                            )
                            .map(|_| ())
                    } else {
                        source
                            .clone_to_memory(
                                &CloneOptions {
                                    cancel: Some(cancel),
                                    deadline,
                                    ..Default::default()
                                },
                                Default::default(),
                            )
                            .map(|_| ())
                    };
                    send.send(result).unwrap();
                });
                if !timeout {
                    flag.store(true, Ordering::Relaxed);
                }
                let received = receive.recv_timeout(Duration::from_secs(2));
                // Always release and join, so failure cannot hang the test process.
                drop(writer);
                task.join().unwrap();
                let result = received.expect("capture must return before the writer is released");
                assert!(
                    if timeout {
                        matches!(result, Err(Error::Timeout))
                    } else {
                        matches!(result, Err(Error::Cancelled))
                    },
                    "{result:?}"
                );
                assert!(!dir.path().join("scratch").exists());
                assert_eq!(s.snapshot().len(), 0);
            }
        }
    }
}

#[test]
fn concurrent_memory_branch_readers_only_observe_initialized_stores() {
    let store = Arc::new(Store::in_memory(StoreOptions {
        max_branches: 128,
        ..Default::default()
    }));
    let barrier = Arc::new(std::sync::Barrier::new(2));
    std::thread::scope(|scope| {
        let creator = store.clone();
        let start = barrier.clone();
        let writer = scope.spawn(move || {
            start.wait();
            for i in 0..100 {
                creator
                    .create_branch(&format!("work{i}"), &Default::default())
                    .unwrap();
            }
        });
        barrier.wait();
        loop {
            for branch in store.branches().unwrap() {
                let opened = store.branch(&branch.name).unwrap();
                assert_eq!(opened.dataset_id(), branch.id);
                assert!(!opened.is_persistent());
            }
            if writer.is_finished() {
                break;
            }
            std::thread::yield_now();
        }
        writer.join().unwrap();
    });
    assert_eq!(store.branches().unwrap().len(), 101);
}

#[test]
fn expired_memory_commit_resolution_and_snapshot_listing_do_not_deadlock() {
    let store = Arc::new(Store::in_memory(StoreOptions {
        memory_commit_ring: 2,
        max_snapshots: 256,
        ..Default::default()
    }));
    for i in 0..128 {
        store
            .create_snapshot(&format!("pin{i}"), &At::Head, None)
            .unwrap();
    }
    for i in 0..4 {
        apply(&store, &format!("+<urn:n{i}> <urn:p> \"value\" ."));
    }
    let barrier = Arc::new(std::sync::Barrier::new(3));
    let (send, receive) = std::sync::mpsc::channel();
    let mut readers = Vec::new();
    for resolve in [false, true] {
        let store = store.clone();
        let barrier = barrier.clone();
        let send = send.clone();
        readers.push(std::thread::spawn(move || {
            barrier.wait();
            for _ in 0..5000 {
                if resolve {
                    assert!(store.resolve(&At::Commit(0)).is_err());
                } else {
                    assert_eq!(store.snapshots().len(), 128);
                }
            }
            send.send(()).unwrap();
        }));
    }
    barrier.wait();
    for _ in 0..2 {
        receive
            .recv_timeout(std::time::Duration::from_secs(10))
            .expect("memory history readers must finish without a lock cycle");
    }
    for reader in readers {
        reader.join().unwrap();
    }
}

/// Only fork points are kept for merges beyond the commit ring. A merge base that is
/// an earlier merge's source commit, or a criss-cross anchor, is read through the
/// commit ring, and a merge after its eviction fails with `merge-base-gone`.
#[test]
fn memory_merge_bases_past_the_commit_ring_are_gone() {
    let ring = StoreOptions {
        memory_commit_ring: 2,
        ..Default::default()
    };
    let churn = |s: &Store, dev: &Store, tag: &str| {
        for i in 0..6 {
            apply(s, &format!("+<urn:m{tag}{i}> <urn:p> \"{i}\" ."));
            apply(dev, &format!("+<urn:d{tag}{i}> <urn:p> \"{i}\" ."));
        }
    };
    // a base that is the source commit of an earlier merge
    let s = Store::in_memory(ring.clone());
    apply(&s, "+<urn:a> <urn:p> \"1\" .");
    s.create_branch("dev", &Default::default()).unwrap();
    let dev = s.branch("dev").unwrap();
    apply(&dev, "+<urn:d0> <urn:p> \"x\" .");
    merged(s.merge("dev", "main", &Default::default()).unwrap());
    churn(&s, &dev, "a");
    let e = s.merge("dev", "main", &Default::default()).unwrap_err();
    assert_eq!(code(&e), "merge-base-gone");
    // a criss-cross, whose virtual base's anchors are ring commits too
    let s = Store::in_memory(ring);
    s.create_branch("dev", &Default::default()).unwrap();
    let dev = s.branch("dev").unwrap();
    apply(&s, "+<urn:m> <urn:p> <urn:x> .");
    apply(&dev, "+<urn:d> <urn:p> <urn:x> .");
    let before = s.head_commit().seq;
    merged(s.merge("dev", "main", &Default::default()).unwrap());
    merge_historical(&s, "main", before, "dev", &Default::default());
    churn(&s, &dev, "b");
    assert_eq!(s.merge_base("dev", "main").unwrap().len(), 2);
    let e = s.merge("dev", "main", &Default::default()).unwrap_err();
    assert_eq!(code(&e), "merge-base-gone");
    // the refused merge wrote nothing
    assert!(!has(&s, "urn:db5"));
}

/// The branch table is not locked while a memory branch's store and indexes are
/// built, and a creation that loses its name meanwhile publishes nothing.
#[test]
fn memory_branch_indexes_build_outside_the_table_lock() {
    use std::sync::mpsc;
    use std::time::Duration;
    let s = Store::in_memory(StoreOptions::default());
    apply(&s, "+<urn:a> <urn:p> \"1\" .");
    let (send, receive) = mpsc::channel();
    let send = Mutex::new(send);
    let once = AtomicBool::new(false);
    s.set_failpoint(
        "memory-branch-indexes",
        Some(Arc::new(move |store: &Store| {
            if once.swap(true, Ordering::SeqCst) {
                return;
            }
            let set = store.branch_set().unwrap();
            let (tx, rx) = mpsc::channel();
            std::thread::spawn(move || tx.send(set.table_len()).unwrap());
            let read = rx.recv_timeout(Duration::from_secs(5)).is_ok();
            // a concurrent creation takes the name while the indexes build (skipped
            // when the table is locked, which would deadlock)
            let raced = read && store.create_branch("taken", &Default::default()).is_ok();
            send.lock().send((read, raced)).unwrap();
        })),
    );
    let lost = s.create_branch("taken", &Default::default());
    let (read, raced) = receive.recv().unwrap();
    assert!(read, "a table reader waited for the index build");
    assert!(raced);
    assert_eq!(
        super::branch_tests::code(&lost.unwrap_err()),
        "branch-exists"
    );
    s.set_failpoint("memory-branch-indexes", None);
    assert_eq!(s.branches().unwrap().len(), 2);
    s.create_branch("next", &Default::default()).unwrap();
}

#[test]
fn failed_creation_cannot_lower_blank_node_ordinal_floor() {
    let s = Store::in_memory(StoreOptions::default());
    let once = AtomicBool::new(false);
    s.set_failpoint(
        "memory-ordinal-reserved",
        Some(Arc::new(move |store: &Store| {
            if !once.swap(true, Ordering::SeqCst) {
                store.create_branch("taken", &Default::default()).unwrap();
            }
        })),
    );
    assert!(s.create_branch("taken", &Default::default()).is_err());
    s.set_failpoint("memory-ordinal-reserved", None);
    assert_eq!(s.branch_info("taken").unwrap().ordinal, 2);
    let taken = s.branch("taken").unwrap();
    apply(&taken, "+_:existing <urn:p> \"old\" .");
    merged(s.merge("taken", "main", &Default::default()).unwrap());
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("clone");
    s.clone_to(&root, &Default::default()).unwrap();
    let cloned = Store::open(&root, Default::default()).unwrap();
    let next = cloned.create_branch("next", &Default::default()).unwrap();
    assert!(
        next.ordinal > 2,
        "clone reused the ordinal of a blank node already merged into its data: {}",
        next.ordinal
    );
}

/// A persistent branch that compacted into its own index is backed up from its own
/// files. The restored dataset keeps its head, the branch's commit records and its
/// blank-node numbering.
#[test]
fn owned_index_branch_backup_restores_head_history_and_blank_nodes() {
    let (tmp, s) = super::branch_tests::setup();
    s.create_branch("work", &Default::default()).unwrap();
    let work = s.branch("work").unwrap();
    let fork = s.branch_info("work").unwrap().from.unwrap().seq;
    apply(&work, "+_:x <urn:p> \"first\" .");
    apply(&work, "+<urn:w> <urn:p> \"2\" .");
    work.compact().unwrap();
    assert!(work.snapshot().generation.linked().is_none());
    apply(&work, "+_:y <urn:p> \"after compaction\" .");
    let head = work.head_commit();
    let expected = dump(&work);
    let commits: Vec<_> = (0..=head.seq).map(|n| work.commit(n)).collect();
    let cap = work
        .backup_capture_with("backup", &Default::default())
        .unwrap();
    let out = tmp.path().join("copy");
    cap.write_to(&out).unwrap();
    drop(cap);
    let restored = Store::open(&out, Default::default()).unwrap();
    assert_eq!(restored.head_commit().seq, head.seq);
    assert_eq!(restored.head_commit(), head);
    assert_eq!(dump(&restored), expected);
    // The branch's own commit records come along. The records of the commits it
    // inherited stay with its upstream, as they do for the branch itself.
    for (n, c) in commits.iter().enumerate() {
        assert_eq!(restored.commit(n as u64), *c, "commit {n}");
        assert_eq!(c.is_some(), n as u64 >= fork, "commit {n}");
    }
    apply(&restored, "+_:z <urn:p> \"restored\" .");
    let nodes: BTreeSet<_> = dump(&restored)
        .iter()
        .filter(|l| l.starts_with("_:"))
        .map(|l| l.split(' ').next().unwrap().to_string())
        .collect();
    assert_eq!(nodes.len(), 3, "a new blank node reuses no stored one");
}
