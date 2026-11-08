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

/// An unpublished destination genesis derived only from a genuine checked
/// source capture. It cannot install a Store, expose a view or release replay.
struct PreparedDestination {
    source: PreparedNativeSource,
    genesis: super::super::managed_history::Prepared,
    coverage: super::super::managed_history::Coverage,
    installed: crate::managed::Installed,
    _held: Reservation,
}

/// An owned unpublished tree, still coupled to its genuine source/RDF lease.
/// There is deliberately no publication, Store/view constructor or path getter.
struct StagedDestination {
    directory: tempfile::TempDir,
    source: StagedNativeSource,
    genesis: super::super::managed_history::Prepared,
    coverage: super::super::managed_history::Coverage,
    installed: crate::managed::Installed,
    _staging: Reservation,
    _held: Reservation,
}

impl PreparedDestination {
    fn stage_candidate(
        self,
        parent: &Path,
        preparation: &Preparation,
    ) -> Result<StagedDestination> {
        self.stage_candidate_with_sync(parent, preparation, &mut |_| Ok(()))
    }

    fn stage_candidate_with_sync(
        self,
        parent: &Path,
        preparation: &Preparation,
        after_directory_sync: &mut dyn FnMut(&Path) -> Result<()>,
    ) -> Result<StagedDestination> {
        use super::super::{configuration_history, managed_history};
        preparation.checkpoint("captured-destination-stage")?;
        // Keep the old source transport outside the new root: moving its one
        // verified digest directory must not leave scratch namespaces inside it.
        let source = self.source.stage_native(parent, preparation)?;
        let capture = &source.source.primary.capture;
        let data = &capture.data;
        let old = data.dataset_id;
        let new = self.genesis.identity.dataset_id;
        let manifest = configuration_history::SourceManifest::decode_native(
            &source.source.descriptor.value,
            preparation,
        )?;
        let members = data
            .files
            .len()
            .checked_add(data.definitions.len())
            .and_then(|n| n.checked_add(manifest.member_count()))
            .and_then(|n| n.checked_add(32))
            .ok_or_else(|| preparation.exhausted())?;
        // Every retained path/ancestor plus fixed copy buffer, owned header and
        // registry encoder backing is paid before allocation. Installed's maps
        // and settings already retain their genuine preparation reservation.
        let path_bytes = (parent.as_os_str().len() as u64)
            .checked_add(2048)
            .and_then(|n| n.checked_mul(6))
            .and_then(|n| n.checked_mul(members as u64))
            .ok_or_else(|| preparation.exhausted())?;
        let _temporary = preparation.reserve(
            path_bytes
                .checked_add(64 << 10)
                .and_then(|n| {
                    n.checked_add((self.genesis.bytes.value.len() as u64).checked_mul(32)?)
                })
                .and_then(|n| n.checked_add(16384))
                .ok_or_else(|| preparation.exhausted())?,
        )?;
        let staging = preparation.reserve(
            (std::mem::size_of::<StagedDestination>() as u64)
                .checked_add(
                    (parent.as_os_str().len() as u64)
                        .checked_mul(4)
                        .ok_or_else(|| preparation.exhausted())?,
                )
                .and_then(|n| n.checked_add(8192))
                .ok_or_else(|| preparation.exhausted())?,
        )?;
        let directory = private_directory(".import-candidate-", parent)?;
        let mut tree = CandidateTree {
            root: directory.path(),
            directories: BTreeSet::new(),
        };
        tree.directories.insert(directory.path().to_path_buf());
        let mut buffer = [0; 64 << 10];
        // Only the native-first-source producer enters here. Its manifest has
        // already checked the exact generation grammar and single main root.
        let generation = &data.generation;
        for (index, file) in data.files.iter().enumerate() {
            preparation.checkpoint("captured-destination-copy")?;
            let path = file.path.as_str();
            if matches!(
                path,
                "dataset.json"
                    | "CURRENT"
                    | "history.json"
                    | "branches.json"
                    | "branch.json"
                    | "managed.json"
            ) || path.starts_with("evidence-history/")
                || crate::managed::OwnerKind::ALL
                    .iter()
                    .any(|owner| path == owner.settings_file())
            {
                continue;
            }
            if let Some(leaf) = path
                .strip_prefix(generation)
                .and_then(|p| p.strip_prefix('/'))
            {
                if !candidate_leaf(leaf) {
                    return Err(Error::invalid(
                        "captured generation has an unsafe output member",
                    ));
                }
                if leaf == "commit.json" {
                    let (bytes, _held) =
                        pinned_small(&source.source, &manifest, path, preparation)?;
                    let output = crate::commit::reidentified_generation_bytes(
                        &bytes,
                        old,
                        new,
                        preparation,
                    )?;
                    tree.bytes(path, &output.value, preparation)?;
                    continue;
                }
            } else if path != "commits.bin"
                && path != "prefixes.json"
                && !OPTIONAL_META.contains(&path)
            {
                return Err(Error::invalid(
                    "captured destination has an unknown output member",
                ));
            }
            let mut input = CapturedReader {
                capture,
                index,
                position: 0,
            };
            let mut output = tree.file(path)?;
            let mut remaining = file.len;
            if path == "commits.bin" {
                let mut header = [0; 64];
                input.read_exact(&mut header)?;
                output.write_all(&crate::commit::reidentified_catalog_header(
                    &header, old, new,
                )?)?;
                remaining = remaining.checked_sub(header.len() as u64).ok_or_else(|| {
                    Error::Corrupt("captured catalog is shorter than its header".into())
                })?;
            } else if path == crate::annotations::FILE {
                let mut header = [0; crate::annotations::REIDENTIFY_HEADER_BYTES];
                input.read_exact(&mut header)?;
                output.write_all(&crate::annotations::reidentified_header(&header, old, new)?)?;
                remaining = remaining.checked_sub(header.len() as u64).ok_or_else(|| {
                    Error::Corrupt("captured annotations are shorter than their header".into())
                })?;
            }
            let wal = path
                .strip_prefix(generation)
                .and_then(|p| p.strip_prefix('/'))
                == Some("wal.log");
            let mut hash = sha2::Sha256::new();
            while remaining != 0 {
                preparation.check()?;
                let want = remaining.min(buffer.len() as u64) as usize;
                input.read_exact(&mut buffer[..want])?;
                if wal {
                    hash.update(&buffer[..want]);
                }
                output.write_all(&buffer[..want])?;
                remaining -= want as u64;
            }
            if wal
                && (file.len != source.source.primary.wal_extent
                    || crate::evidence::Sha256Digest::from_digest_bytes(hash.finalize().into())
                        != source.source.primary.wal_digest)
            {
                return Err(managed_history::unavailable(
                    "captured WAL changed after its complete primary proof",
                ));
            }
            output.sync_all()?;
            preparation.check()?;
        }
        // Definitions keep the capture's running bytes in this unpublished
        // candidate. Actual restore/clone publication still must apply its
        // established explicit pause policy before it can be enabled.
        for definition in &data.definitions {
            preparation.check()?;
            let kind = crate::formats::definition_kind(definition.kind)
                .filter(|kind| kind.name_of(&definition.path).is_some())
                .ok_or_else(|| Error::invalid("captured definition has an unknown output path"))?;
            let _decode = preparation.reserve(
                (definition.bytes.len() as u64)
                    .checked_mul(32)
                    .and_then(|n| n.checked_add(4096))
                    .ok_or_else(|| preparation.exhausted())?,
            )?;
            let format =
                super::super::definitions::check(kind, &definition.path, &definition.bytes)?;
            if format != kind.writes || format != definition.format {
                return Err(Error::invalid(
                    "captured definition is not its running format",
                ));
            }
            tree.bytes(&definition.path, &definition.bytes, preparation)?;
        }
        if capture.original.pins.is_some() {
            let (bytes, _held) =
                pinned_small(&source.source, &manifest, "history.json", preparation)?;
            let output = crate::history::capture::reidentified(&bytes, old, new, preparation)?;
            tree.bytes("history.json", &output.value, preparation)?;
        }
        if data.next_ordinal > 1 {
            let bytes = super::super::branching::initial_import_table_bytes(
                new,
                data.next_ordinal,
                preparation,
            )?;
            tree.bytes("branches.json", &bytes.value, preparation)?;
        }
        let source_name = source.source.digest.to_string();
        let source_name = source_name
            .strip_prefix("sha256:")
            .expect("canonical source digest");
        let relative = format!("evidence-history/imports/{source_name}/origin.json");
        let descriptor_path = tree.path(&relative)?;
        let target = descriptor_path.parent().expect("source descriptor parent");
        // path() created that last directory; rename the whole verified source
        // tree into its exact absent slot rather than copying or following it.
        std::fs::remove_dir(target)?;
        std::fs::rename(source.directory.path().join(source_name), target)?;
        manifest.visit(preparation, |name, _, _| {
            tree.record(&format!(
                "evidence-history/imports/{source_name}/source/{name}"
            ))?;
            Ok(())
        })?;
        super::super::sync_dir(source.directory.path())?;
        // The actual fresh current publication is independent of old admission.
        let lineage = self.genesis.identity.lineage_id;
        let generation_digest = self.genesis.identity.generation.to_string();
        tree.bytes(
            &format!(
                "evidence-history/{lineage}/generations/{}.json",
                generation_digest
                    .strip_prefix("sha256:")
                    .expect("canonical generation digest")
            ),
            &self.genesis.bytes.value,
            preparation,
        )?;
        let coverage = managed_history::encode(&self.coverage, preparation)?;
        let coverage_digest =
            crate::evidence::Sha256Digest::of_bytes(&coverage.value, preparation.control())?;
        if self
            .installed
            .configuration_history
            .as_ref()
            .map(|root| root.coverage)
            != Some(coverage_digest)
        {
            return Err(managed_history::unavailable(
                "prepared destination cutoff changed",
            ));
        }
        tree.bytes(
            &format!("evidence-history/{lineage}/coverage.json"),
            &coverage.value,
            preparation,
        )?;
        for (owner, bytes) in &self.installed.settings {
            tree.bytes(owner.settings_file(), bytes, preparation)?;
        }
        let registry = self.installed.file_bytes();
        tree.bytes(crate::managed::FILE, &registry, preparation)?;
        let origin = configuration_history::encode_first_destination_origin(
            &source.source.descriptor.value,
            source.source.digest,
            self.genesis.identity,
            coverage_digest,
            preparation,
        )?;
        tree.bytes("evidence-history/origin.json", &origin.value, preparation)?;
        tree.bytes("CURRENT", generation.as_bytes(), preparation)?;
        let (dataset, _held) =
            pinned_small(&source.source, &manifest, "dataset.json", preparation)?;
        let dataset = crate::commit::reidentified_dataset_bytes(
            &dataset,
            old,
            new,
            data.commit.seq,
            crate::commit::now_ms(),
            preparation,
        )?;
        tree.bytes("dataset.json", &dataset.value, preparation)?;
        for path in tree.directories.iter().rev() {
            preparation.check()?;
            super::super::sync_dir(path)?;
            after_directory_sync(path)?;
            preparation.check()?;
        }
        preparation.checkpoint("captured-destination-staged")?;
        drop(tree);
        drop(manifest);
        drop(_temporary);
        Ok(StagedDestination {
            directory,
            source,
            genesis: self.genesis,
            coverage: self.coverage,
            installed: self.installed,
            _staging: staging,
            _held: self._held,
        })
    }
}

