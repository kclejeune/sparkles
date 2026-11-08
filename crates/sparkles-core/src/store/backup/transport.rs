//! Private flat first-source transport. The genuine checked capture remains
//! the owner throughout copy, metadata verification and last staged-owner drop.
use super::*;
use crate::access::{Preparation, Reservation};
use sha2::Digest;
use std::collections::BTreeSet;
use std::io::{Read, Write};

struct StagedNativeSource {
    directory: tempfile::TempDir,
    source: PreparedNativeSource,
    _held: Reservation,
}

impl PreparedNativeSource {
    fn stage_native(self, parent: &Path, preparation: &Preparation) -> Result<StagedNativeSource> {
        self.stage_native_with_sync(parent, preparation, &mut |_| Ok(()))
    }

    fn stage_native_with_sync(
        self,
        parent: &Path,
        preparation: &Preparation,
        after_directory_sync: &mut dyn FnMut(&Path) -> Result<()>,
    ) -> Result<StagedNativeSource> {
        preparation.checkpoint("captured-source-stage")?;
        let held = preparation.reserve(
            (std::mem::size_of::<StagedNativeSource>() as u64)
                .checked_add(
                    (parent.as_os_str().len() as u64)
                        .checked_mul(4)
                        .ok_or_else(|| preparation.exhausted())?,
                )
                .and_then(|n| n.checked_add(8192))
                .ok_or_else(|| preparation.exhausted())?,
        )?;
        let manifest = super::super::configuration_history::SourceManifest::decode_native(
            &self.descriptor.value,
            preparation,
        )?;
        if crate::evidence::Sha256Digest::of_bytes(&self.descriptor.value, preparation.control())?
            != self.digest
        {
            return Err(super::super::managed_history::unavailable(
                "genuine source descriptor changed before staging",
            ));
        }
        // Each closed source path is <=256 bytes with at most six ancestors.
        // Cover tree nodes, all ancestor PathBuf capacities and allocator slack
        // before the first insertion. No unbounded filesystem walk is used.
        let _paths = preparation.reserve(
            (parent.as_os_str().len() as u64)
                .checked_add(1024)
                .and_then(|n| n.checked_mul(6))
                .and_then(|n| n.checked_mul(manifest.member_count() as u64 + 2))
                .ok_or_else(|| preparation.exhausted())?,
        )?;
        let _buffer = preparation.reserve(64 << 10)?;
        let mut buffer = [0; 64 << 10];
        let directory = tempfile::Builder::new()
            .prefix(".original-source-")
            .tempdir_in(parent)?;
        let digest = self.digest.to_string();
        let name = digest.strip_prefix("sha256:").expect("canonical digest");
        let root = directory.path().join(name);
        std::fs::create_dir(&root)?;
        let mut directories = BTreeSet::new();
        directories.insert(root.clone());
        manifest.visit(preparation, |name, length, expected| {
            preparation.checkpoint("captured-source-copy")?;
            if name.len() > 256 || length > super::super::managed_history::MAX_BYTES as u64 {
                return Err(super::super::managed_history::unavailable(
                    "source transport member exceeds its closed bounds",
                ));
            }
            let relative = format!("source/{name}");
            let target = super::super::definitions::stage_file(&root, &relative)?;
            let mut ancestor = target.parent();
            while let Some(path) = ancestor.filter(|path| *path != root) {
                if !path.starts_with(&root) {
                    return Err(Error::invalid("source staging left its owned root"));
                }
                directories.insert(path.to_path_buf());
                ancestor = path.parent();
            }
            let mut output = std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&target)?;
            let mut input = self.member(name)?;
            let mut remaining = length;
            let mut hash = sha2::Sha256::new();
            while remaining != 0 {
                preparation.check()?;
                let want = buffer.len().min(remaining as usize);
                let count = input.read(&mut buffer[..want])?;
                if count == 0 {
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::UnexpectedEof,
                        "genuine source member is shorter than its declared extent",
                    )
                    .into());
                }
                output.write_all(&buffer[..count])?;
                hash.update(&buffer[..count]);
                remaining -= count as u64;
            }
            preparation.check()?;
            if input.read(&mut buffer[..1])? != 0
                || crate::evidence::Sha256Digest::from_digest_bytes(hash.finalize().into())
                    != expected
            {
                return Err(super::super::managed_history::unavailable(
                    "genuine source member differs from its selected pin",
                ));
            }
            output.sync_all()?;
            preparation.check()
        })?;
        // The source descriptor is the last leaf. No destination Origin or
        // dataset identity is created/published by this private transport.
        let mut descriptor = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(root.join("origin.json"))?;
        for chunk in self.descriptor.value.chunks(64 << 10) {
            preparation.check()?;
            descriptor.write_all(chunk)?;
        }
        descriptor.sync_all()?;
        drop(descriptor);
        for path in directories.iter().rev() {
            preparation.check()?;
            super::super::sync_dir(path)?;
            after_directory_sync(path)?;
            preparation.check()?;
        }
        super::super::sync_dir(directory.path())?;
        after_directory_sync(directory.path())?;
        preparation.check()?;
        super::super::configuration_history::verify_staged_native_source(
            &root,
            self.digest,
            preparation,
        )?;
        preparation.checkpoint("captured-source-staged")?;
        Ok(StagedNativeSource {
            directory,
            source: self,
            _held: held,
        })
    }

    fn member(&self, path: &str) -> Result<Member<'_>> {
        let original = &self.primary.capture.original;
        let bytes = match path {
            "family-dataset.json" => Some(original.identities.family_document()),
            "branch.json" => original.identities.branch_document(),
            "history.json" => original.pins.as_ref().map(|bytes| bytes.value.as_slice()),
            "branches.json" => original.table.as_ref().map(|bytes| bytes.value.as_slice()),
            _ => None,
        };
        if let Some(bytes) = bytes {
            return Ok(Member::Bytes(std::io::Cursor::new(bytes)));
        }
        let index = self
            .primary
            .capture
            .data
            .files
            .iter()
            .position(|file| file.path == path)
            .ok_or_else(|| {
                super::super::managed_history::unavailable("genuine source member is absent")
            })?;
        Ok(Member::Captured(CapturedReader {
            capture: &self.primary.capture,
            index,
            position: 0,
        }))
    }
}

