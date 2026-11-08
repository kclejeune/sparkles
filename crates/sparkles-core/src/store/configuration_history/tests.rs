use super::*;
use crate::history::At;
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

#[test]
fn memory_activation_does_not_rebind_old_data_and_new_publications_are_bound() {
    let store = Store::in_memory(StoreOptions::default());
    update(&store, "before");
    let old = store.head_commit().seq;
    let preparation = preparation();
    let identity = store
        .activate_memory_configuration_history(&preparation)
        .unwrap();
    assert_eq!(
        store.configuration_binding_at(old, &preparation).unwrap(),
        ConfigurationBinding::LegacyUnbound
    );
    update(&store, "after");
    let new = store.head_commit().seq;
    assert_eq!(
        store
            .configuration_binding_at(new, &preparation)
            .unwrap()
            .configuration(),
        Some(identity)
    );
    assert_eq!(
        store
            .activate_memory_configuration_history(&preparation)
            .unwrap(),
        identity
    );
    assert_eq!(store.head_commit().seq, new);
}

#[test]
fn memory_activation_refuses_a_live_family_writer_before_publication() {
    let store = Store::in_memory(StoreOptions::default());
    let writer = store.write();
    assert!(matches!(
        store.activate_memory_configuration_history(&preparation()),
        Err(Error::WriterBusy)
    ));
    assert!(store.managed_state_shared().is_none());
    drop(writer);
    store
        .activate_memory_configuration_history(&preparation())
        .unwrap();
}

#[test]
fn memory_activation_budget_failure_leaves_legacy_state_unchanged() {
    let store = Store::in_memory(StoreOptions::default());
    let preparation = Preparation::new(Control::none(), 256).unwrap();
    assert!(
        store
            .activate_memory_configuration_history(&preparation)
            .is_err()
    );
    assert!(store.managed_state_shared().is_none());
    update(&store, "still-writable");
}

#[test]
fn closed_activation_uses_real_cutoffs_and_survives_reopen_and_compaction() {
    let temporary = tempfile::tempdir().unwrap();
    let root = temporary.path().join("dataset");
    let store = Store::open(&root, StoreOptions::default()).unwrap();
    update(&store, "old");
    let old = store.head_commit().seq;
    let preparation = preparation();
    assert!(matches!(
        crate::migrate::activate_configuration_history(&root, false, &preparation),
        Err(Error::Locked { .. })
    ));
    drop(store);
    let plan = crate::migrate::configuration_history_plan(&root, &preparation).unwrap();
    assert!(
        plan.steps
            .iter()
            .any(|step| step.kind == "configuration-generation")
    );
    crate::migrate::activate_configuration_history(&root, false, &preparation).unwrap();
    let store = Store::open(&root, StoreOptions::default()).unwrap();
    assert_eq!(
        store.configuration_binding_at(old, &preparation).unwrap(),
        ConfigurationBinding::LegacyUnbound
    );
    update(&store, "new");
    let selected = store.head_commit().seq;
    let original = store
        .configuration_binding_at(selected, &preparation)
        .unwrap();
    assert!(original.configuration().is_some());
    store.compact().unwrap();
    assert_eq!(
        store
            .configuration_binding_at(selected, &preparation)
            .unwrap(),
        original
    );
    drop(store);
    let reopened = Store::open(&root, StoreOptions::default()).unwrap();
    assert_eq!(
        reopened
            .configuration_binding_at(selected, &preparation)
            .unwrap(),
        original
    );
}

#[test]
fn closed_activation_interruption_preserves_truthful_recovery() {
    for boundary in [
        "configuration-generation-written",
        "configuration-coverage-written",
        "configuration-current-logs-upgraded",
        "configuration-root-written",
        "checked",
        "moved-aside",
        "swapped",
    ] {
        let temporary = tempfile::tempdir().unwrap();
        let root = temporary.path().join("dataset");
        let store = Store::open(&root, StoreOptions::default()).unwrap();
        update(&store, "old");
        let old = store.head_commit().seq;
        drop(store);
        let preparation = preparation();
        assert!(
            crate::migrate::activate_configuration_history_with(
                &root,
                false,
                &preparation,
                &mut |at| {
                    if at == boundary {
                        Err(Error::Cancelled)
                    } else {
                        Ok(())
                    }
                }
            )
            .is_err(),
            "{boundary}"
        );
        crate::migrate::recover(&root).unwrap();
        let store = Store::open(&root, StoreOptions::default()).unwrap();
        assert_eq!(store.head_commit().seq, old, "{boundary}");
        assert_eq!(
            store.configuration_binding_at(old, &preparation).unwrap(),
            ConfigurationBinding::LegacyUnbound,
            "{boundary}"
        );
    }
}

