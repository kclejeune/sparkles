use super::*;
use crate::task::Control;

fn preparation() -> Preparation {
    Preparation::new(Control::none(), 64 << 20).unwrap()
}

fn update(store: &Store, name: &str) {
    crate::sparql::update::update(
        store,
        &format!("INSERT DATA {{ <urn:{name}> <urn:p> 1 }}"),
        &Default::default(),
    )
    .unwrap();
}

fn configure(store: &Store, name: &str) -> Vec<u8> {
    let settings = crate::evidence::Settings {
        enabled: true,
        source_records: vec![format!("urn:evidence:{name}")],
        proposals: vec![format!("urn:evidence:{name}:proposals")],
        ..Default::default()
    };
    let canonical = settings.canonical(&Control::none()).unwrap();
    let bytes = canonical.bytes().to_vec();
    store
        .enable_managed(
            OwnerKind::Evidence,
            OwnerEntry::for_settings(1, settings.managed_graphs(), &bytes),
            &bytes,
        )
        .unwrap();
    bytes
}

#[test]
fn bound_views_distinguish_head_from_commit_and_time_at_the_same_data_head() {
    let store = Store::in_memory(Default::default());
    let preparation = preparation();
    let original = store
        .activate_memory_configuration_history(&preparation)
        .unwrap();
    update(&store, "data");
    let commit = store.head_commit();
    let old = store.evidence_view(&At::Head, &preparation).unwrap();
    let bytes = configure(&store, "second");
    assert_eq!(store.head_commit().seq, commit.seq);
    let head = store.evidence_view(&At::Head, &preparation).unwrap();
    let explicit = store
        .evidence_view(&At::Commit(commit.seq), &preparation)
        .unwrap();
    let by_time = store
        .evidence_view(&At::Time(commit.timestamp_ms), &preparation)
        .unwrap();
    assert_eq!(explicit.configuration(), original);
    assert_eq!(by_time.configuration(), original);
    assert_eq!(old.configuration(), original);
    assert_ne!(head.configuration(), original);
    assert_eq!(head.original_data_binding().configuration(), Some(original));
    assert_eq!(
        head.owner_settings(OwnerKind::Evidence),
        Some(bytes.as_slice())
    );
    assert!(old.owner_settings(OwnerKind::Evidence).is_none());
}

#[test]
fn bound_pins_preserve_the_pair_and_legacy_pin_idempotency_remains_sequence_only() {
    let store = Store::in_memory(Default::default());
    let preparation = preparation();
    store
        .activate_memory_configuration_history(&preparation)
        .unwrap();
    update(&store, "data");
    let first = store.evidence_view(&At::Head, &preparation).unwrap();
    configure(&store, "second");
    let second = store.evidence_view(&At::Head, &preparation).unwrap();
    assert!(
        store
            .create_bound_snapshot("pair", &second, &Default::default())
            .unwrap()
            .1
    );
    assert!(
        !store
            .create_bound_snapshot("pair", &second, &Default::default())
            .unwrap()
            .1
    );
    assert!(matches!(
        store.create_bound_snapshot("pair", &first, &Default::default()),
        Err(Error::Conflict(_))
    ));
    assert!(
        !store
            .create_snapshot("pair", &At::Commit(first.commit().seq), None)
            .unwrap()
            .1
    );
    configure(&store, "third");
    let pinned = store
        .evidence_view(&At::Snapshot("pair".into()), &preparation)
        .unwrap();
    assert_eq!(pinned.configuration(), second.configuration());
    assert!(matches!(pinned.selected_by(), At::Head));
    assert_eq!(
        pinned.original_data_binding(),
        first.original_data_binding()
    );
}

#[test]
fn bound_view_payloads_retain_caller_credit_after_store_drop_and_release_on_last_clone() {
    let preparation = preparation();
    let baseline = preparation.inner_used();
    let store = Store::in_memory(Default::default());
    store
        .activate_memory_configuration_history(&self::preparation())
        .unwrap();
    let expected = configure(&store, "retained");
    let view = store.evidence_view(&At::Head, &preparation).unwrap();
    let clone = view.clone();
    let retained = preparation.inner_used();
    assert!(retained > baseline);
    drop(view);
    drop(store);
    assert_eq!(preparation.inner_used(), retained);
    assert_eq!(
        clone.owner_settings(OwnerKind::Evidence),
        Some(expected.as_slice())
    );
    drop(clone);
    assert_eq!(preparation.inner_used(), baseline);
}

