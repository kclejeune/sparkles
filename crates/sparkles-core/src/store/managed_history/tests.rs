use super::*;
use crate::managed::{ManagedGraphs, OwnerEntry};
use crate::task::Control;

fn preparation() -> Preparation {
    Preparation::new(Control::none(), 8 << 20).unwrap()
}

fn empty() -> Installed {
    Installed::new(
        ManagedGraphs {
            revision: 0,
            owners: BTreeMap::new(),
        },
        BTreeMap::new(),
    )
    .unwrap()
}

fn initial(preparation: &Preparation) -> Prepared {
    prepare(
        Uuid::from_u128(1),
        Uuid::from_u128(2),
        1,
        None,
        &empty(),
        preparation,
    )
    .unwrap()
}

fn coverage(first: &Prepared) -> Coverage {
    Coverage {
        format: FORMAT,
        dataset_id: first.identity.dataset_id,
        lineage_id: first.identity.lineage_id,
        activation_generation: first.identity.generation,
        branches: [(
            Uuid::from_u128(1),
            BranchCoverage {
                activation_head: 0,
                first_bound_own: 1,
            },
        )]
        .into(),
    }
}

#[test]
fn empty_generation_has_an_independent_encoding_and_digest() {
    let preparation = preparation();
    let first = initial(&preparation);
    let golden = br#"{"format":1,"datasetId":"00000000-0000-0000-0000-000000000001","lineageId":"00000000-0000-0000-0000-000000000002","ordinal":1,"previous":null,"registryRevision":0,"owners":{},"ownerSettings":{},"evidenceState":"absent"}"#;
    assert_eq!(first.bytes.value, golden);
    assert_eq!(
        first.identity.generation.to_string(),
        "sha256:c4fd704beda2e1ace90201cdf6f2dde7f3a251e8239a636e850c4a22c20f5e51"
    );
    let decoded: Owned<Generation> = decode(golden, &preparation).unwrap();
    decoded.value.validate(&preparation).unwrap();
    assert_eq!(decoded.value, first.document.value);
}

#[test]
fn complete_settings_and_every_protection_are_retained() {
    let preparation = preparation();
    let settings = Settings {
        enabled: true,
        source_records: vec!["urn:source".into()],
        proposals: vec!["urn:proposal".into()],
        ..Default::default()
    };
    let canonical = settings.canonical(preparation.control()).unwrap();
    let mut entry = OwnerEntry::for_settings(1, settings.managed_graphs(), canonical.bytes());
    for (index, graph) in entry.graphs.iter().enumerate() {
        entry
            .protections
            .insert(graph.clone(), Uuid::from_u128(index as u128 + 11));
    }
    let installed = Installed::new(
        ManagedGraphs {
            revision: 7,
            owners: [(OwnerKind::Evidence, entry)].into(),
        },
        [(OwnerKind::Evidence, std::sync::Arc::from(canonical.bytes()))].into(),
    )
    .unwrap();
    let prepared = prepare(
        Uuid::from_u128(1),
        Uuid::from_u128(2),
        1,
        None,
        &installed,
        &preparation,
    )
    .unwrap();
    let decoded: Owned<Generation> = decode(&prepared.bytes.value, &preparation).unwrap();
    decoded.value.validate(&preparation).unwrap();
    assert!(decoded.value.matches_installed(&installed));
    assert_eq!(decoded.value.evidence_state, EvidenceState::Enabled);
    assert_eq!(
        decoded.value.owner_settings[&OwnerKind::Evidence].as_bytes(),
        canonical.bytes()
    );
}

#[test]
fn duplicate_unknown_or_noncanonical_documents_never_become_history() {
    let preparation = preparation();
    let prepared = initial(&preparation);
    let original = std::str::from_utf8(&prepared.bytes.value).unwrap();
    for input in [
        original.replacen("\"format\":1", "\"format\":1,\"format\":1", 1),
        original.replacen("\"owners\":{}", "\"owners\":{},\"surprise\":1", 1),
        original.replacen("\"ordinal\":1", "\"ordinal\":true", 1),
        original.replacen("\"format\":1", "\"format\":true", 1),
        format!(" {original}"),
    ] {
        assert!(
            decode::<Generation>(input.as_bytes(), &preparation).is_err(),
            "{input}"
        );
    }
    let future = br#"{"format":2,"futurePayload":[false]}"#;
    assert!(matches!(
        decode::<Generation>(future, &preparation),
        Err(Error::Unsupported(_))
    ));
}