#[test]
fn activation_with_large_allowed_generation_reopens_under_administrative_budget() {
    let temporary = tempfile::tempdir().unwrap();
    let root = temporary.path().join("dataset");
    let store = Store::open(&root, StoreOptions::default()).unwrap();
    let settings = crate::evidence::Settings {
        enabled: true,
        proposals: vec!["urn:large-generation:proposals".into()],
        source_records: (0..600)
            .map(|index| format!("urn:large-generation:{index}:{}", "x".repeat(4000)))
            .collect(),
        ..Default::default()
    };
    let canonical = settings.canonical(&Control::none()).unwrap();
    assert!(canonical.bytes().len() > 2 << 20);
    let entry =
        crate::managed::OwnerEntry::for_settings(1, settings.managed_graphs(), canonical.bytes());
    store
        .enable_managed(
            crate::managed::OwnerKind::Evidence,
            entry,
            canonical.bytes(),
        )
        .unwrap();
    drop(store);
    let preparation = Preparation::new(Control::none(), 512 << 20).unwrap();
    crate::migrate::activate_configuration_history(&root, false, &preparation).unwrap();
    let reopened = Store::open(&root, StoreOptions::default()).unwrap();
    // A large caller allowance must not publish a generation that ordinary open
    // then declares unavailable solely because its administrative loader is smaller.
    assert_eq!(reopened.managed_graphs().unwrap().unwrap().owners.len(), 1);
    assert!(
        reopened
            .managed_snapshot()
            .unwrap()
            .unwrap()
            .unavailable
            .is_none()
    );
    update(&reopened, "after-large-activation");
    assert!(
        reopened
            .configuration_binding_at(reopened.head_commit().seq, &preparation)
            .unwrap()
            .configuration()
            .is_some()
    );
}

fn primary_fixture() -> (tempfile::TempDir, std::path::PathBuf, Store, Preparation) {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path().join("dataset");
    drop(Store::open(&root, Default::default()).unwrap());
    let preparation = preparation();
    crate::migrate::activate_configuration_history(&root, false, &preparation).unwrap();
    let store = Store::open(&root, Default::default()).unwrap();
    update(&store, "bound");
    (directory, root, store, preparation)
}

fn primary_wal(root: &std::path::Path) -> std::path::PathBuf {
    root.join(
        std::fs::read_to_string(root.join("CURRENT"))
            .unwrap()
            .trim(),
    )
    .join("wal.log")
}

fn primary_orphan(store: &Store, preparation: &Preparation) -> managed_history::Prepared {
    let state = store.managed_state_shared().unwrap();
    let crate::managed::ManagedState::Installed(installed) = state.as_ref() else {
        panic!("installed")
    };
    let history = installed.configuration_history.as_ref().unwrap();
    let current = managed_history::read_generation(
        &store.managed_root().unwrap(),
        history.identity(store.owner_dataset_id()),
        preparation,
    )
    .unwrap();
    let mut graphs = installed.graphs.clone();
    graphs.revision += 1;
    let next = crate::managed::Installed::new(graphs, installed.settings.clone()).unwrap();
    let orphan = managed_history::prepare(
        store.owner_dataset_id(),
        history.lineage_id,
        current.value.ordinal + 1,
        Some(history.current_generation),
        &next,
        preparation,
    )
    .unwrap();
    managed_history::persist(&store.managed_root().unwrap(), &orphan, preparation).unwrap();
    orphan
}