#[test]
fn bound_view_refusal_rolls_back_credit_and_foreign_runtime_cannot_pin() {
    let store = Store::in_memory(Default::default());
    store
        .activate_memory_configuration_history(&preparation())
        .unwrap();
    let tiny = Preparation::new(Control::none(), 1024).unwrap();
    let baseline = tiny.inner_used();
    assert!(store.evidence_view(&At::Head, &tiny).is_err());
    assert_eq!(tiny.inner_used(), baseline);
    let view = store.evidence_view(&At::Head, &preparation()).unwrap();
    let other = Store::in_memory(Default::default());
    assert!(
        other
            .create_bound_snapshot("foreign", &view, &Default::default())
            .is_err()
    );
    assert!(other.snapshots().is_empty());
    let control = Control::none();
    let cancelled = Preparation::new(control.clone(), 64 << 20).unwrap();
    control.cancel.cancel();
    let baseline = cancelled.inner_used();
    assert!(matches!(
        store.evidence_view(&At::Head, &cancelled),
        Err(Error::Cancelled)
    ));
    assert_eq!(cancelled.inner_used(), baseline);
}

#[test]
fn memory_fork_root_uses_logical_source_selection_before_ancestor_delegation() {
    let main = Store::in_memory(Default::default());
    let preparation = preparation();
    let first = main
        .activate_memory_configuration_history(&preparation)
        .unwrap();
    update(&main, "data");
    let seq = main.head_commit().seq;
    configure(&main, "second");
    let second = main
        .evidence_view(&At::Head, &preparation)
        .unwrap()
        .configuration();
    main.create_branch("head-fork", &Default::default())
        .unwrap();
    main.create_branch(
        "commit-fork",
        &crate::branch::BranchOptions {
            at: At::Commit(seq),
            ..Default::default()
        },
    )
    .unwrap();
    configure(&main, "third");
    let fork = main.branch("head-fork").unwrap();
    let root = main
        .evidence_view_with_branch(&fork, &At::Commit(seq), &preparation)
        .unwrap();
    assert_eq!(root.configuration(), second);
    assert_eq!(root.logical_branch_id(), fork.branch_id());
    let explicit = main.branch("commit-fork").unwrap();
    assert_eq!(
        main.evidence_view_with_branch(&explicit, &At::Commit(seq), &preparation)
            .unwrap()
            .configuration(),
        first
    );
    main.create_branch(
        "nested",
        &crate::branch::BranchOptions {
            from: "head-fork".into(),
            at: At::Commit(seq),
            ..Default::default()
        },
    )
    .unwrap();
    let nested = main.branch("nested").unwrap();
    assert_eq!(
        main.evidence_view_with_branch(&nested, &At::Commit(seq), &preparation)
            .unwrap()
            .configuration(),
        second
    );
}

#[test]
fn persistent_fork_and_pin_keep_the_pair_after_reopen_and_compaction() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("dataset");
    drop(Store::open(&path, Default::default()).unwrap());
    let preparation = preparation();
    crate::migrate::activate_configuration_history(&path, false, &preparation).unwrap();
    let store = Store::open(&path, Default::default()).unwrap();
    update(&store, "data");
    let seq = store.head_commit().seq;
    configure(&store, "second");
    let selected = store.evidence_view(&At::Head, &preparation).unwrap();
    store
        .create_bound_snapshot("exact", &selected, &Default::default())
        .unwrap();
    let info = store.create_branch("fork", &Default::default()).unwrap();
    let bytes = std::fs::read(
        path.join("branches")
            .join(info.id.to_string())
            .join("branch.json"),
    )
    .unwrap();
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&bytes).unwrap()["format"],
        2
    );
    store.compact().unwrap();
    let expected = selected.configuration();
    drop(selected);
    drop(store);
    let reopened = Store::open(&path, Default::default()).unwrap();
    configure(&reopened, "third");
    assert_eq!(
        reopened
            .evidence_view(&At::Snapshot("exact".into()), &preparation)
            .unwrap()
            .configuration(),
        expected
    );
    let fork = reopened.branch("fork").unwrap();
    assert_eq!(
        reopened
            .evidence_view_with_branch(&fork, &At::Commit(seq), &preparation)
            .unwrap()
            .configuration(),
        expected
    );
}

