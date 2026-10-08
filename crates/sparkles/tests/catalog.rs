use sparkles::catalog::{Attach, CloneRequest, CreateDataset, DatasetKind, ReservationKind};
use sparkles::{Catalog, Dataset, Error};

#[test]
fn registry_roundtrip_and_lock_free_inspection() {
    let dir = tempfile::tempdir().unwrap();
    let cat = Catalog::open(dir.path(), Default::default()).unwrap();
    let ds = cat.create("wiki", &CreateDataset::default()).unwrap();
    let id = ds.dataset_id();
    ds.update("INSERT DATA { <urn:s> <urn:p> 1 }").unwrap();
    let second = Catalog::open(dir.path(), Default::default());
    assert!(
        matches!(second, Err(Error::Locked { pid: Some(pid), .. }) if pid == std::process::id())
    );
    let info = Catalog::inspect(dir.path()).unwrap();
    assert_eq!((info[0].name.as_str(), info[0].id), ("wiki", id));
    cat.attach("temp", Attach::Memory).unwrap();
    assert_eq!(Catalog::inspect(dir.path()).unwrap().len(), 1);
    assert!(
        cat.get_by_id(id)
            .unwrap()
            .ask("ASK { <urn:s> ?p ?o }")
            .unwrap()
    );
    drop(ds);
    drop(cat);
    let cat = Catalog::open(dir.path(), Default::default()).unwrap();
    assert_eq!(cat.get("wiki").unwrap().dataset_id(), id);
    assert!(cat.get("temp").is_none());
    assert!(cat.delete("wiki").unwrap());
    assert!(!cat.delete("wiki").unwrap());
    assert!(!dir.path().join("databases/wiki").exists());
}

#[test]
fn delete_refuses_live_handles_so_a_new_dataset_is_never_written_by_an_old_one() {
    let dir = tempfile::tempdir().unwrap();
    let cat = Catalog::open(dir.path(), Default::default()).unwrap();
    let old = cat.create("wiki", &CreateDataset::default()).unwrap();
    old.update("INSERT DATA { <urn:old> <urn:p> 1 }").unwrap();
    assert!(matches!(cat.delete("wiki"), Err(Error::Conflict(m)) if m.contains("live handles")));
    old.create_branch("dev", &Default::default()).unwrap();
    let branch = old.branch("dev").unwrap();
    drop(old);
    assert!(matches!(cat.delete("wiki"), Err(Error::Conflict(_))));
    assert!(cat.get("wiki").is_some());
    drop(branch);
    assert!(cat.delete("wiki").unwrap());
    let new = cat.create("wiki", &CreateDataset::default()).unwrap();
    assert_eq!(new.len(), 0);
    // in-memory datasets keep no files, so a live handle does not block their delete
    let mem = cat
        .create(
            "mem",
            &CreateDataset {
                kind: DatasetKind::Memory,
                ..Default::default()
            },
        )
        .unwrap();
    assert!(cat.delete("mem").unwrap());
    drop(mem);
}

#[test]
fn reservations_validate_names_and_release_on_drop() {
    let cat = Catalog::memory(Default::default());
    for name in ["", "../outside", ".hidden", "ui", "$", "wiki@dev", "x/y"] {
        assert!(cat.reserve(name, ReservationKind::Clone, "task").is_err());
        assert!(cat.attach(name, Attach::Memory).is_err());
    }
    let r = cat
        .reserve("wiki", ReservationKind::Clone, "task-7")
        .unwrap();
    assert_eq!(cat.reserved_by("wiki").as_deref(), Some("task-7"));
    assert!(
        matches!(cat.attach("wiki", Attach::Memory), Err(Error::Conflict(m)) if m.contains("task-7"))
    );
    assert!(
        cat.reserve("wiki", ReservationKind::Clone, "task-8")
            .is_err()
    );
    drop(r);
    cat.create(
        "wiki",
        &CreateDataset {
            kind: DatasetKind::Memory,
            ..Default::default()
        },
    )
    .unwrap();
    let r = cat
        .reserve("wiki", ReservationKind::Restore, "restore-1")
        .unwrap();
    assert_eq!(cat.restoring_by("wiki").as_deref(), Some("restore-1"));
    assert!(cat.delete("wiki").is_err());
    drop(r);
    assert!(cat.delete("wiki").unwrap());
}

