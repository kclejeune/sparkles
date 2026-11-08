use super::*;
use crate::store::{MemoryCaptureOptions, Store, StoreOptions};
use std::path::PathBuf;

fn preparation() -> Preparation {
    Preparation::new(crate::task::Control::none(), 64 << 20).unwrap()
}

/// Candidate fixtures copy real captured small documents and the actual family
/// and branch identity files. They deliberately grant no capture/replay authority.
struct Candidate {
    _temporary: tempfile::TempDir,
    destination: PathBuf,
    source: Sha256Digest,
    descriptor: Descriptor,
    original: BTreeMap<String, Vec<u8>>,
    family: Uuid,
    logical: Uuid,
    configuration: ConfigurationIdentity,
}

impl Candidate {
    fn new(standalone_branch: bool) -> Self {
        Self::with_revision(standalone_branch, false)
    }

    fn with_revision(standalone_branch: bool, configure: bool) -> Self {
        let temporary = tempfile::tempdir().unwrap();
        let publisher = temporary.path().join("publisher");
        drop(Store::open(&publisher, StoreOptions::default()).unwrap());
        let preparation = preparation();
        crate::migrate::activate_configuration_history(&publisher, false, &preparation).unwrap();
        let store = Store::open(&publisher, StoreOptions::default()).unwrap();
        if configure {
            let settings = crate::evidence::Settings::default();
            let canonical = settings.canonical(preparation.control()).unwrap();
            store
                .enable_managed(
                    crate::managed::OwnerKind::Evidence,
                    crate::managed::OwnerEntry::for_settings(
                        1,
                        settings.managed_graphs(),
                        canonical.bytes(),
                    ),
                    canonical.bytes(),
                )
                .unwrap();
        }
        let family = store.dataset_id;
        let branch = if standalone_branch {
            store
                .create_branch("original", &Default::default())
                .unwrap();
            store.branch("original").unwrap()
        } else {
            store.branch("main").unwrap()
        };
        let capture = branch
            .materialized_backup_capture(
                "candidate-documents",
                &MemoryCaptureOptions {
                    tmp_dir: temporary.path().join("scratch"),
                    ..Default::default()
                },
            )
            .unwrap();
        let logical = capture.dataset_id;
        assert_eq!(standalone_branch, logical != family);
        let mut original = BTreeMap::new();
        for file in &capture.files {
            if !source_member(&file.path) {
                continue;
            }
            let mut bytes = vec![0; usize::try_from(file.len).unwrap()];
            let mut offset = 0;
            while offset < bytes.len() {
                let read = file.read_at(offset as u64, &mut bytes[offset..]).unwrap();
                assert_ne!(read, 0);
                offset += read;
            }
            original.insert(file.path.clone(), bytes);
        }
        original.insert(
            "family-dataset.json".into(),
            std::fs::read(publisher.join("dataset.json")).unwrap(),
        );
        if standalone_branch {
            original.insert(
                "branch.json".into(),
                std::fs::read(publisher.join(format!("branches/{logical}/branch.json"))).unwrap(),
            );
        }
        let configuration = primary::recorded_root_bytes(&original["managed.json"], &preparation)
            .unwrap()
            .unwrap()
            .identity(family);
        let descriptor = Descriptor {
            format: FORMAT,
            family_id: family,
            roots: BTreeMap::from([(
                "main".into(),
                SourceRoot {
                    logical_id: logical,
                    head: capture.commit.seq,
                    generation: capture.generation.clone(),
                },
            )]),
            files: BTreeMap::new(),
            sources: Vec::new(),
            intervals: Vec::new(),
        };
        let destination = temporary.path().join("destination");
        drop(Store::open(&destination, StoreOptions::default()).unwrap());
        let mut candidate = Self {
            _temporary: temporary,
            destination,
            source: Sha256Digest::from_digest_bytes([0; 32]),
            descriptor,
            original,
            family,
            logical,
            configuration,
        };
        candidate.publish_candidate();
        candidate
    }

    fn directory(&self) -> PathBuf {
        self.destination.join(format!(
            "evidence-history/imports/{}",
            self.source.to_string().trim_start_matches("sha256:")
        ))
    }

    fn member(&self, name: &str) -> PathBuf {
        self.directory().join("source").join(name)
    }