#[test]
fn explicit_pre_activation_data_and_legacy_pins_never_fall_back_to_head() {
    let store = Store::in_memory(Default::default());
    update(&store, "legacy");
    let seq = store.head_commit().seq;
    store.create_snapshot("before", &At::Head, None).unwrap();
    let preparation = preparation();
    store
        .activate_memory_configuration_history(&preparation)
        .unwrap();
    configure(&store, "current");
    assert!(store.evidence_view(&At::Head, &preparation).is_ok());
    assert!(store.evidence_view(&At::Commit(seq), &preparation).is_err());
    assert!(
        store
            .evidence_view(&At::Snapshot("before".into()), &preparation)
            .is_err()
    );
    store
        .create_branch(
            "legacy-fork",
            &crate::branch::BranchOptions {
                at: At::Commit(seq),
                ..Default::default()
            },
        )
        .unwrap();
    let branch = store.branch("legacy-fork").unwrap();
    assert!(
        store
            .evidence_view_with_branch(&branch, &At::Commit(seq), &preparation)
            .is_err()
    );
    assert!(
        store
            .evidence_view_with_branch(&branch, &At::Head, &preparation)
            .is_ok()
    );
    update(&branch, "own-bound");
    assert!(
        store
            .evidence_view_with_branch(&branch, &At::Commit(branch.head_commit().seq), &preparation)
            .is_ok()
    );
}

#[test]
fn history_pair_reader_refuses_missing_unknown_duplicate_and_future_metadata() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("dataset");
    drop(Store::open(&path, Default::default()).unwrap());
    let preparation = preparation();
    crate::migrate::activate_configuration_history(&path, false, &preparation).unwrap();
    let store = Store::open(&path, Default::default()).unwrap();
    let view = store.evidence_view(&At::Head, &preparation).unwrap();
    store
        .create_bound_snapshot("exact", &view, &Default::default())
        .unwrap();
    let file = path.join("history.json");
    let original = std::fs::read(&file).unwrap();
    let source: serde_json::Value = serde_json::from_slice(&original).unwrap();
    assert_eq!(source["format"], 2);
    for kind in ["missing", "unknown", "duplicate", "future", "bad-selector"] {
        let mut altered = source.clone();
        match kind {
            "missing" => {
                altered["snapshots"][0]
                    .as_object_mut()
                    .unwrap()
                    .remove("configuration");
            }
            "unknown" => {
                altered["snapshots"][0]["configuration"]["extra"] = true.into();
            }
            "duplicate" => {
                let first = altered["snapshots"][0].clone();
                altered["snapshots"].as_array_mut().unwrap().push(first);
            }
            "future" => {
                altered["format"] = 3.into();
            }
            "bad-selector" => {
                altered["snapshots"][0]["configuration"]["selector"]["kind"] =
                    "legacy-unspecified".into();
            }
            _ => unreachable!(),
        }
        std::fs::write(&file, serde_json::to_vec_pretty(&altered).unwrap()).unwrap();
        assert!(
            crate::history::read_file(&path, store.dataset_id).is_err(),
            "{kind}"
        );
    }
    std::fs::write(&file, original).unwrap();
    assert!(crate::history::read_file(&path, store.dataset_id).is_ok());
}

#[test]
fn bound_read_source_keeps_genuine_history_view_and_configuration_credit() {
    let store = Store::in_memory(Default::default());
    let preparation = preparation();
    store
        .activate_memory_configuration_history(&preparation)
        .unwrap();
    let before = preparation.inner_used();
    let view = store.evidence_view(&At::Head, &preparation).unwrap();
    let backing = Arc::downgrade(&view.inner);
    let charged = preparation.inner_used();
    assert!(charged > before);
    let source = view.read_source();
    let last = source.clone();
    drop(view);
    drop(source);
    assert!(backing.upgrade().is_some());
    assert_eq!(preparation.inner_used(), charged);
    drop(last);
    assert!(backing.upgrade().is_none());
    assert_eq!(preparation.inner_used(), before);
}