// Directory privacy must be requested at creation: tempfile's default is
// the platform/umask default, not0700. Never change the process umask.
fn private_directory(prefix: &str, parent: &Path) -> Result<tempfile::TempDir> {
    let mut builder = tempfile::Builder::new();
    builder.prefix(prefix);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        builder.permissions(std::fs::Permissions::from_mode(0o700));
    }
    Ok(builder.tempdir_in(parent)?)
}

// Callers establish the owned root and closed relative path before reaching
// this create_new operation. No pre-existing object is opened or chmodded.
fn private_output(path: &Path) -> Result<std::fs::File> {
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600).custom_flags(libc::O_NOFOLLOW);
    }
    Ok(options.open(path)?)
}

fn candidate_leaf(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 128
        && name != "."
        && name != ".."
        && name
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, b'.' | b'_' | b'-'))
}

struct CandidateTree<'a> {
    root: &'a Path,
    directories: BTreeSet<std::path::PathBuf>,
}
impl CandidateTree<'_> {
    fn path(&mut self, relative: &str) -> Result<std::path::PathBuf> {
        self.record(relative)?;
        super::super::definitions::stage_file(self.root, relative)
    }
    fn record(&mut self, relative: &str) -> Result<()> {
        if relative.len() > 512 || relative.split('/').any(|part| !candidate_leaf(part)) {
            return Err(Error::invalid(
                "candidate output path is not closed and relative",
            ));
        }
        let target = self.root.join(relative);
        let mut ancestor = target.parent();
        while let Some(path) = ancestor.filter(|path| *path != self.root) {
            self.directories.insert(path.to_path_buf());
            ancestor = path.parent();
        }
        Ok(())
    }
    fn file(&mut self, relative: &str) -> Result<std::fs::File> {
        let path = self.path(relative)?;
        private_output(&path)
    }
    fn bytes(&mut self, relative: &str, bytes: &[u8], preparation: &Preparation) -> Result<()> {
        preparation.check()?;
        let mut file = self.file(relative)?;
        for chunk in bytes.chunks(64 << 10) {
            preparation.check()?;
            file.write_all(chunk)?;
        }
        file.sync_all()?;
        preparation.check()
    }
}