fn reseal_primary_reference(bytes: &mut Vec<u8>, reference: Option<crate::evidence::Sha256Digest>) {
    let configuration = bytes
        .as_chunks::<WAL_REC>()
        .0
        .iter()
        .position(|record| record[0] == WAL_CONFIGURATION)
        .unwrap();
    let seal = bytes
        .as_chunks::<WAL_REC>()
        .0
        .iter()
        .position(|record| record[0] == WAL_COMMIT)
        .unwrap();
    let original: [u8; WAL_REC] = bytes[seal * WAL_REC..(seal + 1) * WAL_REC]
        .try_into()
        .unwrap();
    let (seq, timestamp, kind, mut flags) =
        commit::open_wal_commit(&original, &bytes[WAL_REC..seal * WAL_REC])
            .unwrap()
            .unwrap();
    if let Some(reference) = reference {
        bytes[configuration * WAL_REC + 1..(configuration + 1) * WAL_REC]
            .copy_from_slice(reference.as_bytes());
    } else {
        bytes.drain(configuration * WAL_REC..(configuration + 1) * WAL_REC);
        flags &= !commit::WAL_FLAG_CONFIGURATION;
    }
    let at = (seal - usize::from(reference.is_none())) * WAL_REC;
    // The seal helper fills only bytes 9..33. Keep the commit opcode and
    // next-blank-node prefix so this remains a complete transaction, rather
    // than damage that recovery may discard as an uncommitted torn suffix.
    let mut record = original;
    commit::seal_wal_commit_unfenced(
        &mut record,
        seq,
        timestamp,
        kind,
        flags,
        original[28],
        &bytes[WAL_REC..at],
    );
    assert_eq!(record[0], WAL_COMMIT);
    assert_eq!(&record[..9], &original[..9]);
    assert_eq!(
        commit::open_wal_commit(&record, &bytes[WAL_REC..at]),
        Some(Ok((seq, timestamp, kind, flags))),
        "the altered reference must remain inside a complete valid seal"
    );
    let version = wal::version_of(bytes, std::path::Path::new("primary-fixture-wal")).unwrap();
    assert_eq!(
        wal::configuration_records(&bytes[WAL_REC..at], version, flags).unwrap(),
        reference,
        "the seal must authenticate exactly the requested reference mutation"
    );
    bytes[at..at + WAL_REC].copy_from_slice(&record);
}

#[test]
fn primary_open_refuses_a_lost_generation_without_truncating_a_torn_suffix() {
    let (_directory, root, store, preparation) = primary_fixture();
    let identity = store
        .configuration_binding_at(store.head_commit().seq, &preparation)
        .unwrap()
        .configuration()
        .unwrap();
    drop(store);
    let file = root
        .join("evidence-history")
        .join(identity.lineage_id().to_string())
        .join("generations")
        .join(format!("{}.json", &identity.generation().to_string()[7..]));
    std::fs::remove_file(file).unwrap();
    let wal = primary_wal(&root);
    let mut complete_and_torn = std::fs::read(&wal).unwrap();
    complete_and_torn.extend_from_slice(&[0; WAL_REC]);
    std::fs::write(&wal, &complete_and_torn).unwrap();
    assert!(Store::open(&root, Default::default()).is_err());
    assert_eq!(
        std::fs::read(wal).unwrap(),
        complete_and_torn,
        "a complete seal with a lost target cannot trigger convenient WAL truncation"
    );
}

#[test]
fn primary_open_refuses_a_resealed_orphan_even_when_its_file_is_complete() {
    let (_directory, root, store, preparation) = primary_fixture();
    let identity = store
        .configuration_binding_at(store.head_commit().seq, &preparation)
        .unwrap()
        .configuration()
        .unwrap();
    let state = store.managed_state_shared().unwrap();
    let crate::managed::ManagedState::Installed(installed) = state.as_ref() else {
        panic!("installed")
    };
    let mut graphs = installed.graphs.clone();
    graphs.revision += 1;
    let next = crate::managed::Installed::new(graphs, installed.settings.clone()).unwrap();
    let orphan = managed_history::prepare(
        store.owner_dataset_id(),
        identity.lineage_id(),
        2,
        Some(identity.generation()),
        &next,
        &preparation,
    )
    .unwrap();
    managed_history::persist(&root, &orphan, &preparation).unwrap();
    drop(state);
    drop(store);
    let wal = primary_wal(&root);
    let mut bytes = std::fs::read(&wal).unwrap();
    reseal_primary_reference(&mut bytes, Some(orphan.identity.generation()));
    std::fs::write(&wal, &bytes).unwrap();
    assert!(
        Store::open(&root, Default::default()).is_err(),
        "an immutable orphan is not published history"
    );
    assert_eq!(std::fs::read(wal).unwrap(), bytes);
}