#[test]
fn rename_preserves_identity_and_refuses_live_persistent_handles() {
    let dir = tempfile::tempdir().unwrap();
    let cat = Catalog::open(dir.path(), Default::default()).unwrap();
    let ds = cat.create("wiki", &Default::default()).unwrap();
    ds.update("INSERT DATA { <urn:s> <urn:p> 1 }").unwrap();
    let id = ds.dataset_id();
    assert!(matches!(
        cat.rename("wiki", "docs"),
        Err(Error::Conflict(_))
    ));
    ds.create_branch("dev", &Default::default()).unwrap();
    let branch = ds.branch("dev").unwrap();
    drop(ds);
    assert!(matches!(
        cat.rename("wiki", "docs"),
        Err(Error::Conflict(_))
    ));
    drop(branch);
    let ds = cat.rename("wiki", "docs").unwrap();
    assert_eq!(ds.name(), Some("docs"));
    assert_eq!(ds.dataset_id(), id);
    assert!(cat.get("wiki").is_none());
    assert!(ds.ask("ASK { <urn:s> ?p ?o }").unwrap());
    assert!(
        ds.branch("dev")
            .unwrap()
            .ask("ASK { <urn:s> ?p ?o }")
            .unwrap()
    );
    drop(ds);
    drop(cat);
    let cat = Catalog::open(dir.path(), Default::default()).unwrap();
    assert_eq!(cat.get("docs").unwrap().dataset_id(), id);
}

#[test]
fn failed_registry_writes_leave_no_dataset() {
    let dir = tempfile::tempdir().unwrap();
    let cat = Catalog::open(dir.path(), Default::default()).unwrap();
    std::fs::create_dir(dir.path().join("config.json")).unwrap();
    assert!(cat.create("wiki", &Default::default()).is_err());
    assert!(cat.get("wiki").is_none());
    assert!(!dir.path().join("databases/wiki").exists());
}

#[test]
fn failed_persistent_rename_keeps_the_registered_dataset() {
    let dir = tempfile::tempdir().unwrap();
    let cat = Catalog::open(dir.path(), Default::default()).unwrap();
    let ds = cat.create("wiki", &Default::default()).unwrap();
    ds.update("INSERT DATA { <urn:s> <urn:p> 1 }").unwrap();
    let id = ds.dataset_id();
    drop(ds);
    // A failed registry operation must not move the registered store aside.
    std::fs::remove_file(dir.path().join("config.json")).unwrap();
    std::fs::create_dir(dir.path().join("config.json")).unwrap();
    assert!(cat.rename("wiki", "docs").is_err());
    assert!(cat.get("docs").is_none());
    assert_eq!(cat.get("wiki").unwrap().dataset_id(), id);
    assert_eq!(cat.get("wiki").unwrap().len(), 1);
    assert!(!dir.path().join("databases/docs").exists());
}

#[test]
fn orphan_directories_and_foreign_reservations_are_refused() {
    let dir = tempfile::tempdir().unwrap();
    let cat = Catalog::open(dir.path(), Default::default()).unwrap();
    std::fs::create_dir(dir.path().join("databases/orphan")).unwrap();
    assert!(matches!(
        cat.create("orphan", &Default::default()),
        Err(Error::Conflict(_))
    ));
    let source = cat.create("source", &Default::default()).unwrap();
    let other = Catalog::memory(Default::default());
    let r = other
        .reserve("foreign", ReservationKind::Clone, "task")
        .unwrap();
    assert!(
        cat.clone_reserved("source", r, &CloneRequest::default(), &Default::default())
            .is_err()
    );
    assert!(cat.get("foreign").is_none());
    drop(source);
}

#[test]
fn memory_catalog_can_attach_a_persistent_dataset() {
    let dir = tempfile::tempdir().unwrap();
    drop(Dataset::open(dir.path()).unwrap());
    let cat = Catalog::memory(Default::default());
    cat.attach("wiki", Attach::Directory(dir.path().into()))
        .unwrap();
    assert!(cat.info("wiki").unwrap().attached);
    assert!(cat.create("persistent", &Default::default()).is_err());
}

#[test]
fn catalog_clones_are_independent_and_cancellation_releases_the_name() {
    let dir = tempfile::tempdir().unwrap();
    let cat = Catalog::open(dir.path(), Default::default()).unwrap();
    let source = cat.create("wiki", &Default::default()).unwrap();
    source.update("INSERT DATA { <urn:s> <urn:p> 1 }").unwrap();
    source.create_branch("dev", &Default::default()).unwrap();
    let cloned = cat
        .clone_dataset(
            "wiki",
            "sandbox",
            &CloneRequest::default(),
            &Default::default(),
        )
        .unwrap();
    assert_ne!(source.dataset_id(), cloned.dataset_id());
    assert_eq!(cloned.branches().unwrap().len(), 1);
    assert_eq!(cloned.len(), 1);
    assert!(cloned.origin().is_some());
    let ctl = sparkles::task::Control::none();
    ctl.cancel.cancel();
    assert!(matches!(
        cat.clone_dataset("wiki", "cancelled", &Default::default(), &ctl),
        Err(Error::Cancelled)
    ));
    assert!(cat.get("cancelled").is_none());
    assert!(cat.reserved_by("cancelled").is_none());
}