enum Member<'a> {
    Bytes(std::io::Cursor<&'a [u8]>),
    Captured(CapturedReader<'a>),
}
impl Read for Member<'_> {
    fn read(&mut self, output: &mut [u8]) -> std::io::Result<usize> {
        match self {
            Self::Bytes(reader) => reader.read(output),
            Self::Captured(reader) => reader.read(output),
        }
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use crate::history::At;
    use crate::store::{Store, StoreOptions};
    use crate::task::{Control, Progress};
    use std::sync::atomic::{AtomicBool, Ordering};

    fn preparation() -> Preparation {
        Preparation::new(Control::none(), 64 << 20).unwrap()
    }
    fn store(root: &Path) -> Store {
        drop(Store::open(root, StoreOptions::default()).unwrap());
        crate::migrate::activate_configuration_history(root, false, &preparation()).unwrap();
        let store = Store::open(root, StoreOptions::default()).unwrap();
        crate::sparql::update::update(
            &store,
            "INSERT DATA { <urn:s> <urn:p> 1 }",
            &Default::default(),
        )
        .unwrap();
        store
            .create_snapshot("original", &At::Head, Some("original pin".into()))
            .unwrap();
        store
    }
    fn source(store: &Store, preparation: &Preparation) -> PreparedNativeSource {
        store
            .configuration_capture("transport", &Default::default(), preparation)
            .unwrap()
            .checked_primary(preparation)
            .unwrap()
            .describe_native(preparation)
            .unwrap()
    }
    fn root(staged: &StagedNativeSource) -> std::path::PathBuf {
        staged.directory.path().join(
            staged
                .source
                .digest
                .to_string()
                .strip_prefix("sha256:")
                .unwrap(),
        )
    }
    fn assert_closed(store: &Store, preparation: &Preparation, before: u64, parent: &Path) {
        assert_eq!(store.history.as_ref().unwrap().lock().leases.len(), 0);
        assert_eq!(preparation.room(), before);
        assert_eq!(std::fs::read_dir(parent).unwrap().count(), 0);
    }

    #[test]
    fn genuine_source_transport_roundtrips_small_members_without_wal_copy() {
        let temporary = tempfile::tempdir().unwrap();
        let store = store(&temporary.path().join("source"));
        let parent = temporary.path().join("staged");
        std::fs::create_dir(&parent).unwrap();
        let preparation = preparation();
        let before = preparation.room();
        let original = source(&store, &preparation);
        let original_pins = original
            .primary
            .capture
            .original
            .pins
            .as_ref()
            .unwrap()
            .value
            .clone();
        store.delete_snapshot("original").unwrap();
        let staged = original.stage_native(&parent, &preparation).unwrap();
        let root = root(&staged);
        assert_eq!(
            std::fs::read(root.join("origin.json")).unwrap(),
            staged.source.descriptor.value
        );
        assert_eq!(
            std::fs::read(root.join("source/history.json")).unwrap(),
            original_pins
        );
        assert!(
            !root
                .join("source")
                .join(&staged.source.primary.capture.data.generation)
                .join("wal.log")
                .exists()
        );
        assert!(!root.join("evidence-history/origin.json").exists());
        assert_eq!(store.history.as_ref().unwrap().lock().leases.len(), 1);
        super::super::super::configuration_history::verify_staged_native_source(
            &root,
            staged.source.digest,
            &preparation,
        )
        .unwrap();
        drop(staged);
        assert_closed(&store, &preparation, before, &parent);
    }

    #[test]
    fn genuine_source_transport_memory_branch_keeps_actual_family_documents() {
        let temporary = tempfile::tempdir().unwrap();
        let memory = Store::in_memory(StoreOptions::default());
        let preparation = preparation();
        memory
            .activate_memory_configuration_history(&preparation)
            .unwrap();
        memory.create_branch("child", &Default::default()).unwrap();
        let child = memory.branch("child").unwrap();
        let parent = temporary.path().join("staged");
        std::fs::create_dir(&parent).unwrap();
        let before = preparation.room();
        let original = child
            .materialized_configuration_capture(
                "transport",
                &MemoryCaptureOptions {
                    tmp_dir: temporary.path().join("scratch"),
                    ..Default::default()
                },
                &preparation,
            )
            .unwrap()
            .checked_primary(&preparation)
            .unwrap()
            .describe_native(&preparation)
            .unwrap();
        let staged = original.stage_native(&parent, &preparation).unwrap();
        let root = root(&staged);
        let family: serde_json::Value = serde_json::from_slice(
            &std::fs::read(root.join("source/family-dataset.json")).unwrap(),
        )
        .unwrap();
        let logical: serde_json::Value =
            serde_json::from_slice(&std::fs::read(root.join("source/dataset.json")).unwrap())
                .unwrap();
        assert_eq!(family["id"], memory.dataset_id.to_string());
        assert_eq!(logical["id"], child.dataset_id.to_string());
        assert_ne!(memory.dataset_id, child.dataset_id);
        assert!(root.join("source/branch.json").is_file());
        assert!(root.join("source/branches.json").is_file());
        assert!(root.join("source/history.json").is_file());
        drop(staged);
        assert_eq!(preparation.room(), before);
        assert_eq!(std::fs::read_dir(parent).unwrap().count(), 0);
        assert_eq!(
            std::fs::read_dir(temporary.path().join("scratch"))
                .unwrap()
                .count(),
            0
        );
    }

    #[test]
    fn genuine_source_transport_rechecks_selected_metadata_inode_digest() {
        let temporary = tempfile::tempdir().unwrap();
        let source_root = temporary.path().join("source");
        let store = store(&source_root);
        let parent = temporary.path().join("staged");
        std::fs::create_dir(&parent).unwrap();
        let preparation = preparation();
        let before = preparation.room();
        let original = source(&store, &preparation);
        let metadata = source_root
            .join(&original.primary.capture.data.generation)
            .join("meta.json");
        let mut changed = std::fs::read(&metadata).unwrap();
        changed[0] ^= 1;
        // In-place mutation reaches the same captured inode. A path replacement
        // would intentionally leave that retained descriptor unchanged.
        std::fs::write(&metadata, &changed).unwrap();
        assert!(original.stage_native(&parent, &preparation).is_err());
        assert_closed(&store, &preparation, before, &parent);
    }

    #[test]
    fn genuine_source_transport_syncs_nested_directories_before_parents_and_cleans_failure() {
        let temporary = tempfile::tempdir().unwrap();
        let store = store(&temporary.path().join("source"));
        let parent = temporary.path().join("staged");
        std::fs::create_dir(&parent).unwrap();
        let preparation = preparation();
        let before = preparation.room();
        let mut order = Vec::new();
        let staged = source(&store, &preparation)
            .stage_native_with_sync(&parent, &preparation, &mut |path| {
                assert!(path.is_dir());
                order.push(path.to_path_buf());
                Ok(())
            })
            .unwrap();
        assert!(order.iter().any(|path| path.ends_with("generations")));
        let root = root(&staged);
        for (index, directory) in order.iter().enumerate() {
            if directory != staged.directory.path() {
                let parent = directory.parent().unwrap();
                if parent.starts_with(staged.directory.path()) {
                    assert!(
                        order
                            .iter()
                            .position(|path| path == parent)
                            .is_some_and(|at| at > index)
                    );
                }
            }
        }
        assert_eq!(order[order.len() - 2], root);
        assert_eq!(order.last().unwrap(), staged.directory.path());
        drop(staged);
        assert_closed(&store, &preparation, before, &parent);
        let mut failed = false;
        let result = source(&store, &preparation).stage_native_with_sync(
            &parent,
            &preparation,
            &mut |path| {
                if path.ends_with("generations") {
                    failed = true;
                    return Err(Error::Cancelled);
                }
                Ok(())
            },
        );
        assert!(failed);
        assert!(matches!(result, Err(Error::Cancelled)));
        drop(result);
        assert_closed(&store, &preparation, before, &parent);
    }

    #[test]
    fn genuine_source_transport_refuses_pressure_and_late_control_delivery() {
        let temporary = tempfile::tempdir().unwrap();
        let store = store(&temporary.path().join("source"));
        let parent = temporary.path().join("staged");
        std::fs::create_dir(&parent).unwrap();
        let preparation = preparation();
        let before = preparation.room();
        let receiving = Preparation::new(Control::none(), 8192).unwrap();
        let receiving_before = receiving.room();
        let result = source(&store, &preparation).stage_native(&parent, &receiving);
        assert!(matches!(result, Err(Error::BudgetExceeded(_))));
        drop(result);
        assert_eq!(receiving.room(), receiving_before);
        assert_closed(&store, &preparation, before, &parent);
        for (phase, timeout) in [
            ("captured-source-copy", false),
            ("captured-source-staged", false),
            ("captured-source-staged", true),
        ] {
            let mut control = Control::none();
            if timeout {
                control.deadline = Some(Instant::now() + Duration::from_secs(2));
            }
            let interrupted = control.clone();
            let reached = Arc::new(AtomicBool::new(false));
            let observed = reached.clone();
            let timely = Arc::new(AtomicBool::new(false));
            let seen = timely.clone();
            control.progress = Progress::new(move |_, name| {
                if name == phase {
                    observed.store(true, Ordering::SeqCst);
                    if let Some(deadline) = interrupted.deadline {
                        seen.store(Instant::now() < deadline, Ordering::SeqCst);
                        std::thread::sleep(
                            deadline.saturating_duration_since(Instant::now())
                                + Duration::from_millis(1),
                        );
                    } else {
                        interrupted.cancel.cancel();
                    }
                }
            });
            let receiving = Preparation::new(control, 64 << 20).unwrap();
            let receiving_before = receiving.room();
            let result = source(&store, &preparation).stage_native(&parent, &receiving);
            assert!(reached.load(Ordering::SeqCst));
            assert!(!timeout || timely.load(Ordering::SeqCst));
            assert!(if timeout {
                matches!(result, Err(Error::Timeout))
            } else {
                matches!(result, Err(Error::Cancelled))
            });
            drop(result);
            assert_eq!(receiving.room(), receiving_before);
            assert_closed(&store, &preparation, before, &parent);
        }
    }
}