#[test]
fn primary_open_refuses_a_resealed_missing_reference_after_coverage() {
    let (_directory, root, store, _preparation) = primary_fixture();
    drop(store);
    let wal = primary_wal(&root);
    let mut bytes = std::fs::read(&wal).unwrap();
    reseal_primary_reference(&mut bytes, None);
    std::fs::write(&wal, &bytes).unwrap();
    assert!(
        Store::open(&root, Default::default()).is_err(),
        "coverage cannot be downgraded to a legacy transaction by clearing its flag"
    );
    assert_eq!(std::fs::read(wal).unwrap(), bytes);
}

#[test]
fn primary_retained_snapshot_refuses_orphan_without_recovering_old_wal() {
    let (_directory, root, store, preparation) = primary_fixture();
    let selected = store.head_commit().seq;
    let old_wal = primary_wal(&root);
    store
        .create_snapshot("retained", &crate::history::At::Commit(selected), None)
        .unwrap();
    update(&store, "later");
    store.compact().unwrap();
    assert_ne!(old_wal, primary_wal(&root));
    assert!(
        old_wal.exists(),
        "the durable pin retains the old generation"
    );
    let orphan = primary_orphan(&store, &preparation);
    drop(store);
    // First establish that this exact physical generation serves the pin before
    // changing only its configuration reference and recomputing the seal.
    let reopened = Store::open(&root, Default::default()).unwrap();
    reopened
        .snapshot_at(
            &crate::history::At::Snapshot("retained".into()),
            &Default::default(),
        )
        .unwrap();
    drop(reopened);
    let mut bytes = std::fs::read(&old_wal).unwrap();
    reseal_primary_reference(&mut bytes, Some(orphan.identity.generation()));
    std::fs::write(&old_wal, &bytes).unwrap();
    let reopened = Store::open(&root, Default::default()).unwrap();
    assert!(
        reopened
            .snapshot_at(&crate::history::At::Commit(selected), &Default::default())
            .is_err(),
        "a valid current generation cannot certify an orphan in retained primary history"
    );
    assert!(
        reopened
            .snapshot_at_limited(
                &crate::history::At::Snapshot("retained".into()),
                &Default::default(),
                64 << 20,
            )
            .is_err()
    );
    assert_eq!(std::fs::read(old_wal).unwrap(), bytes);
}

#[test]
fn primary_cold_branch_refuses_orphan_without_opening_a_writer() {
    let (_directory, root, main, preparation) = primary_fixture();
    let branch = main.create_branch("cold", &Default::default()).unwrap();
    let fork = main.branch("cold").unwrap();
    update(&fork, "cold-own");
    let selected = fork.head_commit().seq;
    let branch_root = root.join("branches").join(branch.id.to_string());
    let wal = primary_wal(&branch_root);
    let orphan = primary_orphan(&main, &preparation);
    drop(fork);
    drop(main);
    let mut bytes = std::fs::read(&wal).unwrap();
    reseal_primary_reference(&mut bytes, Some(orphan.identity.generation()));
    std::fs::write(&wal, &bytes).unwrap();
    let reopened = Store::open(&root, Default::default()).unwrap();
    let options = crate::history::HistoryOptions::default();
    let limit = crate::history::ReadLimit {
        options: &options,
        memory: Some(64 << 20),
        held: 0,
    };
    assert!(
        reopened
            .readonly_branch_snapshot_limited(
                branch.id,
                &crate::history::At::Commit(selected),
                &limit,
            )
            .is_err()
    );
    assert!(
        reopened
            .try_resident_branch(branch.id, &Default::default())
            .unwrap()
            .is_none()
    );
    assert_eq!(std::fs::read(wal).unwrap(), bytes);
}