/// A catalog clone of a dataset with full-text search answers text queries as soon as
/// the clone is returned, as an HTTP clone task's dataset is served.
#[cfg(feature = "text")]
#[test]
fn catalog_clones_search_text_at_once() {
    let dir = tempfile::tempdir().unwrap();
    let cat = Catalog::open(dir.path(), Default::default()).unwrap();
    let source = cat.create("wiki", &Default::default()).unwrap();
    source
        .update("INSERT DATA { <urn:s> <urn:label> \"quick brown fox\" }")
        .unwrap();
    source
        .store()
        .enable_text(sparkles::text::TextConfig::default())
        .unwrap();
    let cloned = cat
        .clone_dataset(
            "wiki",
            "sandbox",
            &CloneRequest::default(),
            &Default::default(),
        )
        .unwrap();
    assert_eq!(cloned.store().text_status().unwrap().state, "ready");
    let r = cloned
        .query("SELECT ?s { ?s <http://jena.apache.org/text#query> \"fox\" }")
        .unwrap();
    assert_eq!(r.rows().len(), 1);
}

#[test]
fn a_clone_that_passes_its_deadline_is_not_published() {
    let dir = tempfile::tempdir().unwrap();
    let cat = Catalog::open(dir.path(), Default::default()).unwrap();
    let source = cat.create("wiki", &Default::default()).unwrap();
    source.update("INSERT DATA { <urn:s> <urn:p> 1 }").unwrap();
    let deadline = std::time::Instant::now() + std::time::Duration::from_millis(25);
    let ctl = sparkles::task::Control {
        deadline: Some(deadline),
        progress: sparkles::task::Progress::new(move |_, _| {
            if let Some(remaining) = deadline.checked_duration_since(std::time::Instant::now()) {
                std::thread::sleep(remaining);
            }
        }),
        ..Default::default()
    };
    assert!(matches!(
        cat.clone_dataset("wiki", "late", &Default::default(), &ctl),
        Err(Error::Timeout)
    ));
    assert!(cat.get("late").is_none());
    assert!(cat.reserved_by("late").is_none());
    assert!(!dir.path().join("databases/late").exists());
    assert!(
        std::fs::read_dir(dir.path().join("databases"))
            .unwrap()
            .all(|e| !e
                .unwrap()
                .file_name()
                .to_string_lossy()
                .starts_with(".clone-"))
    );
}

#[test]
fn racing_creates_publish_exactly_one_dataset() {
    let dir = tempfile::tempdir().unwrap();
    let cat = Catalog::open(dir.path(), Default::default()).unwrap();
    let barrier = std::sync::Arc::new(std::sync::Barrier::new(4));
    let jobs: Vec<_> = (0..4)
        .map(|_| {
            let cat = cat.clone();
            let barrier = barrier.clone();
            std::thread::spawn(move || {
                barrier.wait();
                cat.create("wiki", &Default::default()).is_ok()
            })
        })
        .collect();
    assert_eq!(
        jobs.into_iter()
            .map(|job| job.join().unwrap())
            .filter(|ok| *ok)
            .count(),
        1
    );
    assert_eq!(cat.list().len(), 1);
}

#[test]
fn memory_renames_keep_live_handles_and_failed_renames_roll_back() {
    let dir = tempfile::tempdir().unwrap();
    let cat = Catalog::open(dir.path(), Default::default()).unwrap();
    let ds = cat
        .create(
            "wiki",
            &CreateDataset {
                kind: DatasetKind::Memory,
                ..Default::default()
            },
        )
        .unwrap();
    let renamed = cat.rename("wiki", "docs").unwrap();
    assert_eq!(renamed.name(), Some("docs"));
    assert_eq!(ds.dataset_id(), renamed.dataset_id());
    ds.update("INSERT DATA { <urn:s> <urn:p> 1 }").unwrap();
    assert_eq!(renamed.len(), 1);
    std::fs::remove_file(dir.path().join("config.json")).unwrap();
    std::fs::create_dir(dir.path().join("config.json")).unwrap();
    assert!(cat.rename("docs", "failed").is_err());
    assert!(cat.get("failed").is_none());
    assert_eq!(cat.get("docs").unwrap().dataset_id(), ds.dataset_id());
}

#[test]
fn a_live_retired_branch_also_blocks_a_persistent_rename() {
    let dir = tempfile::tempdir().unwrap();
    let cat = Catalog::open(dir.path(), Default::default()).unwrap();
    let ds = cat.create("wiki", &Default::default()).unwrap();
    ds.create_branch("dev", &Default::default()).unwrap();
    let retired = ds.branch("dev").unwrap();
    ds.delete_branch("dev", true).unwrap();
    ds.create_branch("dev", &Default::default()).unwrap();
    drop(ds.branch("dev").unwrap());
    drop(ds);
    assert!(matches!(
        cat.rename("wiki", "docs"),
        Err(Error::Conflict(_))
    ));
    drop(retired);
    assert!(cat.rename("wiki", "docs").is_ok());
}