#[test]
fn bounds_and_cancellation_are_checked_before_copy_or_decode() {
    let preparation = Preparation::new(Control::none(), 256).unwrap();
    assert!(
        prepare(
            Uuid::from_u128(1),
            Uuid::from_u128(2),
            1,
            None,
            &empty(),
            &preparation
        )
        .is_err()
    );
    assert!(decode::<Generation>(br#"{"format":1}"#, &preparation).is_err());
    let control = Control::none();
    let cancel = control.cancel.clone();
    let preparation = Preparation::new(control, 1 << 20).unwrap();
    cancel.cancel();
    assert!(matches!(encode(&1, &preparation), Err(Error::Cancelled)));
    assert!(matches!(
        decode::<Generation>(br#"{"format":1}"#, &preparation),
        Err(Error::Cancelled)
    ));
}

#[test]
fn coverage_never_wraps_its_next_sequence() {
    let preparation = preparation();
    let first = initial(&preparation);
    let mut coverage = coverage(&first);
    coverage.branches.insert(
        Uuid::from_u128(1),
        BranchCoverage {
            activation_head: u64::MAX,
            first_bound_own: 0,
        },
    );
    assert!(coverage.validate().is_err());
}

#[test]
fn published_lineage_refuses_an_orphan_and_missing_predecessor() {
    let preparation = preparation();
    let directory = tempfile::tempdir().unwrap();
    let first = initial(&preparation);
    persist(directory.path(), &first, &preparation).unwrap();
    let coverage_digest =
        persist_coverage(directory.path(), &coverage(&first), &preparation).unwrap();
    let root = Root {
        format: FORMAT,
        lineage_id: first.identity.lineage_id,
        coverage: coverage_digest,
        current_generation: first.identity.generation,
    };
    let mut next_registry = empty();
    next_registry.graphs.revision = 1;
    let second = prepare(
        first.identity.dataset_id,
        first.identity.lineage_id,
        2,
        Some(first.identity.generation),
        &next_registry,
        &preparation,
    )
    .unwrap();
    persist(directory.path(), &second, &preparation).unwrap();
    assert!(
        resolve(
            directory.path(),
            first.identity.dataset_id,
            &root,
            second.identity,
            &preparation
        )
        .is_err(),
        "a prepared file is not a publication"
    );
    assert!(
        resolve(
            directory.path(),
            first.identity.dataset_id,
            &root,
            first.identity,
            &preparation
        )
        .is_ok()
    );
    let advanced = Root {
        current_generation: second.identity.generation,
        ..root
    };
    assert!(
        resolve(
            directory.path(),
            first.identity.dataset_id,
            &advanced,
            first.identity,
            &preparation
        )
        .is_ok()
    );
    std::fs::remove_file(
        super::directory(directory.path(), first.identity.lineage_id)
            .unwrap()
            .join("generations")
            .join(digest_name(first.identity.generation)),
    )
    .unwrap();
    assert!(
        resolve(
            directory.path(),
            first.identity.dataset_id,
            &advanced,
            second.identity,
            &preparation
        )
        .is_err(),
        "even the current body cannot hide a missing predecessor"
    );
}

#[cfg(unix)]
#[test]
fn immutable_generation_does_not_follow_or_overwrite_an_alias() {
    let preparation = preparation();
    let directory = tempfile::tempdir().unwrap();
    let first = initial(&preparation);
    let generation_dir = create_directory(directory.path(), first.identity.lineage_id)
        .unwrap()
        .join("generations");
    let unrelated = directory.path().join("unrelated");
    std::fs::write(&unrelated, &first.bytes.value).unwrap();
    std::os::unix::fs::symlink(
        &unrelated,
        generation_dir.join(digest_name(first.identity.generation)),
    )
    .unwrap();
    assert!(persist(directory.path(), &first, &preparation).is_err());
    assert_eq!(std::fs::read(unrelated).unwrap(), first.bytes.value);
}

#[test]
fn activation_is_recorded_by_the_reader_or_the_history_directory() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let dataset = |reader: u32| {
        let bytes = crate::commit::dataset_file_bytes(uuid::Uuid::new_v4(), "create", 0);
        let mut value: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        value["minimumReader"] = reader.into();
        std::fs::write(root.join("dataset.json"), value.to_string()).unwrap();
    };
    assert!(!activation_recorded(root).unwrap());
    dataset(crate::store::wal::HEADER_READER);
    assert!(!activation_recorded(root).unwrap());
    check_activated_registry(root, None).unwrap();
    dataset(READER);
    assert!(activation_recorded(root).unwrap());
    assert_eq!(
        check_activated_registry(root, None).unwrap_err().code(),
        "evidence-unavailable"
    );
    // reader 9 is the job ledger's, with or without its settings file, since an
    // interrupted enablement leaves the reader raised and must stay resumable
    dataset(crate::jobs::READER);
    std::fs::write(root.join(crate::jobs::FILE), b"{}").unwrap();
    assert!(!activation_recorded(root).unwrap());
    check_activated_registry(root, None).unwrap();
    std::fs::remove_file(root.join(crate::jobs::FILE)).unwrap();
    assert!(!activation_recorded(root).unwrap());
    check_activated_registry(root, None).unwrap();
    // the history directory alone records activation too
    dataset(crate::store::wal::HEADER_READER);
    std::fs::create_dir(root.join(DIRECTORY)).unwrap();
    assert!(activation_recorded(root).unwrap());
    assert_eq!(
        check_activated_registry(root, None).unwrap_err().code(),
        "evidence-unavailable"
    );
}