    fn publish_candidate(&mut self) {
        let preparation = preparation();
        self.descriptor.files = self
            .original
            .iter()
            .map(|(name, bytes)| {
                (
                    name.clone(),
                    SourceFile {
                        bytes: bytes.len() as u64,
                        sha256: Sha256Digest::of_bytes(bytes, preparation.control()).unwrap(),
                    },
                )
            })
            .collect();
        self.descriptor.validate(&preparation).unwrap();
        let descriptor = managed_history::encode(&self.descriptor, &preparation).unwrap();
        self.source = Sha256Digest::of_bytes(&descriptor.value, preparation.control()).unwrap();
        let directory = self.directory();
        std::fs::create_dir_all(directory.join("source")).unwrap();
        std::fs::write(directory.join("origin.json"), &descriptor.value).unwrap();
        for (name, bytes) in &self.original {
            let path = self.member(name);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, bytes).unwrap();
        }
        let origin = Origin {
            format: FORMAT,
            destination: None,
            dataset_id: crate::commit::read_dataset_file(&self.destination)
                .unwrap()
                .unwrap(),
            sources: vec![self.source],
            bases: Vec::new(),
            ranges: Vec::new(),
        };
        let origin = managed_history::encode(&origin, &preparation).unwrap();
        std::fs::write(self.destination.join(ROOT_FILE), &origin.value).unwrap();
    }

    fn refused(&self, preparation: &Preparation, message: &str) {
        let before = preparation.room();
        let error = Documents::open(&self.destination, self.source, preparation)
            .err()
            .expect("invalid candidate was accepted");
        assert!(error.to_string().contains(message), "{error}");
        assert_eq!(preparation.room(), before, "refusal leaked caller credit");
    }
}

#[test]
fn original_documents_retain_genuine_main_and_standalone_branch_credit() {
    for branch in [false, true] {
        let candidate = Candidate::new(branch);
        let preparation = preparation();
        let before = preparation.room();
        let documents =
            Documents::open(&candidate.destination, candidate.source, &preparation).unwrap();
        assert_eq!(documents.descriptor.value.family_id, candidate.family);
        assert_eq!(
            documents.descriptor.value.roots["main"].logical_id,
            candidate.logical
        );
        assert!(preparation.room() < before);
        let retained = preparation.room();
        let member = documents.read("dataset.json", &preparation).unwrap();
        assert!(preparation.room() < retained);
        drop(documents);
        assert!(
            preparation.room() < before,
            "returned bytes need their own actual credit"
        );
        drop(member);
        assert_eq!(preparation.room(), before);
    }
}

#[test]
fn original_documents_reject_resealed_wrong_family_and_logical_identity() {
    let mut candidate = Candidate::new(true);
    let original = candidate.original["family-dataset.json"].clone();
    candidate.original.insert(
        "family-dataset.json".into(),
        crate::commit::dataset_file_bytes(Uuid::new_v4(), "test", 0),
    );
    candidate.publish_candidate();
    candidate.refused(&preparation(), "publisher family differs");
    candidate
        .original
        .insert("family-dataset.json".into(), original);
    candidate.original.insert(
        "dataset.json".into(),
        crate::commit::dataset_file_bytes(candidate.family, "test", 0),
    );
    candidate.publish_candidate();
    candidate.refused(&preparation(), "logical dataset differs");
}

#[test]
fn original_documents_reject_resealed_wrong_branch_identity_and_current() {
    let mut candidate = Candidate::new(true);
    let bytes = candidate.original["branch.json"].clone();
    let mut branch: crate::store::branching::BranchFile = serde_json::from_slice(&bytes).unwrap();
    branch.id = Uuid::new_v4();
    candidate.original.insert(
        "branch.json".into(),
        serde_json::to_vec_pretty(&branch).unwrap(),
    );
    candidate.publish_candidate();
    candidate.refused(&preparation(), "branch/family identity differs");
    candidate.original.insert("branch.json".into(), bytes);
    candidate
        .original
        .insert("CURRENT".into(), b"gen-9999".to_vec());
    candidate.publish_candidate();
    candidate.refused(&preparation(), "CURRENT differs");
}

#[test]
fn original_documents_reject_descriptor_corruption_and_missing_members() {
    let candidate = Candidate::new(false);
    let path = candidate.directory().join("origin.json");
    let original = std::fs::read(&path).unwrap();
    let mut changed = original.clone();
    changed.push(b' ');
    std::fs::write(&path, changed).unwrap();
    candidate.refused(&preparation(), "descriptor digest differs");
    std::fs::write(path, original).unwrap();
    std::fs::remove_file(candidate.member("managed.json")).unwrap();
    candidate.refused(&preparation(), "No such file");
}

#[test]
fn original_documents_refuse_unknown_reader_before_dataset_shape() {
    let preparation = preparation();
    let before = preparation.room();
    assert!(matches!(
        dataset_id(
            br#"{"format":1,"minimumReader":4294967295,"id":false}"#,
            &preparation
        ),
        Err(Error::Unsupported(_))
    ));
    assert_eq!(preparation.room(), before);
}