fn pinned_small(
    source: &PreparedNativeSource,
    manifest: &super::super::configuration_history::SourceManifest,
    path: &str,
    preparation: &Preparation,
) -> Result<(Vec<u8>, Reservation)> {
    let (length, expected) = manifest.member_pin(path).ok_or_else(|| {
        super::super::managed_history::unavailable("captured small member has no source pin")
    })?;
    if length > super::super::managed_history::MAX_BYTES as u64 {
        return Err(preparation.exhausted());
    }
    let held = preparation.reserve(length)?;
    let mut bytes = vec![0; length as usize];
    let mut input = source.member(path)?;
    input.read_exact(&mut bytes)?;
    let mut extra = [0; 1];
    if input.read(&mut extra)? != 0 {
        return Err(super::super::managed_history::unavailable(
            "captured small member exceeds its source pin",
        ));
    }
    if crate::evidence::Sha256Digest::of_bytes(&bytes, preparation.control())? != expected {
        return Err(super::super::managed_history::unavailable(
            "captured small member changed after selection",
        ));
    }
    Ok((bytes, held))
}

impl PreparedNativeSource {
    fn prepare_reidentified(
        self,
        destination: uuid::Uuid,
        preparation: &Preparation,
    ) -> Result<PreparedDestination> {
        preparation.checkpoint("captured-destination-prepare")?;
        if destination.is_nil() {
            return Err(Error::invalid("destination identity is nil"));
        }
        let capture = &self.primary.capture;
        let current = capture
            .original
            .history
            .identity(capture.original.identities.family());
        let encoded = capture.publication_bytes(Some(current), preparation)?;
        // Complete graph strings/nonces, map/hash backing and fresh settings
        // Arc payloads are admitted before cloning or allocating any of them.
        let held = preparation.reserve(
            (encoded.value.len() as u64)
                .checked_mul(32)
                .and_then(|n| n.checked_add(std::mem::size_of::<PreparedDestination>() as u64))
                .and_then(|n| n.checked_add(16384))
                .ok_or_else(|| preparation.exhausted())?,
        )?;
        drop(encoded);
        let selected = capture.resolve_publication(current, preparation)?;
        let mut settings = std::collections::BTreeMap::new();
        for (owner, bytes) in &selected.value.owner_settings {
            preparation.check()?;
            settings.insert(*owner, std::sync::Arc::<[u8]>::from(bytes.as_bytes()));
        }
        let mut installed = crate::managed::Installed::new(
            crate::managed::ManagedGraphs {
                revision: selected.value.registry_revision,
                owners: selected.value.owners.clone(),
            },
            settings,
        )?;
        drop(selected);
        let genesis = super::super::managed_history::prepare(
            destination,
            uuid::Uuid::new_v4(),
            1,
            None,
            &installed,
            preparation,
        )?;
        let head = capture.data.commit.seq;
        let first = head.checked_add(1).ok_or_else(|| {
            super::super::managed_history::unavailable("destination own sequence overflows")
        })?;
        let coverage = super::super::managed_history::Coverage {
            format: super::super::managed_history::FORMAT,
            dataset_id: destination,
            lineage_id: genesis.identity.lineage_id,
            activation_generation: genesis.identity.generation,
            branches: std::collections::BTreeMap::from([(
                destination,
                super::super::managed_history::BranchCoverage {
                    activation_head: head,
                    first_bound_own: first,
                },
            )]),
        };
        coverage.validate()?;
        let bytes = super::super::managed_history::encode(&coverage, preparation)?;
        let digest = crate::evidence::Sha256Digest::of_bytes(&bytes.value, preparation.control())?;
        installed.configuration_history = Some(super::super::managed_history::Root {
            format: super::super::managed_history::FORMAT,
            lineage_id: genesis.identity.lineage_id,
            coverage: digest,
            current_generation: genesis.identity.generation,
        });
        drop(bytes);
        preparation.checkpoint("captured-destination-prepared")?;
        Ok(PreparedDestination {
            source: self,
            genesis,
            coverage,
            installed,
            _held: held,
        })
    }

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
        let directory = private_directory(".original-source-", parent)?;
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
            let mut output = private_output(&target)?;
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
        let mut descriptor = private_output(&root.join("origin.json"))?;
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

    fn install_flat_fixture(
        store: &Store,
        root: &Path,
        parent: &Path,
        preparation: &Preparation,
    ) -> crate::evidence::Sha256Digest {
        let staged = source(store, preparation)
            .stage_native(parent, preparation)
            .unwrap();
        let source_directory = self::root(&staged);
        let digest = staged.source.digest;
        let imports = root.join("evidence-history/imports");
        std::fs::create_dir(&imports).unwrap();
        std::fs::rename(
            &source_directory,
            imports.join(source_directory.file_name().unwrap()),
        )
        .unwrap();
        let origin = super::super::super::configuration_history::canonical_origin_test_value(
            serde_json::json!({
                "format":1,"datasetId":store.owner_dataset_id(),"sources":[digest],"bases":[],"ranges":[]
            }),
        );
        std::fs::write(root.join("evidence-history/origin.json"), origin).unwrap();
        drop(staged);
        digest
    }

