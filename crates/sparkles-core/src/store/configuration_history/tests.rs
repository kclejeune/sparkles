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