fn retained_activation_fixture() -> (tempfile::TempDir, std::path::PathBuf, std::path::PathBuf) {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path().join("dataset");
    let store = Store::open(&root, Default::default()).unwrap();
    update(&store, "retained-before-activation");
    let old = primary_wal(&root);
    store
        .create_snapshot("legacy", &crate::history::At::Head, None)
        .unwrap();
    update(&store, "current-before-activation");
    store.compact().unwrap();
    assert_ne!(old, primary_wal(&root));
    assert!(old.exists());
    drop(store);
    (directory, root, old)
}

fn unpublished_binding(dataset: uuid::Uuid) -> ConfigurationBinding {
    ConfigurationBinding::Known {
        configuration: ConfigurationIdentity {
            dataset_id: dataset,
            lineage_id: uuid::Uuid::new_v4(),
            generation: crate::evidence::Sha256Digest::of_bytes(b"unpublished", &Control::none())
                .unwrap(),
        },
    }
}

#[test]
fn activation_retains_legacy_pinned_generation_bytes_and_unbound_meaning() {
    let (_directory, root, old) = retained_activation_fixture();
    let before = std::fs::read(&old).unwrap();
    let preparation = preparation();
    crate::migrate::activate_configuration_history(&root, false, &preparation).unwrap();
    assert_eq!(std::fs::read(&old).unwrap(), before);
    let store = Store::open(&root, Default::default()).unwrap();
    let (snapshot, selected) = store
        .snapshot_at(
            &crate::history::At::Snapshot("legacy".into()),
            &Default::default(),
        )
        .unwrap();
    assert_eq!(snapshot.len(), 1);
    assert_eq!(
        store
            .configuration_binding_at(selected.commit.seq, &preparation)
            .unwrap(),
        ConfigurationBinding::LegacyUnbound
    );
}

#[test]
fn activation_refuses_unpublished_retained_base_before_modifying_current() {
    let (_directory, root, old) = retained_activation_fixture();
    let generation = old.parent().unwrap();
    let (dataset, base, origin) = commit::read_gen_commit(generation).unwrap().unwrap();
    let path = generation.join("commit.json");
    let bytes = commit::gen_commit_bytes_with_binding(
        dataset,
        &origin,
        &base,
        unpublished_binding(dataset),
    );
    std::fs::write(&path, &bytes).unwrap();
    let hot = primary_wal(&root);
    let hot_before = std::fs::read(&hot).unwrap();
    assert!(crate::migrate::configuration_history_plan(&root, &preparation()).is_err());
    assert_eq!(std::fs::read(hot).unwrap(), hot_before);
    assert_eq!(std::fs::read(path).unwrap(), bytes);
    assert!(!root.join("evidence-history").exists());
}

#[test]
fn activation_refuses_unpublished_retained_wal_before_modifying_current() {
    let (_directory, root, old) = retained_activation_fixture();
    let dataset = commit::read_dataset_file(&root).unwrap().unwrap();
    let mut bytes = std::fs::read(&old).unwrap();
    let seal = bytes
        .as_chunks::<WAL_REC>()
        .0
        .iter()
        .position(|record| record[0] == WAL_COMMIT)
        .unwrap();
    let original: [u8; WAL_REC] = bytes[seal * WAL_REC..(seal + 1) * WAL_REC]
        .try_into()
        .unwrap();
    let (seq, time, kind, flags) =
        commit::open_wal_commit(&original, &bytes[WAL_REC..seal * WAL_REC])
            .unwrap()
            .unwrap();
    let mut reference = Vec::new();
    let configuration_flag =
        wal::append_configuration(&mut reference, unpublished_binding(dataset)).unwrap();
    bytes.splice(seal * WAL_REC..seal * WAL_REC, reference);
    let mut replacement = original;
    let at = (seal + 1) * WAL_REC;
    commit::seal_wal_commit_unfenced(
        &mut replacement,
        seq,
        time,
        kind,
        flags | configuration_flag,
        original[28],
        &bytes[WAL_REC..at],
    );
    bytes[at..at + WAL_REC].copy_from_slice(&replacement);
    std::fs::write(&old, &bytes).unwrap();
    let hot = primary_wal(&root);
    let hot_before = std::fs::read(&hot).unwrap();
    assert!(crate::migrate::configuration_history_plan(&root, &preparation()).is_err());
    assert_eq!(std::fs::read(hot).unwrap(), hot_before);
    assert_eq!(std::fs::read(old).unwrap(), bytes);
    assert!(!root.join("evidence-history").exists());
}

