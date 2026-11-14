//! Original disk identity bytes or labelled metadata from a real memory capture.
//! These alone grant no RDF capture, physical-base or imported replay authority.
use super::*;
use crate::access::Reservation;
use crate::managed::ManagedState;
use std::sync::Arc;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum IdentityBacking {
    PersistentDocument,
    MemoryCapture,
}

/// Owned small metadata, not a view/permission token. A source publisher must
/// separately keep its genuine RDF lease and exact selected configuration pair.
pub(in crate::store) struct Identities {
    pub(super) backing: IdentityBacking,
    pub(super) family: Uuid,
    pub(super) logical: Uuid,
    pub(super) family_dataset: managed_history::Bytes,
    pub(super) branch: Option<managed_history::Bytes>,
    _held: Reservation,
}

impl Identities {
    pub(in crate::store) fn capture(
        store: &Store,
        writer: &crate::store::WriterGuard<'_>,
        state: &Option<Arc<ManagedState>>,
        preparation: &Preparation,
    ) -> Result<Self> {
        preparation.check()?;
        if !std::ptr::eq(&**writer, store.writer.data_ptr()) {
            return Err(Error::invalid(
                "original identity needs this Store's actual writer",
            ));
        }
        if writer.poisoned || writer.staged.is_some() {
            return Err(Error::Poisoned);
        }
        if !store.same_managed_state(state)
            || !matches!(state.as_deref(), Some(ManagedState::Installed(installed)) if installed.configuration_history.is_some())
        {
            return Err(managed_history::unavailable(
                "original identity configuration changed",
            ));
        }
        let header = preparation.reserve(std::mem::size_of::<Self>() as u64 + 512)?;
        let _paths = preparation.reserve(
            (store.root.as_ref().map_or(0, |root| root.as_os_str().len()) as u64)
                .saturating_mul(4)
                .saturating_add(1024),
        )?;
        let family = store.owner_dataset_id();
        let logical = store.dataset_id;
        let (backing, family_dataset, branch) = if let Some(root) = &store.root {
            let (family_dataset, branch) = Self::persistent(store, writer, root, preparation)?;
            (IdentityBacking::PersistentDocument, family_dataset, branch)
        } else {
            let (family_dataset, branch) = Self::memory(store, writer, preparation)?;
            (IdentityBacking::MemoryCapture, family_dataset, branch)
        };
        if !store.same_managed_state(state) {
            return Err(managed_history::unavailable(
                "original identity configuration changed",
            ));
        }
        preparation.check()?;
        Ok(Self {
            backing,
            family,
            logical,
            family_dataset,
            branch,
            _held: header,
        })
    }

    pub(in crate::store) fn family(&self) -> Uuid {
        self.family
    }

    pub(in crate::store) fn logical(&self) -> Uuid {
        self.logical
    }

    pub(in crate::store) fn family_document(&self) -> &[u8] {
        &self.family_dataset.value
    }

    pub(in crate::store) fn branch_document(&self) -> Option<&[u8]> {
        self.branch.as_ref().map(|bytes| bytes.value.as_slice())
    }

    fn persistent(
        store: &Store,
        writer: &crate::store::WriterGuard<'_>,
        root: &Path,
        preparation: &Preparation,
    ) -> Result<(managed_history::Bytes, Option<managed_history::Bytes>)> {
        let family_root = store.managed_root().ok_or_else(|| {
            managed_history::unavailable("original persistent family root is missing")
        })?;
        let family = store.owner_dataset_id();
        let logical = store.dataset_id;
        let directory = documents::Opened::root(&family_root, preparation)?;
        let family_dataset = directory
            .child("dataset.json", false, preparation)?
            .bytes(None, preparation)?;
        if documents::dataset_id(&family_dataset.value, preparation)? != family {
            return Err(managed_history::unavailable(
                "original family identity differs from Store",
            ));
        }
        let branch = if family != logical {
            let actual = store.branching.ident.as_ref().ok_or_else(|| {
                managed_history::unavailable("original branch has no live identity")
            })?;
            let directory = documents::Opened::root(root, preparation)?;
            let bytes = directory
                .child("branch.json", false, preparation)?
                .bytes(None, preparation)?;
            documents::live_branch_identity(
                &bytes.value,
                family,
                logical,
                writer.head.seq,
                preparation,
                actual,
            )?;
            Some(bytes)
        } else {
            if store.branching.ident.is_some() {
                return Err(managed_history::unavailable(
                    "original main Store has an inconsistent branch identity",
                ));
            }
            None
        };
        Ok((family_dataset, branch))
    }