#[test]
fn original_documents_reject_root_intermediate_and_leaf_symlinks() {
    use std::os::unix::fs::symlink;
    let candidate = Candidate::new(false);
    let preparation = preparation();
    let alias = candidate._temporary.path().join("alias");
    symlink(&candidate.destination, &alias).unwrap();
    let before = preparation.room();
    assert!(Documents::open(&alias, candidate.source, &preparation).is_err());
    assert_eq!(preparation.room(), before);
    let history = candidate.destination.join("evidence-history");
    let moved = candidate.destination.join("retained-history");
    std::fs::rename(&history, &moved).unwrap();
    symlink(&moved, &history).unwrap();
    assert!(Documents::open(&candidate.destination, candidate.source, &preparation).is_err());
    assert_eq!(preparation.room(), before);
    std::fs::remove_file(&history).unwrap();
    std::fs::rename(&moved, &history).unwrap();
    let member = candidate.member("dataset.json");
    let old = member.with_extension("old");
    std::fs::rename(&member, &old).unwrap();
    symlink(&old, &member).unwrap();
    assert!(Documents::open(&candidate.destination, candidate.source, &preparation).is_err());
    assert_eq!(preparation.room(), before);
}

#[test]
fn original_documents_keep_open_directory_and_file_identity_across_replacement() {
    let temporary = tempfile::tempdir().unwrap();
    let directory = temporary.path().join("original");
    std::fs::create_dir(&directory).unwrap();
    std::fs::write(directory.join("member"), b"original").unwrap();
    let preparation = preparation();
    let before = preparation.room();
    let root = Opened::root(&directory, &preparation).unwrap();
    let member = root.child("member", false, &preparation).unwrap();
    let moved = temporary.path().join("renamed");
    std::fs::rename(&directory, &moved).unwrap();
    std::fs::create_dir(&directory).unwrap();
    std::fs::write(directory.join("member"), b"impostor").unwrap();
    std::fs::rename(moved.join("member"), moved.join("old-member")).unwrap();
    std::fs::write(moved.join("member"), b"new-file").unwrap();
    assert_eq!(
        member.bytes(Some(8), &preparation).unwrap().value,
        b"original"
    );
    assert_eq!(
        root.child("member", false, &preparation)
            .unwrap()
            .bytes(Some(8), &preparation)
            .unwrap()
            .value,
        b"new-file"
    );
    drop(root);
    assert_eq!(preparation.room(), before);
}

#[test]
fn original_documents_recheck_hash_and_length_on_every_read() {
    let candidate = Candidate::new(false);
    let preparation = preparation();
    let documents =
        Documents::open(&candidate.destination, candidate.source, &preparation).unwrap();
    let before = preparation.room();
    let path = candidate.member("CURRENT");
    let mut changed = std::fs::read(&path).unwrap();
    changed[0] ^= 1;
    std::fs::write(&path, &changed).unwrap();
    assert!(
        documents
            .read("CURRENT", &preparation)
            .err()
            .unwrap()
            .to_string()
            .contains("digest differs")
    );
    assert_eq!(preparation.room(), before);
    changed.push(b' ');
    std::fs::write(path, changed).unwrap();
    assert!(documents.read("CURRENT", &preparation).is_err());
    assert_eq!(preparation.room(), before);
    assert!(documents.read("gen-0001/wal.log", &preparation).is_err());
    assert_eq!(preparation.room(), before);
}

#[test]
fn original_documents_cancel_and_pressure_refuse_before_retained_credit() {
    let candidate = Candidate::new(false);
    let preparation = preparation();
    let before = preparation.room();
    let pressure = preparation.reserve(before - 64).unwrap();
    assert!(matches!(
        Documents::open(&candidate.destination, candidate.source, &preparation),
        Err(Error::BudgetExceeded(_))
    ));
    assert_eq!(preparation.room(), 64);
    drop(pressure);
    assert_eq!(preparation.room(), before);
    preparation.control().cancel.cancel();
    assert!(matches!(
        Documents::open(&candidate.destination, candidate.source, &preparation),
        Err(Error::Cancelled)
    ));
    assert_eq!(preparation.room(), before);
}

#[test]
fn original_documents_opened_fd_refuses_wrong_length_kind_and_oversize() {
    let temporary = tempfile::tempdir().unwrap();
    let preparation = preparation();
    let before = preparation.room();
    let path = temporary.path().join("member");
    std::fs::write(&path, b"tiny").unwrap();
    assert!(
        managed_history::read_opened(File::open(&path).unwrap(), Some(5), &preparation).is_err()
    );
    assert_eq!(preparation.room(), before);
    assert!(
        managed_history::read_opened(File::open(temporary.path()).unwrap(), None, &preparation)
            .is_err()
    );
    assert_eq!(preparation.room(), before);
    File::options()
        .write(true)
        .open(&path)
        .unwrap()
        .set_len((64 << 20) + 1)
        .unwrap();
    assert!(managed_history::read_opened(File::open(path).unwrap(), None, &preparation).is_err());
    assert_eq!(preparation.room(), before);
}