#[test]
fn copied_primary_validation_reads_a_valid_closure_without_recovery() {
    let (_temporary, root, store, preparation) = primary_fixture();
    drop(store);
    let wal = primary_wal(&root);
    let mut bytes = std::fs::read(&wal).unwrap();
    bytes.extend_from_slice(&[0; WAL_REC]);
    std::fs::write(&wal, &bytes).unwrap();
    validate_copy(&root, true, &preparation).unwrap();
    assert_eq!(std::fs::read(wal).unwrap(), bytes);
}

#[test]
fn copied_primary_validation_refuses_a_resealed_unpublished_target() {
    let (_temporary, root, store, preparation) = primary_fixture();
    let orphan = primary_orphan(&store, &preparation);
    drop(store);
    let wal = primary_wal(&root);
    let mut bytes = std::fs::read(&wal).unwrap();
    reseal_primary_reference(&mut bytes, Some(orphan.identity.generation()));
    std::fs::write(&wal, &bytes).unwrap();
    assert!(validate_copy(&root, true, &preparation).is_err());
    assert_eq!(std::fs::read(wal).unwrap(), bytes);
}

#[test]
fn copied_primary_validation_refuses_a_cleared_covered_reference() {
    let (_temporary, root, store, preparation) = primary_fixture();
    drop(store);
    let wal = primary_wal(&root);
    let mut bytes = std::fs::read(&wal).unwrap();
    reseal_primary_reference(&mut bytes, None);
    std::fs::write(&wal, &bytes).unwrap();
    assert!(validate_copy(&root, true, &preparation).is_err());
    assert_eq!(std::fs::read(wal).unwrap(), bytes);
}

#[test]
fn copied_primary_validation_refuses_a_missing_publication_before_torn_suffix_recovery() {
    let (_temporary, root, store, preparation) = primary_fixture();
    let identity = store
        .evidence_view(&At::Head, &preparation)
        .unwrap()
        .configuration();
    drop(store);
    let wal = primary_wal(&root);
    let mut bytes = std::fs::read(&wal).unwrap();
    bytes.extend_from_slice(&[0; WAL_REC]);
    std::fs::write(&wal, &bytes).unwrap();
    std::fs::remove_file(
        root.join("evidence-history")
            .join(identity.lineage_id().to_string())
            .join("generations")
            .join(format!("{}.json", &identity.generation().to_string()[7..])),
    )
    .unwrap();
    assert!(validate_copy(&root, true, &preparation).is_err());
    assert_eq!(std::fs::read(wal).unwrap(), bytes);
}

#[test]
fn copied_primary_validation_refuses_unknown_coverage_and_missing_required_root() {
    let (_temporary, root, store, preparation) = primary_fixture();
    let identity = store
        .evidence_view(&At::Head, &preparation)
        .unwrap()
        .configuration();
    drop(store);
    let coverage = root
        .join("evidence-history")
        .join(identity.lineage_id().to_string())
        .join("coverage.json");
    let bytes = std::fs::read(&coverage).unwrap();
    let mut value: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    value["format"] = 99.into();
    std::fs::write(&coverage, serde_json::to_vec(&value).unwrap()).unwrap();
    assert!(validate_copy(&root, true, &preparation).is_err());
    std::fs::write(coverage, bytes).unwrap();
    std::fs::remove_file(root.join("managed.json")).unwrap();
    assert!(validate_copy(&root, true, &preparation).is_err());
}

#[test]
fn copied_primary_validation_keeps_genuinely_unmarked_upgrade_behavior() {
    let temporary = tempfile::tempdir().unwrap();
    let root = temporary.path().join("legacy");
    let store = Store::open(&root, StoreOptions::default()).unwrap();
    update(&store, "legacy");
    drop(store);
    let wal = primary_wal(&root);
    let before = std::fs::read(&wal).unwrap();
    std::fs::remove_file(wal.parent().unwrap().join("commit.json")).unwrap();
    validate_copy(&root, false, &preparation()).unwrap();
    std::fs::remove_file(root.join("dataset.json")).unwrap();
    validate_copy(&root, false, &preparation()).unwrap();
    assert_eq!(std::fs::read(wal).unwrap(), before);
}