    fn memory(
        store: &Store,
        writer: &crate::store::WriterGuard<'_>,
        preparation: &Preparation,
    ) -> Result<(managed_history::Bytes, Option<managed_history::Bytes>)> {
        #[derive(Serialize)]
        #[serde(rename_all = "camelCase")]
        struct CapturedFamily<'a> {
            format: u32,
            minimum_reader: u32,
            written_by: &'static str,
            id: Uuid,
            created: &'a str,
            origin: &'static str,
        }
        #[derive(Serialize)]
        #[serde(rename_all = "camelCase")]
        struct CapturedBranch<'a> {
            format: u32,
            dataset_id: Uuid,
            name: &'a str,
            id: Uuid,
            ordinal: u16,
            from: &'a crate::branch::CommitRef,
            created: &'a str,
            #[serde(skip_serializing_if = "Option::is_none")]
            configuration_origin: Option<&'a crate::history::BranchConfigurationOrigin>,
        }
        // This labels newly created capture metadata, never an original disk
        // document or the family's unknown in-memory birth timestamp.
        let _temporary = preparation.reserve(1024)?;
        let created = crate::commit::rfc3339_ms(store.now_ms());
        let family = store.owner_dataset_id();
        let logical = store.dataset_id;
        let family_dataset = managed_history::encode(
            &CapturedFamily {
                format: crate::commit::DATASET_FORMAT,
                minimum_reader: managed_history::READER,
                written_by: crate::formats::SPARKLES_VERSION,
                id: family,
                created: &created,
                origin: "memory-capture",
            },
            preparation,
        )?;
        if documents::dataset_id(&family_dataset.value, preparation)? != family {
            return Err(managed_history::unavailable(
                "memory capture family differs",
            ));
        }
        let branch = if family != logical {
            let ident = store.branching.ident.as_ref().ok_or_else(|| {
                managed_history::unavailable("memory capture branch identity is missing")
            })?;
            if ident.dataset_id != family {
                return Err(managed_history::unavailable(
                    "memory capture branch family differs",
                ));
            }
            let name = preparation.read_lock(&store.branching.name)?;
            let bytes = managed_history::encode_pretty(
                &CapturedBranch {
                    format: if ident.configuration_origin.is_some() {
                        2
                    } else {
                        1
                    },
                    dataset_id: family,
                    name: &name,
                    id: logical,
                    ordinal: ident.ordinal,
                    from: &ident.from,
                    created: &created,
                    configuration_origin: ident.configuration_origin.as_ref(),
                },
                preparation,
            )?;
            documents::live_branch_identity(
                &bytes.value,
                family,
                logical,
                writer.head.seq,
                preparation,
                ident,
            )?;
            Some(bytes)
        } else {
            if store.branching.ident.is_some() {
                return Err(managed_history::unavailable(
                    "memory main Store has an inconsistent branch identity",
                ));
            }
            None
        };
        Ok((family_dataset, branch))
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    fn preparation() -> Preparation {
        Preparation::new(crate::task::Control::none(), 64 << 20).unwrap()
    }

    fn store(root: &Path) -> Store {
        drop(Store::open(root, StoreOptions::default()).unwrap());
        crate::migrate::activate_configuration_history(root, false, &preparation()).unwrap();
        Store::open(root, StoreOptions::default()).unwrap()
    }

    #[test]
    fn genuine_identity_capture_keeps_actual_main_and_branch_bytes_and_credit() {
        let temporary = tempfile::tempdir().unwrap();
        let root = temporary.path().join("publisher");
        let main = store(&root);
        main.create_branch("original", &Default::default()).unwrap();
        for name in ["main", "original"] {
            let branch = main.branch(name).unwrap();
            let preparation = preparation();
            let before = preparation.room();
            let writer = branch.lock_writer(&Default::default()).unwrap();
            let state = branch.managed_state_shared();
            let captured = Identities::capture(&branch, &writer, &state, &preparation).unwrap();
            assert_eq!(captured.backing, IdentityBacking::PersistentDocument);
            assert_eq!(captured.family, main.dataset_id);
            assert_eq!(captured.logical, branch.dataset_id);
            assert_eq!(
                captured.family_dataset.value,
                std::fs::read(root.join("dataset.json")).unwrap()
            );
            assert_eq!(captured.branch.is_some(), name != "main");
            if let Some(bytes) = &captured.branch {
                assert_ne!(captured.family, captured.logical);
                assert_eq!(
                    bytes.value,
                    std::fs::read(root.join(format!("branches/{}/branch.json", branch.dataset_id)))
                        .unwrap()
                );
            }
            drop(writer);
            assert!(preparation.room() < before);
            drop(captured);
            assert_eq!(preparation.room(), before);
        }
    }

    #[test]
    fn genuine_identity_capture_refuses_foreign_writer_and_stale_state() {
        let temporary = tempfile::tempdir().unwrap();
        let main = store(&temporary.path().join("main"));
        let other = store(&temporary.path().join("other"));
        let preparation = preparation();
        let before = preparation.room();
        let foreign = other.lock_writer(&Default::default()).unwrap();
        assert!(matches!(
            Identities::capture(&main, &foreign, &main.managed_state_shared(), &preparation),
            Err(Error::Invalid(_))
        ));
        drop(foreign);
        let stale = main.managed_state_shared();
        let settings = crate::evidence::Settings::default();
        let canonical = settings.canonical(preparation.control()).unwrap();
        main.enable_managed(
            crate::managed::OwnerKind::Evidence,
            crate::managed::OwnerEntry::for_settings(
                1,
                settings.managed_graphs(),
                canonical.bytes(),
            ),
            canonical.bytes(),
        )
        .unwrap();
        let writer = main.lock_writer(&Default::default()).unwrap();
        assert!(Identities::capture(&main, &writer, &stale, &preparation).is_err());
        assert_eq!(preparation.room(), before);
    }

    #[test]
    fn genuine_identity_capture_checks_actual_family_bytes_and_rolls_back_pressure() {
        let temporary = tempfile::tempdir().unwrap();
        let root = temporary.path().join("main");
        let main = store(&root);
        let preparation = preparation();
        let writer = main.lock_writer(&Default::default()).unwrap();
        let state = main.managed_state_shared();
        let before = preparation.room();
        let pressure = preparation.reserve(before - 64).unwrap();
        assert!(matches!(
            Identities::capture(&main, &writer, &state, &preparation),
            Err(Error::BudgetExceeded(_))
        ));
        assert_eq!(preparation.room(), 64);
        drop(pressure);
        let path = root.join("dataset.json");
        let original = std::fs::read(&path).unwrap();
        std::fs::write(
            &path,
            crate::commit::dataset_file_bytes(Uuid::new_v4(), "test", 0),
        )
        .unwrap();
        assert!(Identities::capture(&main, &writer, &state, &preparation).is_err());
        assert_eq!(preparation.room(), before);
        std::fs::write(path, original).unwrap();
        preparation.control().cancel.cancel();
        assert!(matches!(
            Identities::capture(&main, &writer, &state, &preparation),
            Err(Error::Cancelled)
        ));
        assert_eq!(preparation.room(), before);
    }

    #[test]
    fn genuine_identity_capture_labels_actual_memory_main_and_branch_metadata() {
        let main = Store::in_memory(StoreOptions::default());
        let preparation = preparation();
        main.activate_memory_configuration_history(&preparation)
            .unwrap();
        main.create_branch("captured", &Default::default()).unwrap();
        let original_branch = main.branch("captured").unwrap();
        main.rename_branch("captured", "renamed").unwrap();
        assert_eq!(
            original_branch.branching.ident.as_ref().unwrap().name,
            "captured"
        );
        for name in ["main", "renamed"] {
            let branch = main.branch(name).unwrap();
            let before = preparation.room();
            let writer = branch.lock_writer(&Default::default()).unwrap();
            let state = branch.managed_state_shared();
            let captured = Identities::capture(&branch, &writer, &state, &preparation).unwrap();
            assert_eq!(captured.backing, IdentityBacking::MemoryCapture);
            assert_eq!(captured.family, main.dataset_id);
            assert_eq!(captured.logical, branch.dataset_id);
            let family: serde_json::Value =
                serde_json::from_slice(&captured.family_dataset.value).unwrap();
            assert_eq!(family["origin"], "memory-capture");
            assert_eq!(family["minimumReader"], 8);
            assert_eq!(family["id"], main.dataset_id.to_string());
            assert_eq!(captured.branch.is_some(), name != "main");
            if let Some(bytes) = &captured.branch {
                let metadata: crate::store::branching::BranchFile =
                    serde_json::from_slice(&bytes.value).unwrap();
                assert_eq!(metadata.name, "renamed");
                assert_eq!(metadata.id, branch.dataset_id);
                assert_eq!(metadata.dataset_id, main.dataset_id);
                assert_eq!(
                    metadata.configuration_origin,
                    branch
                        .branching
                        .ident
                        .as_ref()
                        .unwrap()
                        .configuration_origin
                );
                assert_eq!(serde_json::to_vec_pretty(&metadata).unwrap(), bytes.value);
                assert_ne!(captured.family, captured.logical);
            }
            drop(writer);
            assert!(preparation.room() < before);
            drop(captured);
            assert_eq!(preparation.room(), before);
        }
    }

    #[test]
    fn genuine_memory_identity_capture_refuses_inactive_and_foreign_state() {
        let main = Store::in_memory(StoreOptions::default());
        let other = Store::in_memory(StoreOptions::default());
        let preparation = preparation();
        let before = preparation.room();
        {
            let writer = main.lock_writer(&Default::default()).unwrap();
            assert!(Identities::capture(&main, &writer, &None, &preparation).is_err());
            assert_eq!(preparation.room(), before);
        }
        main.activate_memory_configuration_history(&preparation)
            .unwrap();
        other
            .activate_memory_configuration_history(&preparation)
            .unwrap();
        // These real live histories retain their own preparation credit.
        let activated = preparation.room();
        assert!(activated < before);
        let writer = main.lock_writer(&Default::default()).unwrap();
        assert!(
            Identities::capture(&main, &writer, &other.managed_state_shared(), &preparation)
                .is_err()
        );
        assert_eq!(preparation.room(), activated);
        drop(writer);
        drop(main);
        drop(other);
        assert_eq!(preparation.room(), before);
    }

    #[test]
    fn genuine_memory_identity_capture_retains_no_credit_on_pressure_or_cancel() {
        let main = Store::in_memory(StoreOptions::default());
        let preparation = preparation();
        main.activate_memory_configuration_history(&preparation)
            .unwrap();
        let writer = main.lock_writer(&Default::default()).unwrap();
        let state = main.managed_state_shared();
        let before = preparation.room();
        let pressure = preparation.reserve(before - 64).unwrap();
        assert!(matches!(
            Identities::capture(&main, &writer, &state, &preparation),
            Err(Error::BudgetExceeded(_))
        ));
        assert_eq!(preparation.room(), 64);
        drop(pressure);
        assert_eq!(preparation.room(), before);
        preparation.control().cancel.cancel();
        assert!(matches!(
            Identities::capture(&main, &writer, &state, &preparation),
            Err(Error::Cancelled)
        ));
        assert_eq!(preparation.room(), before);
    }

    #[test]
    fn genuine_memory_identity_capture_keeps_a_legacy_branch_unbound() {
        let main = Store::in_memory(StoreOptions::default());
        main.create_branch("legacy", &Default::default()).unwrap();
        let branch = main.branch("legacy").unwrap();
        let preparation = preparation();
        main.activate_memory_configuration_history(&preparation)
            .unwrap();
        assert!(
            branch
                .branching
                .ident
                .as_ref()
                .unwrap()
                .configuration_origin
                .is_none()
        );
        let before = preparation.room();
        let writer = branch.lock_writer(&Default::default()).unwrap();
        let captured = Identities::capture(
            &branch,
            &writer,
            &branch.managed_state_shared(),
            &preparation,
        )
        .unwrap();
        assert_eq!(captured.backing, IdentityBacking::MemoryCapture);
        let metadata: crate::store::branching::BranchFile =
            serde_json::from_slice(&captured.branch.as_ref().unwrap().value).unwrap();
        assert_eq!(metadata.format, 1);
        assert!(metadata.configuration_origin.is_none());
        assert_eq!(
            metadata.from.branch_id,
            branch.branching.ident.as_ref().unwrap().from.branch_id
        );
        drop(captured);
        assert_eq!(preparation.room(), before);
    }

    #[test]
    fn genuine_identity_capture_refuses_canonical_live_fork_substitution() {
        let temporary = tempfile::tempdir().unwrap();
        let root = temporary.path().join("publisher");
        let main = store(&root);
        main.create_branch("original", &Default::default()).unwrap();
        let branch = main.branch("original").unwrap();
        let preparation = preparation();
        let before = preparation.room();
        let writer = branch.lock_writer(&Default::default()).unwrap();
        let state = branch.managed_state_shared();
        let path = root.join(format!("branches/{}/branch.json", branch.dataset_id));
        let original = std::fs::read(&path).unwrap();
        let mut metadata: crate::store::branching::BranchFile =
            serde_json::from_slice(&original).unwrap();
        let real_fork = metadata.from.branch_id;
        metadata.from.branch_id = Uuid::new_v4();
        assert_ne!(metadata.from.branch_id, real_fork);
        let forged = serde_json::to_vec_pretty(&metadata).unwrap();
        metadata.check_configuration(&forged).unwrap();
        std::fs::write(&path, forged).unwrap();
        let result = Identities::capture(&branch, &writer, &state, &preparation);
        std::fs::write(&path, original).unwrap();
        let refused = result.is_err();
        drop(result);
        assert_eq!(preparation.room(), before);
        assert!(
            refused,
            "canonical fork substitution must refuse against the live Store identity"
        );
        drop(writer);
        let writer = branch.lock_writer(&Default::default()).unwrap();
        let state = branch.managed_state_shared();
        let valid = Identities::capture(&branch, &writer, &state, &preparation).unwrap();
        assert!(preparation.room() < before);
        drop(valid);
        assert_eq!(preparation.room(), before);
    }

    #[test]
    fn genuine_identity_capture_refuses_other_schema_valid_live_identity_changes() {
        let temporary = tempfile::tempdir().unwrap();
        let root = temporary.path().join("publisher");
        let main = store(&root);
        crate::sparql::update::update(
            &main,
            "INSERT DATA { <urn:before-fork> <urn:p> 1 }",
            &Default::default(),
        )
        .unwrap();
        main.create_branch("original", &Default::default()).unwrap();
        let branch = main.branch("original").unwrap();
        let preparation = preparation();
        let before = preparation.room();
        let writer = branch.lock_writer(&Default::default()).unwrap();
        let state = branch.managed_state_shared();
        let path = root.join(format!("branches/{}/branch.json", branch.dataset_id));
        let original = std::fs::read(&path).unwrap();
        for changed in ["ordinal", "origin", "fork-sequence"] {
            let mut metadata: crate::store::branching::BranchFile =
                serde_json::from_slice(&original).unwrap();
            match changed {
                "ordinal" => metadata.ordinal = metadata.ordinal.checked_add(1).unwrap(),
                "origin" => {
                    metadata
                        .configuration_origin
                        .as_mut()
                        .unwrap()
                        .source_branch = Uuid::new_v4();
                }
                "fork-sequence" => {
                    assert!(metadata.from.seq > 0);
                    metadata.from.seq -= 1;
                    metadata
                        .configuration_origin
                        .as_mut()
                        .unwrap()
                        .first_bound_own = metadata.from.seq + 1;
                    assert!(metadata.from.seq <= writer.head.seq);
                }
                _ => unreachable!(),
            }
            let forged = serde_json::to_vec_pretty(&metadata).unwrap();
            metadata.check_configuration(&forged).unwrap();
            // The candidate parser intentionally has no live-Store authority.
            documents::branch_identity(
                &forged,
                main.dataset_id,
                branch.dataset_id,
                writer.head.seq,
                &preparation,
            )
            .unwrap();
            std::fs::write(&path, forged).unwrap();
            let result = Identities::capture(&branch, &writer, &state, &preparation);
            std::fs::write(&path, &original).unwrap();
            let refused = result.is_err();
            drop(result);
            assert_eq!(preparation.room(), before);
            assert!(refused, "live Store must refuse changed {changed}");
        }
        drop(Identities::capture(&branch, &writer, &state, &preparation).unwrap());
        assert_eq!(preparation.room(), before);
    }

    #[test]
    fn genuine_identity_capture_accepts_a_legitimate_live_rename() {
        let temporary = tempfile::tempdir().unwrap();
        let main = store(&temporary.path().join("publisher"));
        main.create_branch("original", &Default::default()).unwrap();
        let branch = main.branch("original").unwrap();
        main.rename_branch("original", "renamed").unwrap();
        assert_eq!(branch.branching.ident.as_ref().unwrap().name, "original");
        assert_eq!(&*branch.branching.name.read(), "renamed");
        let preparation = preparation();
        let before = preparation.room();
        let writer = branch.lock_writer(&Default::default()).unwrap();
        let captured = Identities::capture(
            &branch,
            &writer,
            &branch.managed_state_shared(),
            &preparation,
        )
        .unwrap();
        let metadata: crate::store::branching::BranchFile =
            serde_json::from_slice(&captured.branch.as_ref().unwrap().value).unwrap();
        assert_eq!(metadata.name, "renamed");
        drop(captured);
        assert_eq!(preparation.room(), before);
    }

    #[test]
    fn genuine_identity_capture_accepts_a_real_legacy_branch_after_activation() {
        let temporary = tempfile::tempdir().unwrap();
        let root = temporary.path().join("publisher");
        let main = Store::open(&root, StoreOptions::default()).unwrap();
        main.create_branch("legacy", &Default::default()).unwrap();
        drop(main);
        crate::migrate::activate_configuration_history(&root, false, &preparation()).unwrap();
        let main = Store::open(&root, StoreOptions::default()).unwrap();
        let branch = main.branch("legacy").unwrap();
        assert!(
            branch
                .branching
                .ident
                .as_ref()
                .unwrap()
                .configuration_origin
                .is_none()
        );
        let preparation = preparation();
        let before = preparation.room();
        let writer = branch.lock_writer(&Default::default()).unwrap();
        let captured = Identities::capture(
            &branch,
            &writer,
            &branch.managed_state_shared(),
            &preparation,
        )
        .unwrap();
        let metadata: crate::store::branching::BranchFile =
            serde_json::from_slice(&captured.branch.as_ref().unwrap().value).unwrap();
        assert_eq!(metadata.format, 1);
        assert!(metadata.configuration_origin.is_none());
        drop(captured);
        assert_eq!(preparation.room(), before);
    }

    #[test]
    fn genuine_identity_capture_refuses_poisoned_and_staged_actual_writers() {
        let temporary = tempfile::tempdir().unwrap();
        let main = store(&temporary.path().join("publisher"));
        let preparation = preparation();
        let before = preparation.room();
        let state = main.managed_state_shared();
        let mut writer = main.lock_writer(&Default::default()).unwrap();
        writer.poisoned = true;
        assert!(matches!(
            Identities::capture(&main, &writer, &state, &preparation),
            Err(Error::Poisoned)
        ));
        writer.poisoned = false;
        writer.staged = Some(main.snapshot());
        assert!(matches!(
            Identities::capture(&main, &writer, &state, &preparation),
            Err(Error::Poisoned)
        ));
        writer.staged = None;
        assert_eq!(preparation.room(), before);
        drop(Identities::capture(&main, &writer, &state, &preparation).unwrap());
        assert_eq!(preparation.room(), before);
    }

    #[test]
    fn genuine_identity_capture_refuses_branch_leaf_symlink_and_wrong_id() {
        let temporary = tempfile::tempdir().unwrap();
        let root = temporary.path().join("publisher");
        let main = store(&root);
        main.create_branch("original", &Default::default()).unwrap();
        let branch = main.branch("original").unwrap();
        let preparation = preparation();
        let before = preparation.room();
        let writer = branch.lock_writer(&Default::default()).unwrap();
        let state = branch.managed_state_shared();
        let path = root.join(format!("branches/{}/branch.json", branch.dataset_id));
        let original = std::fs::read(&path).unwrap();
        let outside = temporary.path().join("outside.json");
        std::fs::write(&outside, &original).unwrap();
        std::fs::remove_file(&path).unwrap();
        std::os::unix::fs::symlink(&outside, &path).unwrap();
        let result = Identities::capture(&branch, &writer, &state, &preparation);
        std::fs::remove_file(&path).unwrap();
        std::fs::write(&path, &original).unwrap();
        assert!(result.is_err());
        drop(result);
        assert_eq!(preparation.room(), before);
        let mut metadata: crate::store::branching::BranchFile =
            serde_json::from_slice(&original).unwrap();
        metadata.id = Uuid::new_v4();
        let forged = serde_json::to_vec_pretty(&metadata).unwrap();
        metadata.check_configuration(&forged).unwrap();
        std::fs::write(&path, forged).unwrap();
        let result = Identities::capture(&branch, &writer, &state, &preparation);
        std::fs::write(&path, original).unwrap();
        assert!(result.is_err());
        drop(result);
        assert_eq!(preparation.room(), before);
        drop(Identities::capture(&branch, &writer, &state, &preparation).unwrap());
        assert_eq!(preparation.room(), before);
    }
}