#[test]
fn original_publication_walks_actual_main_and_branch_predecessors_with_caller_credit() {
    for branch in [false, true] {
        let candidate = Candidate::with_revision(branch, true);
        let preparation = preparation();
        let before = preparation.room();
        let documents =
            Documents::open(&candidate.destination, candidate.source, &preparation).unwrap();
        let generation = documents
            .publication(candidate.configuration, &preparation)
            .unwrap();
        assert_eq!(generation.value.dataset_id, candidate.family);
        assert!(generation.value.ordinal >= 2);
        assert!(generation.value.previous.is_some());
        drop(documents);
        assert!(
            preparation.room() < before,
            "selected generation must retain actual caller credit"
        );
        drop(generation);
        assert_eq!(preparation.room(), before);
    }
}

#[test]
fn original_publication_refuses_foreign_family_lineage_and_unpublished_target() {
    let candidate = Candidate::with_revision(true, true);
    let preparation = preparation();
    let documents =
        Documents::open(&candidate.destination, candidate.source, &preparation).unwrap();
    let before = preparation.room();
    for wanted in [
        ConfigurationIdentity {
            dataset_id: candidate.logical,
            ..candidate.configuration
        },
        ConfigurationIdentity {
            lineage_id: Uuid::new_v4(),
            ..candidate.configuration
        },
        ConfigurationIdentity {
            generation: Sha256Digest::from_digest_bytes([19; 32]),
            ..candidate.configuration
        },
    ] {
        assert!(documents.publication(wanted, &preparation).is_err());
        assert_eq!(preparation.room(), before);
    }
}

#[test]
fn original_publication_requires_declared_predecessor_even_when_selected_body_is_intact() {
    let mut candidate = Candidate::with_revision(false, true);
    let preparation = preparation();
    let current = candidate.configuration.generation.to_string();
    let current = current.strip_prefix("sha256:").unwrap();
    let path = candidate
        .original
        .keys()
        .find(|name| name.contains("/generations/") && !name.ends_with(&format!("{current}.json")))
        .unwrap()
        .clone();
    candidate.original.remove(&path).unwrap();
    candidate.publish_candidate();
    let documents =
        Documents::open(&candidate.destination, candidate.source, &preparation).unwrap();
    let before = preparation.room();
    let error = documents
        .publication(candidate.configuration, &preparation)
        .err()
        .unwrap();
    assert!(error.to_string().contains("not declared"), "{error}");
    assert_eq!(preparation.room(), before);
}

#[test]
fn original_publication_refuses_resealed_coverage_genesis_and_registry_root_mismatch() {
    let mut candidate = Candidate::with_revision(false, true);
    let preparation = preparation();
    let coverage_path = format!(
        "evidence-history/{}/coverage.json",
        candidate.configuration.lineage_id
    );
    let original_coverage = candidate.original[&coverage_path].clone();
    let mut coverage: managed_history::Coverage =
        serde_json::from_slice(&original_coverage).unwrap();
    coverage.activation_generation = Sha256Digest::from_digest_bytes([17; 32]);
    let encoded = managed_history::encode(&coverage, &preparation).unwrap();
    let hash = Sha256Digest::of_bytes(&encoded.value, preparation.control()).unwrap();
    candidate
        .original
        .insert(coverage_path, encoded.value.clone());
    let mut registry: serde_json::Value =
        serde_json::from_slice(&candidate.original["managed.json"]).unwrap();
    registry["configurationHistory"]["coverage"] = serde_json::to_value(hash).unwrap();
    candidate.original.insert(
        "managed.json".into(),
        serde_json::to_vec_pretty(&registry).unwrap(),
    );
    candidate.publish_candidate();
    let documents =
        Documents::open(&candidate.destination, candidate.source, &preparation).unwrap();
    let before = preparation.room();
    let error = documents
        .publication(candidate.configuration, &preparation)
        .err()
        .unwrap();
    assert!(error.to_string().contains("genesis differs"), "{error}");
    assert_eq!(preparation.room(), before);
    drop(documents);
    registry["configurationHistory"]["lineageId"] = serde_json::to_value(Uuid::new_v4()).unwrap();
    candidate.original.insert(
        "managed.json".into(),
        serde_json::to_vec_pretty(&registry).unwrap(),
    );
    candidate.publish_candidate();
    let documents =
        Documents::open(&candidate.destination, candidate.source, &preparation).unwrap();
    let before = preparation.room();
    let error = documents
        .publication(candidate.configuration, &preparation)
        .err()
        .unwrap();
    assert!(
        error.to_string().contains("foreign configuration identity"),
        "{error}"
    );
    assert_eq!(preparation.room(), before);
}