    #[test]
    fn genuine_destination_stage_streams_captured_bytes_and_keeps_both_real_owners() {
        let temporary = tempfile::tempdir().unwrap();
        let root = temporary.path().join("source");
        let store = store(&root);
        let parent = temporary.path().join("outputs");
        std::fs::create_dir(&parent).unwrap();
        let source_preparation = preparation();
        let source_room = source_preparation.room();
        let receiving = preparation();
        let room = receiving.room();
        let selected = source(&store, &source_preparation);
        let generation = selected.primary.capture.data.generation.clone();
        let mut before_wal = Vec::new();
        selected
            .primary
            .capture
            .primary_reader(CapturedPrimary::Wal)
            .unwrap()
            .read_to_end(&mut before_wal)
            .unwrap();
        assert_eq!(before_wal.len() as u64, selected.primary.wal_extent);
        let mut before_catalog = Vec::new();
        selected
            .member("commits.bin")
            .unwrap()
            .read_to_end(&mut before_catalog)
            .unwrap();
        let old_generation = std::fs::read(root.join(&generation).join("commit.json")).unwrap();
        let original_pins = selected
            .primary
            .capture
            .original
            .pins
            .as_ref()
            .unwrap()
            .value
            .clone();
        let head = selected.primary.capture.data.commit.seq;
        let original_id = store.dataset_id;
        let new = uuid::Uuid::new_v4();
        let prepared = selected.prepare_reidentified(new, &receiving).unwrap();
        let genesis = prepared.genesis.identity;
        // Subsequent source appends cannot enlarge the retained copy extent.
        crate::sparql::update::update(
            &store,
            "INSERT DATA { <urn:later> <urn:p> 2 }",
            &Default::default(),
        )
        .unwrap();
        let mut synced = Vec::new();
        let staged = prepared
            .stage_candidate_with_sync(&parent, &receiving, &mut |path| {
                synced.push(path.to_path_buf());
                Ok(())
            })
            .unwrap();
        let output = staged.directory.path();
        assert_eq!(
            std::fs::read(output.join(&generation).join("wal.log")).unwrap(),
            before_wal
        );
        let catalog = std::fs::read(output.join("commits.bin")).unwrap();
        assert_eq!(&catalog[64..], &before_catalog[64..]);
        assert_eq!(
            &catalog[..64],
            &crate::commit::reidentified_catalog_header(
                before_catalog[..64].try_into().unwrap(),
                original_id,
                new
            )
            .unwrap()
        );
        let generation_bytes = std::fs::read(output.join(&generation).join("commit.json")).unwrap();
        assert_eq!(
            generation_bytes,
            crate::commit::reidentified_generation_bytes(
                &old_generation,
                original_id,
                new,
                &receiving
            )
            .unwrap()
            .value
        );
        let pins = std::fs::read(output.join("history.json")).unwrap();
        assert_eq!(
            pins,
            crate::history::capture::reidentified(&original_pins, original_id, new, &receiving)
                .unwrap()
                .value
        );
        let dataset: serde_json::Value =
            serde_json::from_slice(&std::fs::read(output.join("dataset.json")).unwrap()).unwrap();
        assert_eq!(dataset["id"], new.to_string());
        assert_eq!(dataset["minimumReader"], 9);
        assert_eq!(dataset["forkedFrom"]["id"], original_id.to_string());
        assert_eq!(dataset["forkedFrom"]["seq"], head);
        let origin: serde_json::Value = serde_json::from_slice(
            &std::fs::read(output.join("evidence-history/origin.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(origin["format"], 2);
        assert_eq!(
            origin["destination"]["genesis"],
            serde_json::to_value(genesis).unwrap()
        );
        assert_eq!(origin["bases"].as_array().unwrap().len(), 1);
        assert_eq!(origin["ranges"].as_array().unwrap().len(), 1);
        assert_eq!(staged.coverage.branches[&new].first_bound_own, head + 1);
        assert!(
            staged
                .genesis
                .document
                .value
                .matches_installed(&staged.installed)
        );
        let history = staged.installed.configuration_history.as_ref().unwrap();
        super::super::super::managed_history::read_coverage(output, new, history, &receiving)
            .unwrap();
        super::super::super::managed_history::resolve(output, new, history, genesis, &receiving)
            .unwrap();
        let digest = staged.source.source.digest.to_string();
        let imported = output
            .join("evidence-history/imports")
            .join(digest.strip_prefix("sha256:").unwrap());
        super::super::super::configuration_history::verify_staged_native_source(
            &imported,
            staged.source.source.digest,
            &receiving,
        )
        .unwrap();
        assert_eq!(
            std::fs::read(imported.join("source/history.json")).unwrap(),
            original_pins
        );
        assert_eq!(
            std::fs::read(
                imported
                    .join("source")
                    .join(&generation)
                    .join("commit.json")
            )
            .unwrap(),
            old_generation
        );
        assert!(!output.join("branch.json").exists());
        assert_eq!(
            std::fs::read_dir(staged.source.directory.path())
                .unwrap()
                .count(),
            0
        );
        for (index, directory) in synced.iter().enumerate() {
            for (other, child) in synced.iter().enumerate() {
                if child != directory && child.starts_with(directory) {
                    assert!(other < index);
                }
            }
        }
        assert_eq!(synced.last().unwrap(), output);
        assert_eq!(
            std::os::unix::fs::PermissionsExt::mode(
                &std::fs::metadata(output).unwrap().permissions()
            ) & 0o777,
            0o700
        );
        assert_eq!(
            std::os::unix::fs::PermissionsExt::mode(
                &std::fs::metadata(staged.source.directory.path())
                    .unwrap()
                    .permissions()
            ) & 0o777,
            0o700
        );
        let before = std::fs::read(output.join(&generation).join("wal.log")).unwrap();
        assert!(matches!(
            Store::open(output, StoreOptions::default()),
            Err(Error::Unsupported(_))
        ));
        assert_eq!(
            std::fs::read(output.join(&generation).join("wal.log")).unwrap(),
            before
        );
        assert_eq!(store.history.as_ref().unwrap().lock().leases.len(), 1);
        assert!(receiving.room() < room);
        drop(staged);
        assert_eq!(receiving.room(), room);
        assert_closed(&store, &source_preparation, source_room, &parent);
    }

    #[test]
    fn genuine_destination_stage_materialized_child_keeps_family_provenance_and_ordinal_reserve() {
        let temporary = tempfile::tempdir().unwrap();
        let parent = temporary.path().join("outputs");
        std::fs::create_dir(&parent).unwrap();
        let scratch = temporary.path().join("scratch");
        let memory = Store::in_memory(StoreOptions::default());
        let source_preparation = preparation();
        memory
            .activate_memory_configuration_history(&source_preparation)
            .unwrap();
        memory.create_branch("child", &Default::default()).unwrap();
        let child = memory.branch("child").unwrap();
        let room = source_preparation.room();
        let selected = child
            .materialized_configuration_capture(
                "destination",
                &MemoryCaptureOptions {
                    tmp_dir: scratch.clone(),
                    ..Default::default()
                },
                &source_preparation,
            )
            .unwrap()
            .checked_primary(&source_preparation)
            .unwrap()
            .describe_native(&source_preparation)
            .unwrap();
        let next = selected.primary.capture.data.next_ordinal;
        assert!(next > 1);
        let receiving = preparation();
        let receiving_room = receiving.room();
        let new = uuid::Uuid::new_v4();
        let staged = selected
            .prepare_reidentified(new, &receiving)
            .unwrap()
            .stage_candidate(&parent, &receiving)
            .unwrap();
        assert_eq!(staged.source.source.primary.first_wal_seq, None);
        assert!(!staged.directory.path().join("branch.json").exists());
        let table = crate::store::read_branch_table(staged.directory.path())
            .unwrap()
            .unwrap();
        assert_eq!(table["datasetId"], new.to_string());
        assert_eq!(table["nextOrdinal"], next);
        assert!(table["branches"].as_array().unwrap().is_empty());
        let digest = staged.source.source.digest.to_string();
        let original = staged
            .directory
            .path()
            .join("evidence-history/imports")
            .join(digest.strip_prefix("sha256:").unwrap())
            .join("source");
        let family: serde_json::Value =
            serde_json::from_slice(&std::fs::read(original.join("family-dataset.json")).unwrap())
                .unwrap();
        let logical: serde_json::Value =
            serde_json::from_slice(&std::fs::read(original.join("dataset.json")).unwrap()).unwrap();
        assert_eq!(family["id"], memory.dataset_id.to_string());
        assert_eq!(logical["id"], child.dataset_id.to_string());
        assert!(original.join("branch.json").is_file());
        let origin: serde_json::Value = serde_json::from_slice(
            &std::fs::read(staged.directory.path().join("evidence-history/origin.json")).unwrap(),
        )
        .unwrap();
        assert!(origin["ranges"].as_array().unwrap().is_empty());
        assert_eq!(std::fs::read_dir(&scratch).unwrap().count(), 1);
        drop(staged);
        assert_eq!(receiving.room(), receiving_room);
        assert_eq!(source_preparation.room(), room);
        assert_eq!(std::fs::read_dir(scratch).unwrap().count(), 0);
        assert_eq!(std::fs::read_dir(parent).unwrap().count(), 0);
    }

    #[test]
    fn genuine_destination_stage_controls_and_sync_failure_remove_owned_trees_and_credit() {
        let temporary = tempfile::tempdir().unwrap();
        let store = store(&temporary.path().join("source"));
        let parent = temporary.path().join("outputs");
        std::fs::create_dir(&parent).unwrap();
        let source_preparation = preparation();
        let before = source_preparation.room();
        for refusal in ["pressure", "cancel", "timeout", "sync"] {
            let mut control = Control::none();
            if refusal == "timeout" {
                control.deadline = Some(Instant::now() + Duration::from_secs(2));
            }
            let reached = Arc::new(AtomicBool::new(false));
            let observed = reached.clone();
            let interrupted = control.clone();
            control.progress = Progress::new(move |_, phase| {
                if phase == "captured-destination-staged" {
                    observed.store(true, Ordering::SeqCst);
                    if let Some(deadline) = interrupted.deadline {
                        std::thread::sleep(
                            deadline.saturating_duration_since(Instant::now())
                                + Duration::from_millis(1),
                        );
                    } else if refusal == "cancel" {
                        interrupted.cancel.cancel();
                    }
                }
            });
            let receiving = Preparation::new(control, 64 << 20).unwrap();
            let room = receiving.room();
            let selected = source(&store, &source_preparation)
                .prepare_reidentified(uuid::Uuid::new_v4(), &receiving)
                .unwrap();
            let pressure = if refusal == "pressure" {
                Some(receiving.reserve(receiving.room() - 1024).unwrap())
            } else {
                None
            };
            let mut sync_reached = false;
            let result = selected.stage_candidate_with_sync(&parent, &receiving, &mut |_| {
                sync_reached = true;
                if refusal == "sync" {
                    Err(Error::Cancelled)
                } else {
                    Ok(())
                }
            });
            match refusal {
                "pressure" => assert!(matches!(result, Err(Error::BudgetExceeded { .. }))),
                "cancel" => {
                    assert!(reached.load(Ordering::SeqCst));
                    assert!(matches!(result, Err(Error::Cancelled)));
                }
                "timeout" => {
                    assert!(reached.load(Ordering::SeqCst));
                    assert!(matches!(result, Err(Error::Timeout)));
                }
                "sync" => {
                    assert!(sync_reached);
                    assert!(matches!(result, Err(Error::Cancelled)));
                }
                _ => unreachable!(),
            }
            drop(pressure);
            assert_eq!(receiving.room(), room);
            assert_closed(&store, &source_preparation, before, &parent);
        }
    }

    #[test]
    fn genuine_destination_stage_refuses_changed_wal_after_complete_capture_proof() {
        use std::os::unix::fs::FileExt;
        let temporary = tempfile::tempdir().unwrap();
        let root = temporary.path().join("source");
        let store = store(&root);
        let parent = temporary.path().join("outputs");
        std::fs::create_dir(&parent).unwrap();
        let source_preparation = preparation();
        let before = source_preparation.room();
        let receiving = preparation();
        let room = receiving.room();
        let selected = source(&store, &source_preparation)
            .prepare_reidentified(uuid::Uuid::new_v4(), &receiving)
            .unwrap();
        let path = root
            .join(&selected.source.primary.capture.data.generation)
            .join("wal.log");
        let old_byte =
            std::fs::read(&path).unwrap()[selected.source.primary.wal_extent as usize - 1];
        let file = std::fs::OpenOptions::new().write(true).open(path).unwrap();
        file.write_all_at(&[old_byte ^ 1], selected.source.primary.wal_extent - 1)
            .unwrap();
        let error = selected.stage_candidate(&parent, &receiving).err().unwrap();
        assert!(
            error
                .to_string()
                .contains("captured WAL changed after its complete primary proof"),
            "{error}"
        );
        assert_eq!(receiving.room(), room);
        assert_closed(&store, &source_preparation, before, &parent);
    }

    #[test]
    fn genuine_destination_stage_refuses_escaping_duplicate_and_link_output_members() {
        let temporary = tempfile::tempdir().unwrap();
        let store = store(&temporary.path().join("source"));
        let parent = temporary.path().join("outputs");
        std::fs::create_dir(&parent).unwrap();
        let source_preparation = preparation();
        let before = source_preparation.room();
        for path in ["../escaped", "prefixes.json"] {
            let receiving = preparation();
            let room = receiving.room();
            let mut selected = source(&store, &source_preparation)
                .prepare_reidentified(uuid::Uuid::new_v4(), &receiving)
                .unwrap();
            selected
                .source
                .primary
                .capture
                .data
                .files
                .push(CapturedFile {
                    path: path.into(),
                    kind: FileKind::Meta,
                    len: 1,
                    src: FileSource::Bytes(Arc::from(&b"x"[..])),
                });
            assert!(selected.stage_candidate(&parent, &receiving).is_err());
            assert_eq!(receiving.room(), room);
            assert_closed(&store, &source_preparation, before, &parent);
        }
        let owned = tempfile::tempdir_in(&parent).unwrap();
        let foreign = tempfile::tempdir().unwrap();
        std::os::unix::fs::symlink(foreign.path(), owned.path().join("linked")).unwrap();
        let mut tree = CandidateTree {
            root: owned.path(),
            directories: BTreeSet::new(),
        };
        assert!(tree.bytes("linked/file", b"x", &preparation()).is_err());
        assert!(tree.bytes("gen-0001/..", b"x", &preparation()).is_err());
        assert!(tree.bytes("gen-0001/.", b"x", &preparation()).is_err());
        assert_eq!(std::fs::read_dir(foreign.path()).unwrap().count(), 0);
    }

    #[test]
    fn genuine_destination_preparation_uses_captured_head_settings_and_keeps_old_admission() {
        let temporary = tempfile::tempdir().unwrap();
        let root = temporary.path().join("source");
        let store = store(&root);
        let preparation = preparation();
        let before = preparation.room();
        let settings = crate::evidence::Settings {
            enabled: true,
            source_records: vec!["urn:source-records".into()],
            proposals: vec!["urn:proposals".into()],
            ..Default::default()
        };
        let canonical = settings.canonical(&Control::none()).unwrap();
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
        let selected = source(&store, &preparation);
        let actual_head = selected.primary.capture.data.commit;
        let old_admission = selected.primary.capture.original.binding;
        let original_current = selected
            .primary
            .capture
            .original
            .history
            .identity(store.owner_dataset_id());
        assert_ne!(old_admission.configuration(), Some(original_current));
        let mut later = settings.clone();
        later.enabled = !settings.enabled;
        let later = later.canonical(&Control::none()).unwrap();
        let decoded =
            crate::evidence::Settings::from_slice(later.bytes(), &Control::none()).unwrap();
        store
            .enable_managed(
                crate::managed::OwnerKind::Evidence,
                crate::managed::OwnerEntry::for_settings(
                    1,
                    decoded.managed_graphs(),
                    later.bytes(),
                ),
                later.bytes(),
            )
            .unwrap();
        let receiving = Preparation::new(Control::none(), 64 << 20).unwrap();
        let room = receiving.room();
        let new_id = uuid::Uuid::new_v4();
        let destination = selected.prepare_reidentified(new_id, &receiving).unwrap();
        assert!(receiving.room() < room);
        assert_eq!(destination.genesis.identity.dataset_id, new_id);
        assert_ne!(
            destination.genesis.identity.lineage_id,
            original_current.lineage_id
        );
        assert_eq!(destination.genesis.document.value.ordinal, 1);
        assert!(destination.genesis.document.value.previous.is_none());
        assert!(
            destination
                .genesis
                .document
                .value
                .matches_installed(&destination.installed)
        );
        let captured = destination
            .source
            .primary
            .capture
            .resolve_publication(original_current, &preparation)
            .unwrap();
        let expected = &captured.value.owners[&crate::managed::OwnerKind::Evidence];
        assert!(!expected.protections.is_empty());
        assert_eq!(
            &destination.installed.graphs.owners[&crate::managed::OwnerKind::Evidence],
            expected
        );
        drop(captured);
        assert_eq!(
            destination.installed.settings[&crate::managed::OwnerKind::Evidence].as_ref(),
            canonical.bytes()
        );
        assert_ne!(
            destination.installed.settings[&crate::managed::OwnerKind::Evidence].as_ref(),
            later.bytes()
        );
        assert_eq!(
            destination.coverage.branches[&new_id].activation_head,
            actual_head.seq
        );
        assert_eq!(
            destination.coverage.branches[&new_id].first_bound_own,
            actual_head.seq + 1
        );
        assert_eq!(
            destination.coverage.activation_generation,
            destination.genesis.identity.generation
        );
        assert_eq!(
            destination.source.primary.capture.original.binding,
            old_admission
        );
        assert_eq!(destination.source.primary.capture.data.commit, actual_head);
        assert_eq!(store.history.as_ref().unwrap().lock().leases.len(), 1);
        assert!(!root.join("evidence-history/origin.json").exists());
        drop(destination);
        assert_eq!(receiving.room(), room);
        assert_eq!(preparation.room(), before);
        assert_eq!(store.history.as_ref().unwrap().lock().leases.len(), 0);
    }

    #[test]
    fn genuine_destination_preparation_keeps_materialized_memory_owner_and_no_fake_wal() {
        let temporary = tempfile::tempdir().unwrap();
        let scratch = temporary.path().join("scratch");
        let store = Store::in_memory(StoreOptions::default());
        let preparation = preparation();
        store
            .activate_memory_configuration_history(&preparation)
            .unwrap();
        crate::sparql::update::update(
            &store,
            "INSERT DATA { <urn:s> <urn:p> 1 }",
            &Default::default(),
        )
        .unwrap();
        let before = preparation.room();
        let selected = store
            .materialized_configuration_capture(
                "destination-memory",
                &MemoryCaptureOptions {
                    tmp_dir: scratch.clone(),
                    ..Default::default()
                },
                &preparation,
            )
            .unwrap()
            .checked_primary(&preparation)
            .unwrap()
            .describe_native(&preparation)
            .unwrap();
        let receiving = Preparation::new(Control::none(), 64 << 20).unwrap();
        let room = receiving.room();
        let destination = selected
            .prepare_reidentified(uuid::Uuid::new_v4(), &receiving)
            .unwrap();
        assert!(destination.source.primary.capture.data.materialized);
        assert_eq!(destination.source.primary.first_wal_seq, None);
        assert_eq!(
            destination.source.primary.wal_extent,
            crate::store::WAL_REC as u64
        );
        assert_eq!(std::fs::read_dir(&scratch).unwrap().count(), 1);
        assert_eq!(
            destination
                .coverage
                .branches
                .values()
                .next()
                .unwrap()
                .activation_head,
            1
        );
        drop(destination);
        assert_eq!(receiving.room(), room);
        assert_eq!(preparation.room(), before);
        assert_eq!(std::fs::read_dir(scratch).unwrap().count(), 0);
    }

    #[test]
    fn genuine_destination_preparation_refuses_controls_with_real_credit_and_lease_rollback() {
        let temporary = tempfile::tempdir().unwrap();
        let store = store(&temporary.path().join("source"));
        let preparation = preparation();
        let before = preparation.room();
        for refusal in ["pressure", "cancel", "timeout", "nil"] {
            let selected = source(&store, &preparation);
            assert_eq!(store.history.as_ref().unwrap().lock().leases.len(), 1);
            let mut control = Control::none();
            if refusal == "timeout" {
                control.deadline = Some(Instant::now() + Duration::from_secs(2));
            }
            let reached = Arc::new(AtomicBool::new(false));
            let timely = Arc::new(AtomicBool::new(false));
            if matches!(refusal, "cancel" | "timeout") {
                let interrupted = control.clone();
                let observed = reached.clone();
                let seen = timely.clone();
                control.progress = Progress::new(move |_, phase| {
                    if phase == "captured-destination-prepared" {
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
            }
            let receiving = Preparation::new(
                control.clone(),
                if refusal == "pressure" {
                    8192
                } else {
                    64 << 20
                },
            )
            .unwrap();
            let room = receiving.room();
            let result = selected.prepare_reidentified(
                if refusal == "nil" {
                    uuid::Uuid::nil()
                } else {
                    uuid::Uuid::new_v4()
                },
                &receiving,
            );
            let error = result.err().expect("destination preparation should refuse");
            if matches!(refusal, "cancel" | "timeout") {
                assert!(reached.load(Ordering::SeqCst));
            }
            if refusal == "timeout" {
                assert!(timely.load(Ordering::SeqCst));
            }
            match refusal {
                "pressure" => assert!(matches!(error, Error::BudgetExceeded(_))),
                "cancel" => assert!(matches!(error, Error::Cancelled)),
                "timeout" => assert!(matches!(error, Error::Timeout)),
                _ => {}
            }
            assert_eq!(receiving.room(), room);
            assert_eq!(preparation.room(), before);
            assert_eq!(store.history.as_ref().unwrap().lock().leases.len(), 0);
        }
    }

    #[test]
    fn genuine_flat_sources_refuse_canonical_origin_substitution_after_open() {
        let temporary = tempfile::tempdir().unwrap();
        let root = temporary.path().join("source");
        let store = store(&root);
        let parent = temporary.path().join("staged");
        std::fs::create_dir(&parent).unwrap();
        let preparation = preparation();
        let before = preparation.room();
        let digest = install_flat_fixture(&store, &root, &parent, &preparation);
        drop(store);
        let store = Store::open(&root, StoreOptions::default()).unwrap();
        drop(
            store
                .configuration_capture("opening-selection", &Default::default(), &preparation)
                .unwrap(),
        );
        let changed = super::super::super::configuration_history::canonical_origin_test_value(
            serde_json::json!({
                "format":1,"datasetId":store.owner_dataset_id(),"sources":[digest],
                "bases":[{"local":{"generation":"gen-0001"},"source":digest,"original":{"generation":"gen-0001"}}],"ranges":[]
            }),
        );
        std::fs::write(root.join("evidence-history/origin.json"), changed).unwrap();
        let result = store.configuration_capture(
            "changed-opening-selection",
            &Default::default(),
            &preparation,
        );
        let refused = result.is_err();
        drop(result);
        assert_closed(&store, &preparation, before, &parent);
        assert!(
            refused,
            "a canonical-valid imported root must not replace the live opening selection"
        );
    }

    #[test]
    fn genuine_flat_sources_refuse_insertion_after_absent_opening() {
        let temporary = tempfile::tempdir().unwrap();
        let root = temporary.path().join("source");
        let store = store(&root);
        let parent = temporary.path().join("staged");
        std::fs::create_dir(&parent).unwrap();
        let preparation = preparation();
        let before = preparation.room();
        install_flat_fixture(&store, &root, &parent, &preparation);
        assert!(
            store
                .configuration_capture("late-origin", &Default::default(), &preparation)
                .is_err()
        );
        assert_closed(&store, &preparation, before, &parent);
        drop(store);
        let store = Store::open(&root, StoreOptions::default()).unwrap();
        let captured = store
            .configuration_capture("new-opening-origin", &Default::default(), &preparation)
            .unwrap();
        assert_eq!(
            captured.original.imports.as_ref().unwrap().source_count(),
            1
        );
        drop(captured);
        assert_closed(&store, &preparation, before, &parent);
    }

    #[test]
    fn genuine_flat_sources_refuse_deleted_selection_without_orphan_imports() {
        let temporary = tempfile::tempdir().unwrap();
        let root = temporary.path().join("source");
        let store = store(&root);
        let parent = temporary.path().join("staged");
        std::fs::create_dir(&parent).unwrap();
        let preparation = preparation();
        let before = preparation.room();
        install_flat_fixture(&store, &root, &parent, &preparation);
        drop(store);
        let store = Store::open(&root, StoreOptions::default()).unwrap();
        std::fs::remove_file(root.join("evidence-history/origin.json")).unwrap();
        std::fs::rename(
            root.join("evidence-history/imports"),
            temporary.path().join("removed-imports"),
        )
        .unwrap();
        assert!(
            store
                .configuration_capture("deleted-origin", &Default::default(), &preparation)
                .is_err()
        );
        assert_closed(&store, &preparation, before, &parent);
        drop(store);
        let store = Store::open(&root, StoreOptions::default()).unwrap();
        let captured = store
            .configuration_capture("absent-new-opening", &Default::default(), &preparation)
            .unwrap();
        assert!(captured.original.imports.is_none());
        drop(captured);
        assert_closed(&store, &preparation, before, &parent);
    }

    #[test]
    fn opening_origin_version_refuses_before_wal_or_catalog_recovery() {
        use std::io::Write;
        let temporary = tempfile::tempdir().unwrap();
        let root = temporary.path().join("source");
        let store = store(&root);
        let parent = temporary.path().join("staged");
        std::fs::create_dir(&parent).unwrap();
        let preparation = preparation();
        install_flat_fixture(&store, &root, &parent, &preparation);
        drop(store);
        let path = root.join("evidence-history/origin.json");
        let mut origin: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        origin["format"] = 999.into();
        std::fs::write(
            path,
            super::super::super::configuration_history::canonical_origin_test_value(origin),
        )
        .unwrap();
        let generation = std::fs::read_to_string(root.join("CURRENT")).unwrap();
        let wal = root.join(generation.trim()).join("wal.log");
        std::fs::OpenOptions::new()
            .append(true)
            .open(&wal)
            .unwrap()
            .write_all(&[0; crate::store::WAL_REC])
            .unwrap();
        let before = ["CURRENT", "dataset.json", "commits.bin"]
            .map(|name| std::fs::read(root.join(name)).unwrap());
        let wal_before = std::fs::read(&wal).unwrap();
        assert!(matches!(
            Store::open(&root, StoreOptions::default()),
            Err(Error::Unsupported(_))
        ));
        assert_eq!(std::fs::read(wal).unwrap(), wal_before);
        assert_eq!(
            ["CURRENT", "dataset.json", "commits.bin"]
                .map(|name| std::fs::read(root.join(name)).unwrap()),
            before
        );
    }

    #[test]
    fn genuine_flat_sources_retain_metadata_without_native_import_authority() {
        let temporary = tempfile::tempdir().unwrap();
        let root = temporary.path().join("source");
        let store = store(&root);
        let parent = temporary.path().join("staged");
        std::fs::create_dir(&parent).unwrap();
        let preparation = preparation();
        let before = preparation.room();
        let digest = install_flat_fixture(&store, &root, &parent, &preparation);
        drop(store);
        let store = Store::open(&root, StoreOptions::default()).unwrap();
        let captured = store
            .configuration_capture("prior-metadata", &Default::default(), &preparation)
            .unwrap();
        let prior = captured.original.imports.as_ref().unwrap();
        assert_eq!(prior.source_count(), 1);
        let bytes = prior
            .read(digest, "family-dataset.json", &preparation)
            .unwrap();
        assert_eq!(
            bytes.value,
            std::fs::read(root.join("dataset.json")).unwrap()
        );
        drop(bytes);
        assert!(prior.read(digest, "../dataset.json", &preparation).is_err());
        let result = captured.checked_primary(&preparation);
        assert!(matches!(result, Err(Error::Unsupported(_))));
        drop(result);
        assert_closed(&store, &preparation, before, &parent);
    }

    #[test]
    fn genuine_flat_sources_refuse_foreign_version_symlink_and_dangling_roots() {
        let temporary = tempfile::tempdir().unwrap();
        let root = temporary.path().join("source");
        let store = store(&root);
        let parent = temporary.path().join("staged");
        std::fs::create_dir(&parent).unwrap();
        let preparation = preparation();
        let before = preparation.room();
        install_flat_fixture(&store, &root, &parent, &preparation);
        drop(store);
        let store = Store::open(&root, StoreOptions::default()).unwrap();
        let path = root.join("evidence-history/origin.json");
        let original = std::fs::read(&path).unwrap();
        for damage in ["family", "version", "symlink", "missing"] {
            std::fs::remove_file(&path).unwrap();
            if damage == "symlink" {
                let other = temporary.path().join("original-origin.json");
                std::fs::write(&other, &original).unwrap();
                std::os::unix::fs::symlink(other, &path).unwrap();
            } else if damage != "missing" {
                let mut value: serde_json::Value = serde_json::from_slice(&original).unwrap();
                if damage == "family" {
                    value["datasetId"] = uuid::Uuid::new_v4().to_string().into();
                } else {
                    value["format"] = 999.into();
                }
                std::fs::write(
                    &path,
                    super::super::super::configuration_history::canonical_origin_test_value(value),
                )
                .unwrap();
            }
            assert!(
                store
                    .configuration_capture("damaged-origin", &Default::default(), &preparation)
                    .is_err(),
                "{damage}"
            );
            assert_closed(&store, &preparation, before, &parent);
            let _ = std::fs::remove_file(&path);
            std::fs::write(&path, &original).unwrap();
        }
    }

    #[test]
    fn genuine_flat_sources_refuse_missing_members_and_unclosed_dependencies() {
        let temporary = tempfile::tempdir().unwrap();
        let root = temporary.path().join("source");
        let store = store(&root);
        let parent = temporary.path().join("staged");
        std::fs::create_dir(&parent).unwrap();
        let preparation = preparation();
        let before = preparation.room();
        let digest = install_flat_fixture(&store, &root, &parent, &preparation);
        drop(store);
        let store = Store::open(&root, StoreOptions::default()).unwrap();
        let imports = root.join("evidence-history/imports");
        let text = digest.to_string();
        let directory = imports.join(text.strip_prefix("sha256:").unwrap());
        let member = directory.join("source/managed.json");
        let saved = std::fs::read(&member).unwrap();
        std::fs::remove_file(&member).unwrap();
        assert!(
            store
                .configuration_capture("missing-member", &Default::default(), &preparation)
                .is_err()
        );
        assert_closed(&store, &preparation, before, &parent);
        std::fs::write(member, saved).unwrap();
        let mut descriptor: serde_json::Value =
            serde_json::from_slice(&std::fs::read(directory.join("origin.json")).unwrap()).unwrap();
        descriptor["sources"] =
            serde_json::to_value(vec![crate::evidence::Sha256Digest::from_digest_bytes(
                [7; 32],
            )])
            .unwrap();
        let wire =
            super::super::super::configuration_history::canonical_descriptor_test_value(descriptor);
        let changed =
            crate::evidence::Sha256Digest::of_bytes(&wire, preparation.control()).unwrap();
        std::fs::write(directory.join("origin.json"), wire).unwrap();
        let changed_text = changed.to_string();
        std::fs::rename(
            &directory,
            imports.join(changed_text.strip_prefix("sha256:").unwrap()),
        )
        .unwrap();
        std::fs::write(root.join("evidence-history/origin.json"),super::super::super::configuration_history::canonical_origin_test_value(serde_json::json!({
            "format":1,"datasetId":store.owner_dataset_id(),"sources":[changed],"bases":[],"ranges":[]
        }))).unwrap();
        drop(store);
        let store = Store::open(&root, StoreOptions::default()).unwrap();
        assert!(
            store
                .configuration_capture("missing-dependency", &Default::default(), &preparation)
                .is_err()
        );
        assert_closed(&store, &preparation, before, &parent);
    }

    #[test]
    fn genuine_flat_sources_keep_the_original_anchored_source_directory() {
        let temporary = tempfile::tempdir().unwrap();
        let root = temporary.path().join("source");
        let store = store(&root);
        let parent = temporary.path().join("staged");
        std::fs::create_dir(&parent).unwrap();
        let preparation = preparation();
        let before = preparation.room();
        let digest = install_flat_fixture(&store, &root, &parent, &preparation);
        drop(store);
        let store = Store::open(&root, StoreOptions::default()).unwrap();
        let captured = store
            .configuration_capture("anchored-source", &Default::default(), &preparation)
            .unwrap();
        let text = digest.to_string();
        let directory = root
            .join("evidence-history/imports")
            .join(text.strip_prefix("sha256:").unwrap());
        let moved = temporary.path().join("retained-source-directory");
        std::fs::rename(&directory, &moved).unwrap();
        std::fs::create_dir(&directory).unwrap();
        std::fs::create_dir(directory.join("source")).unwrap();
        std::fs::write(directory.join("source/managed.json"), b"replacement").unwrap();
        let bytes = captured
            .original
            .imports
            .as_ref()
            .unwrap()
            .read(digest, "managed.json", &preparation)
            .unwrap();
        assert_eq!(
            bytes.value,
            std::fs::read(moved.join("source/managed.json")).unwrap()
        );
        drop(bytes);
        drop(captured);
        assert_closed(&store, &preparation, before, &parent);
    }

    #[test]
    fn genuine_flat_sources_preserve_receiving_cancel_pressure_and_real_lease_rollback() {
        let temporary = tempfile::tempdir().unwrap();
        let root = temporary.path().join("source");
        let store = store(&root);
        let parent = temporary.path().join("staged");
        std::fs::create_dir(&parent).unwrap();
        let preparation = preparation();
        let before = preparation.room();
        install_flat_fixture(&store, &root, &parent, &preparation);
        drop(store);
        let store = Store::open(&root, StoreOptions::default()).unwrap();
        let small = Preparation::new(Control::none(), 8192).unwrap();
        let available = small.room();
        assert!(matches!(
            store.configuration_capture("pressure", &Default::default(), &small),
            Err(Error::BudgetExceeded(_))
        ));
        assert_eq!(small.room(), available);
        assert_closed(&store, &preparation, before, &parent);
        let control = Control::none();
        let receiving = Preparation::new(control.clone(), 64 << 20).unwrap();
        let available = receiving.room();
        store.set_failpoint(
            "backup-capture-unlocked",
            Some(Arc::new(move |_| control.cancel.cancel())),
        );
        let result = store.configuration_capture("cancelled", &Default::default(), &receiving);
        store.set_failpoint("backup-capture-unlocked", None);
        assert!(matches!(result, Err(Error::Cancelled)));
        drop(result);
        assert_eq!(receiving.room(), available);
        assert_closed(&store, &preparation, before, &parent);
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
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            std::fs::metadata(staged.directory.path())
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o700
        );
        assert_eq!(
            std::fs::metadata(root.join("origin.json"))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
        let manifest = super::super::super::configuration_history::SourceManifest::decode_native(
            &staged.source.descriptor.value,
            &preparation,
        )
        .unwrap();
        manifest
            .visit(&preparation, |path, _, _| {
                assert_eq!(
                    std::fs::metadata(root.join("source").join(path))
                        .unwrap()
                        .permissions()
                        .mode()
                        & 0o777,
                    0o600
                );
                Ok(())
            })
            .unwrap();
        drop(manifest);
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