#[test]
fn copied_primary_validation_preserves_cancellation_and_deadline() {
    let (_temporary, root, store, _preparation) = primary_fixture();
    drop(store);
    let cancel = crate::task::Cancel::new();
    let preparation = Preparation::new(
        Control {
            cancel: cancel.clone(),
            ..Default::default()
        },
        64 << 20,
    )
    .unwrap();
    cancel.cancel();
    assert!(matches!(
        validate_copy(&root, true, &preparation),
        Err(Error::Cancelled)
    ));
    let preparation = Preparation::new(
        Control {
            deadline: Some(std::time::Instant::now()),
            ..Default::default()
        },
        64 << 20,
    );
    match preparation {
        Ok(preparation) => assert!(matches!(
            validate_copy(&root, true, &preparation),
            Err(Error::Timeout)
        )),
        Err(error) => assert!(matches!(error, Error::Timeout)),
    }
}

#[test]
fn copied_primary_validation_refuses_before_exceeding_its_preparation() {
    let (_temporary, root, store, _preparation) = primary_fixture();
    drop(store);
    let wal = primary_wal(&root);
    let before = std::fs::read(&wal).unwrap();
    let preparation = Preparation::new(Control::none(), 1024).unwrap();
    assert!(matches!(
        validate_copy(&root, true, &preparation),
        Err(Error::BudgetExceeded(_))
    ));
    assert_eq!(std::fs::read(wal).unwrap(), before);
}

#[test]
fn copied_primary_validation_refuses_native_base_after_identity_markers_are_removed() {
    let (_temporary, root, store, preparation) = primary_fixture();
    store.compact().unwrap();
    drop(store);
    validate_copy(&root, true, &preparation).unwrap();
    let wal = primary_wal(&root);
    let before = std::fs::read(&wal).unwrap();
    std::fs::remove_file(root.join("dataset.json")).unwrap();
    std::fs::remove_file(root.join("managed.json")).unwrap();
    assert!(validate_copy(&root, false, &preparation).is_err());
    assert_eq!(std::fs::read(wal).unwrap(), before);
}

#[test]
fn copied_primary_validation_refuses_wal_only_native_seal_after_marker_removal() {
    let (_temporary, root, store, preparation) = primary_fixture();
    drop(store);
    validate_copy(&root, true, &preparation).unwrap();
    let wal = primary_wal(&root);
    let mut before = std::fs::read(&wal).unwrap();
    before.extend_from_slice(&[0; WAL_REC]);
    std::fs::write(&wal, &before).unwrap();
    for path in [
        root.join("managed.json"),
        wal.parent().unwrap().join("commit.json"),
    ] {
        std::fs::remove_file(path).unwrap();
    }
    assert!(
        validate_copy(&root, false, &preparation).is_err(),
        "the surviving dataset identity cannot excuse a missing base marker"
    );
    assert_eq!(std::fs::read(&wal).unwrap(), before);
    std::fs::remove_file(root.join("dataset.json")).unwrap();
    assert!(validate_copy(&root, false, &preparation).is_err());
    assert_eq!(std::fs::read(wal).unwrap(), before);
}

#[test]
fn copied_primary_validation_refuses_native_seal_with_unreadable_registry_and_no_identity() {
    let (_temporary, root, store, preparation) = primary_fixture();
    drop(store);
    validate_copy(&root, true, &preparation).unwrap();
    let registry = std::fs::read(root.join("managed.json")).unwrap();
    let wal = primary_wal(&root);
    let before = std::fs::read(&wal).unwrap();
    std::fs::remove_file(root.join("dataset.json")).unwrap();
    std::fs::remove_file(wal.parent().unwrap().join("commit.json")).unwrap();
    std::fs::write(root.join("managed.json"), b"unreadable legacy registry").unwrap();
    assert!(validate_copy(&root, false, &preparation).is_err());
    assert_eq!(std::fs::read(&wal).unwrap(), before);
    let mut registry: serde_json::Value = serde_json::from_slice(&registry).unwrap();
    registry["format"] = 99.into();
    std::fs::write(
        root.join("managed.json"),
        serde_json::to_vec(&registry).unwrap(),
    )
    .unwrap();
    assert!(matches!(
        validate_copy(&root, false, &preparation),
        Err(Error::Unsupported(_))
    ));
    registry["format"] = 3.into();
    registry
        .as_object_mut()
        .unwrap()
        .remove("configurationHistory");
    std::fs::write(
        root.join("managed.json"),
        serde_json::to_vec(&registry).unwrap(),
    )
    .unwrap();
    assert!(matches!(
        validate_copy(&root, false, &preparation),
        Err(Error::Unsupported(_))
    ));
    assert_eq!(std::fs::read(wal).unwrap(), before);
}

