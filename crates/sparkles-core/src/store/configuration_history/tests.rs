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
    // a format 2 commit.json in a dataset without the configuration reader is
    // refused by the plan itself, as open refuses it, before activation starts
    match crate::migrate::configuration_history_plan(&root, &preparation()) {
        Err(_) => {}
        Ok(plan) => assert!(!plan.refused.is_empty(), "{plan:?}"),
    }
    assert!(crate::migrate::activate_configuration_history(&root, false, &preparation()).is_err());
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
fn copied_primary_validation_refuses_a_pre_release_copy() {
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
    // the validation reads the log's first record by the open's rule, so the headerless
    // pre-release copy is refused with the dump-and-reload message, and the open refuses
    // the copy for its missing dataset.json with the same message
    let refusal = validate_copy(&root, false, &preparation()).err().unwrap();
    assert!(matches!(refusal, Error::Unsupported(_)), "{refusal}");
    assert!(refusal.to_string().contains("wal version 0"), "{refusal}");
    assert!(refusal.to_string().contains("sparkles dump"), "{refusal}");
    let refusal = Store::open(&root, StoreOptions::default()).err().unwrap();
    assert!(matches!(refusal, Error::Unsupported(_)), "{refusal}");
    assert!(refusal.to_string().contains("sparkles dump"), "{refusal}");
    assert_eq!(std::fs::read(&wal).unwrap(), legacy);
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

#[test]
fn a_clone_racing_memory_activation_establishes_an_independent_destination_root() {
    let store = Store::in_memory(StoreOptions::default());
    update(&store, "before");
    store.set_failpoint(
        "clone-admitting",
        Some(std::sync::Arc::new(|s: &Store| {
            s.activate_memory_configuration_history(&preparation())
                .unwrap();
        })),
    );
    let temporary = tempfile::tempdir().unwrap();
    let destination = temporary.path().join("clone");
    let report = store
        .clone_to(&destination, &Default::default())
        .expect("a clone admitted after activation establishes a destination root");
    assert!(destination.join("evidence-history/origin.json").is_file());
    let cloned = Store::open(&destination, StoreOptions::default()).unwrap();
    assert_eq!(cloned.dataset_id(), report.dataset_id);
    assert_ne!(cloned.dataset_id(), store.dataset_id());
    let src_head = store.evidence_view(&At::Head, &preparation()).unwrap();
    let dst_head = cloned.evidence_view(&At::Head, &preparation()).unwrap();
    assert_eq!(dst_head.configuration().dataset_id(), cloned.dataset_id());
    assert_ne!(dst_head.configuration(), src_head.configuration());
    drop(cloned);
    store.set_failpoint("clone-admitting", None);
    let (memory, mem_report) = store
        .clone_to_memory(&Default::default(), StoreOptions::default())
        .expect("an activated in-memory store clones into memory");
    let mem_head = memory.evidence_view(&At::Head, &preparation()).unwrap();
    assert_eq!(mem_head.configuration().dataset_id(), mem_report.dataset_id);
    assert_ne!(mem_head.configuration(), src_head.configuration());
    update(&store, "after");
    update(&memory, "clone-after");
}

/// `sparkles check` runs this validation on a live root. Commits that land while it
/// reads, after it counts the vocabulary or after it has buffered the log, must not
/// read as a damaged committed prefix.
#[test]
fn validation_beside_a_live_writer_reads_one_consistent_prefix() {
    let mut failures = Vec::new();
    for point in ["bound", "vocabulary", "observation"] {
        let dir = tempfile::tempdir().unwrap();
        let store = std::sync::Arc::new(Store::open(dir.path(), StoreOptions::default()).unwrap());
        update(&store, "seed");
        let seeded = store.head_commit().seq;
        let writer = store.clone();
        let mut fired = false;
        validation::INSPECTION_HOOK.with(|hook| {
            *hook.borrow_mut() = Some(Box::new(move |at: &str| {
                if at == point && !fired {
                    fired = true;
                    for i in 0..3 {
                        update(&writer, &format!("live-{point}-{i}"));
                    }
                }
            }))
        });
        let result = validate_copy(dir.path(), false, &preparation());
        validation::INSPECTION_HOOK.with(|hook| hook.borrow_mut().take());
        if let Err(error) = result {
            failures.push(format!("commits after {point}: {error}"));
        }
        // the commits landed and a later validation still proves all of them
        assert_eq!(store.head_commit().seq, seeded + 3, "{point}");
        validate_copy(dir.path(), false, &preparation()).unwrap();
    }
    assert!(failures.is_empty(), "{failures:#?}");
}

/// A compaction that retires the generation after its log is open must leave that
/// generation out, as it did when the log was opened last, not read its vanished
/// vocabulary as empty and the log's terms as damage.
#[test]
fn validation_skips_a_generation_retired_while_it_reads() {
    let dir = tempfile::tempdir().unwrap();
    let options = StoreOptions {
        history_max_generations: 0,
        ..StoreOptions::default()
    };
    let store = std::sync::Arc::new(Store::open(dir.path(), options).unwrap());
    update(&store, "retired-0");
    update(&store, "retired-1");
    let first = std::fs::read_to_string(dir.path().join("CURRENT")).unwrap();
    let compactor = store.clone();
    let mut fired = false;
    validation::INSPECTION_HOOK.with(|hook| {
        *hook.borrow_mut() = Some(Box::new(move |at: &str| {
            if at == "bound" && !fired {
                fired = true;
                compactor.compact().unwrap();
            }
        }))
    });
    let result = validate_copy(dir.path(), false, &preparation());
    validation::INSPECTION_HOOK.with(|hook| hook.borrow_mut().take());
    assert!(
        !dir.path().join(first.trim()).exists(),
        "the compaction retired {first}"
    );
    result.unwrap();
}

/// A closed copy whose generation lost its log still refuses a vocabulary of an
/// unknown format, so the order of the reads hides no refusal.
#[test]
fn validation_without_a_log_still_refuses_an_unknown_vocabulary() {
    let dir = tempfile::tempdir().unwrap();
    {
        let store = Store::open(dir.path(), StoreOptions::default()).unwrap();
        update(&store, "closed");
    }
    let current = std::fs::read_to_string(dir.path().join("CURRENT")).unwrap();
    let generation = dir.path().join(current.trim());
    let vocabulary = generation.join("delta.vocab");
    let mut bytes = std::fs::read(&vocabulary).unwrap();
    assert!(bytes.len() > 12, "the update added delta terms");
    bytes[8..12].copy_from_slice(&u32::MAX.to_le_bytes());
    std::fs::write(&vocabulary, &bytes).unwrap();
    std::fs::remove_file(generation.join("wal.log")).unwrap();
    assert!(validate_copy(dir.path(), false, &preparation()).is_err());
}

/// A compaction trims the old log's preallocated tail to its last commit once it
/// switches `CURRENT` (`trim_wal`). A trim after the validation took the log's length
/// ends the bound at the trimmed end, which still holds every commit record.
#[test]
fn validation_reads_a_log_trimmed_after_its_length_was_taken() {
    let dir = tempfile::tempdir().unwrap();
    {
        let store = Store::open(dir.path(), StoreOptions::default()).unwrap();
        update(&store, "trimmed-0");
        update(&store, "trimmed-1");
    }
    let current = std::fs::read_to_string(dir.path().join("CURRENT")).unwrap();
    let log = dir.path().join(current.trim()).join("wal.log");
    let bytes = std::fs::read(&log).unwrap();
    let records = bytes.len() / crate::store::WAL_REC;
    let end = (0..records)
        .rev()
        .find(|&i| bytes[i * crate::store::WAL_REC] == crate::store::WAL_COMMIT)
        .map(|i| (i + 1) * crate::store::WAL_REC)
        .unwrap();
    // grow the preallocated zeros, as an open writer's log has them
    let file = std::fs::OpenOptions::new().write(true).open(&log).unwrap();
    file.set_len((end + 64 * crate::store::WAL_REC) as u64)
        .unwrap();
    let mut fired = false;
    validation::INSPECTION_HOOK.with(|hook| {
        *hook.borrow_mut() = Some(Box::new(move |at: &str| {
            if at == "extent" && !fired {
                fired = true;
                file.set_len(end as u64).unwrap();
            }
        }))
    });
    let result = validate_copy(dir.path(), false, &preparation());
    validation::INSPECTION_HOOK.with(|hook| hook.borrow_mut().take());
    assert_eq!(
        std::fs::metadata(&log).unwrap().len(),
        end as u64,
        "{result:?}"
    );
    result.unwrap();
}

#[test]
fn clone_destination_syncs_intermediate_import_directories_deepest_first() {
    let dir = tempfile::tempdir().unwrap();
    let src_root = dir.path().join("src");
    drop(Store::open(&src_root, StoreOptions::default()).unwrap());
    crate::migrate::activate_configuration_history(&src_root, false, &preparation()).unwrap();
    let src = Store::open(&src_root, StoreOptions::default()).unwrap();
    update(&src, "seed");
    let state = src.managed_state_shared();
    let installed = match state.as_deref() {
        Some(crate::managed::ManagedState::Installed(i)) => i,
        _ => panic!("expected installed"),
    };
    let prep = preparation();
    let mut source_files = Vec::new();
    for rel in [
        "dataset.json",
        "CURRENT",
        "managed.json",
        "gen-0001/commit.json",
        "gen-0001/meta.json",
    ] {
        let bytes = read_anchored_local_file(&src_root, rel, false, &prep)
            .unwrap()
            .unwrap();
        if rel == "dataset.json" {
            source_files.push((
                "family-dataset.json".into(),
                managed_history::Bytes::copy_from_slice(&bytes.value, &prep).unwrap(),
            ));
        }
        source_files.push((rel.into(), bytes));
    }
    let history = installed.configuration_history.as_ref().unwrap();
    let lineage = history.lineage_id;
    let cov_rel = format!("evidence-history/{lineage}/coverage.json");
    source_files.push((
        cov_rel.clone(),
        read_anchored_local_file(&src_root, &cov_rel, false, &prep)
            .unwrap()
            .unwrap(),
    ));
    let hex = history.current_generation.to_string();
    let hex = hex.strip_prefix("sha256:").unwrap();
    let gen_rel = format!("evidence-history/{lineage}/generations/{hex}.json");
    source_files.push((
        gen_rel.clone(),
        read_anchored_local_file(&src_root, &gen_rel, false, &prep)
            .unwrap()
            .unwrap(),
    ));

    let dst_root = dir.path().join("dst");
    std::fs::create_dir(&dst_root).unwrap();
    let mut synced: Vec<std::path::PathBuf> = Vec::new();
    let root_commit = crate::commit::CommitInfo {
        seq: 0,
        timestamp_ms: 100,
        kind: crate::commit::CommitKind::Create,
        inserted: 1,
        deleted: 0,
        quads: 1,
        generation: 1,
        bulk: true,
        exact: true,
        reconstructed: false,
        default_graph: true,
        unvalidated: false,
    };
    write_clone_destination_with_sync(
        &dst_root,
        uuid::Uuid::new_v4(),
        &root_commit,
        installed,
        src.dataset_id(),
        src.dataset_id(),
        src.head_commit().seq,
        "gen-0001",
        source_files,
        &[],
        Vec::new(),
        &prep,
        &mut |path| {
            assert!(path.is_dir(), "{} must be a directory", path.display());
            synced.push(path.to_path_buf());
            crate::store::sync_dir(path)
        },
    )
    .unwrap();
    let eh = dst_root.join("evidence-history");
    let imports = eh.join("imports");
    assert!(
        synced.iter().any(|p| p == &imports),
        "evidence-history/imports must be synced: {synced:?}"
    );
    assert!(
        synced.iter().any(|p| p.ends_with("generations")),
        "nested generations dir must be synced: {synced:?}"
    );
    assert_eq!(synced.last().unwrap(), &eh);
    for (idx, path) in synced.iter().enumerate() {
        if path != &eh {
            let parent = path.parent().unwrap();
            if parent.starts_with(&eh) {
                let parent_pos = synced.iter().position(|p| p == parent).unwrap_or_else(|| {
                    panic!(
                        "parent {} of {} was never synced: {synced:?}",
                        parent.display(),
                        path.display()
                    )
                });
                assert!(
                    parent_pos > idx,
                    "child {} synced at {idx} after parent {} at {parent_pos}",
                    path.display(),
                    parent.display()
                );
            }
        }
    }
}

#[test]
fn clone_to_with_preparation_accounts_aggregate_metadata_and_rolls_back() {
    let dir = tempfile::tempdir().unwrap();
    let src_root = dir.path().join("src");
    drop(Store::open(&src_root, StoreOptions::default()).unwrap());
    crate::migrate::activate_configuration_history(&src_root, false, &preparation()).unwrap();
    let src = Store::open(&src_root, StoreOptions::default()).unwrap();
    for i in 0..5 {
        let settings = crate::evidence::Settings {
            enabled: true,
            source_records: vec![format!("urn:src:{i}")],
            proposals: vec![format!("urn:prop:{i}")],
            ..Default::default()
        };
        let canonical = settings.canonical(&crate::task::Control::none()).unwrap();
        src.enable_managed(
            crate::managed::OwnerKind::Evidence,
            crate::managed::OwnerEntry::for_settings(
                1,
                settings.managed_graphs(),
                canonical.bytes(),
            ),
            canonical.bytes(),
        )
        .unwrap();
        update(&src, &format!("gen-{i}"));
    }
    // A tiny preparation budget cannot retain all 5 captured generations simultaneously
    // and must fail with BudgetExceeded and restore its room cleanly.
    let tight = Preparation::new(crate::task::Control::none(), 8 << 10).unwrap();
    let before = tight.room();
    let dst = dir.path().join("tight-clone");
    let err = src
        .clone_to_with_preparation(&dst, &Default::default(), &tight)
        .unwrap_err();
    assert!(
        matches!(err, Error::BudgetExceeded(_)),
        "expected BudgetExceeded, got {err:?}"
    );
    assert_eq!(tight.room(), before, "preparation credit must roll back");
    assert!(!dst.exists(), "failed clone must not leave destination");
}

#[test]
fn native_wal_binding_lookup_uses_retained_imports_under_tight_budget() {
    let dir = tempfile::tempdir().unwrap();
    let src_root = dir.path().join("src");
    drop(Store::open(&src_root, StoreOptions::default()).unwrap());
    crate::migrate::activate_configuration_history(&src_root, false, &preparation()).unwrap();
    let src = Store::open(&src_root, StoreOptions::default()).unwrap();
    // Create several configuration generations so a full DiskClosure::load would be expensive.
    for i in 0..6 {
        let settings = crate::evidence::Settings {
            enabled: true,
            source_records: vec![format!("urn:s:{i}")],
            proposals: vec![format!("urn:p:{i}")],
            ..Default::default()
        };
        let canonical = settings.canonical(&crate::task::Control::none()).unwrap();
        src.enable_managed(
            crate::managed::OwnerKind::Evidence,
            crate::managed::OwnerEntry::for_settings(
                1,
                settings.managed_graphs(),
                canonical.bytes(),
            ),
            canonical.bytes(),
        )
        .unwrap();
        update(&src, &format!("s-{i}"));
    }
    let clone_root = dir.path().join("clone");
    src.clone_to(&clone_root, &Default::default()).unwrap();
    let dst = Store::open(&clone_root, StoreOptions::default()).unwrap();
    update(&dst, "native-own");
    let seq = dst.head_commit().seq;
    // A modest 256 KiB budget resolves the native WAL commit cleanly via retained imports
    // without reloading/re-hashing all 6 source generations.
    let small = Preparation::new(crate::task::Control::none(), 256 << 10).unwrap();
    let binding = dst.configuration_binding_at(seq, &small).unwrap();
    assert!(binding.configuration().is_some());
}

#[test]
fn retained_cold_read_charges_closure_and_scopes_wal_proof() {
    let dir = tempfile::tempdir().unwrap();
    let src_root = dir.path().join("src");
    drop(Store::open(&src_root, StoreOptions::default()).unwrap());
    crate::migrate::activate_configuration_history(&src_root, false, &preparation()).unwrap();
    let src = Store::open(&src_root, StoreOptions::default()).unwrap();
    // Create several configuration generations so DiskClosure retains substantial bytes.
    // Give the earliest imported WAL generation (`imp-0`) a large settings payload while
    // keeping later generations small so `imp-0`'s publication decode inside `prove_wal`'s
    // callback cleanly dominates setup and current-registry verification.
    for i in 0..8 {
        let source_records = if i == 0 {
            (0..400).map(|k| format!("urn:r2src:0:{k:04}")).collect()
        } else {
            vec![format!("urn:r2src:{i}")]
        };
        let settings = crate::evidence::Settings {
            enabled: true,
            source_records,
            proposals: vec![format!("urn:r2prop:{i}")],
            ..Default::default()
        };
        let canonical = settings.canonical(&crate::task::Control::none()).unwrap();
        src.enable_managed(
            crate::managed::OwnerKind::Evidence,
            crate::managed::OwnerEntry::for_settings(
                1,
                settings.managed_graphs(),
                canonical.bytes(),
            ),
            canonical.bytes(),
        )
        .unwrap();
        update(&src, &format!("imp-{i}"));
    }
    let imported_seq = src.head_commit().seq;
    let src_id = src.dataset_id();
    let cap = src.backup_capture("b-cold-budget").unwrap();
    drop(src);

    let dst_root = dir.path().join("dst");
    cap.write_to(&dst_root).unwrap();
    let new_id = uuid::Uuid::new_v4();
    reidentify_activated_dir(
        &dst_root,
        new_id,
        crate::commit::ForkedFrom {
            id: src_id,
            seq: imported_seq,
        },
        &crate::task::Control::none(),
    )
    .unwrap();

    let retain_opts = StoreOptions {
        history_max_generations: 4,
        ..StoreOptions::default()
    };
    {
        let dst = Store::open(&dst_root, retain_opts.clone()).unwrap();
        dst.set_retention(crate::history::Retention {
            keep_commits: Some(100),
            ..Default::default()
        })
        .unwrap();
        update(&dst, "own-1");
        update(&dst, "own-2");
        update(&dst, "own-3");
        dst.compact().unwrap();
    }
    let dst = Store::open(&dst_root, retain_opts).unwrap();
    let own_1_seq = imported_seq + 1;
    let own_2_seq = imported_seq + 2;
    let own_last_seq = dst.head_commit().seq;
    assert!(own_last_seq > own_2_seq);

    // 1. Verify disjoint live memory accounting in `DiskClosure::prove_wal` across
    //    BOTH callers (`authenticate_retained_generation` and closed-copy
    //    `validate_copy_imported`):
    //    (a) At a fixed full budget, measure every admission boundary and assert that:
    //        - `actual_commit` stays charged in `budget` during setup and is released
    //          before `physical::prove_prefix*`,
    //        - `actual_meta` (`Owned<IndexMeta>`) and `orig` (`DiskOriginalBase`) keep
    //          their full decoded charges attached inside `setup_prep`,
    //        - `setup_prep` and `callback_prep` never overlap live `budget` charges
    //          (`budget_used + sub_limit <= cap`), directly catching any regression
    //          that passes a pre-proof allowance into the callback.
    //    (b) At a tight budget derived independently from `DiskClosure::load`'s own peak
    //        (no binary search over `prove_wal`), verify that BOTH callers reach
    //        `"callback"` with non-overlapping `sub_limit` and refuse with
    //        `Error::BudgetExceeded`.
    let opts = HistoryOptions::default();
    let entry = {
        let hist = dst.history.as_ref().unwrap().lock();
        hist.gens.get(&1).expect("gen-0001 retained").clone()
    };
    let meta_len = std::fs::metadata(entry.dir.join("meta.json"))
        .unwrap()
        .len();

    let capture_stages = |run: &mut dyn FnMut() -> Result<()>| -> (Result<()>, Vec<ProveWalStage>) {
        let observed = std::sync::Arc::new(parking_lot::Mutex::new(Vec::<ProveWalStage>::new()));
        let observed_clone = observed.clone();
        PROVE_WAL_STAGE_HOOK.with(|hook| {
            *hook.borrow_mut() = Some(Box::new(move |stage| {
                observed_clone.lock().push(stage);
            }));
        });
        let res = run();
        PROVE_WAL_STAGE_HOOK.with(|hook| hook.borrow_mut().take());
        let stages = observed.lock().clone();
        (res, stages)
    };

    let assert_full_disjoint_stages = |label: &str, stages: &[ProveWalStage], expected_max: u64| {
        let entered = stages
            .iter()
            .find(|s| s.stage == "entered")
            .unwrap_or_else(|| panic!("{label}: missing entered stage"));
        let setup_meta = stages
            .iter()
            .find(|s| s.stage == "setup_meta")
            .unwrap_or_else(|| panic!("{label}: missing setup_meta stage"));
        let setup_source = stages
            .iter()
            .find(|s| s.stage == "setup_source")
            .unwrap_or_else(|| panic!("{label}: missing setup_source stage"));
        let before_physical = stages
            .iter()
            .find(|s| s.stage == "before_physical")
            .unwrap_or_else(|| panic!("{label}: missing before_physical stage"));
        let callback = stages
            .iter()
            .find(|s| s.stage == "callback")
            .unwrap_or_else(|| panic!("{label}: missing callback stage"));
        let cap = entered.budget_used + entered.budget_available;
        assert!(
            cap <= expected_max,
            "{label}: budget cap {cap} exceeds caller limit {expected_max}"
        );
        assert!(
            before_physical.budget_used > entered.budget_used,
            "{label}: prove_wal must charge range/span storage ({} > {})",
            before_physical.budget_used,
            entered.budget_used
        );
        assert!(
            setup_meta.budget_used > before_physical.budget_used,
            "{label}: destination commit must remain charged in budget during setup and release before physical proof ({} > {})",
            setup_meta.budget_used,
            before_physical.budget_used
        );
        let meta_min_charge = meta_len * 128 + 1024;
        assert!(
            setup_meta.sub_used.unwrap() >= meta_min_charge,
            "{label}: actual_meta must retain its decoded charge in setup_prep ({} >= {meta_min_charge})",
            setup_meta.sub_used.unwrap()
        );
        assert!(
            setup_source.sub_used.unwrap() > setup_meta.sub_used.unwrap() + meta_len * 128,
            "{label}: DiskOriginalBase must retain its decoded charge on top of actual_meta ({} > {} + {})",
            setup_source.sub_used.unwrap(),
            setup_meta.sub_used.unwrap(),
            meta_len * 128
        );
        assert!(
            setup_meta.budget_used + setup_meta.sub_limit.unwrap() <= cap,
            "{label}: setup_prep limit ({}) must not overlap live budget charge ({}) against cap ({cap})",
            setup_meta.sub_limit.unwrap(),
            setup_meta.budget_used
        );
        assert!(
            callback.budget_used > before_physical.budget_used,
            "{label}: physical proof buffer/state must be charged during callback ({} > {})",
            callback.budget_used,
            before_physical.budget_used
        );
        assert_eq!(
            callback.sub_limit.unwrap(),
            callback.budget_available,
            "{label}: callback_prep must receive only proof_budget.available()"
        );
        assert!(
            callback.budget_used + callback.sub_limit.unwrap() <= cap,
            "{label}: callback_prep limit ({}) must not overlap proof_budget charge ({}) against cap ({cap})",
            callback.sub_limit.unwrap(),
            callback.budget_used
        );
        (
            cap,
            before_physical.budget_used,
            setup_source.sub_used.unwrap(),
        )
    };

    let full_budget = 64u64 << 20;
    let full_limit = ReadLimit {
        options: &opts,
        memory: Some(full_budget),
        held: 0,
    };
    let (res, retained_stages) = capture_stages(&mut || {
        super::branching::readonly::authenticate_retained_generation(
            &dst,
            &entry,
            &full_limit,
            imported_seq,
        )
    });
    res.unwrap();
    let (retained_cap, retained_before_physical, retained_setup_peak) =
        assert_full_disjoint_stages("retained_cold_read", &retained_stages, full_budget);
    assert_eq!(retained_cap, full_budget);

    let (res, validate_stages) = capture_stages(&mut || {
        let prep = Preparation::new(crate::task::Control::none(), full_budget)?;
        validate_copy_imported(&dst_root, &prep)
    });
    res.unwrap();
    let (validate_cap, validate_before_physical, validate_setup_peak) =
        assert_full_disjoint_stages("closed_copy_validate", &validate_stages, full_budget);
    assert!(
        validate_cap < full_budget,
        "closed-copy validation must reserve DiskClosure in Preparation before ColdBudget ({validate_cap} < {full_budget})"
    );

    // Independently measure the exact publication-walk peak on `src_root` via
    // `managed_history::resolve` (which makes no whole-room loan and never touches
    // `prove_wal`). Adding a 16 KiB margin above `before_physical + src_pub_peak` clears
    // `DiskClosure::load`, base/setup verification, and `physical::prove_prefix`
    // buffering while leaving `callback_prep` short of `imp-0`'s publication walk by
    // the ~64 KiB physical proof allowance. A future regression that overlaps
    // `callback_prep` with the pre-proof allowance would therefore succeed here instead
    // of failing with `BudgetExceeded`.
    let src_history = primary::recorded_root(&src_root, &preparation())
        .unwrap()
        .expect("source history root");
    let src_pub_peak = {
        let probe_prep = Preparation::new(crate::task::Control::none(), full_budget).unwrap();
        let doc = managed_history::resolve(
            &src_root,
            src_id,
            &src_history,
            src_history.identity(src_id),
            &probe_prep,
        )
        .unwrap();
        drop(doc);
        probe_prep.test_peak()
    };
    assert!(
        src_pub_peak < (4 << 20),
        "src_pub_peak ({src_pub_peak}) must reflect actual decode peak, not a whole-room loan"
    );
    let max_setup_peak = retained_setup_peak.max(validate_setup_peak);
    assert!(
        src_pub_peak > max_setup_peak + (64 << 10),
        "independent src_pub_peak ({src_pub_peak}) must exceed setup peak ({max_setup_peak}) + headroom"
    );
    let tight_room = src_pub_peak + (16 << 10);

    // Tight-budget refusal through `authenticate_retained_generation`.
    let tight_retained = retained_before_physical + tight_room;
    let tight_limit = ReadLimit {
        options: &opts,
        memory: Some(tight_retained),
        held: 0,
    };
    let (res, tight_retained_stages) = capture_stages(&mut || {
        super::branching::readonly::authenticate_retained_generation(
            &dst,
            &entry,
            &tight_limit,
            imported_seq,
        )
    });
    let err = res.unwrap_err();
    let cb = tight_retained_stages
        .iter()
        .find(|s| s.stage == "callback")
        .unwrap_or_else(|| {
            panic!(
                "retained tight budget ({tight_retained}) must reach callback before failing; stages={tight_retained_stages:?}, err={err:?}"
            )
        });
    assert!(
        cb.budget_used > retained_before_physical,
        "retained tight callback must include physical proof charges ({} > {retained_before_physical})",
        cb.budget_used
    );
    assert!(
        cb.budget_used + cb.sub_limit.unwrap() <= tight_retained,
        "retained tight callback must not overlap budget ({} + {} <= {tight_retained})",
        cb.budget_used,
        cb.sub_limit.unwrap()
    );
    assert!(
        cb.sub_limit.unwrap() < src_pub_peak,
        "retained tight callback limit ({}) must be below imp-0 publication walk peak ({src_pub_peak})",
        cb.sub_limit.unwrap()
    );
    assert!(
        matches!(err, Error::BudgetExceeded(_)),
        "expected BudgetExceeded at retained proof callback under independent tight limit ({tight_retained}), got {err:?}"
    );

    // Tight-budget refusal through closed-copy `validate_copy_imported`.
    let validate_pre_budget = full_budget - validate_cap;
    let tight_validate = validate_pre_budget + validate_before_physical + tight_room;
    let (res, tight_validate_stages) = capture_stages(&mut || {
        let prep = Preparation::new(crate::task::Control::none(), tight_validate)?;
        validate_copy_imported(&dst_root, &prep)
    });
    let err = res.unwrap_err();
    let v_cb = tight_validate_stages
        .iter()
        .find(|s| s.stage == "callback")
        .unwrap_or_else(|| {
            panic!(
                "closed-copy tight budget ({tight_validate}) must reach callback before failing; stages={tight_validate_stages:?}, err={err:?}"
            )
        });
    assert!(
        v_cb.budget_used > validate_before_physical,
        "closed-copy tight callback must include physical proof charges ({} > {validate_before_physical})",
        v_cb.budget_used
    );
    assert!(
        v_cb.budget_used + v_cb.sub_limit.unwrap() <= tight_validate - validate_pre_budget,
        "closed-copy tight callback must not overlap budget ({} + {} <= {})",
        v_cb.budget_used,
        v_cb.sub_limit.unwrap(),
        tight_validate - validate_pre_budget
    );
    assert!(
        v_cb.sub_limit.unwrap() < src_pub_peak,
        "closed-copy tight callback limit ({}) must be below imp-0 publication walk peak ({src_pub_peak})",
        v_cb.sub_limit.unwrap()
    );
    assert!(
        matches!(err, Error::BudgetExceeded(_)),
        "expected BudgetExceeded at closed-copy proof callback under independent tight limit ({tight_validate}), got {err:?}"
    );

    // 2. Scoped WAL proof: corrupt `own-1` (strictly between `imported_seq` and
    //    `own-2`, both owned by retained `gen-0001`) while leaving `own-2` intact so
    //    `durable_after` proves `own-1` durable. Reading `imported_seq` must still
    //    succeed while proving through `own_2_seq` or `u64::MAX` (and cold-reading
    //    `own_2_seq` from retained `gen-0001`) must fail with `Error::Corrupt`.
    let wal_path = entry.dir.join("wal.log");
    let own_1_end = {
        let mut cursor = wal::WalCursor::open(
            &wal_path,
            wal::WalPoint {
                seq: entry.base.seq,
                offset: 0,
            },
        )
        .unwrap();
        let mut end = None;
        while let Some((seq, _)) = cursor.next().unwrap() {
            if seq == own_1_seq {
                end = Some(cursor.position().offset);
                break;
            }
        }
        end.expect("own-1 must be present in retained gen-0001 wal.log")
    };
    let mut wal_bytes = std::fs::read(&wal_path).unwrap();
    assert!(
        (own_1_end as usize) < wal_bytes.len(),
        "own-2 must follow own-1 as a later durability witness"
    );
    // Flip a byte inside `own-1`'s WAL_COMMIT record while leaving `own-2` intact.
    wal_bytes[own_1_end as usize - 1] ^= 0xFF;
    std::fs::write(&wal_path, &wal_bytes).unwrap();
    super::branching::readonly::authenticate_retained_generation(
        &dst,
        &entry,
        &full_limit,
        imported_seq,
    )
    .expect("scoped prove_wal through imported_seq must not depend on unrelated native suffix");
    let full_err = super::branching::readonly::authenticate_retained_generation(
        &dst,
        &entry,
        &full_limit,
        u64::MAX,
    )
    .unwrap_err();
    assert!(
        matches!(full_err, Error::Corrupt(_)),
        "unscoped proof must fail with Error::Corrupt on durable damaged own-1, got {full_err:?}"
    );
    let own_2_err = super::branching::readonly::authenticate_retained_generation(
        &dst,
        &entry,
        &full_limit,
        own_2_seq,
    )
    .unwrap_err();
    assert!(
        matches!(own_2_err, Error::Corrupt(_)),
        "proof through own-2 must fail with Error::Corrupt on durable damaged own-1, got {own_2_err:?}"
    );
    assert!(
        dst.snapshot_at(&crate::history::At::Commit(own_2_seq), &opts)
            .is_err(),
        "cold RDF read of own_2_seq in retained gen-0001 must fail when durable own-1 is corrupted"
    );
    let (snap, _) = dst
        .snapshot_at(&crate::history::At::Commit(imported_seq), &opts)
        .expect("cold RDF read of imported_seq must succeed despite corrupted native suffix");
    assert!(snap.len() >= 8);
}

#[test]
fn reidentify_streams_multi_chunk_under_tight_budget_and_cancels_in_flight() {
    let dir = tempfile::tempdir().unwrap();
    let src_root = dir.path().join("src");
    drop(Store::open(&src_root, StoreOptions::default()).unwrap());
    crate::migrate::activate_configuration_history(&src_root, false, &preparation()).unwrap();
    let src = Store::open(&src_root, StoreOptions::default()).unwrap();
    // Write enough commits and annotations so both `annotations.bin` and `wal.log`
    // exceed 150 KiB (>2.3 x 64 KiB streaming chunks) within the 1024-byte message
    // cap, reusing a small vocabulary so `delta.vocab` stays tiny and the tight
    // streaming budget directly exercises constant-buffer streaming.
    for i in 0..180 {
        let mut w = src.write_with(
            crate::commit::CommitKind::Update,
            crate::guard::WriteOptions {
                message: Some(std::sync::Arc::from(format!(
                    "multi-chunk-{i:03}-{}",
                    "a".repeat(960)
                ))),
                ..Default::default()
            },
        );
        let p = w
            .intern(&oxrdf::Term::NamedNode(
                oxrdf::NamedNode::new("urn:p").unwrap(),
            ))
            .unwrap();
        for j in 0..25 {
            let s = w
                .intern(&oxrdf::Term::NamedNode(
                    oxrdf::NamedNode::new(format!("urn:s:{j}")).unwrap(),
                ))
                .unwrap();
            let o = w
                .intern(&oxrdf::Term::Literal(oxrdf::Literal::new_simple_literal(
                    format!("val-{j}"),
                )))
                .unwrap();
            if i % 2 == 0 {
                w.insert([s, p, o, crate::id::Id::DEFAULT_GRAPH]).unwrap();
            } else {
                w.delete([s, p, o, crate::id::Id::DEFAULT_GRAPH]).unwrap();
            }
        }
        w.commit().unwrap();
    }
    let src_id = src.dataset_id();
    let src_seq = src.head_commit().seq;
    let cap = src.backup_capture("b-stream").unwrap();
    drop(src);

    // 1. Multi-chunk streaming succeeds under a tight Preparation allowance smaller
    //    than `annotations.bin.len() + wal.log.len()`.
    let stream_dir = dir.path().join("stream-ok");
    cap.write_to(&stream_dir).unwrap();
    let ann_len = std::fs::metadata(stream_dir.join(crate::annotations::FILE))
        .unwrap()
        .len();
    let cur = std::fs::read_to_string(stream_dir.join("CURRENT"))
        .unwrap()
        .trim()
        .to_string();
    let wal_len = std::fs::metadata(stream_dir.join(&cur).join("wal.log"))
        .unwrap()
        .len();
    assert!(
        ann_len > 150_000,
        "expected multi-chunk annotations.bin > 150 KiB, got {ann_len}"
    );
    assert!(
        wal_len > 150_000,
        "expected multi-chunk wal.log > 150 KiB, got {wal_len}"
    );
    let tight_budget = 256 << 10;
    assert!(
        tight_budget < ann_len + wal_len,
        "tight budget {tight_budget} must be smaller than combined files {}",
        ann_len + wal_len
    );
    let tight_prep = Preparation::new(crate::task::Control::none(), tight_budget).unwrap();
    let new_id = uuid::Uuid::new_v4();
    reidentify_activated_dir_with(
        &stream_dir,
        new_id,
        crate::commit::ForkedFrom {
            id: src_id,
            seq: src_seq,
        },
        &tight_prep,
    )
    .expect("multi-chunk streaming reidentification must fit in tight budget");
    let reopened = Store::open(&stream_dir, StoreOptions::default()).unwrap();
    assert_eq!(reopened.dataset_id(), new_id);
    assert_eq!(crate::annotations::read(&stream_dir).unwrap().len(), 180);

    // 2. In-flight cancellation triggered after the first 64 KiB WAL hashing chunk
    //    must abort before reading chunk 1 (exactly one chunk observed).
    let cancel_dir = dir.path().join("cancel-inflight");
    cap.write_to(&cancel_dir).unwrap();
    let ctl = crate::task::Control::none();
    let cancel_handle = ctl.cancel.clone();
    let chunks_seen = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let chunks_seen_clone = chunks_seen.clone();
    WAL_HASH_CHUNK_HOOK.with(|hook| {
        *hook.borrow_mut() = Some(Box::new(move |chunk_index| {
            chunks_seen_clone.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            if chunk_index == 0 {
                cancel_handle.cancel();
            }
        }));
    });
    let res = reidentify_activated_dir(
        &cancel_dir,
        uuid::Uuid::new_v4(),
        crate::commit::ForkedFrom {
            id: src_id,
            seq: src_seq,
        },
        &ctl,
    );
    WAL_HASH_CHUNK_HOOK.with(|hook| hook.borrow_mut().take());
    assert_eq!(
        chunks_seen.load(std::sync::atomic::Ordering::SeqCst),
        1,
        "WAL hashing must stop immediately after chunk 0 cancellation rather than hashing remaining chunks"
    );
    assert!(
        matches!(res, Err(Error::Cancelled)),
        "expected Error::Cancelled on in-flight WAL hash cancellation, got {res:?}"
    );
}

#[test]
fn direct_branch_backup_capture_survives_concurrent_compaction() {
    let dir = tempfile::tempdir().unwrap();
    let src_root = dir.path().join("src");
    drop(Store::open(&src_root, StoreOptions::default()).unwrap());
    crate::migrate::activate_configuration_history(&src_root, false, &preparation()).unwrap();
    let src = std::sync::Arc::new(Store::open(&src_root, StoreOptions::default()).unwrap());
    update(&src, "seed");
    src.create_branch("concurrent-br", &Default::default())
        .unwrap();
    let branch = src.branch("concurrent-br").unwrap();
    update(&branch, "br-1");
    branch.compact().unwrap();
    update(&branch, "br-2");

    // Fire a concurrent compaction right after `capture_once_prepared` releases
    // the branch writer lock (once), forcing the capture retry loop to handle
    // the generation advance without producing a mismatched source/CURRENT.
    let compacted = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let compacted_clone = compacted.clone();
    let src_for_fp = src.clone();
    branch.set_failpoint(
        "backup-capture-unlocked",
        Some(std::sync::Arc::new(move |_| {
            if !compacted_clone.swap(true, std::sync::atomic::Ordering::SeqCst) {
                src_for_fp
                    .branch("concurrent-br")
                    .unwrap()
                    .compact()
                    .unwrap();
            }
        })),
    );
    let cap = branch.backup_capture("b-concurrent-br").unwrap();
    branch.set_failpoint("backup-capture-unlocked", None);
    assert!(
        compacted.load(std::sync::atomic::Ordering::SeqCst),
        "concurrent compaction failpoint must have fired"
    );

    let staged = dir.path().join("staged-br");
    cap.write_to(&staged).unwrap();
    validate_copy_imported(&staged, &preparation())
        .expect("captured branch backup after concurrent compaction must pass imported validation");
    let opened = Store::open(&staged, StoreOptions::default()).unwrap();
    assert_eq!(opened.snapshot().len(), branch.snapshot().len());
}

#[test]
fn retained_binding_uses_authoritative_proof_observation() {
    let dir = tempfile::tempdir().unwrap();
    let src_root = dir.path().join("src");
    drop(Store::open(&src_root, StoreOptions::default()).unwrap());
    crate::migrate::activate_configuration_history(&src_root, false, &preparation()).unwrap();
    let src = Store::open(&src_root, StoreOptions::default()).unwrap();
    update(&src, "imp-1");
    let imported_seq = src.head_commit().seq;
    let src_id = src.dataset_id();
    let cap = src.backup_capture("b-ret-obs").unwrap();
    drop(src);

    let dst_root = dir.path().join("dst");
    cap.write_to(&dst_root).unwrap();
    let new_id = uuid::Uuid::new_v4();
    reidentify_activated_dir(
        &dst_root,
        new_id,
        crate::commit::ForkedFrom {
            id: src_id,
            seq: imported_seq,
        },
        &crate::task::Control::none(),
    )
    .unwrap();

    let retain_opts = StoreOptions {
        history_max_generations: 4,
        ..StoreOptions::default()
    };
    let own_1_seq = {
        let dst = Store::open(&dst_root, retain_opts.clone()).unwrap();
        dst.set_retention(crate::history::Retention {
            keep_commits: Some(100),
            ..Default::default()
        })
        .unwrap();
        // Write a final annotated native commit in gen-0001 so WAL_FLAG_ANNOTATED is set.
        let mut w = dst.write_with(
            crate::commit::CommitKind::Update,
            crate::guard::WriteOptions {
                message: Some(std::sync::Arc::from("annotated-native-tail")),
                ..Default::default()
            },
        );
        let s = w
            .intern(&oxrdf::Term::NamedNode(
                oxrdf::NamedNode::new("urn:own-ann").unwrap(),
            ))
            .unwrap();
        let p = w
            .intern(&oxrdf::Term::NamedNode(
                oxrdf::NamedNode::new("urn:p").unwrap(),
            ))
            .unwrap();
        let o = w
            .intern(&oxrdf::Term::Literal(oxrdf::Literal::new_simple_literal(
                "v",
            )))
            .unwrap();
        w.insert([s, p, o, crate::id::Id::DEFAULT_GRAPH]).unwrap();
        let info = w.commit().unwrap();
        // Commit one more update before compacting so `gen-0002` starts after `own-1`
        // and retained `gen-0001` owns `own-1`.
        update(&dst, "own-after-ann");
        dst.compact().unwrap();
        info.commit.seq
    };

    let dst = Store::open(&dst_root, retain_opts).unwrap();
    let prep = preparation();
    // Both the imported commit and the valid annotated native suffix commit in retained
    // gen-0001 resolve cleanly through the authoritative WAL proof.
    let imp_binding = dst.configuration_binding_at(imported_seq, &prep).unwrap();
    assert!(matches!(
        imp_binding,
        ConfigurationBinding::ImportedUnbound { .. }
    ));
    let native_binding = dst.configuration_binding_at(own_1_seq, &prep).unwrap();
    assert!(matches!(native_binding, ConfigurationBinding::Known { .. }));

    // Truncate retained `gen-0001/wal.log` right after `own-1` (so `own-1` is the tail
    // commit with no later durability witness) and remove `own-1`'s annotation from
    // `annotations.bin` (leaving only the 32-byte header). `WalCursor` still observes
    // `own-1`, whereas the authoritative WAL proof treats an annotated tail without its
    // annotation as incomplete and omits it.
    let gen1_wal = dst_root.join("gen-0001").join("wal.log");
    let own_1_end = {
        let mut cursor =
            wal::WalCursor::open(&gen1_wal, wal::WalPoint { seq: 0, offset: 0 }).unwrap();
        let mut end = None;
        while let Some((seq, _)) = cursor.next().unwrap() {
            if seq == own_1_seq {
                end = Some(cursor.position().offset);
                break;
            }
        }
        end.expect("own-1 must be present in gen-0001/wal.log")
    };
    let wal_bytes = std::fs::read(&gen1_wal).unwrap();
    std::fs::write(&gen1_wal, &wal_bytes[..own_1_end as usize]).unwrap();

    let ann_path = dst_root.join(crate::annotations::FILE);
    let ann_bytes = std::fs::read(&ann_path).unwrap();
    assert!(ann_bytes.len() > 32);
    // Keep only the 32-byte annotations header so `seqs_controlled` returns an empty set.
    std::fs::write(&ann_path, &ann_bytes[..32]).unwrap();

    assert!(
        dst.configuration_binding_at(own_1_seq, &prep).is_err(),
        "retained WAL binding must refuse a commit omitted by the authoritative proof"
    );
    assert!(
        matches!(
            dst.configuration_binding_at(imported_seq, &prep).unwrap(),
            ConfigurationBinding::ImportedUnbound { .. }
        ),
        "earlier imported commit must still resolve via the authoritative proof"
    );
}

#[test]
fn direct_branch_backup_uses_single_anchored_commit_read() {
    for (case, write_after_compact) in [("header-only-wal", false), ("nonempty-wal", true)] {
        let dir = tempfile::tempdir().unwrap();
        let src_root = dir.path().join("src");
        drop(Store::open(&src_root, StoreOptions::default()).unwrap());
        crate::migrate::activate_configuration_history(&src_root, false, &preparation()).unwrap();
        let src = Store::open(&src_root, StoreOptions::default()).unwrap();
        update(&src, "seed");
        src.create_branch("single-read-br", &Default::default())
            .unwrap();
        let branch = src.branch("single-read-br").unwrap();
        update(&branch, "br-1");
        // Compact once to unlink the branch so `backup_capture` takes the direct capture path.
        // Without a post-compaction write, `wal.log` is header-only and `check_wal_prefix`
        // must reuse the anchored base sequence instead of rereading `commit.json`.
        branch.compact().unwrap();
        if write_after_compact {
            update(&branch, "br-2");
        }

        let br_root = branch.root.clone().expect("persistent branch root");
        let gen_name = std::fs::read_to_string(br_root.join("CURRENT"))
            .unwrap()
            .trim()
            .to_string();
        let commit_path = br_root.join(&gen_name).join("commit.json");
        let orig_commit_bytes = std::fs::read(&commit_path).unwrap();

        // Immediately after the single anchored read of `commit.json`, overwrite it on disk
        // with garbage; any second open (in `build_branch_standalone_provenance` or
        // `check_wal_prefix`) would fail or emit mismatched source provenance.
        let fired = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let fired_clone = fired.clone();
        let commit_path_clone = commit_path.clone();
        branch.set_failpoint(
            "backup-capture-branch-commit-read",
            Some(std::sync::Arc::new(move |_| {
                fired_clone.store(true, std::sync::atomic::Ordering::SeqCst);
                std::fs::write(&commit_path_clone, b"{\"mutated\":true}\n").unwrap();
            })),
        );
        let cap = branch
            .backup_capture(&format!("b-single-read-{case}"))
            .unwrap_or_else(|err| panic!("{case}: backup_capture must succeed, got {err:?}"));
        branch.set_failpoint("backup-capture-branch-commit-read", None);
        assert!(
            fired.load(std::sync::atomic::Ordering::SeqCst),
            "{case}: single-read failpoint must have fired"
        );

        let staged = dir.path().join("staged");
        cap.write_to(&staged).unwrap();
        validate_copy_imported(&staged, &preparation()).unwrap_or_else(|err| {
            panic!("{case}: direct branch backup must authenticate cleanly, got {err:?}")
        });

        // Verify the source descriptor embedded the exact original commit.json bytes.
        let imports_dir = staged.join("evidence-history").join("imports");
        let mut found_source_commit = None;
        for entry in std::fs::read_dir(&imports_dir).unwrap() {
            let candidate = entry
                .unwrap()
                .path()
                .join("source")
                .join(&gen_name)
                .join("commit.json");
            if candidate.exists() {
                found_source_commit = Some(std::fs::read(&candidate).unwrap());
            }
        }
        assert_eq!(
            found_source_commit.as_deref(),
            Some(orig_commit_bytes.as_slice()),
            "{case}: staged source commit.json must match the single anchored read, not the later mutated file"
        );
    }
}

#[test]
fn direct_branch_backup_rejects_malformed_canonical_timestamp_with_wal_suffix() {
    let dir = tempfile::tempdir().unwrap();
    let src_root = dir.path().join("src");
    drop(Store::open(&src_root, StoreOptions::default()).unwrap());
    crate::migrate::activate_configuration_history(&src_root, false, &preparation()).unwrap();
    let src = Store::open(&src_root, StoreOptions::default()).unwrap();
    update(&src, "seed");
    src.create_branch("bad-ts-br", &Default::default()).unwrap();
    let branch = src.branch("bad-ts-br").unwrap();
    update(&branch, "br-1");
    branch.compact().unwrap();
    // Append a WAL suffix so `check_wal_prefix` checks the WAL tail rather than `commit.json`.
    update(&branch, "br-2");

    let br_root = branch.root.clone().expect("persistent branch root");
    let gen_name = std::fs::read_to_string(br_root.join("CURRENT"))
        .unwrap()
        .trim()
        .to_string();
    let commit_path = br_root.join(&gen_name).join("commit.json");
    let orig_text = std::fs::read_to_string(&commit_path).unwrap();
    let parsed: serde_json::Value = serde_json::from_str(&orig_text).unwrap();
    let orig_ts = parsed["commit"]["timestamp"]
        .as_str()
        .expect("timestamp field")
        .to_owned();
    let mutated_text = orig_text.replacen(
        &format!("\"timestamp\": \"{orig_ts}\""),
        "\"timestamp\": \"not-a-timestamp\"",
        1,
    );
    assert_ne!(
        mutated_text, orig_text,
        "must replace canonical timestamp field"
    );
    std::fs::write(&commit_path, mutated_text.as_bytes()).unwrap();

    let err = match branch.backup_capture("b-bad-ts") {
        Ok(_) => panic!("direct branch capture must reject malformed canonical timestamp"),
        Err(err) => err,
    };
    assert!(
        matches!(err, Error::Corrupt(_)) && err.to_string().contains("timestamp"),
        "expected Corrupt timestamp refusal, got {err:?}"
    );
}

#[test]
fn materialized_import_capture_checks_max_files_before_pinning() {
    let dir = tempfile::tempdir().unwrap();
    let src_root = dir.path().join("src");
    drop(Store::open(&src_root, StoreOptions::default()).unwrap());
    crate::migrate::activate_configuration_history(&src_root, false, &preparation()).unwrap();
    let src = Store::open(&src_root, StoreOptions::default()).unwrap();
    update(&src, "seed");
    let src_id = src.dataset_id();
    let src_seq = src.head_commit().seq;
    let cap = src.backup_capture("b-max-files").unwrap();

    // 1. Imported persistent destination: `capture_materialized_imported_origin` and
    //    `capture_import_files` must refuse BEFORE staging or pinning handles when
    //    `existing_files + imported_count >= MAX_FILES`.
    let dst_root = dir.path().join("dst");
    cap.write_to(&dst_root).unwrap();
    reidentify_activated_dir(
        &dst_root,
        uuid::Uuid::new_v4(),
        crate::commit::ForkedFrom {
            id: src_id,
            seq: src_seq,
        },
        &crate::task::Control::none(),
    )
    .unwrap();
    let dst = Store::open(&dst_root, StoreOptions::default()).unwrap();
    let prep = preparation();
    let wopts = crate::guard::WriteOptions::default();
    let existing = super::managed_history::capture::MAX_FILES - 2;

    let staging_disk = dir.path().join("staging-disk");
    std::fs::create_dir_all(&staging_disk).unwrap();
    let err = capture_materialized_imported_origin(
        &dst_root,
        &dst.family,
        &staging_disk,
        existing,
        &prep,
        &wopts,
    )
    .unwrap_err();
    assert!(
        err.to_string().contains("backup manifest limits"),
        "expected manifest limit refusal, got {err:?}"
    );
    assert_eq!(
        std::fs::read_dir(&staging_disk).unwrap().count(),
        0,
        "capture_materialized_imported_origin must not stage or open files once limit is exceeded"
    );

    let err_direct = capture_import_files(&dst_root, existing, &prep, &wopts).unwrap_err();
    assert!(
        err_direct.to_string().contains("backup manifest limits"),
        "expected manifest limit refusal from capture_import_files, got {err_direct:?}"
    );

    // 2. In-memory clone: `write_memory_clone_origin` must refuse BEFORE staging or
    //    pinning handles when `existing_files + planned_files >= MAX_FILES`.
    let (mem_clone, _) = src
        .clone_to_memory(&CloneOptions::default(), StoreOptions::default())
        .unwrap();
    let state = mem_clone.managed_state_shared().unwrap();
    let crate::managed::ManagedState::Installed(installed) = state.as_ref() else {
        panic!("expected installed managed state on memory clone");
    };
    let history = installed.configuration_history.as_ref().unwrap();
    let memory = installed.history_memory.as_ref().unwrap();
    let mem_origin = memory.origin.as_ref().unwrap();
    let genesis = ConfigurationIdentity {
        dataset_id: mem_clone.owner_dataset_id(),
        lineage_id: history.lineage_id,
        generation: memory.coverage.value.activation_generation,
    };
    let staging_mem = dir.path().join("staging-mem");
    std::fs::create_dir_all(&staging_mem).unwrap();
    let err_mem = write_memory_clone_origin(
        &staging_mem,
        mem_clone.dataset_id(),
        genesis,
        history.coverage,
        mem_origin,
        existing,
        &prep,
        &wopts,
    )
    .unwrap_err();
    assert!(
        err_mem.to_string().contains("backup manifest limits"),
        "expected manifest limit refusal from write_memory_clone_origin, got {err_mem:?}"
    );
    assert_eq!(
        std::fs::read_dir(&staging_mem).unwrap().count(),
        0,
        "write_memory_clone_origin must not stage or open files once limit is exceeded"
    );
}