#[test]
fn copied_primary_validation_keeps_real_pre_metadata_wal_upgrade() {
    let temporary = tempfile::tempdir().unwrap();
    let root = temporary.path().join("legacy");
    let store = Store::open(&root, StoreOptions::default()).unwrap();
    update(&store, "legacy");
    drop(store);
    let wal = primary_wal(&root);
    let bytes = std::fs::read(&wal).unwrap();
    assert_eq!(bytes[0], WAL_HEADER);
    let mut legacy = bytes[WAL_REC..].to_vec();
    let mut seals = 0;
    for record in legacy.as_chunks_mut::<WAL_REC>().0 {
        if record[0] == WAL_COMMIT {
            // The original pre-metadata record has its opcode and blank-node
            // prefix, with no sequence, checksum, flags or configuration seal.
            record[9..].fill(0);
            assert!(commit::open_wal_commit(record, &[]).is_none());
            seals += 1;
        }
    }
    assert_eq!(seals, 1);
    std::fs::write(&wal, &legacy).unwrap();
    for path in [
        root.join("dataset.json"),
        root.join("commits.bin"),
        wal.parent().unwrap().join("commit.json"),
    ] {
        std::fs::remove_file(path).unwrap();
    }
    validate_copy(&root, false, &preparation()).unwrap();
    assert_eq!(std::fs::read(&wal).unwrap(), legacy);
    let upgraded = Store::open(&root, StoreOptions::default()).unwrap();
    assert_eq!(upgraded.snapshot().len(), 1);
    assert!(upgraded.evidence_view(&At::Head, &preparation()).is_err());
}

#[test]
fn retained_original_header_reauthenticates_its_target_in_each_operation() {
    let temporary = tempfile::tempdir().unwrap();
    let root = temporary.path().join("dataset");
    drop(Store::open(&root, StoreOptions::default()).unwrap());
    let preparation = preparation();
    crate::migrate::activate_configuration_history(&root, false, &preparation).unwrap();
    let store = Store::open(&root, StoreOptions::default()).unwrap();
    update(&store, "actually-admitted");
    let seq = store.head_commit().seq;
    let binding = store.configuration_binding_at(seq, &preparation).unwrap();
    let identity = match binding {
        ConfigurationBinding::Known { configuration } => configuration,
        other => panic!("real publication is not bound: {other:?}"),
    };
    let state = store.managed_state_shared();
    let mut first = primary::Header::from_state(
        &root,
        store.owner_dataset_id(),
        store.dataset_id,
        None,
        state.as_deref(),
        &preparation,
    )
    .unwrap();
    first.generation(&root, &preparation, seq, binding).unwrap();
    let mut next_operation = first.fresh();
    let digest = identity.generation.to_string();
    let target = root
        .join("evidence-history")
        .join(identity.lineage_id.to_string())
        .join("generations")
        .join(format!("{}.json", digest.strip_prefix("sha256:").unwrap()));
    let actual_bytes = std::fs::read(&target).unwrap();
    std::fs::remove_file(&target).unwrap();
    assert!(
        next_operation
            .generation(&root, &preparation, seq, binding)
            .is_err(),
        "a previous operation's successful target lookup cannot certify a missing publication"
    );
    std::fs::write(&target, &actual_bytes).unwrap();
    let mut restored_operation = first.fresh();
    restored_operation
        .generation(&root, &preparation, seq, binding)
        .unwrap();
}
