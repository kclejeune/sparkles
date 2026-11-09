//! Consistent captures of a live database for backup repositories (`sparkles-backup`).
//!
//! A capture pins one commit `s` of a persistent store without blocking writers for
//! more than a few system calls: under the writer mutex it records the length of every
//! append-only file (the WAL up to the end of `s`'s record, the flushed delta
//! vocabulary, the commit catalog through `s`), clones the prefixes, and leases the
//! generation so that history garbage collection keeps its directory until the lease is
//! dropped. After the lock it opens a read handle for every file of the generation and
//! reads through them (`pread`), so a compaction or bulk commit during the upload does
//! not affect the backup.

use super::Store;
#[allow(
    dead_code,
    reason = "private item 761 import machinery that stays unwired until item 650 consumers agree"
)]
mod transport;

use crate::commit::CommitInfo;
use crate::error::{Error, Result};
use std::fs::File;
use std::io::Write;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

/// How a captured file is stored in a backup repository.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum FileKind {
    /// never changes once written (`gen-NNNN/*.dat`, `vocab.dat`, `meta.json`, …):
    /// stored whole, split into fixed-size pieces
    Immutable,
    /// only grows (`gen-NNNN/wal.log`, `gen-NNNN/delta.vocab`, `commits.bin`): stored as
    /// ordered segments, so a later backup uploads only the appended bytes
    Append,
    /// small files that may be rewritten (`CURRENT`, `dataset.json`, `prefixes.json`,
    /// `text.json`, `origin.json`, `reasoning.json`, `validation*`): one blob each
    Meta,
}

impl FileKind {
    pub fn as_str(self) -> &'static str {
        match self {
            FileKind::Immutable => "immutable",
            FileKind::Append => "append",
            FileKind::Meta => "meta",
        }
    }
}

/// Where a captured file's bytes come from.
#[derive(Debug)]
pub enum FileSource {
    /// a read handle opened while the writer lock was held; read with positioned reads
    /// ([`CapturedFile::read_at`]) up to [`CapturedFile::len`] only
    File(std::fs::File),
    /// content rendered at capture time (`prefixes.json`) or read whole after it
    Bytes(Arc<[u8]>),
}

/// One file of a capture.
#[derive(Debug)]
pub struct CapturedFile {
    /// path relative to the database root, with `/` separators (`gen-0001/spo.dat`,
    /// `commits.bin`, `CURRENT`)
    pub path: String,
    pub kind: FileKind,
    /// bytes that belong to the backup: the whole file for immutable and meta files, the
    /// captured prefix for append-only ones (the file may have grown since)
    pub len: u64,
    pub src: FileSource,
}

impl CapturedFile {
    /// Read up to `buf.len()` bytes at `offset`, never past [`len`](Self::len). Returns
    /// the number of bytes read (0 at `len`). Positioned: concurrent calls on one file
    /// are fine.
    pub fn read_at(&self, offset: u64, buf: &mut [u8]) -> std::io::Result<usize> {
        if offset >= self.len {
            return Ok(0);
        }
        let n = buf.len().min((self.len - offset) as usize);
        match &self.src {
            FileSource::Bytes(b) => {
                let start = offset as usize;
                let avail = b.len().saturating_sub(start).min(n);
                buf[..avail].copy_from_slice(&b[start..start + avail]);
                Ok(avail)
            }
            FileSource::File(f) => {
                #[cfg(unix)]
                {
                    std::os::unix::fs::FileExt::read_at(f, &mut buf[..n], offset)
                }
                #[cfg(windows)]
                {
                    std::os::windows::fs::FileExt::seek_read(f, &mut buf[..n], offset)
                }
                #[cfg(not(any(unix, windows)))]
                {
                    let _ = f;
                    Err(std::io::Error::other("positioned reads are not supported"))
                }
            }
        }
    }

    /// Fill `buf` completely from `offset`; a short file is an `UnexpectedEof` error
    /// (a generation file that shrank during the backup: `Error::Corrupt` upstream).
    pub fn read_exact_at(&self, mut offset: u64, mut buf: &mut [u8]) -> std::io::Result<()> {
        while !buf.is_empty() {
            match self.read_at(offset, buf) {
                Ok(0) => {
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::UnexpectedEof,
                        format!("{}: file shorter than captured", self.path),
                    ));
                }
                Ok(n) => {
                    buf = &mut buf[n..];
                    offset += n as u64;
                }
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
                Err(e) => return Err(e),
            }
        }
        Ok(())
    }
}

/// Keeps a captured generation leased (history GC keeps its directory) until dropped.
/// Dropping it removes the lease and runs a best-effort history collection
/// ([`Store::try_collect_history`]): if the writer is busy, the next collection point
/// removes the generation instead. A guard made with [`LeaseGuard::none`] holds nothing.
pub struct LeaseGuard {
    /// the leased generation number (`gen-NNNN`), 0 for [`none`](Self::none)
    pub(super) generation: u32,
    /// the lease's label, shown in `HistoryStatus` as `backup:<label>`
    pub(super) label: String,
    /// removes the lease (and collects); run once, on drop
    pub(super) release: Option<Box<dyn FnOnce() + Send + Sync>>,
}

impl LeaseGuard {
    /// A guard that holds no lease (a capture of a closed database directory).
    pub fn none() -> LeaseGuard {
        LeaseGuard {
            generation: 0,
            label: String::new(),
            release: None,
        }
    }

    /// Keep an additional owner alive until this capture is released.
    pub fn hold_owner(&mut self, owner: Box<dyn FnOnce() + Send + Sync>) {
        let release = self.release.take();
        self.release = Some(Box::new(move || {
            if let Some(r) = release {
                r();
            }
            owner();
        }));
    }

    /// The leased generation number (0 when nothing is leased).
    pub fn generation(&self) -> u32 {
        self.generation
    }

    pub fn label(&self) -> &str {
        &self.label
    }
}

impl std::fmt::Debug for LeaseGuard {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LeaseGuard")
            .field("generation", &self.generation)
            .field("label", &self.label)
            .field("held", &self.release.is_some())
            .finish()
    }
}

impl Drop for LeaseGuard {
    fn drop(&mut self) {
        if let Some(release) = self.release.take() {
            release();
        }
    }
}

/// The branch captured as a standalone database. UUIDs identify its lineage even
/// when its name is renamed or reused.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BackupBranch {
    pub dataset_id: uuid::Uuid,
    pub id: uuid::Uuid,
    pub name: String,
    pub next_ordinal: u32,
}

/// A consistent capture of a persistent store at one commit ([`Store::backup_capture`]).
#[derive(Debug)]
pub struct BackupCapture {
    pub dataset_id: uuid::Uuid,
    /// the captured commit `s` (the head when the writer lock was held)
    pub commit: CommitInfo,
    /// the generation directory holding `s` (`gen-NNNN`)
    pub generation: String,
    /// the index format of that generation (`builder::FORMAT_VERSION` when it was built)
    pub index_format: u32,
    /// the index features of that generation (`IndexMeta::features`)
    pub index_features: Vec<String>,
    /// every file of the backup, in a stable order: the generation's files, then
    /// `commits.bin`, then the meta files (`CURRENT`, `dataset.json`, `prefixes.json`,
    /// and when present `annotations.bin`, `text.json`, `origin.json`, `validation.json`,
    /// `validation-shapes.ttl`, `validation-schema.shex`, `validation-schema.json`, and
    /// the dataset's managed-graph registry, `managed.json` with its owners' settings,
    /// whose `dataset.json` then needs reader 6).
    /// `reasoning.json` is the caller's to add (the server
    /// holds the current status and applies the "not after `s`" rule).
    pub files: Vec<CapturedFile>,
    /// how long the writer lock was held (metric `sparkles_backup_capture_lock_seconds`)
    pub lock_hold: Duration,
    /// keeps the generation until the upload ends
    pub lease: LeaseGuard,
    /// a capture of an in-memory store ([`Store::memory_backup_capture`]): the files
    /// are a temporary database built from its snapshot, removed with the lease
    pub in_memory: bool,
    /// Scratch materialization is independent of whether the source lives in memory.
    pub materialized: bool,
    pub branch: Option<BackupBranch>,
    /// Preserve every allocated branch blank-node range, including merged/deleted branches.
    pub next_ordinal: u32,
    /// the dataset's branches other than `main`, which a backup leaves out
    pub branches_omitted: u64,
    /// the dataset-managed definitions current at `commit`, read under the writer
    /// lock (see [`super::definitions`])
    pub definitions: Vec<super::DefinitionFile>,
}

/// Genuine data capture coupled to original metadata under the same writer.
/// This private owner establishes neither imported physical framing nor replay
/// authority. It never accepts a public capture DTO or arbitrary lease.
pub(super) struct ConfigurationCapture {
    data: BackupCapture,
    original: CapturedOriginal,
}

/// The closed primary members needed by the original framing proof. A member
/// selects an already captured descriptor, never a new filesystem open.
#[allow(
    dead_code,
    reason = "private item 761 import machinery that stays unwired until item 650 consumers agree"
)]
#[derive(Clone, Copy)]
pub(super) enum CapturedPrimary {
    Commit,
    Meta,
    Wal,
}

#[allow(
    dead_code,
    reason = "private item 761 import machinery that stays unwired until item 650 consumers agree"
)]
impl CapturedPrimary {
    fn name(self) -> &'static str {
        match self {
            Self::Commit => "commit.json",
            Self::Meta => "meta.json",
            Self::Wal => "wal.log",
        }
    }
}

/// Borrowing this cursor retains the genuine capture and its actual data lease.
/// It proves only the selected descriptor/extent, not an imported identity.
pub(super) struct CapturedReader<'a> {
    capture: &'a ConfigurationCapture,
    index: usize,
    position: u64,
}

#[allow(
    dead_code,
    reason = "private item 761 import machinery that stays unwired until item 650 consumers agree"
)]
impl ConfigurationCapture {
    pub(super) fn primary_reader(&self, member: CapturedPrimary) -> Option<CapturedReader<'_>> {
        let index = self.data.files.iter().position(|file| {
            file.path
                .strip_prefix(self.data.generation.as_str())
                .and_then(|suffix| suffix.strip_prefix('/'))
                == Some(member.name())
        })?;
        Some(CapturedReader {
            capture: self,
            index,
            position: 0,
        })
    }
}

/// A private native primary proof. It retains the genuine captured lease and
/// the receiving allowance, and grants no imported interval/replay authority.
#[allow(
    dead_code,
    reason = "private item 761 import machinery that stays unwired until item 650 consumers agree"
)]
struct CheckedPrimaryCapture {
    capture: ConfigurationCapture,
    base: CommitInfo,
    wal_extent: u64,
    wal_digest: crate::evidence::Sha256Digest,
    first_wal_seq: Option<u64>,
    metadata_pin: super::configuration_history::SourceFilePin,
    commit_pin: super::configuration_history::SourceFilePin,
    _held: crate::access::Reservation,
}

impl std::fmt::Debug for CheckedPrimaryCapture {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("CheckedPrimaryCapture")
            .field("base", &self.base)
            .field("wal_extent", &self.wal_extent)
            .finish_non_exhaustive()
    }
}

/// Native source metadata only. Its actual checked data lease and encoded
/// receiving bytes remain owned together; this is not an imported replay token.
struct PreparedNativeSource {
    primary: CheckedPrimaryCapture,
    descriptor: super::managed_history::Bytes,
    digest: crate::evidence::Sha256Digest,
    _held: crate::access::Reservation,
}

#[allow(
    dead_code,
    reason = "private item 761 import machinery that stays unwired until item 650 consumers agree"
)]
impl CheckedPrimaryCapture {
    fn describe_native(
        self,
        preparation: &crate::access::Preparation,
    ) -> Result<PreparedNativeSource> {
        use super::configuration_history::{
            SOURCE_MEMBER_LIMIT, SourceFilePin, source_metadata_member,
        };
        use sha2::Digest;
        preparation.check()?;
        let held = preparation.reserve(std::mem::size_of::<PreparedNativeSource>() as u64)?;
        let identities = &self.capture.original.identities;
        let aliases = 1
            + usize::from(identities.branch_document().is_some())
            + usize::from(self.capture.original.pins.is_some())
            + usize::from(self.capture.original.table.is_some());
        let members = self
            .capture
            .data
            .files
            .iter()
            .filter(|file| source_metadata_member(&file.path))
            .count()
            .checked_add(aliases)
            .ok_or_else(|| preparation.exhausted())?;
        if members > SOURCE_MEMBER_LIMIT {
            return Err(super::managed_history::unavailable(
                "native source metadata has too many members",
            ));
        }
        // Conservatively cover each tree node, bounded path/capacity and
        // allocator slack. This loan outlives the map's synchronous encoding.
        let per_member = std::mem::size_of::<(String, SourceFilePin)>()
            .checked_add(256)
            .and_then(|size| size.checked_add(512))
            .ok_or_else(|| preparation.exhausted())?;
        let map_bytes = members
            .checked_mul(per_member)
            .ok_or_else(|| preparation.exhausted())?;
        let _map =
            preparation.reserve(u64::try_from(map_bytes).map_err(|_| preparation.exhausted())?)?;
        let _buffer = preparation.reserve(64 << 10)?;
        let mut buffer = [0; 64 << 10];
        let mut files = std::collections::BTreeMap::new();
        for (index, file) in self.capture.data.files.iter().enumerate() {
            if !source_metadata_member(&file.path) {
                continue;
            }
            preparation.checkpoint("captured-source-member")?;
            if file.path.len() > 256 || file.len > super::managed_history::MAX_BYTES as u64 {
                return Err(super::managed_history::unavailable(
                    "native source metadata exceeds its closed bounds",
                ));
            }
            let mut reader = CapturedReader {
                capture: &self.capture,
                index,
                position: 0,
            };
            let mut digest = sha2::Sha256::new();
            while reader.position < file.len {
                preparation.check()?;
                let count = std::io::Read::read(&mut reader, &mut buffer)?;
                if count == 0 {
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::UnexpectedEof,
                        "native source metadata is shorter than its captured extent",
                    )
                    .into());
                }
                digest.update(&buffer[..count]);
            }
            preparation.check()?;
            let pin = SourceFilePin {
                bytes: file.len,
                sha256: crate::evidence::Sha256Digest::from_digest_bytes(digest.finalize().into()),
            };
            // These exact bytes supplied the actual producer/basis proof.
            // A later read of the same mutable FD cannot mint another pin.
            let checked = match file
                .path
                .strip_prefix(self.capture.data.generation.as_str())
            {
                Some("/meta.json") => Some(&self.metadata_pin),
                Some("/commit.json") => Some(&self.commit_pin),
                _ => None,
            };
            if checked
                .is_some_and(|checked| checked.bytes != pin.bytes || checked.sha256 != pin.sha256)
            {
                return Err(super::managed_history::unavailable(
                    "checked primary member changed before descriptor",
                ));
            }
            // The registry and settings descriptors were verified against the
            // selected configuration at capture. Their pins are that selection.
            let registry_member = file.path == crate::managed::FILE
                || crate::managed::OwnerKind::ALL
                    .iter()
                    .any(|owner| file.path == owner.settings_file());
            if registry_member
                && !self
                    .capture
                    .original
                    .registry
                    .iter()
                    .any(|(path, bytes, sha256)| {
                        *path == file.path && *bytes == pin.bytes && *sha256 == pin.sha256
                    })
            {
                return Err(super::managed_history::unavailable(
                    "captured registry member changed before descriptor",
                ));
            }
            if files.insert(file.path.clone(), pin).is_some() {
                return Err(super::managed_history::unavailable(
                    "duplicate native source metadata",
                ));
            }
        }
        // Family identity is independent of this logical/physical output.
        for (name, bytes) in [
            ("family-dataset.json", Some(identities.family_document())),
            ("branch.json", identities.branch_document()),
            (
                "history.json",
                self.capture
                    .original
                    .pins
                    .as_ref()
                    .map(|bytes| bytes.value.as_slice()),
            ),
            (
                "branches.json",
                self.capture
                    .original
                    .table
                    .as_ref()
                    .map(|bytes| bytes.value.as_slice()),
            ),
        ] {
            let Some(bytes) = bytes else {
                continue;
            };
            preparation.check()?;
            let pin = SourceFilePin {
                bytes: bytes.len() as u64,
                sha256: crate::evidence::Sha256Digest::of_bytes(bytes, preparation.control())?,
            };
            if files.insert(name.to_owned(), pin).is_some() {
                return Err(super::managed_history::unavailable(
                    "ambiguous original identity member",
                ));
            }
        }
        let descriptor = super::configuration_history::encode_native_descriptor(
            identities.family(),
            identities.logical(),
            self.capture.data.commit.seq,
            &self.capture.data.generation,
            files,
            self.first_wal_seq
                .map(|first| (self.wal_extent, self.wal_digest, first)),
            preparation,
        )?;
        let digest =
            crate::evidence::Sha256Digest::of_bytes(&descriptor.value, preparation.control())?;
        preparation.checkpoint("captured-source-delivery")?;
        Ok(PreparedNativeSource {
            primary: self,
            descriptor,
            digest,
            _held: held,
        })
    }
}

#[allow(
    dead_code,
    reason = "private item 761 import machinery that stays unwired until item 650 consumers agree"
)]
struct CapturedMembership {
    seqs: Option<Vec<u64>>,
    _held: crate::access::Reservation,
}

#[allow(
    dead_code,
    reason = "private item 761 import machinery that stays unwired until item 650 consumers agree"
)]
impl ConfigurationCapture {
    fn annotation_membership(
        &self,
        preparation: &crate::access::Preparation,
    ) -> Result<CapturedMembership> {
        let allowance = preparation.room();
        let mut held = preparation.reserve(allowance)?;
        let options = crate::history::HistoryOptions {
            cancel: Some(preparation.control().cancel.flag()),
            deadline: preparation.control().deadline,
        };
        let mut budget = super::cold::ColdBudget::new(crate::history::ReadLimit {
            options: &options,
            memory: Some(allowance),
            held: 0,
        })?;
        let value = if let Some(index) = self
            .data
            .files
            .iter()
            .position(|file| file.path == crate::annotations::FILE)
        {
            let mut reader = CapturedReader {
                capture: self,
                index,
                position: 0,
            };
            crate::annotations::seqs_from(&mut reader, self.data.dataset_id, &mut budget)?.value
        } else {
            None
        };
        let charge = budget.used();
        held.release_to(charge);
        preparation.check()?;
        Ok(CapturedMembership {
            seqs: value,
            _held: held,
        })
    }

    fn check_metadata_basis(
        &self,
        expected: crate::evidence::Sha256Digest,
        preparation: &crate::access::Preparation,
    ) -> Result<super::configuration_history::SourceFilePin> {
        preparation.check()?;
        // The fixed streaming hash state is paid before lending the remaining
        // allowance to input/JSON/type decoding. No serialized Meta Vec exists.
        let _workspace =
            preparation.reserve(std::mem::size_of::<MetadataDigest<'_>>() as u64 + 1024)?;
        let allowance = preparation.room();
        let _loan = preparation.reserve(allowance)?;
        let options = crate::history::HistoryOptions {
            cancel: Some(preparation.control().cancel.flag()),
            deadline: preparation.control().deadline,
        };
        let mut budget = super::cold::ColdBudget::new(crate::history::ReadLimit {
            options: &options,
            memory: Some(allowance),
            held: 0,
        })?;
        let mut reader = self.primary_reader(CapturedPrimary::Meta).ok_or_else(|| {
            super::managed_history::unavailable("captured primary index metadata is absent")
        })?;
        if reader.extent() > super::managed_history::MAX_BYTES as u64 {
            return Err(preparation.exhausted());
        }
        let extent = reader.extent();
        let bytes = budget.read_from(&mut reader, extent)?;
        let raw: super::cold::Loaded<serde_json::Value> = budget.json(&bytes)?;
        let version = raw
            .value
            .get("format_version")
            .and_then(serde_json::Value::as_u64)
            .and_then(|n| u32::try_from(n).ok())
            .ok_or_else(|| {
                Error::Corrupt("captured meta.json: format_version is missing or not a u32".into())
            })?;
        crate::formats::INDEX.check("captured-primary/meta.json", version, None)?;
        let meta: crate::builder::IndexMeta = budget.value(raw.value)?;
        if meta
            .features
            .iter()
            .any(|feature| !crate::builder::KNOWN_FEATURES.contains(&feature.as_str()))
        {
            return Err(Error::Unsupported(
                "captured index metadata uses an unsupported feature".into(),
            ));
        }
        if metadata_fingerprint_admitted(&meta, preparation)? != expected {
            return Err(super::managed_history::unavailable(
                "captured index metadata differs from its actual physical producer",
            ));
        }
        let pin = super::configuration_history::SourceFilePin {
            bytes: extent,
            sha256: crate::evidence::Sha256Digest::of_bytes(&bytes, preparation.control())?,
        };
        preparation.check()?;
        Ok(pin)
    }

    fn checked_primary(
        self,
        preparation: &crate::access::Preparation,
    ) -> Result<CheckedPrimaryCapture> {
        preparation.check()?;
        if self.original.imports.is_some() {
            return Err(Error::Unsupported(
                "captured prior origins require authenticated imported physical context".into(),
            ));
        }
        let held = preparation.reserve(
            std::mem::size_of::<CheckedPrimaryCapture>() as u64
                + std::mem::size_of::<super::configuration_history::primary::Header>() as u64
                + std::mem::size_of::<sha2::Sha256>() as u64,
        )?;
        let bad = || {
            super::managed_history::unavailable(
                "captured primary differs from its actual Store basis",
            )
        };
        let expected_head = if self.data.materialized {
            CommitInfo {
                generation: 1,
                ..self.original.head
            }
        } else {
            self.original.head
        };
        if self.data.commit != expected_head
            || self.data.dataset_id != self.original.identities.logical()
        {
            return Err(bad());
        }
        let basis = self.original.native.ok_or_else(bad)?;
        let metadata_pin = self.check_metadata_basis(basis.metadata, preparation)?;
        // This ephemeral loan admits the input, JSON scratch and decoded origin.
        // No publication callback can run while it holds the remaining allowance.
        let (base_binding, strict_from, commit_pin) = {
            let allowance = preparation.room();
            let _loan = preparation.reserve(allowance)?;
            let options = crate::history::HistoryOptions {
                cancel: Some(preparation.control().cancel.flag()),
                deadline: preparation.control().deadline,
            };
            let mut budget = super::cold::ColdBudget::new(crate::history::ReadLimit {
                options: &options,
                memory: Some(allowance),
                held: 0,
            })?;
            let mut reader = self
                .primary_reader(CapturedPrimary::Commit)
                .ok_or_else(bad)?;
            let extent = reader.extent();
            let bytes = budget.read_from(&mut reader, extent)?;
            let decoded = crate::commit::decode_gen_commit_bytes_controlled(
                Path::new("captured-primary/commit.json"),
                &bytes,
                &mut budget,
            )?;
            if decoded.value.dataset_id != basis.physical_owner
                || decoded.value.commit != basis.base
            {
                return Err(bad());
            }
            let pin = super::configuration_history::SourceFilePin {
                bytes: extent,
                sha256: crate::evidence::Sha256Digest::of_bytes(&bytes, preparation.control())?,
            };
            (
                decoded.value.configuration,
                decoded.value.wal_strict_from,
                pin,
            )
        };
        let family = self.original.identities.family();
        let coverage = {
            let bytes = self.publication_bytes(None, preparation)?;
            super::managed_history::coverage_bytes(
                &bytes.value,
                family,
                &self.original.history,
                preparation,
            )?
        };
        let mut header = super::configuration_history::primary::Header::from_coverage(
            family,
            self.original.identities.logical(),
            self.original.origin,
            Some(self.original.history.clone()),
            false,
            Some(&coverage.value),
        )?;
        drop(coverage);
        header.generation_using(preparation, basis.base.seq, base_binding, &mut |identity| {
            self.resolve_publication(identity, preparation).map(|_| ())
        })?;
        let membership = self.annotation_membership(preparation)?;
        // The parser itself uses fixed stack records. Its only heap buffer is
        // admitted separately; publication decoding uses the remaining allowance.
        let _buffer = preparation.reserve(64 << 10)?;
        let wal = self.primary_reader(CapturedPrimary::Wal).ok_or_else(bad)?;
        let extent = wal.extent();
        let mut reader = std::io::BufReader::with_capacity(64 << 10, wal);
        if reader.capacity() != 64 << 10 {
            return Err(preparation.exhausted());
        }
        let options = crate::history::HistoryOptions {
            cancel: Some(preparation.control().cancel.flag()),
            deadline: preparation.control().deadline,
        };
        let budget = super::cold::ColdBudget::new(crate::history::ReadLimit {
            options: &options,
            memory: Some(0),
            held: 0,
        })?;
        let mut last_binding = base_binding
            .configuration()
            .map(|identity| identity.generation);
        let mut configuration_error = None;
        use sha2::Digest;
        let mut digest = sha2::Sha256::new();
        let mut bytes_hashed = 0u64;
        let mut first_wal_seq = None;
        let proof = super::wal::probe::prove_prefix_recording(
            &mut reader,
            extent,
            basis.base.seq,
            self.data.commit.seq,
            basis.fold_legacy,
            strict_from,
            basis.dvocab_len,
            Some(membership.seqs.as_deref().unwrap_or(&[])),
            Path::new("captured-primary/wal.log"),
            &budget,
            &mut |observation| {
                first_wal_seq.get_or_insert(observation.point.seq);
                if configuration_error.is_none() {
                    configuration_error = header
                        .observation_using(preparation, observation, &mut |identity| {
                            self.resolve_publication(identity, preparation).map(|_| ())
                        })
                        .err();
                }
                last_binding = observation.configuration;
                if observation.point.seq == self.data.commit.seq
                    && let Some((timestamp, kind, flags)) = observation.metadata
                    && (timestamp != self.data.commit.timestamp_ms
                        || kind != self.data.commit.kind
                        || (flags & crate::commit::WAL_FLAG_UNVALIDATED != 0)
                            != self.data.commit.unvalidated)
                {
                    return Err(bad());
                }
                Ok(())
            },
            &mut |offset, record| {
                if offset != bytes_hashed {
                    return Err(bad());
                }
                digest.update(record);
                bytes_hashed = bytes_hashed
                    .checked_add(record.len() as u64)
                    .ok_or_else(bad)?;
                Ok(())
            },
        )?;
        if bytes_hashed != extent
            || proof.head.seq != self.data.commit.seq
            || proof.head.offset != extent
            || proof
                .selected
                .is_none_or(|point| point.seq != self.data.commit.seq)
            || last_binding
                != self
                    .original
                    .binding
                    .configuration()
                    .map(|identity| identity.generation)
        {
            return Err(bad());
        }
        if let Some(error) = configuration_error {
            return Err(error);
        }
        drop(reader);
        drop(membership);
        preparation.check()?;
        Ok(CheckedPrimaryCapture {
            capture: self,
            base: basis.base,
            wal_extent: extent,
            wal_digest: crate::evidence::Sha256Digest::from_digest_bytes(digest.finalize().into()),
            first_wal_seq,
            metadata_pin,
            commit_pin,
            _held: held,
        })
    }
}

impl ConfigurationCapture {
    /// Resolve the retained original closure, even after live settings change.
    /// The receiving body owns its actual Preparation independently of this
    /// capture. This validates publication, never a foreign WAL replay right.
    fn resolve_publication(
        &self,
        wanted: crate::history::ConfigurationIdentity,
        preparation: &crate::access::Preparation,
    ) -> Result<super::managed_history::Owned<super::managed_history::Generation>> {
        preparation.check()?;
        let family = self.original.identities.family();
        let history = &self.original.history;
        let resolved = super::managed_history::resolve_using(
            family,
            history,
            wanted,
            preparation,
            || {
                let bytes = self.publication_bytes(None, preparation)?;
                super::managed_history::coverage_bytes(&bytes.value, family, history, preparation)
            },
            |identity| {
                let bytes = self.publication_bytes(Some(identity), preparation)?;
                super::managed_history::generation_bytes(&bytes.value, identity, preparation)
            },
        )?;
        preparation.check()?;
        Ok(resolved)
    }

    fn publication_bytes(
        &self,
        identity: Option<crate::history::ConfigurationIdentity>,
        preparation: &crate::access::Preparation,
    ) -> Result<super::managed_history::Bytes> {
        preparation.check()?;
        let history = &self.original.history;
        if identity.is_some_and(|identity| {
            identity.dataset_id != self.original.identities.family()
                || identity.lineage_id != history.lineage_id
        }) {
            return Err(super::managed_history::unavailable(
                "foreign captured publication",
            ));
        }
        let mut lineage = [0; 36];
        let lineage = history.lineage_id.hyphenated().encode_lower(&mut lineage);
        let mut digest = [0; 64];
        if let Some(identity) = identity {
            const HEX: &[u8; 16] = b"0123456789abcdef";
            for (at, byte) in identity.generation.as_bytes().iter().enumerate() {
                digest[2 * at] = HEX[(byte >> 4) as usize];
                digest[2 * at + 1] = HEX[(byte & 15) as usize];
            }
        }
        let digest = std::str::from_utf8(&digest).ok();
        let index = self
            .data
            .files
            .iter()
            .position(|file| {
                let Some(member) = file
                    .path
                    .strip_prefix("evidence-history/")
                    .and_then(|path| path.strip_prefix(&*lineage))
                    .and_then(|path| path.strip_prefix('/'))
                else {
                    return false;
                };
                if identity.is_none() {
                    return member == "coverage.json";
                }
                member
                    .strip_prefix("generations/")
                    .and_then(|name| name.strip_suffix(".json"))
                    == digest
            })
            .ok_or_else(|| {
                super::managed_history::unavailable(
                    "original captured publication member is missing",
                )
            })?;
        let mut reader = CapturedReader {
            capture: self,
            index,
            position: 0,
        };
        let extent = reader.extent();
        super::managed_history::read_bytes_from(&mut reader, extent, preparation)
    }
}

impl CapturedReader<'_> {
    pub(super) fn extent(&self) -> u64 {
        self.capture.data.files[self.index].len
    }
}

impl CapturedReader<'_> {
    // Only Read's compile-selected capability enters production. Keeping the
    // refusal here lets the same private branch be tested without Windows I/O.
    fn read_checked(
        &mut self,
        bytes: &mut [u8],
        independent_file_cursor: bool,
    ) -> std::io::Result<usize> {
        if bytes.is_empty() || self.position >= self.extent() {
            return Ok(0);
        }
        let file = &self.capture.data.files[self.index];
        if !independent_file_cursor && matches!(&file.src, FileSource::File(_)) {
            return Err(std::io::Error::new(
                std::io::ErrorKind::Unsupported,
                "this platform has no independent captured-file cursor backend",
            ));
        }
        let count = file.read_at(self.position, bytes)?;
        self.position += count as u64;
        Ok(count)
    }
}

impl std::io::Read for CapturedReader<'_> {
    fn read(&mut self, bytes: &mut [u8]) -> std::io::Result<usize> {
        self.read_checked(bytes, cfg!(unix))
    }
}

impl std::io::Seek for CapturedReader<'_> {
    fn seek(&mut self, request: std::io::SeekFrom) -> std::io::Result<u64> {
        let position = match request {
            std::io::SeekFrom::Start(position) => Some(position),
            std::io::SeekFrom::Current(offset) => self.position.checked_add_signed(offset),
            std::io::SeekFrom::End(offset) => self.extent().checked_add_signed(offset),
        }
        .filter(|position| *position <= self.extent())
        .ok_or_else(|| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "seek outside the captured primary extent",
            )
        })?;
        self.position = position;
        Ok(position)
    }
}

#[allow(
    dead_code,
    reason = "private item 761 import machinery that stays unwired until item 650 consumers agree"
)]
struct CapturedOriginal {
    identities: super::configuration_history::OriginalIdentities,
    binding: crate::history::ConfigurationBinding,
    history: super::managed_history::Root,
    head: CommitInfo,
    native: Option<CapturedBasis>,
    origin: Option<(u64, crate::history::ConfigurationBinding)>,
    pins: Option<super::managed_history::Bytes>,
    table: Option<super::managed_history::Bytes>,
    imports: Option<super::configuration_history::OriginalSources>,
    /// `managed.json` and each owner's settings file as the selected registry
    /// encodes them, the bytes their captured descriptors were verified against.
    registry: Vec<(&'static str, u64, crate::evidence::Sha256Digest)>,
    _held: crate::access::Reservation,
}

/// Facts from the actual selected Generation under its writer, or the real
/// completed materialized Builder output. Never inferred from captured JSON.
#[allow(
    dead_code,
    reason = "private item 761 import machinery that stays unwired until item 650 consumers agree"
)]
#[derive(Clone, Copy, Debug)]
struct CapturedBasis {
    base: CommitInfo,
    physical_owner: uuid::Uuid,
    dvocab_len: u64,
    fold_legacy: bool,
    metadata: crate::evidence::Sha256Digest,
}

/// Fixed-size streaming semantic fingerprint. It binds every known IndexMeta
/// field without cloning its strings/maps or materializing its serialized form.
struct MetadataDigest<'a> {
    hash: sha2::Sha256,
    written: usize,
    preparation: &'a crate::access::Preparation,
    failure: Option<Error>,
}

impl Write for MetadataDigest<'_> {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        use sha2::Digest;
        let result = (|| {
            self.preparation.check()?;
            let next = self
                .written
                .checked_add(bytes.len())
                .filter(|n| *n <= super::managed_history::MAX_BYTES)
                .ok_or_else(|| self.preparation.exhausted())?;
            for chunk in bytes.chunks(64 << 10) {
                self.preparation.check()?;
                self.hash.update(chunk);
            }
            self.written = next;
            Ok(())
        })();
        if let Err(error) = result {
            self.failure.get_or_insert(error);
            return Err(std::io::Error::other(
                "actual index metadata fingerprint refused",
            ));
        }
        Ok(bytes.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

fn metadata_fingerprint(
    meta: &crate::builder::IndexMeta,
    preparation: &crate::access::Preparation,
) -> Result<crate::evidence::Sha256Digest> {
    let _workspace =
        preparation.reserve(std::mem::size_of::<MetadataDigest<'_>>() as u64 + 1024)?;
    metadata_fingerprint_admitted(meta, preparation)
}

// Both call sites already hold the fixed workspace, including the decoder that
// lends all remaining request room to its existing controlled ColdBudget.
fn metadata_fingerprint_admitted(
    meta: &crate::builder::IndexMeta,
    preparation: &crate::access::Preparation,
) -> Result<crate::evidence::Sha256Digest> {
    use sha2::Digest;
    preparation.check()?;
    let mut writer = MetadataDigest {
        hash: sha2::Sha256::new(),
        written: 0,
        preparation,
        failure: None,
    };
    let result = serde_json::to_writer(&mut writer, meta);
    if let Some(error) = writer.failure {
        return Err(error);
    }
    result.map_err(|_| {
        super::managed_history::unavailable("actual index metadata does not encode")
    })?;
    preparation.check()?;
    Ok(crate::evidence::Sha256Digest::from_digest_bytes(
        writer.hash.finalize().into(),
    ))
}

impl CapturedOriginal {
    fn under_writer(
        store: &Store,
        writer: &super::WriterGuard<'_>,
        state: &Option<Arc<crate::managed::ManagedState>>,
        preparation: &crate::access::Preparation,
    ) -> Result<Self> {
        let header = std::mem::size_of::<Self>()
            .checked_add(std::mem::size_of::<ConfigurationCapture>())
            .ok_or_else(|| preparation.exhausted())?;
        let held = preparation.reserve(header as u64)?;
        let identities = super::configuration_history::OriginalIdentities::capture(
            store,
            writer,
            state,
            preparation,
        )?;
        let binding = store.configuration_binding_at(writer.head.seq, preparation)?;
        let history = match state.as_deref() {
            Some(crate::managed::ManagedState::Installed(installed)) => {
                installed.configuration_history.clone()
            }
            _ => None,
        }
        .ok_or_else(|| {
            super::managed_history::unavailable("original capture has no published root")
        })?;

        // Family table precedes this logical Store's history mutex in the
        // existing writer/table/history lock order.
        let options = crate::guard::WriteOptions {
            cancel: Some(preparation.control().cancel.flag()),
            deadline: preparation.control().deadline,
            ..Default::default()
        };
        let set = store.try_branch_set(&options)?.ok_or_else(|| {
            super::managed_history::unavailable("original family branch set is unavailable")
        })?;
        let table = set.original_table(preparation)?;
        let imports = super::configuration_history::OriginalSources::capture(
            store,
            writer,
            state,
            preparation,
        )?;
        let snapshot = store.snapshot();
        let (native, pins) = if let Some(history) = &store.history {
            let history = preparation.lock(history)?;
            let root = store.root.as_ref().ok_or_else(|| {
                super::managed_history::unavailable("persistent history has no Store root")
            })?;
            let pins = super::configuration_history::original_history_document(root, preparation)?;
            crate::history::capture::validate_persistent(
                pins.as_ref().map(|bytes| bytes.value.as_slice()),
                store.dataset_id,
                &history,
                preparation,
            )?;
            let native = if snapshot.generation.linked().is_none() {
                let generation = crate::commit::generation_number(&snapshot.generation.name);
                let entry = history
                    .gens
                    .get(&generation)
                    .filter(|entry| entry.name == snapshot.generation.name)
                    .ok_or_else(|| {
                        super::managed_history::unavailable("captured physical base is absent")
                    })?;
                Some(CapturedBasis {
                    base: entry.base,
                    physical_owner: store.dataset_id,
                    dvocab_len: snapshot.dvocab_len,
                    fold_legacy: entry.fold_legacy,
                    metadata: metadata_fingerprint(&snapshot.generation.meta, preparation)?,
                })
            } else {
                None
            };
            (native, pins)
        } else if let Some(history) = &store.mem_history {
            let history = preparation.lock(history)?;
            let pins =
                crate::history::capture::encode_memory(store.dataset_id, &history, preparation)?;
            (None, Some(pins))
        } else {
            return Err(super::managed_history::unavailable(
                "original capture has no actual pin history",
            ));
        };
        let origin = store
            .branching
            .ident
            .as_ref()
            .and_then(|ident| {
                ident
                    .configuration_origin
                    .as_ref()
                    .map(|origin| (ident.from.seq, origin))
            })
            .map(|(seq, origin)| {
                origin.validate(identities.family(), seq)?;
                Ok::<_, Error>((seq, origin.binding))
            })
            .transpose()?;
        let Some(crate::managed::ManagedState::Installed(installed)) = state.as_deref() else {
            return Err(super::managed_history::unavailable(
                "original capture has no installed registry",
            ));
        };
        let _registry = preparation.reserve(
            ((crate::managed::OwnerKind::ALL.len() + 1)
                * std::mem::size_of::<(&'static str, u64, crate::evidence::Sha256Digest)>())
                as u64,
        )?;
        let mut registry = Vec::with_capacity(crate::managed::OwnerKind::ALL.len() + 1);
        let (length, digest) = installed.file_pin();
        registry.push((
            crate::managed::FILE,
            length,
            crate::evidence::Sha256Digest::from_digest_bytes(digest),
        ));
        for (owner, bytes) in &installed.settings {
            preparation.check()?;
            registry.push((
                owner.settings_file(),
                bytes.len() as u64,
                crate::evidence::Sha256Digest::of_bytes(bytes, preparation.control())?,
            ));
        }
        if !store.same_managed_state(state) {
            return Err(Error::Conflict(
                "configuration changed while selecting original capture metadata".into(),
            ));
        }
        preparation.check()?;
        Ok(Self {
            identities,
            binding,
            history,
            head: writer.head,
            native,
            origin,
            pins,
            table,
            imports,
            registry,
            _held: held,
        })
    }
}

impl BackupCapture {
    /// The captured file at `path` (`gen-0001/wal.log`, `CURRENT`, …).
    pub fn file(&self, path: &str) -> Option<&CapturedFile> {
        self.files.iter().find(|f| f.path == path)
    }

    /// Write every captured file (up to its captured length) into the directory `dir`,
    /// which must not exist: the result is the database as it was right after
    /// `commit`, which opens with head `commit.seq`. For tests and local copies; files
    /// are synced, and `dir` itself is created last by rename.
    pub fn write_to(&self, dir: &Path) -> Result<()> {
        self.write_to_with_sync_hook(dir, &mut |_| Ok(()))
    }

    fn write_to_with_sync_hook(
        &self,
        dir: &Path,
        after_directory_sync: &mut dyn FnMut(&Path) -> Result<()>,
    ) -> Result<()> {
        if dir.exists() {
            return Err(Error::Invalid(format!("{} exists", dir.display())));
        }
        let parent = dir.parent().unwrap_or(Path::new("."));
        let tmp = tempfile::Builder::new()
            .prefix(".capture-")
            .tempdir_in(parent)?;
        let mut directories = std::collections::BTreeSet::new();
        directories.insert(tmp.path().join(&self.generation));
        let mut buf = vec![0u8; 1 << 20];
        for f in &self.files {
            let path = tmp.path().join(&f.path);
            if let Some(p) = path.parent() {
                std::fs::create_dir_all(p)?;
                let mut ancestor = Some(p);
                while let Some(directory) = ancestor.filter(|directory| {
                    directory.starts_with(tmp.path()) && *directory != tmp.path()
                }) {
                    directories.insert(directory.to_path_buf());
                    ancestor = directory.parent();
                }
            }
            let mut out = std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&path)?;
            let mut off = 0;
            while off < f.len {
                let n = buf.len().min((f.len - off) as usize);
                f.read_exact_at(off, &mut buf[..n])?;
                out.write_all(&buf[..n])?;
                off += n as u64;
            }
            out.sync_all()?;
        }
        // synced with their directories, as the other files are
        super::definitions::write_all(tmp.path(), &self.definitions, None)?;
        if self.next_ordinal > 1 {
            crate::commit::reserve_branch_ordinals(
                tmp.path(),
                self.dataset_id,
                self.next_ordinal as u64,
            )?;
        }
        let required = self
            .files
            .iter()
            .any(|file| file.path.starts_with("evidence-history/"));
        crate::check::configuration_history(tmp.path(), required, &crate::task::Control::none())?;
        // A synced file is not sufficient to persist its name in each newly
        // created parent. Persist descendants before their parents, then publish.
        for directory in directories.iter().rev() {
            super::sync_dir(directory)?;
            after_directory_sync(directory)?;
        }
        super::sync_dir(tmp.path())?;
        after_directory_sync(tmp.path())?;
        std::fs::rename(tmp.keep(), dir)?;
        super::sync_dir(parent)?;
        Ok(())
    }
}

/// The name prefix of the temporary directory of an in-memory capture
/// ([`Store::memory_backup_capture`]); a server removes leftovers at start.
pub const MEMORY_CAPTURE_PREFIX: &str = "memory-backup-";

/// Options of [`Store::memory_backup_capture`].
#[derive(Clone, Default)]
pub struct MemoryCaptureOptions {
    /// where the temporary database is built (created if absent): a fresh
    /// `memory-backup-*` directory inside it, removed with the capture's lease
    pub tmp_dir: std::path::PathBuf,
    /// free space to keep on the file system of `tmp_dir` while building
    pub min_free_disk_bytes: Option<u64>,
    /// set to `true` to cancel the build
    pub cancel: Option<Arc<AtomicBool>>,
    /// Stop admission and materialization after this deadline.
    pub deadline: Option<Instant>,
    /// Refuse capture while another writer holds the store.
    pub no_wait: bool,
    /// progress of the build: (fraction done in `[0, 1]`, message)
    pub progress: Option<super::ProgressFn>,
}

impl std::fmt::Debug for MemoryCaptureOptions {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MemoryCaptureOptions")
            .field("tmp_dir", &self.tmp_dir)
            .field("min_free_disk_bytes", &self.min_free_disk_bytes)
            .finish_non_exhaustive()
    }
}

/// A test hook run at a named point of the store's code (`Store::set_failpoint`).
#[cfg(any(test, feature = "failpoints"))]
pub type Failpoint = Arc<dyn Fn(&Store) + Send + Sync>;

/// Files of a generation directory that grow (read up to their captured length).
const APPEND_FILES: [&str; 2] = ["wal.log", "delta.vocab"];

/// Meta files at the root, besides `CURRENT`, `dataset.json` and `prefixes.json`, that a
/// backup holds when present.
const OPTIONAL_META: [&str; 14] = [
    crate::annotations::FILE,
    super::REMOVED_PREFIXES_FILE,
    "text.json",
    "geo.json",
    "vector.json",
    "origin.json",
    crate::guard::config::CONFIG_FILE,
    crate::guard::config::SHACL_SHAPES_FILE,
    crate::guard::config::SHEX_SCHEMA_SHEXC_FILE,
    crate::guard::config::SHEX_SCHEMA_SHEXJ_FILE,
    crate::stored::FILE,
    crate::stored_mappings::FILE,
    crate::algo::catalog::FILE,
    // the GraphQL configuration (crate sparkles-graphql)
    "graphql.json",
];

impl Store {
    /// Capture the current commit for a backup labelled `label` (the backup name, shown
    /// as `backup:<label>` in the history status while the lease is held).
    ///
    /// Contract:
    /// * persistent stores only; in-memory stores fail with `Error::Unsupported`;
    /// * the writer mutex is held only to read lengths, flush the delta vocabulary and
    ///   the commit catalog, clone the prefixes and the managed definitions
    ///   ([`super::definitions`]) and add the lease (a few system calls;
    ///   the files are opened after it); a poisoned store fails with `Error::Poisoned`;
    /// * a commit catalog that cannot be flushed completely fails with
    ///   `Error::Conflict("catalog-lagging: …")` (retryable, `503 catalog-lagging`);
    /// * `CURRENT` read after the lock must name `generation`, else the capture is
    ///   retried once (a compaction raced it), then fails with `Error::Conflict`;
    /// * the WAL prefix ends exactly at the end of `commit`'s record and the
    ///   `commits.bin` prefix at its catalog record, so a restored store's head is
    ///   `commit.seq`.
    pub fn backup_capture(&self, label: &str) -> Result<BackupCapture> {
        self.backup_capture_with(label, &Default::default())
    }

    /// A capture whose writer admission follows cancellation, deadline and no-wait.
    pub fn backup_capture_with(
        &self,
        label: &str,
        control: &crate::guard::WriteOptions,
    ) -> Result<BackupCapture> {
        if self.root.is_none() || self.history.is_none() {
            return Err(Error::unsupported(
                "backups of in-memory datasets are not supported",
            ));
        }
        // a backup copies the dataset's own store; a branch's linked generation would
        // copy without the upstream files it reads
        if self.snapshot().generation.linked().is_some() {
            return Err(Error::unsupported(
                "a linked branch needs a materialized backup capture",
            ));
        }
        if let Some(c) = self.capture_once(label, control)? {
            return Ok(c);
        }
        // a compaction or bulk commit switched CURRENT right after the lock: once more
        match self.capture_once(label, control)? {
            Some(c) => Ok(c),
            None => Err(Error::Conflict(
                "the database's generation changed twice during the backup capture; retry".into(),
            )),
        }
    }

    /// [`backup_capture`](Self::backup_capture) of a store opened only for the backup:
    /// the store stays open (its directory locked, so no other process writes it) until
    /// the capture's lease is dropped.
    pub fn into_backup_capture(self, label: &str) -> Result<BackupCapture> {
        let mut c = self.backup_capture(label)?;
        let release = c.lease.release.take();
        c.lease.release = Some(Box::new(move || {
            if let Some(r) = release {
                r();
            }
            drop(self);
        }));
        Ok(c)
    }

    /// Capture the current commit of an in-memory store for a backup labelled `label`.
    ///
    /// An in-memory store has no files to read, so the capture builds them: the
    /// snapshot of the head commit is written as a new generation `gen-0001` (the bulk
    /// builder, as a compaction does) in a fresh `memory-backup-*` directory inside
    /// `o.tmp_dir`, next to an empty WAL, a commit catalog that holds only the head
    /// commit, `dataset.json` with this store's dataset id, `prefixes.json`, and the
    /// configurations of the full-text and spatial indexes. The result is the database
    /// a persistent store with the same content and head would be, minus its earlier
    /// commit records, so it opens with head `commit.seq`.
    ///
    /// Contract:
    /// * in-memory stores only; a persistent store fails with `Error::Invalid` (use
    ///   [`backup_capture`](Self::backup_capture));
    /// * the writer mutex is held only to take the snapshot, so writes continue while
    ///   the generation is built; the snapshot keeps the captured state in memory until
    ///   the build ends;
    /// * the build needs about as much disk space as a compacted generation of the
    ///   dataset, and stops with `507` once the file system of `o.tmp_dir` would keep
    ///   less than `o.min_free_disk_bytes` free;
    /// * `o.cancel` is checked every 65536 quads (`Error::Cancelled`);
    /// * the temporary directory is removed when the capture's lease is dropped, and on
    ///   any error.
    pub fn memory_backup_capture(
        &self,
        label: &str,
        o: &MemoryCaptureOptions,
    ) -> Result<BackupCapture> {
        if self.root.is_some() {
            return Err(Error::Invalid(
                "a persistent store is captured with backup_capture".into(),
            ));
        }
        self.materialized_backup_capture(label, o)
    }

    /// Build standalone files for any store without changing its source generation.
    pub fn materialized_backup_capture(
        &self,
        label: &str,
        o: &MemoryCaptureOptions,
    ) -> Result<BackupCapture> {
        self.materialized_capture_prepared(label, o, None)
            .map(|(capture, _)| capture)
    }

    #[allow(
        dead_code,
        reason = "private item 761 import machinery that stays unwired until item 650 consumers agree"
    )]
    pub(super) fn materialized_configuration_capture(
        &self,
        label: &str,
        o: &MemoryCaptureOptions,
        preparation: &crate::access::Preparation,
    ) -> Result<ConfigurationCapture> {
        preparation.check()?;
        let (data, original) = self.materialized_capture_prepared(label, o, Some(preparation))?;
        let original =
            original.ok_or_else(|| Error::Corrupt("original capture metadata missing".into()))?;
        preparation.check()?;
        Ok(ConfigurationCapture { data, original })
    }

    fn materialized_capture_prepared(
        &self,
        label: &str,
        o: &MemoryCaptureOptions,
        preparation: Option<&crate::access::Preparation>,
    ) -> Result<(BackupCapture, Option<CapturedOriginal>)> {
        let check_preparation = || preparation.map_or(Ok(()), |preparation| preparation.check());
        check_preparation()?;
        let report = |f: f32, msg: &str| {
            if let Some(p) = &o.progress {
                p(f, msg);
            }
        };
        let control = crate::guard::WriteOptions {
            cancel: o.cancel.clone(),
            deadline: o.deadline,
            no_wait: o.no_wait,
            ..Default::default()
        };
        control.check()?;
        let t0 = Instant::now();
        let (
            snap,
            next_bnode,
            head,
            prefixes,
            definitions,
            managed,
            managed_state,
            binding,
            mut original,
        ) = {
            let w = self.lock_writer(&control)?;
            check_preparation()?;
            if w.poisoned {
                return Err(Error::Poisoned);
            }
            let state = self.managed_state_shared();
            let history = matches!(state.as_deref(), Some(crate::managed::ManagedState::Installed(installed)) if installed.configuration_history.is_some());
            let managed = if history {
                Vec::new()
            } else {
                self.managed_files_at(state.as_deref())?
            };
            let binding = if history {
                let preparation = super::managed_history::administrative_preparation_with(
                    crate::task::Control {
                        cancel: control
                            .cancel
                            .clone()
                            .map(crate::task::Cancel::from_flag)
                            .unwrap_or_default(),
                        deadline: control.deadline,
                        ..Default::default()
                    },
                )?;
                self.configuration_binding_at(w.head.seq, &preparation)?
            } else {
                crate::history::ConfigurationBinding::LegacyUnbound
            };
            let original = preparation
                .map(|preparation| CapturedOriginal::under_writer(self, &w, &state, preparation))
                .transpose()?;
            if original
                .as_ref()
                .is_some_and(|original| original.binding != binding)
            {
                return Err(Error::Conflict(
                    "configuration changed while capturing original binding".into(),
                ));
            }
            (
                self.snapshot().without_cache_fill(),
                w.next_bnode,
                w.head,
                self.prefixes.lock().clone(),
                self.capture_definitions()?,
                managed,
                state,
                binding,
                original,
            )
        };
        let lock_hold = t0.elapsed();
        check_preparation()?;
        std::fs::create_dir_all(&o.tmp_dir)?;
        let tmp = tempfile::Builder::new()
            .prefix(MEMORY_CAPTURE_PREFIX)
            .tempdir_in(&o.tmp_dir)?;
        let dir = tmp.path();
        // The captured registry, not the live one, decides the versions this
        // capture writes: in-memory activation can race the build below.
        let activated = super::managed_history::activated(
            managed_state.as_deref(),
            self.managed_root().as_deref(),
        );
        let history_files = match managed_state.as_deref() {
            Some(crate::managed::ManagedState::Installed(installed))
                if installed.configuration_history.is_some() =>
            {
                match self.managed_root() {
                    Some(root) => super::managed_history::capture::files(
                        &root,
                        self.owner_dataset_id(),
                        installed,
                        &control,
                    )?,
                    None => super::managed_history::capture::memory_files(
                        dir,
                        self.owner_dataset_id(),
                        installed,
                        &control,
                    )?,
                }
            }
            _ => Vec::new(),
        };
        check_preparation()?;
        if !self.same_managed_state(&managed_state) {
            return Err(Error::Conflict(
                "configuration changed while establishing a materialized capture; retry".into(),
            ));
        }
        let generation = "gen-0001";
        let gdir = dir.join(generation);
        let total = snap.len().max(1);
        let mut seen = 0u64;
        report(0.0, "writing quads");
        check_preparation()?;
        let meta = self.build_from_snapshot(
            &gdir,
            &snap,
            None,
            next_bnode,
            o.min_free_disk_bytes,
            prefixes.clone(),
            |_| {
                seen += 1;
                if seen.is_multiple_of(65_536) {
                    control.check()?;
                    check_preparation()?;
                    report(0.7 * seen as f32 / total as f32, "writing quads");
                    check_preparation()?;
                }
                Ok(true)
            },
            || report(0.7, "building indexes"),
        )?;
        drop(snap);
        control.check()?;
        check_preparation()?;
        // the head commit, held by the base of generation 1
        let commit = CommitInfo {
            generation: 1,
            quads: meta.quads,
            ..head
        };
        if let (Some(original), Some(preparation)) = (original.as_mut(), preparation) {
            original.native = Some(CapturedBasis {
                base: commit,
                physical_owner: self.dataset_id,
                dvocab_len: 0,
                fold_legacy: false,
                metadata: metadata_fingerprint(&meta, preparation)?,
            });
        }
        super::write_synced(
            &gdir.join("commit.json"),
            &crate::commit::gen_commit_bytes_for(
                self.dataset_id,
                "memory",
                &commit,
                activated,
                binding,
            ),
        )?;
        super::wal::create(&gdir.join("wal.log"), super::wal::version_for(activated))?;
        super::sync_dir(&gdir)?;
        crate::commit::Catalog::create(&dir.join("commits.bin"), self.dataset_id, commit)?;
        let mut files = Vec::new();
        let mut names: Vec<String> = Vec::new();
        for e in std::fs::read_dir(&gdir)? {
            check_preparation()?;
            let e = e?;
            let name = e.file_name().to_string_lossy().into_owned();
            if e.file_type()?.is_file() && !name.ends_with(".tmp") {
                names.push(name);
            }
        }
        names.sort();
        for name in names {
            check_preparation()?;
            let f = File::open(gdir.join(&name))?;
            let kind = if APPEND_FILES.contains(&name.as_str()) {
                FileKind::Append
            } else {
                FileKind::Immutable
            };
            files.push(CapturedFile {
                path: format!("{generation}/{name}"),
                kind,
                len: f.metadata()?.len(),
                src: FileSource::File(f),
            });
        }
        let catalog = File::open(dir.join("commits.bin"))?;
        files.push(CapturedFile {
            path: "commits.bin".into(),
            kind: FileKind::Append,
            len: catalog.metadata()?.len(),
            src: FileSource::File(catalog),
        });
        let meta_file = |path: &str, bytes: Vec<u8>| CapturedFile {
            path: path.to_string(),
            kind: FileKind::Meta,
            len: bytes.len() as u64,
            src: FileSource::Bytes(bytes.into()),
        };
        files.push(meta_file("CURRENT", generation.as_bytes().to_vec()));
        let mut dataset =
            crate::commit::dataset_file_bytes(self.dataset_id, "memory", commit.timestamp_ms);
        if !managed.is_empty() {
            dataset = crate::commit::with_reader(&dataset, crate::managed::READER)?;
        }
        // the capture holds version 2 files of an activated store, which need its reader
        if activated {
            dataset = crate::commit::with_reader(&dataset, super::managed_history::READER)?;
        }
        files.push(meta_file("dataset.json", dataset));
        for (name, bytes) in managed {
            files.push(meta_file(name, bytes));
        }
        files.extend(history_files);
        files.push(meta_file(
            "prefixes.json",
            serde_json::to_vec_pretty(&prefixes).unwrap(),
        ));
        for (name, bytes) in self.index_config_files()? {
            files.push(meta_file(name, bytes));
        }
        if let Some(root) = &self.root {
            for name in OPTIONAL_META {
                check_preparation()?;
                if files.iter().any(|f| f.path == name) {
                    continue;
                }
                match std::fs::read(root.join(name)) {
                    Ok(bytes) => files.push(meta_file(name, bytes)),
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                    Err(e) => return Err(e.into()),
                }
            }
        }
        report(1.0, "built");
        check_preparation()?;
        Ok((
            BackupCapture {
                dataset_id: self.dataset_id,
                commit,
                generation: generation.to_string(),
                index_format: meta.format_version,
                index_features: meta.features.clone(),
                files,
                lock_hold,
                lease: LeaseGuard {
                    generation: 1,
                    label: label.to_string(),
                    // the open handles keep reading after the directory is gone on Unix,
                    // but the upload has ended by the time the lease is dropped
                    release: Some(Box::new(move || drop(tmp))),
                },
                in_memory: self.root.is_none(),
                materialized: true,
                branch: self.backup_branch(),
                next_ordinal: self.backup_next_ordinal(),
                branches_omitted: self.backup_omitted(),
                definitions,
            },
            original,
        ))
    }

    fn backup_next_ordinal(&self) -> u32 {
        self.branch_set().map_or_else(
            || self.branching.next_ordinal.load(Ordering::Relaxed).max(1) as u32,
            |set| set.next_ordinal() as u32,
        )
    }

    fn backup_branch(&self) -> Option<BackupBranch> {
        self.branching.ident.as_ref().map(|i| BackupBranch {
            dataset_id: i.dataset_id,
            id: self.dataset_id,
            name: self.branch_name(),
            next_ordinal: self.branching.next_ordinal.load(Ordering::Relaxed) as u32,
        })
    }

    fn backup_omitted(&self) -> u64 {
        self.branch_set().map_or(0, |set| set.table_len() as u64)
    }

    /// One capture attempt: `None` if `CURRENT` no longer names the captured generation
    /// once the lock is released.
    fn capture_once(
        &self,
        label: &str,
        control: &crate::guard::WriteOptions,
    ) -> Result<Option<BackupCapture>> {
        self.capture_once_prepared(label, control, None)
            .map(|capture| capture.map(|(data, _)| data))
    }

    #[allow(
        dead_code,
        reason = "private item 761 import machinery that stays unwired until item 650 consumers agree"
    )]
    pub(super) fn configuration_capture(
        &self,
        label: &str,
        control: &crate::guard::WriteOptions,
        preparation: &crate::access::Preparation,
    ) -> Result<ConfigurationCapture> {
        preparation.check()?;
        if self.snapshot().generation.linked().is_some() {
            return Err(Error::unsupported(
                "a linked branch needs a materialized configuration capture",
            ));
        }
        for _ in 0..2 {
            if let Some((data, original)) =
                self.capture_once_prepared(label, control, Some(preparation))?
            {
                let original = original
                    .ok_or_else(|| Error::Corrupt("original capture metadata missing".into()))?;
                preparation.check()?;
                return Ok(ConfigurationCapture { data, original });
            }
        }
        Err(Error::Conflict(
            "the generation changed twice during original capture; retry".into(),
        ))
    }

    fn capture_once_prepared(
        &self,
        label: &str,
        control: &crate::guard::WriteOptions,
        preparation: Option<&crate::access::Preparation>,
    ) -> Result<Option<(BackupCapture, Option<CapturedOriginal>)>> {
        let check_preparation = || preparation.map_or(Ok(()), |preparation| preparation.check());
        check_preparation()?;
        let (Some(root), Some(hist)) = (&self.root, &self.history) else {
            return Err(Error::unsupported(
                "backups of in-memory datasets are not supported",
            ));
        };
        let w = self.lock_writer(control)?;
        check_preparation()?;
        let t0 = Instant::now();
        if w.poisoned {
            return Err(Error::Poisoned);
        }
        // Under the lock only lengths, the prefixes and the lease: no write transaction is
        // open, and every commit flushed and synced its WAL records, so the WAL ends at the
        // head's commit record. The lease keeps the generation directory until dropped, so
        // its files can be opened after the lock (commits.bin is only ever appended to
        // while the store is open).
        let snap = self.snapshot();
        let head = w.head;
        let generation = snap.generation.name.clone();
        let gen_no = crate::commit::generation_number(&generation);
        // the end of the last commit: the file may hold preallocated zeros after it
        let wal_len = match &w.wal {
            Some(_) => w.wal_len,
            None => 0,
        };
        // the vocabulary's own handle: a first write can rewrite a legacy file under a
        // new inode after the lock, and the captured length is of this one
        let mut dvocab = snap.generation.dvocab.capture()?;
        let catalog_len = self.catalog.lock().flushed_len()?;
        // opened under the lock: pruning replaces the file, and holds the lock to do it
        let catalog = File::open(root.join("commits.bin"))?;
        let prefixes = self.prefixes.lock().clone();
        // the definitions current at the head, before the lock lets either change
        let definitions = self.capture_definitions()?;
        // A branch writer does not exclude main configure. Retain one family
        // state across the RDF capture and every configuration descriptor open.
        let managed_state = self.managed_state_shared();
        let history_installed = managed_state.as_deref().and_then(|state| match state {
            crate::managed::ManagedState::Installed(installed)
                if installed.configuration_history.is_some() =>
            {
                Some(installed)
            }
            _ => None,
        });
        let managed = if history_installed.is_some() {
            Vec::new()
        } else {
            self.managed_files_at(managed_state.as_deref())?
        };
        let original = preparation
            .map(|preparation| {
                CapturedOriginal::under_writer(self, &w, &managed_state, preparation)
            })
            .transpose()?;
        self.failpoint("backup-capture-locked");
        check_preparation()?;
        let lease_id = hist.lock().lease(gen_no, label);
        drop(w);
        let lock_hold = t0.elapsed();
        // from here on, dropping the guard releases the lease
        let collector = self.collector();
        let lease = LeaseGuard {
            generation: gen_no,
            label: label.to_string(),
            release: Some(Box::new(move || {
                if let Some(c) = collector {
                    c.release(lease_id);
                }
            })),
        };
        let gdir = root.join(&generation);
        let mut gen_files: Vec<(String, File, u64)> = Vec::new();
        for e in std::fs::read_dir(&gdir)? {
            check_preparation()?;
            let e = e?;
            let name = e.file_name().to_string_lossy().into_owned();
            if !e.file_type()?.is_file() || name.ends_with(".tmp") {
                continue;
            }
            if name == "delta.vocab"
                && let Some((f, len)) = dvocab.take()
            {
                gen_files.push((name, f, len));
                continue;
            }
            let f = File::open(e.path())?;
            let len = match name.as_str() {
                "wal.log" => wal_len,
                "delta.vocab" => 0,
                // immutable: its whole length
                _ => f.metadata()?.len(),
            };
            gen_files.push((name, f, len));
        }
        self.failpoint("backup-capture-unlocked");
        check_preparation()?;

        let current = std::fs::read(root.join("CURRENT"))?;
        if String::from_utf8_lossy(&current).trim() != generation {
            return Ok(None);
        }
        if !self.same_managed_state(&managed_state) {
            return Ok(None);
        }
        let history_files = match history_installed {
            Some(installed) => {
                let family_root = self.managed_root().ok_or_else(|| {
                    Error::Corrupt("captured configuration has no family root".into())
                })?;
                let captured = super::managed_history::capture::files(
                    &family_root,
                    self.owner_dataset_id(),
                    installed,
                    control,
                );
                control.check()?;
                check_preparation()?;
                // Configure may replace settings names while descriptors open.
                // Retry an unstable selection before interpreting its I/O result.
                if !self.same_managed_state(&managed_state) {
                    return Ok(None);
                }
                captured?
            }
            None => Vec::new(),
        };
        let mut files = Vec::with_capacity(gen_files.len() + 8);
        gen_files.sort_by(|a, b| a.0.cmp(&b.0));
        for (name, f, len) in gen_files {
            let kind = if APPEND_FILES.contains(&name.as_str()) {
                FileKind::Append
            } else {
                FileKind::Immutable
            };
            files.push(CapturedFile {
                path: format!("{generation}/{name}"),
                kind,
                len,
                src: FileSource::File(f),
            });
        }
        let wal = files.iter().find(|f| f.path.ends_with("/wal.log"));
        check_wal_prefix(wal, &gdir, &head)?;
        let catalog = CapturedFile {
            path: "commits.bin".into(),
            kind: FileKind::Append,
            len: catalog_len,
            src: FileSource::File(catalog),
        };
        check_catalog_prefix(&catalog, &head)?;
        files.push(catalog);
        let meta = |path: &str, bytes: Vec<u8>| CapturedFile {
            path: path.to_string(),
            kind: FileKind::Meta,
            len: bytes.len() as u64,
            src: FileSource::Bytes(bytes.into()),
        };
        files.push(meta("CURRENT", current));
        let dataset = std::fs::read(root.join("dataset.json"))?;
        match crate::commit::dataset_id_of(&dataset)? {
            id if id == self.dataset_id => {}
            id => {
                return Err(Error::Corrupt(format!(
                    "dataset.json names dataset {id}, not {}",
                    self.dataset_id
                )));
            }
        }
        let dataset = if managed.is_empty() && history_files.is_empty() {
            dataset
        } else if history_files.is_empty() {
            crate::commit::with_reader(&dataset, crate::managed::READER)?
        } else {
            // an activated store's files need the configuration-history reader
            crate::commit::with_reader(&dataset, super::managed_history::READER)?
        };
        files.push(meta("dataset.json", dataset));
        for (name, bytes) in managed {
            files.push(meta(name, bytes));
        }
        files.extend(history_files);
        files.push(meta(
            "prefixes.json",
            serde_json::to_vec_pretty(&prefixes).unwrap(),
        ));
        for name in OPTIONAL_META {
            check_preparation()?;
            match std::fs::read(root.join(name)) {
                Ok(b) => files.push(meta(name, b)),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => return Err(e.into()),
            }
        }
        control.check()?;
        check_preparation()?;
        if !self.same_managed_state(&managed_state) {
            return Ok(None);
        }
        Ok(Some((
            BackupCapture {
                dataset_id: self.dataset_id,
                commit: head,
                generation,
                index_format: snap.generation.meta.format_version,
                index_features: snap.generation.meta.features.clone(),
                files,
                lock_hold,
                lease,
                in_memory: false,
                materialized: false,
                branch: self.backup_branch(),
                next_ordinal: self.backup_next_ordinal(),
                branches_omitted: self.backup_omitted(),
                definitions,
            },
            original,
        )))
    }

    /// Best-effort history collection: removes generations nothing needs any more if the
    /// writer mutex is free right now (`try_lock`), and otherwise does nothing (the next
    /// collection point — a compaction, a bulk commit, a snapshot or retention change,
    /// or the server's hourly tick — does it). Called when a backup lease is dropped.
    pub fn try_collect_history(&self) {
        if let Some(c) = self.collector() {
            c.try_collect();
        }
    }

    /// Install (`Some`) or remove (`None`) the test hook run at failpoint `name`:
    /// `backup-capture-locked` (writer lock held: the hook must not write) or
    /// `backup-capture-unlocked` (right after the lock is released, before `CURRENT`
    /// is read), or `clone-captured` (a clone took its snapshot and released the lock,
    /// and has not copied anything yet). The views run `view-pass-applied` after a
    /// pass changed a circuit in memory, before any checkpoint, and
    /// `view-checkpoint-written` once a checkpoint is synced under its temporary name,
    /// before the rename. Both run with the view's state locked, so their hooks must
    /// not call the views.
    #[cfg(any(test, feature = "failpoints"))]
    #[doc(hidden)]
    pub fn set_failpoint(&self, name: &'static str, hook: Option<Failpoint>) {
        let mut f = self.failpoints.lock();
        match hook {
            Some(h) => {
                f.insert(name, h);
            }
            None => {
                f.remove(name);
            }
        }
    }

    /// Make appends to the commit catalog fail (`on`) or work again, as after a write
    /// error: the catalog lags the commits until an append succeeds.
    #[cfg(any(test, feature = "failpoints"))]
    #[doc(hidden)]
    pub fn fail_catalog_writes(&self, on: bool) {
        self.catalog.lock().fail_writes = on;
    }

    /// Run the test hook installed at failpoint `name`, if any. Typed writers outside
    /// this crate mark their own crash points with it. It does nothing in a build
    /// without failpoints.
    #[inline]
    #[doc(hidden)]
    pub fn failpoint(&self, name: &'static str) {
        #[cfg(any(test, feature = "failpoints"))]
        {
            let hook = self.failpoints.lock().get(name).cloned();
            if let Some(h) = hook {
                h(self);
            }
        }
        #[cfg(not(any(test, feature = "failpoints")))]
        let _ = name;
    }
}

/// The WAL prefix must end with the head's commit record, or be empty when the head is
/// the generation's base commit.
fn check_wal_prefix(wal: Option<&CapturedFile>, gdir: &Path, head: &CommitInfo) -> Result<()> {
    let len = wal.map_or(0, |f| f.len);
    let bad = |m: String| Err(Error::Corrupt(format!("{}: {m}", gdir.display())));
    // a log that holds only its header record has no commits either
    let header_only = len == super::WAL_REC as u64 && {
        let mut rec = [0u8; super::WAL_REC];
        wal.unwrap().read_exact_at(0, &mut rec)?;
        rec[0] == super::WAL_HEADER
    };
    if len == 0 || header_only {
        return match crate::commit::read_gen_commit(gdir)? {
            Some((_, base, _)) if base.seq == head.seq => Ok(()),
            _ => bad(format!(
                "empty WAL, but the head {} is not its base",
                head.seq
            )),
        };
    }
    let rec_len = super::WAL_REC as u64;
    if !len.is_multiple_of(rec_len) {
        return bad(format!("WAL length {len} is not a whole number of records"));
    }
    let mut rec = [0u8; super::WAL_REC];
    wal.unwrap().read_exact_at(len - rec_len, &mut rec)?;
    if rec[0] != super::WAL_COMMIT {
        return bad("the WAL does not end with a commit record".into());
    }
    // version-2 records carry their seq (legacy ones do not)
    if rec[26] == crate::commit::WAL_COMMIT_V2 {
        let seq = u64::from_le_bytes(rec[9..17].try_into().unwrap());
        if seq != head.seq {
            return bad(format!(
                "the WAL ends at commit {seq}, not the head {}",
                head.seq
            ));
        }
    }
    Ok(())
}

/// The captured catalog must end with the head's record.
fn check_catalog_prefix(f: &CapturedFile, head: &CommitInfo) -> Result<()> {
    let rec = crate::commit::REC as u64;
    let bad = |m: String| Err(Error::Corrupt(format!("commits.bin: {m}")));
    if f.len < 2 * rec || !f.len.is_multiple_of(rec) {
        return bad(format!(
            "length {} is not a header and whole records",
            f.len
        ));
    }
    let mut r = [0u8; crate::commit::REC];
    f.read_exact_at(f.len - rec, &mut r)?;
    match crate::commit::decode_record(&r) {
        Some(c) if c.seq == head.seq => Ok(()),
        Some(c) => bad(format!(
            "ends at commit {}, not the head {}",
            c.seq, head.seq
        )),
        None => bad("the last record is damaged".into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::history::{At, Hold};
    use crate::sparql::QueryOptions;
    use crate::sparql::update::update;
    use crate::store::StoreOptions;
    use std::collections::BTreeMap;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

    fn upd(s: &Store, u: &str) {
        update(s, u, &QueryOptions::default()).unwrap();
    }

    fn ins(s: &Store, i: u64) {
        upd(s, &format!("INSERT DATA {{ <urn:s{i}> <urn:p> \"v{i}\" }}"));
    }

    fn lines(b: Vec<u8>) -> Vec<String> {
        let mut v: Vec<String> = String::from_utf8(b)
            .unwrap()
            .lines()
            .map(str::to_string)
            .collect();
        v.sort();
        v
    }

    fn dump_at(s: &Store, seq: u64) -> Vec<String> {
        let mut out = Vec::new();
        s.dump_nquads_at(&At::Commit(seq), &mut out).unwrap();
        lines(out)
    }

    fn dump(s: &Store) -> Vec<String> {
        let mut out = Vec::new();
        s.dump_nquads(&mut out).unwrap();
        lines(out)
    }

    fn gens(root: &Path) -> Vec<String> {
        let mut v: Vec<String> = std::fs::read_dir(root)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .filter(|n| n.starts_with("gen-"))
            .collect();
        v.sort();
        v
    }

    fn leases(s: &Store) -> usize {
        s.history.as_ref().unwrap().lock().leases.len()
    }

    fn history_store(root: &Path) -> Store {
        drop(Store::open(root, StoreOptions::default()).unwrap());
        let preparation =
            crate::access::Preparation::new(crate::task::Control::none(), 64 << 20).unwrap();
        crate::migrate::activate_configuration_history(root, false, &preparation).unwrap();
        Store::open(root, StoreOptions::default()).unwrap()
    }

    #[derive(Debug, PartialEq, Eq)]
    struct CaptureParentEntry {
        kind: &'static str,
        #[cfg(unix)]
        unix_identity: (u64, u64, u32, u32, u32),
    }

    fn history_capture_parent_entries(
        parent: &Path,
    ) -> BTreeMap<std::ffi::OsString, CaptureParentEntry> {
        let entries = std::fs::read_dir(parent)
            .unwrap()
            .map(|entry| {
                let entry = entry.unwrap();
                let metadata = std::fs::symlink_metadata(entry.path()).unwrap();
                let file_type = metadata.file_type();
                let kind = if file_type.is_dir() {
                    "directory"
                } else if file_type.is_file() {
                    "regular file"
                } else if file_type.is_symlink() {
                    "symlink"
                } else {
                    "other"
                };
                #[cfg(unix)]
                let unix_identity = {
                    use std::os::unix::fs::MetadataExt;
                    (
                        metadata.dev(),
                        metadata.ino(),
                        metadata.uid(),
                        metadata.gid(),
                        metadata.mode(),
                    )
                };
                (
                    entry.file_name(),
                    CaptureParentEntry {
                        kind,
                        #[cfg(unix)]
                        unix_identity,
                    },
                )
            })
            .collect::<BTreeMap<_, _>>();
        assert_eq!(
            entries.keys().cloned().collect::<Vec<_>>(),
            vec![
                std::ffi::OsString::from(".migrate.lock-source"),
                std::ffi::OsString::from("source"),
            ],
            "capture parent entries: {entries:?}"
        );
        let source = &entries[std::ffi::OsStr::new("source")];
        let migration_lock = &entries[std::ffi::OsStr::new(".migrate.lock-source")];
        assert_eq!(source.kind, "directory", "{entries:?}");
        assert_eq!(migration_lock.kind, "regular file", "{entries:?}");
        #[cfg(unix)]
        {
            let owner = |entry: &CaptureParentEntry| {
                let (_, _, uid, gid, _) = entry.unix_identity;
                (uid, gid)
            };
            assert_eq!(owner(source), owner(migration_lock), "{entries:?}");
        }
        entries
    }

    fn capture_preparation() -> crate::access::Preparation {
        crate::access::Preparation::new(crate::task::Control::none(), 64 << 20).unwrap()
    }

    #[cfg(unix)]
    #[test]
    fn genuine_original_family_table_retains_bytes_across_live_rename() {
        let temporary = tempfile::tempdir().unwrap();
        let root = temporary.path().join("source");
        let store = history_store(&root);
        ins(&store, 1);
        store.create_branch("child", &Default::default()).unwrap();
        let child = store.branch("child").unwrap();
        let original = std::fs::read(root.join("branches.json")).unwrap();
        let preparation = capture_preparation();
        let before = preparation.room();
        let captured = store
            .configuration_capture("table", &Default::default(), &preparation)
            .unwrap();
        assert_eq!(captured.original.table.as_ref().unwrap().value, original);
        let scratch = temporary.path().join("scratch");
        let child_capture = child
            .materialized_configuration_capture(
                "table",
                &MemoryCaptureOptions {
                    tmp_dir: scratch.clone(),
                    ..Default::default()
                },
                &preparation,
            )
            .unwrap();
        assert_eq!(
            child_capture.original.table.as_ref().unwrap().value,
            original
        );
        assert_ne!(
            child_capture.original.identities.family(),
            child_capture.data.dataset_id
        );
        store.rename_branch("child", "renamed").unwrap();
        assert_ne!(std::fs::read(root.join("branches.json")).unwrap(), original);
        let source = captured
            .checked_primary(&preparation)
            .unwrap()
            .describe_native(&preparation)
            .unwrap();
        inspect_native_descriptor(&source, &preparation, |value| {
            assert_eq!(value["files"]["branches.json"]["bytes"], original.len());
            assert_eq!(
                value["files"]["branches.json"]["sha256"],
                crate::evidence::Sha256Digest::of_bytes(&original, preparation.control())
                    .unwrap()
                    .to_string()
            );
            assert_eq!(value["roots"].as_object().unwrap().len(), 1);
        });
        assert_eq!(
            child_capture.original.table.as_ref().unwrap().value,
            original
        );
        drop(child_capture);
        drop(source);
        assert_eq!(leases(&store), 0);
        assert_eq!(preparation.room(), before);
    }

    #[cfg(unix)]
    #[test]
    fn genuine_original_family_table_refuses_valid_live_substitutions() {
        let temporary = tempfile::tempdir().unwrap();
        let root = temporary.path().join("source");
        let store = history_store(&root);
        ins(&store, 1);
        store.create_branch("child", &Default::default()).unwrap();
        let original = std::fs::read(root.join("branches.json")).unwrap();
        let preparation = capture_preparation();
        let before = preparation.room();
        for field in [
            "fork",
            "protected",
            "note",
            "ordinal",
            "missing",
            "symlink",
            "version",
        ] {
            std::fs::remove_file(root.join("branches.json")).unwrap();
            if field == "symlink" {
                let other = temporary.path().join("valid-table.json");
                std::fs::write(&other, &original).unwrap();
                std::os::unix::fs::symlink(other, root.join("branches.json")).unwrap();
            } else if field != "missing" {
                let mut value: serde_json::Value = serde_json::from_slice(&original).unwrap();
                match field {
                    "fork" => {
                        value["branches"][0]["from"]["branchId"] =
                            uuid::Uuid::new_v4().to_string().into()
                    }
                    "protected" => value["branches"][0]["protected"] = true.into(),
                    "note" => value["branches"][0]["note"] = "substitute".into(),
                    "ordinal" => value["nextOrdinal"] = 77.into(),
                    "version" => value["format"] = 999.into(),
                    _ => unreachable!(),
                }
                std::fs::write(
                    root.join("branches.json"),
                    super::super::branching::canonical_test_table(value),
                )
                .unwrap();
                if field != "version" {
                    super::super::branching::read_branch_table(&root).unwrap();
                }
            }
            assert!(
                store
                    .configuration_capture("changed-table", &Default::default(), &preparation)
                    .is_err(),
                "{field}"
            );
            assert_eq!(leases(&store), 0);
            assert_eq!(preparation.room(), before);
            let _ = std::fs::remove_file(root.join("branches.json"));
            std::fs::write(root.join("branches.json"), &original).unwrap();
        }
        // Ordinary legacy whitespace and unknown extensions do not change the
        // full decoded semantic table and must remain accepted.
        let mut legacy: serde_json::Value = serde_json::from_slice(&original).unwrap();
        legacy["unknownLegacyExtension"] = true.into();
        let wire = serde_json::to_vec(&legacy).unwrap();
        std::fs::write(root.join("branches.json"), &wire).unwrap();
        let captured = store
            .configuration_capture("legacy-table", &Default::default(), &preparation)
            .unwrap();
        assert_eq!(captured.original.table.as_ref().unwrap().value, wire);
        drop(captured);
        assert_eq!(preparation.room(), before);
    }

    #[cfg(unix)]
    #[test]
    fn genuine_original_memory_family_table_keeps_family_separate_from_logical() {
        let temporary = tempfile::tempdir().unwrap();
        let store = Store::in_memory(StoreOptions::default());
        let preparation = capture_preparation();
        store
            .activate_memory_configuration_history(&preparation)
            .unwrap();
        ins(&store, 1);
        store.create_branch("child", &Default::default()).unwrap();
        let child = store.branch("child").unwrap();
        let before = preparation.room();
        let capture = child
            .materialized_configuration_capture(
                "memory-table",
                &MemoryCaptureOptions {
                    tmp_dir: temporary.path().join("scratch"),
                    ..Default::default()
                },
                &preparation,
            )
            .unwrap();
        let value: serde_json::Value =
            serde_json::from_slice(&capture.original.table.as_ref().unwrap().value).unwrap();
        assert_eq!(value["datasetId"], store.dataset_id.to_string());
        assert_ne!(store.dataset_id, child.dataset_id);
        assert_eq!(value["branches"][0]["id"], child.dataset_id.to_string());
        assert_eq!(value["branches"][0]["name"], "child");
        assert_eq!(
            value["branches"][0]["from"]["branchId"],
            store.dataset_id.to_string()
        );
        assert_eq!(value["branches"][0]["from"]["seq"], 1);
        assert_eq!(capture.original.identities.logical(), child.dataset_id);
        drop(capture);
        assert_eq!(preparation.room(), before);
    }

    #[cfg(unix)]
    #[test]
    fn genuine_original_history_keeps_exact_pins_after_live_changes() {
        let temporary = tempfile::tempdir().unwrap();
        let root = temporary.path().join("source");
        let store = history_store(&root);
        ins(&store, 1);
        store
            .create_snapshot("selected", &At::Head, Some("original note".into()))
            .unwrap();
        store
            .set_retention(crate::history::Retention {
                keep_commits: Some(3),
                ..Default::default()
            })
            .unwrap();
        let original = std::fs::read(root.join("history.json")).unwrap();
        let preparation = capture_preparation();
        let before = preparation.room();
        let capture = store
            .configuration_capture("pins", &Default::default(), &preparation)
            .unwrap();
        assert_eq!(capture.original.pins.as_ref().unwrap().value, original);
        store.delete_snapshot("selected").unwrap();
        store.create_snapshot("later", &At::Head, None).unwrap();
        assert_ne!(std::fs::read(root.join("history.json")).unwrap(), original);
        let source = capture
            .checked_primary(&preparation)
            .unwrap()
            .describe_native(&preparation)
            .unwrap();
        inspect_native_descriptor(&source, &preparation, |value| {
            assert_eq!(value["files"]["history.json"]["bytes"], original.len());
            assert_eq!(
                value["files"]["history.json"]["sha256"],
                crate::evidence::Sha256Digest::of_bytes(&original, preparation.control())
                    .unwrap()
                    .to_string()
            );
        });
        assert_eq!(
            source.primary.capture.original.pins.as_ref().unwrap().value,
            original
        );
        drop(source);
        assert_eq!(leases(&store), 0);
        assert_eq!(preparation.room(), before);
    }

    #[cfg(unix)]
    #[test]
    fn genuine_original_history_refuses_valid_live_pin_substitutions() {
        let temporary = tempfile::tempdir().unwrap();
        let root = temporary.path().join("source");
        let store = history_store(&root);
        ins(&store, 1);
        store
            .create_snapshot("selected", &At::Head, Some("original".into()))
            .unwrap();
        let original = std::fs::read(root.join("history.json")).unwrap();
        let preparation = capture_preparation();
        let before = preparation.room();
        for field in ["seq", "note", "warm", "selector", "retention"] {
            let mut changed: serde_json::Value = serde_json::from_slice(&original).unwrap();
            match field {
                "seq" => changed["snapshots"][0]["seq"] = 0.into(),
                "note" => changed["snapshots"][0]["note"] = "substitute".into(),
                "warm" => changed["snapshots"][0]["warm"] = true.into(),
                "selector" => {
                    changed["snapshots"][0]["configuration"]["selector"] =
                        serde_json::to_value(crate::history::ConfigurationSelector::Commit {
                            seq: 1,
                        })
                        .unwrap()
                }
                "retention" => changed["retention"]["keepCommits"] = 99.into(),
                _ => unreachable!(),
            }
            // Re-encode through the actual closed schema so the refusal proves
            // live-state binding rather than malformed/canonical JSON rejection.
            std::fs::write(
                root.join("history.json"),
                crate::history::capture::canonical_test_history(changed),
            )
            .unwrap();
            crate::history::read_file(&root, store.dataset_id).unwrap();
            assert!(
                store
                    .configuration_capture("substitution", &Default::default(), &preparation)
                    .is_err(),
                "{field}"
            );
            assert_eq!(leases(&store), 0);
            assert_eq!(preparation.room(), before);
        }
        std::fs::write(root.join("history.json"), original).unwrap();
        drop(
            store
                .configuration_capture("valid", &Default::default(), &preparation)
                .unwrap(),
        );
        assert_eq!(preparation.room(), before);
    }

    #[cfg(unix)]
    #[test]
    fn genuine_original_history_refuses_missing_foreign_version_and_symlink() {
        let temporary = tempfile::tempdir().unwrap();
        let root = temporary.path().join("source");
        let store = history_store(&root);
        ins(&store, 1);
        store.create_snapshot("selected", &At::Head, None).unwrap();
        let original = std::fs::read(root.join("history.json")).unwrap();
        let preparation = capture_preparation();
        let before = preparation.room();
        for damage in ["missing", "foreign", "version", "symlink"] {
            std::fs::remove_file(root.join("history.json")).unwrap();
            match damage {
                "missing" => {}
                "foreign" | "version" => {
                    let mut changed: serde_json::Value = serde_json::from_slice(&original).unwrap();
                    if damage == "foreign" {
                        changed["datasetId"] = uuid::Uuid::new_v4().to_string().into();
                    } else {
                        changed["format"] = 999.into();
                    }
                    std::fs::write(
                        root.join("history.json"),
                        crate::history::capture::canonical_test_history(changed),
                    )
                    .unwrap();
                }
                "symlink" => {
                    let other = temporary.path().join("valid-history.json");
                    std::fs::write(&other, &original).unwrap();
                    std::os::unix::fs::symlink(other, root.join("history.json")).unwrap();
                }
                _ => unreachable!(),
            }
            assert!(
                store
                    .configuration_capture("damaged", &Default::default(), &preparation)
                    .is_err(),
                "{damage}"
            );
            assert_eq!(leases(&store), 0);
            assert_eq!(preparation.room(), before);
            let _ = std::fs::remove_file(root.join("history.json"));
            std::fs::write(root.join("history.json"), &original).unwrap();
        }
    }

    #[cfg(unix)]
    #[test]
    fn genuine_original_history_accepts_legacy_semantics_without_rewriting_bytes() {
        let temporary = tempfile::tempdir().unwrap();
        let root = temporary.path().join("source");
        let store = Store::open(&root, StoreOptions::default()).unwrap();
        ins(&store, 1);
        store.create_snapshot("legacy", &At::Head, None).unwrap();
        drop(store);
        let preparation = capture_preparation();
        crate::migrate::activate_configuration_history(&root, false, &preparation).unwrap();
        let store = Store::open(&root, StoreOptions::default()).unwrap();
        let mut value: serde_json::Value =
            serde_json::from_slice(&std::fs::read(root.join("history.json")).unwrap()).unwrap();
        assert_eq!(value["format"], 1);
        let mut stale = value["snapshots"][0].clone();
        stale["seq"] = 0.into();
        value["snapshots"].as_array_mut().unwrap().insert(0, stale);
        value["unknownLegacyExtension"] = true.into();
        let wire = serde_json::to_vec(&value).unwrap();
        std::fs::write(root.join("history.json"), &wire).unwrap();
        let before = preparation.room();
        let captured = store
            .configuration_capture("legacy", &Default::default(), &preparation)
            .unwrap();
        assert_eq!(captured.original.pins.as_ref().unwrap().value, wire);
        drop(captured);
        assert_eq!(leases(&store), 0);
        assert_eq!(preparation.room(), before);
    }

    #[cfg(unix)]
    #[test]
    fn genuine_original_memory_history_borrows_pin_metadata_without_snapshot_clone() {
        let store = Store::in_memory(StoreOptions::default());
        let preparation = capture_preparation();
        store
            .activate_memory_configuration_history(&preparation)
            .unwrap();
        ins(&store, 1);
        store
            .create_snapshot("selected", &At::Head, Some("memory".into()))
            .unwrap();
        let history = store.mem_history.as_ref().unwrap().lock();
        let before_credit = preparation.room();
        let before_owners = Arc::strong_count(&history.pins["selected"].1);
        let document =
            crate::history::capture::encode_memory(store.dataset_id, &history, &preparation)
                .unwrap();
        assert_eq!(
            Arc::strong_count(&history.pins["selected"].1),
            before_owners
        );
        let value: serde_json::Value = serde_json::from_slice(&document.value).unwrap();
        assert_eq!(value["datasetId"], store.dataset_id.to_string());
        assert_eq!(value["snapshots"][0]["name"], "selected");
        assert_eq!(value["snapshots"][0]["note"], "memory");
        assert_eq!(value["snapshots"][0]["seq"], 1);
        assert_eq!(value["format"], 2);
        assert!(!value["snapshots"][0]["configuration"].is_null());
        drop(document);
        assert_eq!(preparation.room(), before_credit);
    }

    #[cfg(unix)]
    #[test]
    fn genuine_checked_primary_binds_all_actual_index_metadata_fields() {
        let temporary = tempfile::tempdir().unwrap();
        let root = temporary.path().join("source");
        let store = history_store(&root);
        ins(&store, 1);
        let preparation = capture_preparation();
        let room = preparation.room();
        let snapshot = store.snapshot();
        let path = root.join(&snapshot.generation.name).join("meta.json");
        let original = std::fs::read(&path).unwrap();
        let actual = serde_json::to_value(&snapshot.generation.meta).unwrap();
        for field in [
            "format_version",
            "quads",
            "terms",
            "next_bnode",
            "prefixes",
            "created",
            "patterns",
            "features",
        ] {
            let mut changed: serde_json::Value = serde_json::from_slice(&original).unwrap();
            match field {
                "format_version" => {
                    changed[field] = if snapshot.generation.meta.format_version == 3 {
                        4.into()
                    } else {
                        3.into()
                    }
                }
                "quads" | "terms" | "next_bnode" => {
                    changed[field] = changed[field]
                        .as_u64()
                        .unwrap()
                        .checked_add(1)
                        .unwrap()
                        .into()
                }
                "prefixes" => {
                    changed[field]["__meta_basis_witness"] = "urn:different-physical-prefix".into();
                }
                "created" => changed[field] = "different-physical-creation".into(),
                "patterns" => {
                    changed[field] = serde_json::to_value(crate::patterns::PatternInfo::skipped(
                        "fingerprint-test-replacement",
                    ))
                    .unwrap()
                }
                "features" => {
                    changed[field] = if snapshot.generation.meta.features.is_empty() {
                        serde_json::json!(["fsst-vocab"])
                    } else {
                        serde_json::json!([])
                    }
                }
                _ => unreachable!(),
            }
            assert_ne!(
                changed[field],
                actual.get(field).cloned().unwrap_or_default()
            );
            std::fs::write(&path, serde_json::to_vec_pretty(&changed).unwrap()).unwrap();
            let result = store
                .configuration_capture("all-meta-fields", &Default::default(), &preparation)
                .unwrap()
                .checked_primary(&preparation);
            std::fs::write(&path, &original).unwrap();
            let error = result.expect_err("every complete actual metadata field must be bound");
            assert!(
                error
                    .to_string()
                    .contains("captured index metadata differs from its actual physical producer"),
                "{field}: {error}"
            );
            assert_eq!(
                serde_json::to_value(&snapshot.generation.meta).unwrap(),
                actual
            );
            assert_eq!(leases(&store), 0);
            assert_eq!(preparation.room(), room);
        }
        drop(
            store
                .configuration_capture("restored-meta", &Default::default(), &preparation)
                .unwrap()
                .checked_primary(&preparation)
                .unwrap(),
        );
        assert_eq!(leases(&store), 0);
        assert_eq!(preparation.room(), room);
    }

    #[cfg(unix)]
    #[test]
    fn genuine_checked_primary_uses_real_materialized_builder_metadata() {
        let temporary = tempfile::tempdir().unwrap();
        for persistent in [false, true] {
            let preparation = capture_preparation();
            let store = if persistent {
                history_store(&temporary.path().join("persistent"))
            } else {
                let store = Store::in_memory(StoreOptions::default());
                store
                    .activate_memory_configuration_history(&preparation)
                    .unwrap();
                store
            };
            ins(&store, 1);
            let scratch = temporary.path().join(if persistent {
                "disk-scratch"
            } else {
                "memory-scratch"
            });
            let options = MemoryCaptureOptions {
                tmp_dir: scratch.clone(),
                ..Default::default()
            };
            let room = preparation.room();
            let captured = store
                .materialized_configuration_capture("built-meta", &options, &preparation)
                .unwrap();
            assert!(captured.data.materialized);
            assert_ne!(
                captured.original.native.unwrap().metadata,
                metadata_fingerprint(&store.snapshot().generation.meta, &preparation).unwrap(),
                "rebuilt metadata cannot be inferred from the source generation"
            );
            drop(captured.checked_primary(&preparation).unwrap());
            assert_eq!(std::fs::read_dir(&scratch).unwrap().count(), 0);
            assert_eq!(preparation.room(), room);
            let captured = store
                .materialized_configuration_capture("changed-built-meta", &options, &preparation)
                .unwrap();
            let directory = std::fs::read_dir(&scratch)
                .unwrap()
                .next()
                .unwrap()
                .unwrap()
                .path();
            let path = directory.join(&captured.data.generation).join("meta.json");
            let bytes = std::fs::read(&path).unwrap();
            let mut value: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
            assert_eq!(value["quads"], 1);
            value["quads"] = 2.into();
            let changed = serde_json::to_vec_pretty(&value).unwrap();
            assert_eq!(
                changed.len(),
                bytes.len(),
                "a length mismatch must not substitute for the semantic fingerprint control"
            );
            std::fs::write(&path, changed).unwrap();
            let error = captured.checked_primary(&preparation).err().unwrap();
            assert!(
                error
                    .to_string()
                    .contains("captured index metadata differs from its actual physical producer"),
                "{error}"
            );
            assert_eq!(std::fs::read_dir(&scratch).unwrap().count(), 0);
            assert_eq!(preparation.room(), room);
            if persistent {
                assert_eq!(leases(&store), 0);
            }
        }
    }

    #[cfg(unix)]
    #[test]
    fn genuine_checked_primary_metadata_streaming_limits_controls_and_version_order() {
        let temporary = tempfile::tempdir().unwrap();
        let root = temporary.path().join("source");
        let store = history_store(&root);
        ins(&store, 1);
        let mut borrowed = store.snapshot().generation.meta.clone();
        borrowed
            .prefixes
            .insert("large".into(), "x".repeat(1 << 20));
        let preparation =
            crate::access::Preparation::new(crate::task::Control::none(), 4096).unwrap();
        let room = preparation.room();
        let first = metadata_fingerprint(&borrowed, &preparation).unwrap();
        borrowed.prefixes.get_mut("large").unwrap().push('y');
        assert_ne!(
            metadata_fingerprint(&borrowed, &preparation).unwrap(),
            first
        );
        assert_eq!(
            preparation.room(),
            room,
            "the large borrowed metadata is streamed, not copied into request memory"
        );
        let small = crate::access::Preparation::new(crate::task::Control::none(), 512).unwrap();
        let small_room = small.room();
        assert!(matches!(
            metadata_fingerprint(&borrowed, &small),
            Err(Error::BudgetExceeded(_))
        ));
        assert_eq!(small.room(), small_room);
        let cancelled =
            crate::access::Preparation::new(crate::task::Control::none(), 4096).unwrap();
        cancelled.control().cancel.cancel();
        assert!(matches!(
            metadata_fingerprint(&borrowed, &cancelled),
            Err(Error::Cancelled)
        ));
        let mut control = crate::task::Control::none();
        control.deadline = Some(Instant::now() + Duration::from_secs(1));
        let timed = crate::access::Preparation::new(control, 4096).unwrap();
        std::thread::sleep(
            timed
                .control()
                .deadline
                .unwrap()
                .saturating_duration_since(Instant::now())
                + Duration::from_millis(1),
        );
        assert!(matches!(
            metadata_fingerprint(&borrowed, &timed),
            Err(Error::Timeout)
        ));
        borrowed.prefixes.insert(
            "too-large".into(),
            "z".repeat(super::super::managed_history::MAX_BYTES + 1),
        );
        assert!(matches!(
            metadata_fingerprint(&borrowed, &preparation),
            Err(Error::BudgetExceeded(_))
        ));
        assert_eq!(preparation.room(), room);
        let capture_preparation = capture_preparation();
        let capture_room = capture_preparation.room();
        let path = root
            .join(&store.snapshot().generation.name)
            .join("meta.json");
        let original = std::fs::read(&path).unwrap();
        std::fs::write(&path, br#"{"format_version":4294967295,"quads":false}"#).unwrap();
        let result = store
            .configuration_capture(
                "future-meta-first",
                &Default::default(),
                &capture_preparation,
            )
            .unwrap()
            .checked_primary(&capture_preparation);
        std::fs::write(&path, original).unwrap();
        assert!(
            matches!(result, Err(Error::Unsupported(_))),
            "known index version must be checked before the malformed payload"
        );
        assert_eq!(leases(&store), 0);
        assert_eq!(capture_preparation.room(), capture_room);
    }

    #[cfg(unix)]
    #[test]
    fn genuine_checked_primary_refuses_metadata_that_differs_from_its_actual_generation() {
        let temporary = tempfile::tempdir().unwrap();
        let root = temporary.path().join("source");
        let store = history_store(&root);
        ins(&store, 1);
        let preparation = capture_preparation();
        let room = preparation.room();
        let snapshot = store.snapshot();
        let path = root.join(&snapshot.generation.name).join("meta.json");
        let original = std::fs::read(&path).unwrap();
        let mut changed: serde_json::Value = serde_json::from_slice(&original).unwrap();
        let actual_quads = snapshot.generation.meta.quads;
        assert_eq!(changed["quads"], actual_quads);
        changed["quads"] = actual_quads.checked_add(1).unwrap().into();
        std::fs::write(&path, serde_json::to_vec_pretty(&changed).unwrap()).unwrap();
        assert_eq!(snapshot.generation.meta.quads, actual_quads);
        let result = store
            .configuration_capture("actual-meta-basis", &Default::default(), &preparation)
            .unwrap()
            .checked_primary(&preparation);
        std::fs::write(&path, &original).unwrap();
        let refused = result.is_err();
        drop(result);
        assert_eq!(leases(&store), 0);
        assert_eq!(preparation.room(), room);
        assert!(
            refused,
            "captured metadata must match the actual selected Generation, not only a new source-file pin"
        );
    }

    #[test]
    fn genuine_configuration_capture_retains_actual_lease_metadata_and_fixed_prefix() {
        let temporary = tempfile::tempdir().unwrap();
        let root = temporary.path().join("source");
        let store = history_store(&root);
        ins(&store, 1);
        let preparation = capture_preparation();
        let before = preparation.room();
        let captured = store
            .configuration_capture("genuine", &Default::default(), &preparation)
            .unwrap();
        assert_eq!(leases(&store), 1);
        assert_eq!(captured.data.lease.label(), "genuine");
        assert_eq!(captured.original.identities.family(), store.dataset_id);
        assert_eq!(captured.original.identities.logical(), store.dataset_id);
        assert_eq!(
            captured.original.identities.family_document(),
            std::fs::read(root.join("dataset.json")).unwrap()
        );
        assert!(captured.original.identities.branch_document().is_none());
        assert_eq!(
            captured.original.binding,
            store
                .configuration_binding_at(captured.data.commit.seq, &preparation)
                .unwrap()
        );
        let wal = captured
            .data
            .files
            .iter()
            .find(|f| f.path.ends_with("/wal.log"))
            .unwrap();
        let length = wal.len;
        let head = captured.data.commit;
        ins(&store, 2);
        assert!(store.head_commit().seq > head.seq);
        assert_eq!(captured.data.commit, head);
        assert_eq!(wal.len, length);
        assert_eq!(wal.read_at(length, &mut [0; 1]).unwrap(), 0);
        assert!(preparation.room() < before);
        drop(captured);
        assert_eq!(leases(&store), 0);
        assert_eq!(preparation.room(), before);
    }

    #[test]
    fn genuine_configuration_capture_materializes_a_real_persistent_branch_family() {
        let temporary = tempfile::tempdir().unwrap();
        let main = history_store(&temporary.path().join("source"));
        ins(&main, 1);
        main.create_branch("branch", &Default::default()).unwrap();
        let branch = main.branch("branch").unwrap();
        assert!(branch.snapshot().generation.linked().is_some());
        let preparation = capture_preparation();
        let before = preparation.room();
        let scratch = temporary.path().join("scratch");
        let captured = branch
            .materialized_configuration_capture(
                "genuine-branch",
                &MemoryCaptureOptions {
                    tmp_dir: scratch.clone(),
                    ..Default::default()
                },
                &preparation,
            )
            .unwrap();
        assert_eq!(captured.original.identities.family(), main.dataset_id);
        assert_eq!(captured.original.identities.logical(), branch.dataset_id);
        assert_ne!(
            captured.original.identities.family(),
            captured.data.dataset_id
        );
        assert_eq!(captured.data.dataset_id, branch.dataset_id);
        assert_eq!(captured.data.commit.seq, branch.head_commit().seq);
        let metadata: super::super::branching::BranchFile =
            serde_json::from_slice(captured.original.identities.branch_document().unwrap())
                .unwrap();
        assert_eq!(metadata.dataset_id, main.dataset_id);
        assert_eq!(metadata.id, branch.dataset_id);
        assert!(captured.data.materialized);
        assert!(!captured.data.in_memory);
        assert_eq!(std::fs::read_dir(&scratch).unwrap().count(), 1);
        drop(captured);
        assert_eq!(std::fs::read_dir(&scratch).unwrap().count(), 0);
        assert_eq!(preparation.room(), before);
    }

    #[test]
    fn genuine_configuration_capture_materializes_real_memory_main_and_branch() {
        let temporary = tempfile::tempdir().unwrap();
        let main = Store::in_memory(StoreOptions::default());
        let preparation = capture_preparation();
        main.activate_memory_configuration_history(&preparation)
            .unwrap();
        ins(&main, 1);
        main.create_branch("branch", &Default::default()).unwrap();
        let scratch = temporary.path().join("scratch");
        for name in ["main", "branch"] {
            let store = main.branch(name).unwrap();
            let before = preparation.room();
            let captured = store
                .materialized_configuration_capture(
                    "genuine-memory",
                    &MemoryCaptureOptions {
                        tmp_dir: scratch.clone(),
                        ..Default::default()
                    },
                    &preparation,
                )
                .unwrap();
            assert_eq!(captured.original.identities.family(), main.dataset_id);
            assert_eq!(captured.original.identities.logical(), store.dataset_id);
            assert_eq!(captured.data.commit.seq, store.head_commit().seq);
            let family: serde_json::Value =
                serde_json::from_slice(captured.original.identities.family_document()).unwrap();
            assert_eq!(family["origin"], "memory-capture");
            assert_eq!(
                captured.original.identities.branch_document().is_some(),
                name != "main"
            );
            assert!(captured.data.materialized && captured.data.in_memory);
            assert_eq!(
                captured.original.binding,
                store
                    .configuration_binding_at(captured.data.commit.seq, &preparation)
                    .unwrap()
            );
            drop(captured);
            assert_eq!(std::fs::read_dir(&scratch).unwrap().count(), 0);
            assert_eq!(preparation.room(), before);
        }
    }

    #[test]
    fn genuine_configuration_capture_refuses_live_fork_substitution_before_scratch() {
        let temporary = tempfile::tempdir().unwrap();
        let root = temporary.path().join("source");
        let main = history_store(&root);
        main.create_branch("branch", &Default::default()).unwrap();
        let branch = main.branch("branch").unwrap();
        let preparation = capture_preparation();
        let before = preparation.room();
        let path = root.join(format!("branches/{}/branch.json", branch.dataset_id));
        let original = std::fs::read(&path).unwrap();
        let mut metadata: super::super::branching::BranchFile =
            serde_json::from_slice(&original).unwrap();
        metadata.from.branch_id = uuid::Uuid::new_v4();
        let forged = serde_json::to_vec_pretty(&metadata).unwrap();
        metadata.check_configuration(&forged).unwrap();
        std::fs::write(&path, forged).unwrap();
        let scratch = temporary.path().join("scratch");
        let result = branch.materialized_configuration_capture(
            "genuine-refusal",
            &MemoryCaptureOptions {
                tmp_dir: scratch.clone(),
                ..Default::default()
            },
            &preparation,
        );
        std::fs::write(path, original).unwrap();
        assert!(result.is_err());
        drop(result);
        assert!(!scratch.exists());
        assert_eq!(leases(&branch), 0);
        assert_eq!(preparation.room(), before);
    }

    #[test]
    fn genuine_configuration_capture_pressure_and_cancel_leave_no_lease() {
        let temporary = tempfile::tempdir().unwrap();
        let store = history_store(&temporary.path().join("source"));
        let preparation = capture_preparation();
        let before = preparation.room();
        let pressure = preparation.reserve(before - 64).unwrap();
        assert!(matches!(
            store.configuration_capture("pressure", &Default::default(), &preparation),
            Err(Error::BudgetExceeded(_))
        ));
        assert_eq!(leases(&store), 0);
        assert_eq!(preparation.room(), 64);
        drop(pressure);
        assert_eq!(preparation.room(), before);
        preparation.control().cancel.cancel();
        assert!(matches!(
            store.configuration_capture("cancelled", &Default::default(), &preparation),
            Err(Error::Cancelled)
        ));
        assert_eq!(leases(&store), 0);
        assert_eq!(preparation.room(), before);
    }

    #[cfg(unix)]
    fn inspect_native_descriptor(
        source: &PreparedNativeSource,
        preparation: &crate::access::Preparation,
        inspect: impl FnOnce(&serde_json::Value),
    ) {
        let allowance = preparation.room();
        let _loan = preparation.reserve(allowance).unwrap();
        let options = crate::history::HistoryOptions::default();
        let mut budget = super::super::cold::ColdBudget::new(crate::history::ReadLimit {
            options: &options,
            memory: Some(allowance),
            held: 0,
        })
        .unwrap();
        let parsed = budget.json(&source.descriptor.value).unwrap();
        inspect(&parsed.value);
        drop(parsed.value);
        budget.release(parsed.charge);
        assert_eq!(budget.used(), 0);
    }

    #[cfg(unix)]
    #[test]
    fn genuine_native_source_descriptor_retains_actual_prefix_and_small_metadata() {
        let temporary = tempfile::tempdir().unwrap();
        let store = history_store(&temporary.path().join("source"));
        ins(&store, 1);
        let preparation = capture_preparation();
        let before = preparation.room();
        let checked = store
            .configuration_capture("descriptor", &Default::default(), &preparation)
            .unwrap()
            .checked_primary(&preparation)
            .unwrap();
        let head = checked.capture.data.commit.seq;
        let expected_wal = checked.wal_digest;
        let extent = checked.wal_extent;
        let first = checked.first_wal_seq.unwrap();
        let source = checked.describe_native(&preparation).unwrap();
        assert_eq!(leases(&store), 1);
        ins(&store, 2);
        assert!(head < store.head_commit().seq);
        assert_eq!(
            source.digest,
            crate::evidence::Sha256Digest::of_bytes(
                &source.descriptor.value,
                preparation.control()
            )
            .unwrap()
        );
        inspect_native_descriptor(&source, &preparation, |value| {
            assert_eq!(value["format"], 1);
            assert_eq!(value["familyId"], store.dataset_id.to_string());
            assert_eq!(
                value["roots"]["main"]["logicalId"],
                store.dataset_id.to_string()
            );
            assert_eq!(value["roots"]["main"]["head"], head);
            assert_eq!(value["sources"].as_array().unwrap().len(), 0);
            let intervals = value["intervals"].as_array().unwrap();
            assert_eq!(intervals.len(), 1);
            assert_eq!(intervals[0]["start"], 0);
            assert_eq!(intervals[0]["end"], extent);
            assert_eq!(intervals[0]["firstSeq"], first);
            assert_eq!(intervals[0]["lastSeq"], head);
            assert_eq!(intervals[0]["sha256"], expected_wal.to_string());
            let files = value["files"].as_object().unwrap();
            for required in [
                "dataset.json",
                "family-dataset.json",
                "managed.json",
                "CURRENT",
            ] {
                assert!(files.contains_key(required));
            }
            assert!(files.keys().any(|path| path.ends_with("/commit.json")));
            assert!(files.keys().any(|path| path.ends_with("/meta.json")));
            assert!(files.keys().any(|path| path.ends_with("/coverage.json")));
            assert!(
                files
                    .keys()
                    .all(|path| !path.ends_with("wal.log") && !path.ends_with(".dat"))
            );
        });
        drop(source);
        assert_eq!(leases(&store), 0);
        assert_eq!(preparation.room(), before);
    }

    #[cfg(unix)]
    #[test]
    fn genuine_native_source_materialized_family_and_base_only_descriptor() {
        let temporary = tempfile::tempdir().unwrap();
        let preparation = capture_preparation();
        let memory = Store::in_memory(StoreOptions::default());
        memory
            .activate_memory_configuration_history(&preparation)
            .unwrap();
        ins(&memory, 1);
        memory.create_branch("child", &Default::default()).unwrap();
        let persistent = history_store(&temporary.path().join("source"));
        ins(&persistent, 1);
        persistent
            .create_branch("child", &Default::default())
            .unwrap();
        let scratch = temporary.path().join("scratch");
        for store in [
            memory.branch("main").unwrap(),
            memory.branch("child").unwrap(),
            persistent.branch("child").unwrap(),
        ] {
            let before = preparation.room();
            let source = store
                .materialized_configuration_capture(
                    "descriptor",
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
            inspect_native_descriptor(&source, &preparation, |value| {
                assert_eq!(value["familyId"], store.owner_dataset_id().to_string());
                assert_eq!(
                    value["roots"]["main"]["logicalId"],
                    store.dataset_id.to_string()
                );
                assert!(value["intervals"].as_array().unwrap().is_empty());
                assert!(value["sources"].as_array().unwrap().is_empty());
                if store.is_branch() {
                    assert_ne!(store.dataset_id, store.owner_dataset_id());
                    assert!(
                        value["files"]
                            .as_object()
                            .unwrap()
                            .contains_key("branch.json")
                    );
                }
            });
            assert!(std::fs::read_dir(&scratch).unwrap().next().is_some());
            drop(source);
            assert_eq!(preparation.room(), before);
            assert_eq!(std::fs::read_dir(&scratch).unwrap().count(), 0);
        }
    }

    #[cfg(unix)]
    #[test]
    fn genuine_native_source_metadata_pin_uses_original_descriptor_after_replace() {
        let temporary = tempfile::tempdir().unwrap();
        let root = temporary.path().join("source");
        let store = history_store(&root);
        ins(&store, 1);
        let preparation = capture_preparation();
        let before = preparation.room();
        let checked = store
            .configuration_capture("descriptor", &Default::default(), &preparation)
            .unwrap()
            .checked_primary(&preparation)
            .unwrap();
        let name = format!("{}/commit.json", checked.capture.data.generation);
        let path = root.join(&name);
        let expected = crate::evidence::Sha256Digest::of_bytes(
            &std::fs::read(&path).unwrap(),
            preparation.control(),
        )
        .unwrap();
        let retained = path.with_extension("retained");
        std::fs::rename(&path, &retained).unwrap();
        std::fs::write(&path, b"replacement, never a source").unwrap();
        let result = checked.describe_native(&preparation);
        std::fs::remove_file(&path).unwrap();
        std::fs::rename(&retained, &path).unwrap();
        let source = result.unwrap();
        inspect_native_descriptor(&source, &preparation, |value| {
            assert_eq!(value["files"][&name]["sha256"], expected.to_string());
        });
        drop(source);
        assert_eq!(leases(&store), 0);
        assert_eq!(preparation.room(), before);
    }

    #[cfg(unix)]
    #[test]
    fn genuine_native_source_missing_required_member_cannot_encode() {
        let temporary = tempfile::tempdir().unwrap();
        let store = history_store(&temporary.path().join("source"));
        ins(&store, 1);
        let preparation = capture_preparation();
        let before = preparation.room();
        let mut checked = store
            .configuration_capture("descriptor", &Default::default(), &preparation)
            .unwrap()
            .checked_primary(&preparation)
            .unwrap();
        let old = checked.capture.data.files.len();
        checked
            .capture
            .data
            .files
            .retain(|file| file.path != "managed.json");
        assert_eq!(checked.capture.data.files.len() + 1, old);
        let result = checked.describe_native(&preparation);
        assert!(
            matches!(&result, Err(error) if error.to_string().contains("invalid imported source descriptor"))
        );
        drop(result);
        assert_eq!(leases(&store), 0);
        assert_eq!(preparation.room(), before);
    }

    #[cfg(unix)]
    #[test]
    fn genuine_native_source_receiving_pressure_and_late_controls_rollback() {
        let temporary = tempfile::tempdir().unwrap();
        let store = history_store(&temporary.path().join("source"));
        ins(&store, 1);
        let preparation = capture_preparation();
        let before = preparation.room();
        for kind in 0..3 {
            let checked = store
                .configuration_capture("descriptor", &Default::default(), &preparation)
                .unwrap()
                .checked_primary(&preparation)
                .unwrap();
            let control = crate::task::Control {
                deadline: (kind == 2).then(|| Instant::now() + Duration::from_secs(2)),
                ..crate::task::Control::none()
            };
            let callback = control.clone();
            let reached = Arc::new(AtomicBool::new(false));
            let timely = Arc::new(AtomicBool::new(false));
            let observed = reached.clone();
            let before_deadline = timely.clone();
            let receiving = crate::access::Preparation::new(
                crate::task::Control {
                    progress: crate::task::Progress::new(move |_, stage| {
                        if stage == "captured-source-delivery" && kind != 0 {
                            interrupt_supplied_capture(&callback, &observed, &before_deadline);
                        }
                    }),
                    ..control
                },
                if kind == 0 { 8192 } else { 64 << 20 },
            )
            .unwrap();
            let receiving_before = receiving.room();
            let result = checked.describe_native(&receiving);
            assert!(matches!(
                (kind, &result),
                (0, Err(Error::BudgetExceeded(_)))
                    | (1, Err(Error::Cancelled))
                    | (2, Err(Error::Timeout))
            ));
            drop(result);
            if kind != 0 {
                assert!(reached.load(Ordering::SeqCst));
            }
            if kind == 2 {
                assert!(timely.load(Ordering::SeqCst));
            }
            assert_eq!(leases(&store), 0);
            assert_eq!(receiving.room(), receiving_before);
            assert_eq!(preparation.room(), before);
        }
    }

    #[cfg(unix)]
    #[test]
    fn captured_primary_digest_uses_exact_retained_parser_bytes_after_append() {
        use std::io::Read;
        let temporary = tempfile::tempdir().unwrap();
        let store = history_store(&temporary.path().join("source"));
        ins(&store, 1);
        ins(&store, 2);
        let preparation = capture_preparation();
        let before = preparation.room();
        let captured = store
            .configuration_capture("digest", &Default::default(), &preparation)
            .unwrap();
        let mut wire = Vec::new();
        captured
            .primary_reader(CapturedPrimary::Wal)
            .unwrap()
            .read_to_end(&mut wire)
            .unwrap();
        let expected =
            crate::evidence::Sha256Digest::of_bytes(&wire, preparation.control()).unwrap();
        let first = captured.original.native.unwrap().base.seq + 1;
        ins(&store, 3);
        let checked = captured.checked_primary(&preparation).unwrap();
        assert_eq!(checked.wal_digest, expected);
        assert_eq!(checked.wal_extent, wire.len() as u64);
        assert_eq!(checked.first_wal_seq, Some(first));
        assert!(checked.capture.data.commit.seq < store.head_commit().seq);
        drop(checked);
        assert_eq!(leases(&store), 0);
        assert_eq!(preparation.room(), before);
    }

    #[cfg(unix)]
    #[test]
    fn captured_primary_digest_header_only_has_no_transaction_interval() {
        let temporary = tempfile::tempdir().unwrap();
        let store = Store::in_memory(StoreOptions::default());
        let preparation = capture_preparation();
        store
            .activate_memory_configuration_history(&preparation)
            .unwrap();
        ins(&store, 1);
        let before = preparation.room();
        let scratch = temporary.path().join("scratch");
        let captured = store
            .materialized_configuration_capture(
                "digest",
                &MemoryCaptureOptions {
                    tmp_dir: scratch.clone(),
                    ..Default::default()
                },
                &preparation,
            )
            .unwrap();
        let checked = captured.checked_primary(&preparation).unwrap();
        assert_eq!(checked.first_wal_seq, None);
        assert_eq!(checked.wal_extent, super::super::WAL_REC as u64);
        assert_eq!(
            checked.wal_digest,
            // an activated store's capture starts its log with the version 2 header
            crate::evidence::Sha256Digest::of_bytes(
                &super::super::wal::configuration_header_record(),
                preparation.control()
            )
            .unwrap()
        );
        assert_eq!(checked.base, checked.capture.data.commit);
        drop(checked);
        assert_eq!(preparation.room(), before);
        assert_eq!(std::fs::read_dir(&scratch).unwrap().count(), 0);
    }

    #[cfg(unix)]
    #[test]
    fn captured_primary_proof_retains_native_base_and_original_multi_publication_wal() {
        let temporary = tempfile::tempdir().unwrap();
        let store = history_store(&temporary.path().join("source"));
        ins(&store, 1);
        let settings = crate::evidence::Settings::default();
        let canonical = settings.canonical(&crate::task::Control::none()).unwrap();
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
        ins(&store, 2);
        let preparation = capture_preparation();
        let before = preparation.room();
        let captured = store
            .configuration_capture("framing", &Default::default(), &preparation)
            .unwrap();
        let expected = captured.data.commit;
        let base = captured.original.native.unwrap().base;
        assert!(base.seq < expected.seq);
        let receiving = capture_preparation();
        let receiving_before = receiving.room();
        ins(&store, 3); // its complete transaction is outside the retained extent
        let checked = captured.checked_primary(&receiving).unwrap();
        assert_eq!(checked.capture.data.commit, expected);
        assert_eq!(checked.base, base);
        assert!(checked.wal_extent > super::super::WAL_REC as u64);
        assert_eq!(leases(&store), 1);
        assert!(receiving.room() < receiving_before);
        drop(checked);
        assert_eq!(leases(&store), 0);
        assert_eq!(preparation.room(), before);
        assert_eq!(receiving.room(), receiving_before);
    }

    #[cfg(unix)]
    #[test]
    fn captured_primary_proof_reads_same_descriptor_after_wal_name_replacement() {
        let temporary = tempfile::tempdir().unwrap();
        let root = temporary.path().join("source");
        let store = history_store(&root);
        ins(&store, 1);
        let preparation = capture_preparation();
        let before = preparation.room();
        let captured = store
            .configuration_capture("framing", &Default::default(), &preparation)
            .unwrap();
        let wal = root.join(&captured.data.generation).join("wal.log");
        let retained = wal.with_extension("retained");
        std::fs::rename(&wal, &retained).unwrap();
        std::fs::write(&wal, b"unrelated replacement").unwrap();
        let result = captured.checked_primary(&preparation);
        std::fs::remove_file(&wal).unwrap();
        std::fs::rename(&retained, &wal).unwrap();
        let checked = result.unwrap();
        drop(checked);
        assert_eq!(leases(&store), 0);
        assert_eq!(preparation.room(), before);
    }

    #[cfg(unix)]
    #[test]
    fn captured_primary_proof_refuses_canonical_foreign_physical_base() {
        let temporary = tempfile::tempdir().unwrap();
        let store = history_store(&temporary.path().join("source"));
        ins(&store, 1);
        let preparation = capture_preparation();
        let before = preparation.room();
        let mut captured = store
            .configuration_capture("framing", &Default::default(), &preparation)
            .unwrap();
        let base = captured.original.native.unwrap().base;
        let bytes = crate::commit::gen_commit_bytes_with_binding(
            uuid::Uuid::new_v4(),
            "native",
            &base,
            crate::history::ConfigurationBinding::LegacyUnbound,
        );
        let path = format!("{}/commit.json", captured.data.generation);
        let file = captured
            .data
            .files
            .iter_mut()
            .find(|file| file.path == path)
            .unwrap();
        file.len = bytes.len() as u64;
        file.src = FileSource::Bytes(bytes.into());
        let result = captured.checked_primary(&preparation);
        assert!(
            matches!(result, Err(ref error) if error.to_string().contains("actual Store basis"))
        );
        drop(result);
        assert_eq!(leases(&store), 0);
        assert_eq!(preparation.room(), before);
    }

    #[cfg(unix)]
    #[test]
    fn captured_primary_proof_refuses_corrupt_or_incomplete_exact_wal_prefix() {
        use std::io::Read;
        let temporary = tempfile::tempdir().unwrap();
        let store = history_store(&temporary.path().join("source"));
        ins(&store, 1);
        let preparation = capture_preparation();
        let before = preparation.room();
        for shorten in [false, true] {
            let mut captured = store
                .configuration_capture("framing", &Default::default(), &preparation)
                .unwrap();
            let mut reader = captured.primary_reader(CapturedPrimary::Wal).unwrap();
            let mut wire = Vec::new();
            reader.read_to_end(&mut wire).unwrap();
            if shorten {
                wire.pop();
            } else {
                wire[super::super::WAL_REC + 1] ^= 1;
            }
            let path = format!("{}/wal.log", captured.data.generation);
            let file = captured
                .data
                .files
                .iter_mut()
                .find(|file| file.path == path)
                .unwrap();
            file.len = wire.len() as u64;
            file.src = FileSource::Bytes(wire.into());
            let result = captured.checked_primary(&preparation);
            assert!(
                result.is_err(),
                "a damaged captured transaction cannot yield a proof"
            );
            drop(result);
            assert_eq!(leases(&store), 0);
            assert_eq!(preparation.room(), before);
        }
    }

    #[cfg(unix)]
    #[test]
    fn captured_primary_proof_requires_published_closure_for_wal_references() {
        let temporary = tempfile::tempdir().unwrap();
        let store = history_store(&temporary.path().join("source"));
        ins(&store, 1);
        let preparation = capture_preparation();
        let before = preparation.room();
        let mut captured = store
            .configuration_capture("framing", &Default::default(), &preparation)
            .unwrap();
        let generation = captured
            .original
            .binding
            .configuration()
            .unwrap()
            .generation();
        let digest = generation.to_string();
        let name = digest.strip_prefix("sha256:").unwrap();
        let suffix = format!("/generations/{name}.json");
        let old_len = captured.data.files.len();
        captured
            .data
            .files
            .retain(|file| !file.path.ends_with(&suffix));
        assert_eq!(captured.data.files.len() + 1, old_len);
        let result = captured.checked_primary(&preparation);
        assert!(result.is_err());
        drop(result);
        assert_eq!(leases(&store), 0);
        assert_eq!(preparation.room(), before);
    }

    #[cfg(unix)]
    #[test]
    fn captured_primary_proof_checks_annotated_tail_membership() {
        let temporary = tempfile::tempdir().unwrap();
        use std::io::{Read, Seek, SeekFrom};
        let root = temporary.path().join("source");
        drop(history_store(&root));
        let store = Store::open(
            &root,
            StoreOptions {
                group_commit_idle_direct: false,
                ..Default::default()
            },
        )
        .unwrap();
        assert!(store.groups_commits());
        update(
            &store,
            "INSERT DATA { <urn:annotated> <urn:p> <urn:o> }",
            &QueryOptions {
                write: crate::guard::WriteOptions {
                    message: Some(Arc::from("retained annotation")),
                    ..Default::default()
                },
                ..Default::default()
            },
        )
        .unwrap();
        let preparation = capture_preparation();
        let before = preparation.room();
        let captured = store
            .configuration_capture("framing", &Default::default(), &preparation)
            .unwrap();
        assert!(
            captured
                .data
                .files
                .iter()
                .any(|file| file.path == crate::annotations::FILE)
        );
        {
            let mut reader = captured.primary_reader(CapturedPrimary::Wal).unwrap();
            reader
                .seek(SeekFrom::End(-(super::super::WAL_REC as i64)))
                .unwrap();
            let mut record = [0; super::super::WAL_REC];
            reader.read_exact(&mut record).unwrap();
            assert_eq!(record[0], super::super::WAL_COMMIT);
            assert_ne!(
                record[27] & crate::commit::WAL_FLAG_ANNOTATED,
                0,
                "the negative requires a real queued, annotated commit"
            );
        }
        drop(captured.checked_primary(&preparation).unwrap());
        assert_eq!(preparation.room(), before);
        let mut captured = store
            .configuration_capture("framing", &Default::default(), &preparation)
            .unwrap();
        captured
            .data
            .files
            .retain(|file| file.path != crate::annotations::FILE);
        let result = captured.checked_primary(&preparation);
        assert!(
            result.is_err(),
            "an annotated captured tail needs its retained membership"
        );
        drop(result);
        assert_eq!(leases(&store), 0);
        assert_eq!(preparation.room(), before);
    }

    #[cfg(unix)]
    #[test]
    fn captured_primary_proof_materialized_branches_keep_publishing_family_separate() {
        let temporary = tempfile::tempdir().unwrap();
        let memory = Store::in_memory(StoreOptions::default());
        let preparation = capture_preparation();
        memory
            .activate_memory_configuration_history(&preparation)
            .unwrap();
        ins(&memory, 1);
        memory.create_branch("child", &Default::default()).unwrap();
        let persistent = history_store(&temporary.path().join("source"));
        ins(&persistent, 1);
        persistent
            .create_branch("child", &Default::default())
            .unwrap();
        let scratch = temporary.path().join("scratch");
        for store in [
            memory.branch("main").unwrap(),
            memory.branch("child").unwrap(),
            persistent.branch("child").unwrap(),
        ] {
            let before = preparation.room();
            let captured = store
                .materialized_configuration_capture(
                    "framing",
                    &MemoryCaptureOptions {
                        tmp_dir: scratch.clone(),
                        ..Default::default()
                    },
                    &preparation,
                )
                .unwrap();
            let checked = captured.checked_primary(&preparation).unwrap();
            assert_eq!(checked.base, checked.capture.data.commit);
            assert_eq!(checked.wal_extent, super::super::WAL_REC as u64);
            if store.is_branch() {
                assert_ne!(
                    checked.capture.original.identities.family(),
                    checked.capture.data.dataset_id
                );
            }
            drop(checked);
            assert_eq!(preparation.room(), before);
            assert_eq!(std::fs::read_dir(&scratch).unwrap().count(), 0);
        }
    }

    #[cfg(unix)]
    #[test]
    fn captured_primary_proof_receiving_controls_and_credit_rollback() {
        let temporary = tempfile::tempdir().unwrap();
        let store = history_store(&temporary.path().join("source"));
        ins(&store, 1);
        let preparation = capture_preparation();
        let before = preparation.room();
        for kind in 0..3 {
            let receiving = crate::access::Preparation::new(
                crate::task::Control {
                    deadline: (kind == 2).then(|| Instant::now() + Duration::from_millis(10)),
                    ..crate::task::Control::none()
                },
                if kind == 0 { 8192 } else { 64 << 20 },
            )
            .unwrap();
            let receiving_total_before = receiving.room();
            let pressure = (kind == 0).then(|| receiving.reserve(receiving.room() - 64).unwrap());
            let receiving_before = receiving.room();
            let captured = store
                .configuration_capture("framing", &Default::default(), &preparation)
                .unwrap();
            if kind == 1 {
                receiving.control().cancel.cancel();
            }
            if kind == 2 {
                std::thread::sleep(Duration::from_millis(15));
            }
            let result = captured.checked_primary(&receiving);
            assert!(matches!(
                (kind, &result),
                (0, Err(Error::BudgetExceeded(_)))
                    | (1, Err(Error::Cancelled))
                    | (2, Err(Error::Timeout))
            ));
            drop(result);
            assert_eq!(receiving.room(), receiving_before);
            drop(pressure);
            assert_eq!(receiving.room(), receiving_total_before);
            assert_eq!(preparation.room(), before);
            assert_eq!(leases(&store), 0);
        }
    }

    #[cfg(unix)]
    #[test]
    fn genuine_captured_readers_leave_shared_kernel_cursor_unchanged() {
        use std::io::{Read, Seek, SeekFrom};
        let temporary = tempfile::tempdir().unwrap();
        let store = history_store(&temporary.path().join("source"));
        ins(&store, 1);
        let preparation = capture_preparation();
        let before = preparation.room();
        let captured = store
            .configuration_capture("kernel-cursor", &Default::default(), &preparation)
            .unwrap();
        let mut first = captured.primary_reader(CapturedPrimary::Wal).unwrap();
        let file = &captured.data.files[first.index];
        let FileSource::File(raw) = &file.src else {
            panic!("genuine WAL must retain its descriptor")
        };
        let mut raw = raw;
        let mut prefix = [0; 16];
        file.read_exact_at(0, &mut prefix).unwrap();
        raw.seek(SeekFrom::Start(7)).unwrap();
        let mut second = captured.primary_reader(CapturedPrimary::Wal).unwrap();
        first.seek(SeekFrom::Start(1)).unwrap();
        second.seek(SeekFrom::Start(5)).unwrap();
        let mut bytes = [0; 3];
        first.read_exact(&mut bytes).unwrap();
        assert_eq!(bytes, prefix[1..4]);
        assert_eq!(raw.stream_position().unwrap(), 7);
        second.read_exact(&mut bytes).unwrap();
        assert_eq!(bytes, prefix[5..8]);
        assert_eq!(raw.stream_position().unwrap(), 7);
        assert_eq!(first.stream_position().unwrap(), 4);
        assert_eq!(second.stream_position().unwrap(), 8);
        assert_eq!(first.read(&mut []).unwrap(), 0);
        first.seek(SeekFrom::End(0)).unwrap();
        assert_eq!(first.read(&mut bytes).unwrap(), 0);
        assert_eq!(raw.stream_position().unwrap(), 7);
        drop(captured);
        assert_eq!(leases(&store), 0);
        assert_eq!(preparation.room(), before);
    }

    #[cfg(unix)]
    #[test]
    fn genuine_captured_reader_simulated_unsupported_dispatch_preserves_cursors_and_credit() {
        use std::io::{Read, Seek, SeekFrom};
        struct Refused<'a>(CapturedReader<'a>);
        impl Read for Refused<'_> {
            fn read(&mut self, bytes: &mut [u8]) -> std::io::Result<usize> {
                self.0.read_checked(bytes, false)
            }
        }
        let temporary = tempfile::tempdir().unwrap();
        let store = history_store(&temporary.path().join("source"));
        ins(&store, 1);
        let preparation = capture_preparation();
        let before = preparation.room();
        let captured = store
            .configuration_capture("unsupported-cursor", &Default::default(), &preparation)
            .unwrap();
        let reader = captured.primary_reader(CapturedPrimary::Wal).unwrap();
        let FileSource::File(raw) = &captured.data.files[reader.index].src else {
            panic!("genuine retained file")
        };
        let mut raw = raw;
        raw.seek(SeekFrom::Start(7)).unwrap();
        let mut reader = Refused(reader);
        assert_eq!(reader.read(&mut []).unwrap(), 0);
        let receiving = capture_preparation();
        let receiving_before = receiving.room();
        {
            let allowance = receiving.room();
            let _loan = receiving.reserve(allowance).unwrap();
            let options = crate::history::HistoryOptions::default();
            let mut budget = super::super::cold::ColdBudget::new(crate::history::ReadLimit {
                options: &options,
                memory: Some(allowance),
                held: 0,
            })
            .unwrap();
            let extent = reader.0.extent();
            let result = budget.read_from(&mut reader, extent);
            assert!(
                matches!(result, Err(Error::Io(ref error)) if error.kind() == std::io::ErrorKind::Unsupported)
            );
            assert_eq!(budget.used(), 0);
        }
        assert_eq!(reader.0.stream_position().unwrap(), 0);
        assert_eq!(raw.stream_position().unwrap(), 7);
        reader.0.seek(SeekFrom::End(0)).unwrap();
        assert_eq!(reader.read(&mut [0; 4]).unwrap(), 0);
        assert_eq!(raw.stream_position().unwrap(), 7);
        assert_eq!(receiving.room(), receiving_before);
        assert_eq!(leases(&store), 1);
        drop(captured);
        assert_eq!(leases(&store), 0);
        assert_eq!(preparation.room(), before);
    }

    #[cfg(unix)]
    #[test]
    fn genuine_captured_reader_byte_backing_needs_no_file_cursor_backend() {
        use std::io::{Read, Seek, SeekFrom};
        let temporary = tempfile::tempdir().unwrap();
        let store = history_store(&temporary.path().join("source"));
        ins(&store, 1);
        let preparation = capture_preparation();
        let before = preparation.room();
        let mut captured = store
            .configuration_capture("byte-cursor", &Default::default(), &preparation)
            .unwrap();
        let index = captured.primary_reader(CapturedPrimary::Wal).unwrap().index;
        let mut prefix = [0; 16];
        captured.data.files[index]
            .read_exact_at(0, &mut prefix)
            .unwrap();
        // This fixture receiving copy is admitted before its actual backing.
        let copy_held = preparation.reserve(128).unwrap();
        captured.data.files[index].src = FileSource::Bytes(Arc::from(prefix.as_slice()));
        captured.data.files[index].len = prefix.len() as u64;
        let mut reader = captured.primary_reader(CapturedPrimary::Wal).unwrap();
        reader.seek(SeekFrom::Start(2)).unwrap();
        let mut bytes = [0; 4];
        assert_eq!(reader.read_checked(&mut bytes, false).unwrap(), 4);
        assert_eq!(bytes, prefix[2..6]);
        assert_eq!(reader.stream_position().unwrap(), 6);
        assert_eq!(reader.read(&mut []).unwrap(), 0);
        drop(captured);
        drop(copy_held);
        assert_eq!(leases(&store), 0);
        assert_eq!(preparation.room(), before);
    }

    #[cfg(unix)]
    #[test]
    fn genuine_configuration_capture_reader_keeps_fixed_prefix_after_append() {
        use std::io::Read;
        let temporary = tempfile::tempdir().unwrap();
        let store = history_store(&temporary.path().join("source"));
        ins(&store, 1);
        let preparation = capture_preparation();
        let before = preparation.room();
        let captured = store
            .configuration_capture("cursor", &Default::default(), &preparation)
            .unwrap();
        assert_eq!(leases(&store), 1);
        let mut reader = captured.primary_reader(CapturedPrimary::Wal).unwrap();
        let extent = reader.extent();
        ins(&store, 2);
        let mut wire = Vec::new();
        reader.read_to_end(&mut wire).unwrap();
        assert_eq!(wire.len() as u64, extent);
        assert_eq!(reader.read(&mut [0; 16]).unwrap(), 0);
        for member in [CapturedPrimary::Commit, CapturedPrimary::Meta] {
            let mut metadata = captured.primary_reader(member).unwrap();
            let mut bytes = Vec::new();
            metadata.read_to_end(&mut bytes).unwrap();
            assert_eq!(bytes.len() as u64, metadata.extent());
            assert!(serde_json::from_slice::<serde_json::Value>(&bytes).is_ok());
        }
        drop(captured);
        assert_eq!(leases(&store), 0);
        assert_eq!(preparation.room(), before);
    }

    #[cfg(unix)]
    #[test]
    fn genuine_configuration_capture_reader_seeks_only_inside_captured_extent() {
        use std::io::{Read, Seek, SeekFrom};
        let temporary = tempfile::tempdir().unwrap();
        let store = history_store(&temporary.path().join("source"));
        ins(&store, 1);
        let preparation = capture_preparation();
        let captured = store
            .configuration_capture("cursor", &Default::default(), &preparation)
            .unwrap();
        let mut reader = captured.primary_reader(CapturedPrimary::Wal).unwrap();
        let extent = reader.extent();
        assert!(extent > 1);
        assert_eq!(reader.seek(SeekFrom::Start(1)).unwrap(), 1);
        for invalid in [
            SeekFrom::Start(u64::MAX),
            SeekFrom::Current(i64::MIN),
            SeekFrom::End(1),
        ] {
            assert_eq!(
                reader.seek(invalid).unwrap_err().kind(),
                std::io::ErrorKind::InvalidInput
            );
            assert_eq!(reader.stream_position().unwrap(), 1);
        }
        assert_eq!(reader.seek(SeekFrom::End(0)).unwrap(), extent);
        assert_eq!(reader.read(&mut [0; 1]).unwrap(), 0);
        assert_eq!(reader.seek(SeekFrom::Current(-(extent as i64))).unwrap(), 0);
        assert_eq!(reader.read(&mut []).unwrap(), 0);
        assert_eq!(reader.stream_position().unwrap(), 0);
    }

    #[cfg(unix)]
    #[test]
    fn genuine_configuration_capture_reader_retains_descriptor_after_path_replacement() {
        use std::io::Read;
        let temporary = tempfile::tempdir().unwrap();
        let root = temporary.path().join("source");
        let store = history_store(&root);
        ins(&store, 1);
        let preparation = capture_preparation();
        let captured = store
            .configuration_capture("cursor", &Default::default(), &preparation)
            .unwrap();
        let path = root.join(&captured.data.generation).join("wal.log");
        let original = std::fs::read(&path).unwrap();
        let parked = path.with_extension("captured-original");
        std::fs::rename(&path, &parked).unwrap();
        std::fs::write(&path, b"foreign same-name file").unwrap();
        let mut reader = captured.primary_reader(CapturedPrimary::Wal).unwrap();
        let mut wire = Vec::new();
        let result = reader.read_to_end(&mut wire);
        // Restore the live Store's path before any outcome assertion can unwind.
        std::fs::remove_file(&path).unwrap();
        std::fs::rename(parked, path).unwrap();
        result.unwrap();
        assert_eq!(wire, original[..reader.extent() as usize]);
        assert_ne!(wire, b"foreign same-name file");
    }

    #[cfg(unix)]
    #[test]
    fn genuine_configuration_capture_reader_refuses_short_captured_descriptor() {
        use std::io::Read;
        let temporary = tempfile::tempdir().unwrap();
        let root = temporary.path().join("source");
        let store = history_store(&root);
        ins(&store, 1);
        let preparation = capture_preparation();
        let before = preparation.room();
        let captured = store
            .configuration_capture("cursor", &Default::default(), &preparation)
            .unwrap();
        let path = root.join(&captured.data.generation).join("wal.log");
        let original = std::fs::read(&path).unwrap();
        let file = std::fs::OpenOptions::new().write(true).open(&path).unwrap();
        file.set_len(1).unwrap();
        let mut reader = captured.primary_reader(CapturedPrimary::Wal).unwrap();
        let mut wire = vec![0; reader.extent() as usize];
        let result = reader.read_exact(&mut wire);
        // Keep the original inode and restore its bytes before Store teardown.
        std::fs::write(&path, original).unwrap();
        drop(captured);
        assert_eq!(leases(&store), 0);
        assert_eq!(preparation.room(), before);
        assert_eq!(
            result.unwrap_err().kind(),
            std::io::ErrorKind::UnexpectedEof
        );
    }

    #[cfg(unix)]
    #[test]
    fn genuine_configuration_capture_reader_uses_controlled_original_generation_codec() {
        let temporary = tempfile::tempdir().unwrap();
        let store = history_store(&temporary.path().join("source"));
        ins(&store, 1);
        let preparation = capture_preparation();
        let before = preparation.room();
        let captured = store
            .configuration_capture("codec", &Default::default(), &preparation)
            .unwrap();
        {
            let allowance = preparation.room();
            let _loan = preparation.reserve(allowance).unwrap();
            let options = crate::history::HistoryOptions {
                cancel: Some(preparation.control().cancel.flag()),
                deadline: preparation.control().deadline,
            };
            let mut budget = super::super::cold::ColdBudget::new(crate::history::ReadLimit {
                options: &options,
                memory: Some(allowance),
                held: 0,
            })
            .unwrap();
            let mut reader = captured.primary_reader(CapturedPrimary::Commit).unwrap();
            let extent = reader.extent();
            let bytes = budget.read_from(&mut reader, extent).unwrap();
            let input_charge = budget.used();
            let loaded = crate::commit::decode_gen_commit_bytes_controlled(
                Path::new("captured-diagnostic-only/commit.json"),
                &bytes,
                &mut budget,
            )
            .unwrap();
            assert_eq!(loaded.value.dataset_id, store.dataset_id);
            assert_eq!(
                loaded.value.commit.generation,
                crate::commit::generation_number(&captured.data.generation)
            );
            assert!(loaded.value.commit.seq <= captured.data.commit.seq);
            drop(loaded.value);
            budget.release(loaded.charge);
            assert_eq!(budget.used(), input_charge);
            budget.drop_vec(bytes);
            assert_eq!(budget.used(), 0);
        }
        drop(captured);
        assert_eq!(leases(&store), 0);
        assert_eq!(preparation.room(), before);
    }

    #[cfg(unix)]
    #[test]
    fn genuine_configuration_capture_resolves_original_publication_after_live_configure() {
        let temporary = tempfile::tempdir().unwrap();
        let store = history_store(&temporary.path().join("source"));
        ins(&store, 1);
        let preparation = capture_preparation();
        let before = preparation.room();
        let captured = store
            .configuration_capture("original-publication", &Default::default(), &preparation)
            .unwrap();
        let selected = captured.original.binding.configuration().unwrap();
        let old_root = captured.original.history.clone();
        let settings = crate::evidence::Settings::default();
        let canonical = settings.canonical(&crate::task::Control::none()).unwrap();
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
        assert_eq!(
            store
                .configuration_binding_at(captured.data.commit.seq, &preparation)
                .unwrap(),
            captured.original.binding,
            "configure must not rewrite the old data publication",
        );
        ins(&store, 2);
        assert!(store.head_commit().seq > captured.data.commit.seq);
        let current = store
            .configuration_binding_at(store.head_commit().seq, &preparation)
            .unwrap();
        assert_ne!(current.configuration(), Some(selected));
        let receiving = capture_preparation();
        let receiving_before = receiving.room();
        let document = captured.resolve_publication(selected, &receiving).unwrap();
        assert_eq!(document.value.dataset_id, selected.dataset_id());
        assert_eq!(document.value.lineage_id, old_root.lineage_id);
        drop(captured);
        assert_eq!(leases(&store), 0);
        assert_eq!(preparation.room(), before);
        assert!(receiving.room() < receiving_before);
        drop(document);
        assert_eq!(receiving.room(), receiving_before);
    }

    #[cfg(unix)]
    #[test]
    fn genuine_configuration_capture_refuses_valid_orphan_and_foreign_publications() {
        let temporary = tempfile::tempdir().unwrap();
        let root = temporary.path().join("source");
        let store = history_store(&root);
        ins(&store, 1);
        let preparation = capture_preparation();
        let before = preparation.room();
        let initial = store
            .configuration_capture("prepare-orphan", &Default::default(), &preparation)
            .unwrap();
        let selected = initial.original.binding.configuration().unwrap();
        let mut document = initial.resolve_publication(selected, &preparation).unwrap();
        document.value.ordinal += 1;
        document.value.registry_revision += 1;
        document.value.previous = Some(selected.generation());
        let bytes = super::super::managed_history::encode(&document.value, &preparation).unwrap();
        let digest =
            crate::evidence::Sha256Digest::of_bytes(&bytes.value, preparation.control()).unwrap();
        let orphan = crate::history::ConfigurationIdentity {
            generation: digest,
            ..selected
        };
        let validated =
            super::super::managed_history::generation_bytes(&bytes.value, orphan, &preparation)
                .unwrap();
        drop(validated);
        let name = digest.to_string();
        let path = root
            .join("evidence-history")
            .join(selected.lineage_id().to_string())
            .join("generations")
            .join(format!("{}.json", name.strip_prefix("sha256:").unwrap()));
        std::fs::write(&path, &bytes.value).unwrap();
        drop(bytes);
        drop(document);
        drop(initial);
        let captured = store
            .configuration_capture("orphan-refusal", &Default::default(), &preparation)
            .unwrap();
        let held = preparation.room();
        let error = captured
            .resolve_publication(orphan, &preparation)
            .err()
            .expect("unpublished canonical orphan must refuse");
        assert!(error.to_string().contains("not published"), "{error}");
        for foreign in [
            crate::history::ConfigurationIdentity {
                dataset_id: uuid::Uuid::new_v4(),
                ..selected
            },
            crate::history::ConfigurationIdentity {
                lineage_id: uuid::Uuid::new_v4(),
                ..selected
            },
        ] {
            assert!(captured.resolve_publication(foreign, &preparation).is_err());
            assert_eq!(preparation.room(), held);
        }
        drop(captured);
        assert_eq!(leases(&store), 0);
        assert_eq!(preparation.room(), before);
        std::fs::remove_file(path).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn genuine_configuration_capture_publication_receiving_controls_rollback() {
        let temporary = tempfile::tempdir().unwrap();
        let store = history_store(&temporary.path().join("source"));
        ins(&store, 1);
        let preparation = capture_preparation();
        let before = preparation.room();
        let captured = store
            .configuration_capture("receiving-refusal", &Default::default(), &preparation)
            .unwrap();
        let selected = captured.original.binding.configuration().unwrap();
        let small = crate::access::Preparation::new(crate::task::Control::none(), 4096).unwrap();
        let small_before = small.room();
        assert!(matches!(
            captured.resolve_publication(selected, &small),
            Err(Error::BudgetExceeded(_))
        ));
        assert_eq!(small.room(), small_before);
        let cancelled = capture_preparation();
        let cancel_before = cancelled.room();
        cancelled.control().cancel.cancel();
        assert!(matches!(
            captured.resolve_publication(selected, &cancelled),
            Err(Error::Cancelled)
        ));
        assert_eq!(cancelled.room(), cancel_before);
        let deadline = Instant::now() + Duration::from_secs(1);
        let expired = crate::access::Preparation::new(
            crate::task::Control {
                deadline: Some(deadline),
                ..crate::task::Control::none()
            },
            64 << 20,
        )
        .unwrap();
        let expired_before = expired.room();
        std::thread::sleep(
            deadline.saturating_duration_since(Instant::now()) + Duration::from_millis(1),
        );
        assert!(matches!(
            captured.resolve_publication(selected, &expired),
            Err(Error::Timeout)
        ));
        assert_eq!(expired.room(), expired_before);
        assert_eq!(leases(&store), 1);
        drop(captured);
        assert_eq!(leases(&store), 0);
        assert_eq!(preparation.room(), before);
    }

    #[cfg(unix)]
    #[test]
    fn genuine_configuration_capture_materialized_publications_keep_original_family() {
        let temporary = tempfile::tempdir().unwrap();
        let memory = Store::in_memory(StoreOptions::default());
        let preparation = capture_preparation();
        memory
            .activate_memory_configuration_history(&preparation)
            .unwrap();
        ins(&memory, 1);
        memory.create_branch("child", &Default::default()).unwrap();
        let persistent = history_store(&temporary.path().join("source"));
        ins(&persistent, 1);
        persistent
            .create_branch("child", &Default::default())
            .unwrap();
        let persistent_child = persistent.branch("child").unwrap();
        assert!(persistent_child.snapshot().generation.linked().is_some());
        let scratch = temporary.path().join("scratch");
        for store in [
            memory.branch("main").unwrap(),
            memory.branch("child").unwrap(),
            persistent_child,
        ] {
            let before = preparation.room();
            let captured = store
                .materialized_configuration_capture(
                    "original-family",
                    &MemoryCaptureOptions {
                        tmp_dir: scratch.clone(),
                        ..Default::default()
                    },
                    &preparation,
                )
                .unwrap();
            let selected = captured.original.binding.configuration().unwrap();
            let document = captured
                .resolve_publication(selected, &preparation)
                .unwrap();
            assert_eq!(
                document.value.dataset_id,
                captured.original.identities.family()
            );
            assert_eq!(selected.dataset_id(), captured.original.identities.family());
            if captured.original.identities.branch_document().is_some() {
                assert_ne!(selected.dataset_id(), captured.data.dataset_id);
            }
            drop(document);
            drop(captured);
            assert_eq!(std::fs::read_dir(&scratch).unwrap().count(), 0);
            assert_eq!(preparation.room(), before);
        }
    }

    fn supplied_capture_preparation(timeout: bool) -> crate::access::Preparation {
        crate::access::Preparation::new(
            crate::task::Control {
                deadline: timeout.then(|| Instant::now() + Duration::from_secs(2)),
                ..crate::task::Control::none()
            },
            64 << 20,
        )
        .unwrap()
    }

    fn interrupt_supplied_capture(
        control: &crate::task::Control,
        reached: &AtomicBool,
        before_deadline: &AtomicBool,
    ) {
        reached.store(true, Ordering::SeqCst);
        if let Some(deadline) = control.deadline {
            before_deadline.store(Instant::now() < deadline, Ordering::SeqCst);
            std::thread::sleep(
                deadline.saturating_duration_since(Instant::now()) + Duration::from_millis(1),
            );
        } else {
            control.cancel.cancel();
        }
    }

    fn supplied_persistent_capture_refuses_after_unlock(timeout: bool) {
        let temporary = tempfile::tempdir().unwrap();
        let store = history_store(&temporary.path().join("source"));
        ins(&store, 1);
        let preparation = supplied_capture_preparation(timeout);
        let before = preparation.room();
        let reached = Arc::new(AtomicBool::new(false));
        let before_deadline = Arc::new(AtomicBool::new(false));
        let observed = reached.clone();
        let timely = before_deadline.clone();
        let control = preparation.control().clone();
        store.set_failpoint(
            "backup-capture-unlocked",
            Some(Arc::new(move |_| {
                interrupt_supplied_capture(&control, &observed, &timely);
            })),
        );
        // The legacy options intentionally have neither this flag nor deadline.
        let result =
            store.configuration_capture("supplied-control", &Default::default(), &preparation);
        store.set_failpoint("backup-capture-unlocked", None);
        let refused = if timeout {
            matches!(&result, Err(Error::Timeout))
        } else {
            matches!(&result, Err(Error::Cancelled))
        };
        // An old successful owner must release its actual lease before we panic.
        drop(result);
        assert!(reached.load(Ordering::SeqCst), "unlocked hook not reached");
        assert!(!timeout || before_deadline.load(Ordering::SeqCst));
        assert_eq!(leases(&store), 0);
        assert_eq!(preparation.room(), before);
        assert!(
            refused,
            "supplied Preparation must refuse persistent delivery"
        );
    }

    fn supplied_materialized_capture_refuses_after_built(timeout: bool) {
        let temporary = tempfile::tempdir().unwrap();
        let store = Store::in_memory(StoreOptions::default());
        let preparation = supplied_capture_preparation(timeout);
        store
            .activate_memory_configuration_history(&preparation)
            .unwrap();
        ins(&store, 1);
        // Activation owns live history credit, which the failed capture must retain.
        let before = preparation.room();
        let reached = Arc::new(AtomicBool::new(false));
        let before_deadline = Arc::new(AtomicBool::new(false));
        let observed = reached.clone();
        let timely = before_deadline.clone();
        let control = preparation.control().clone();
        let scratch = temporary.path().join("scratch");
        let options = MemoryCaptureOptions {
            tmp_dir: scratch.clone(),
            progress: Some(Arc::new(move |_, message| {
                if message == "built" {
                    interrupt_supplied_capture(&control, &observed, &timely);
                }
            })),
            ..Default::default()
        };
        assert!(options.cancel.is_none() && options.deadline.is_none());
        let result =
            store.materialized_configuration_capture("supplied-control", &options, &preparation);
        let refused = if timeout {
            matches!(&result, Err(Error::Timeout))
        } else {
            matches!(&result, Err(Error::Cancelled))
        };
        drop(result);
        assert!(reached.load(Ordering::SeqCst), "built callback not reached");
        assert!(!timeout || before_deadline.load(Ordering::SeqCst));
        assert_eq!(std::fs::read_dir(&scratch).unwrap().count(), 0);
        assert!(
            store.history.is_none(),
            "memory capture has no persistent history lease"
        );
        assert_eq!(preparation.room(), before);
        assert!(
            refused,
            "supplied Preparation must refuse materialized delivery"
        );
    }

    #[test]
    fn genuine_configuration_capture_supplied_cancel_after_unlock() {
        supplied_persistent_capture_refuses_after_unlock(false);
    }

    #[test]
    fn genuine_configuration_capture_supplied_timeout_after_unlock() {
        supplied_persistent_capture_refuses_after_unlock(true);
    }

    #[test]
    fn genuine_configuration_capture_supplied_cancel_after_built() {
        supplied_materialized_capture_refuses_after_built(false);
    }

    #[test]
    fn genuine_configuration_capture_supplied_timeout_after_built() {
        supplied_materialized_capture_refuses_after_built(true);
    }

    #[test]
    fn genuine_configuration_capture_retries_actual_main_configuration_race() {
        let temporary = tempfile::tempdir().unwrap();
        let store = history_store(&temporary.path().join("source"));
        ins(&store, 1);
        let calls = Arc::new(AtomicU64::new(0));
        let observed = calls.clone();
        store.set_failpoint(
            "backup-capture-unlocked",
            Some(Arc::new(move |store| {
                if observed.fetch_add(1, Ordering::SeqCst) != 0 {
                    return;
                }
                let settings = crate::evidence::Settings::default();
                let canonical = settings.canonical(&crate::task::Control::none()).unwrap();
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
            })),
        );
        let preparation = capture_preparation();
        let before = preparation.room();
        let captured = store
            .configuration_capture("racing-original", &Default::default(), &preparation)
            .unwrap();
        store.set_failpoint("backup-capture-unlocked", None);
        assert_eq!(calls.load(Ordering::SeqCst), 2);
        assert_eq!(leases(&store), 1);
        assert_eq!(
            captured.original.binding,
            store
                .configuration_binding_at(captured.data.commit.seq, &preparation)
                .unwrap()
        );
        drop(captured);
        assert_eq!(leases(&store), 0);
        assert_eq!(preparation.room(), before);
    }

    #[test]
    fn configuration_history_capture_keeps_actual_descriptor_bytes_and_closure() {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().join("source");
        let store = history_store(&root);
        ins(&store, 1);
        let capture = store.backup_capture("history").unwrap();
        let history: Vec<_> = capture
            .files
            .iter()
            .filter(|file| file.path.starts_with("evidence-history/"))
            .collect();
        assert_eq!(history.len(), 2, "coverage and the published genesis");
        assert!(history.iter().all(|file| {
            file.kind == FileKind::Immutable && matches!(&file.src, FileSource::File(_))
        }));
        assert!(matches!(
            &capture.file("managed.json").unwrap().src,
            FileSource::File(_)
        ));
        let state = store.managed_state_shared().unwrap();
        let crate::managed::ManagedState::Installed(installed) = state.as_ref() else {
            panic!("installed configuration")
        };
        let expected_registry = installed.file_bytes();
        // Replacing the mutable name after capture must not replace its source.
        super::super::write_atomic(&root.join("managed.json"), b"changed later").unwrap();
        let captured = capture.file("managed.json").unwrap();
        let mut bytes = vec![0; captured.len as usize];
        captured.read_exact_at(0, &mut bytes).unwrap();
        assert_eq!(bytes, expected_registry);
        let restored = directory.path().join("restored");
        capture.write_to(&restored).unwrap();
        let reopened = Store::open(&restored, StoreOptions::default()).unwrap();
        assert_eq!(reopened.head_commit(), capture.commit);
        let preparation =
            crate::access::Preparation::new(crate::task::Control::none(), 64 << 20).unwrap();
        let before = store
            .evidence_view(&At::Commit(capture.commit.seq), &preparation)
            .unwrap();
        let after = reopened
            .evidence_view(&At::Commit(capture.commit.seq), &preparation)
            .unwrap();
        assert_eq!(before.configuration(), after.configuration());
    }

    #[test]
    fn configuration_history_capture_refuses_missing_published_generation() {
        let directory = tempfile::tempdir().unwrap();
        let store = history_store(directory.path());
        let state = store.managed_state_shared().unwrap();
        let crate::managed::ManagedState::Installed(installed) = state.as_ref() else {
            panic!("installed configuration")
        };
        let root = installed.configuration_history.as_ref().unwrap();
        let path = directory
            .path()
            .join("evidence-history")
            .join(root.lineage_id.to_string())
            .join("generations")
            .join(format!(
                "{}.json",
                &root.current_generation.to_string()[7..]
            ));
        std::fs::remove_file(path).unwrap();
        assert!(store.backup_capture("missing-history").is_err());
        assert_eq!(leases(&store), 0);
    }

    #[test]
    fn configuration_history_capture_refuses_incomplete_closure_before_publication() {
        let directory = tempfile::tempdir().unwrap();
        let store = history_store(&directory.path().join("source"));
        ins(&store, 1);
        let mut capture = store.backup_capture("incomplete").unwrap();
        capture
            .files
            .retain(|file| !file.path.contains("/generations/"));
        let output = directory.path().join("destination");
        let before = history_capture_parent_entries(directory.path());
        assert!(capture.write_to(&output).is_err());
        assert!(!output.exists());
        assert_eq!(history_capture_parent_entries(directory.path()), before);
        drop(capture);
        assert_eq!(leases(&store), 0);
    }

    #[test]
    fn configuration_history_capture_syncs_real_nested_directories_before_publication() {
        let directory = tempfile::tempdir().unwrap();
        let store = history_store(&directory.path().join("source"));
        ins(&store, 1);
        let preparation =
            crate::access::Preparation::new(crate::task::Control::none(), 64 << 20).unwrap();
        let selected = store.evidence_view(&At::Head, &preparation).unwrap();
        let capture = store.backup_capture("directory-order").unwrap();
        let destination = directory.path().join("destination");
        let mut synced = Vec::new();
        capture
            .write_to_with_sync_hook(&destination, &mut |path| {
                assert!(
                    !destination.exists(),
                    "a directory sync must precede publication"
                );
                assert!(path.is_dir(), "the hook follows a real directory fsync");
                let relative = path
                    .strip_prefix(directory.path())
                    .unwrap()
                    .components()
                    .skip(1)
                    .collect::<PathBuf>();
                synced.push(relative);
                Ok(())
            })
            .unwrap();
        let history = PathBuf::from("evidence-history");
        let lineage = history.join(selected.configuration().lineage_id().to_string());
        let position = |path: &Path| {
            synced
                .iter()
                .position(|seen| seen == path)
                .unwrap_or_else(|| {
                    panic!("required directory {path:?} was not synced before publication")
                })
        };
        assert!(position(&lineage.join("generations")) < position(&lineage));
        assert!(position(&lineage) < position(&history));
        assert!(position(&history) < position(Path::new("")));
        assert_eq!(synced.last().unwrap(), Path::new(""));
        let restored = Store::open(&destination, StoreOptions::default()).unwrap();
        assert_eq!(
            restored
                .evidence_view(&At::Commit(capture.commit.seq), &preparation)
                .unwrap()
                .configuration(),
            selected.configuration()
        );
    }

    #[test]
    fn configuration_history_capture_directory_sync_interruption_leaves_no_destination() {
        let directory = tempfile::tempdir().unwrap();
        let store = history_store(&directory.path().join("source"));
        ins(&store, 1);
        let capture = store.backup_capture("directory-failure").unwrap();
        let destination = directory.path().join("destination");
        let mut interrupted = false;
        let before = history_capture_parent_entries(directory.path());
        let result = capture.write_to_with_sync_hook(&destination, &mut |path| {
            if path.file_name().is_some_and(|name| name == "generations") {
                assert!(std::fs::read_dir(path)?.next().is_some());
                assert!(!destination.exists());
                interrupted = true;
                return Err(Error::Cancelled);
            }
            Ok(())
        });
        assert!(matches!(result, Err(Error::Cancelled)));
        assert!(
            interrupted,
            "the failure follows a real nested-directory fsync"
        );
        assert!(!destination.exists());
        assert_eq!(history_capture_parent_entries(directory.path()), before);
        drop(capture);
        assert_eq!(leases(&store), 0);
    }

    #[test]
    fn configuration_history_memory_capture_preserves_exact_binding_and_removes_scratch() {
        let directory = tempfile::tempdir().unwrap();
        let store = Store::in_memory(StoreOptions::default());
        let preparation =
            crate::access::Preparation::new(crate::task::Control::none(), 64 << 20).unwrap();
        store
            .activate_memory_configuration_history(&preparation)
            .unwrap();
        ins(&store, 1);
        let selected = store
            .evidence_view(&At::Commit(store.head_commit().seq), &preparation)
            .unwrap();
        let scratch = directory.path().join("scratch");
        let capture = store
            .memory_backup_capture(
                "history-memory",
                &MemoryCaptureOptions {
                    tmp_dir: scratch.clone(),
                    ..Default::default()
                },
            )
            .unwrap();
        assert_eq!(
            capture
                .files
                .iter()
                .filter(|file| file.path.starts_with("evidence-history/"))
                .count(),
            2
        );
        let output = directory.path().join("destination");
        capture.write_to(&output).unwrap();
        let restored = Store::open(&output, StoreOptions::default()).unwrap();
        assert_eq!(dump(&store), dump(&restored));
        assert_eq!(
            restored
                .evidence_view(&At::Commit(capture.commit.seq), &preparation)
                .unwrap()
                .configuration(),
            selected.configuration()
        );
        drop(capture);
        assert_eq!(std::fs::read_dir(scratch).unwrap().count(), 0);
    }

    #[test]
    fn configuration_history_capture_retries_a_real_configure_without_an_rdf_write() {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().join("source");
        let store = history_store(&root);
        ins(&store, 1);
        let head = store.head_commit();
        let calls = Arc::new(AtomicU64::new(0));
        let observed = calls.clone();
        store.set_failpoint(
            "backup-capture-unlocked",
            Some(Arc::new(move |store| {
                if observed.fetch_add(1, Ordering::SeqCst) != 0 {
                    return;
                }
                let settings = crate::evidence::Settings::default();
                let canonical = settings.canonical(&crate::task::Control::none()).unwrap();
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
            })),
        );
        let capture = store.backup_capture("racing-configure").unwrap();
        store.set_failpoint("backup-capture-unlocked", None);
        assert_eq!(calls.load(Ordering::SeqCst), 2);
        assert_eq!(store.head_commit(), head, "configure did not write RDF");
        let preparation =
            crate::access::Preparation::new(crate::task::Control::none(), 64 << 20).unwrap();
        let selected = store.evidence_view(&At::Head, &preparation).unwrap();
        let restored = directory.path().join("restored");
        capture.write_to(&restored).unwrap();
        let reopened = Store::open(&restored, StoreOptions::default()).unwrap();
        assert_eq!(
            reopened
                .evidence_view(&At::Head, &preparation)
                .unwrap()
                .configuration(),
            selected.configuration()
        );
        assert_ne!(
            reopened
                .evidence_view(&At::Commit(head.seq), &preparation)
                .unwrap()
                .configuration(),
            selected.configuration(),
            "capturing current settings must not rebind the original data commit"
        );
    }

    /// Materialize `c` into `dir`, check it, open it, and compare it with the source's
    /// state at the captured commit.
    fn restore_and_compare(src: &Store, c: &BackupCapture, dir: &Path) {
        c.write_to(dir).unwrap();
        let opts = crate::check::CheckOptions { quick: false };
        let report = crate::check::check(dir, &opts).unwrap();
        assert_eq!(
            report.status,
            crate::check::Status::Ok,
            "{}",
            serde_json::to_string_pretty(&report).unwrap()
        );
        let r = Store::open(dir, StoreOptions::default()).unwrap();
        assert_eq!(r.head_commit(), c.commit);
        assert_eq!(r.snapshot().len(), c.commit.quads);
        assert_eq!(r.dataset_id(), c.dataset_id);
        assert_eq!(dump(&r), dump_at(src, c.commit.seq));
    }

    #[test]
    fn capture_lists_files_and_pins_prefixes() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("db");
        let s = Store::open(&root, StoreOptions::default()).unwrap();
        for i in 0..3 {
            ins(&s, i);
        }
        s.set_prefix("ex", "http://example.org/").unwrap();
        std::fs::write(root.join("validation-shapes.ttl"), b"# shapes\n").unwrap();
        std::fs::write(root.join("validation-schema.shex"), b"# schema\n").unwrap();
        std::fs::write(root.join("validation-schema.json"), b"{}").unwrap();
        std::fs::write(root.join("origin.json"), b"{}").unwrap();
        std::fs::write(root.join(".migrate-checked"), b"remove-old").unwrap();
        let c = s.backup_capture("b1").unwrap();
        assert_eq!(c.commit.seq, 3);
        assert_eq!(c.generation, "gen-0001");
        assert_eq!(c.index_format, crate::builder::PLAIN_FORMAT_VERSION);
        assert_eq!(c.lease.generation(), 1);
        assert_eq!(c.lease.label(), "b1");
        let paths: Vec<&str> = c.files.iter().map(|f| f.path.as_str()).collect();
        let n = paths.iter().filter(|p| p.starts_with("gen-0001/")).count();
        assert!(n >= 20, "{paths:?}");
        assert!(paths[..n].is_sorted());
        for f in [
            "wal.log",
            "delta.vocab",
            "commit.json",
            "meta.json",
            "vocab.dat",
        ] {
            assert!(paths.contains(&format!("gen-0001/{f}").as_str()), "{f}");
        }
        assert_eq!(
            paths[n..],
            [
                "commits.bin",
                "CURRENT",
                "dataset.json",
                "prefixes.json",
                "origin.json",
                "validation-shapes.ttl",
                "validation-schema.shex",
                "validation-schema.json"
            ]
        );
        for f in &c.files {
            let want = match f.path.as_str() {
                "gen-0001/wal.log" | "gen-0001/delta.vocab" | "commits.bin" => FileKind::Append,
                p if p.starts_with("gen-") => FileKind::Immutable,
                _ => FileKind::Meta,
            };
            assert_eq!(f.kind, want, "{}", f.path);
        }
        // a header and the records 0..=3
        assert_eq!(c.file("commits.bin").unwrap().len, 5 * 64);
        let wal = c.file("gen-0001/wal.log").unwrap();
        // the end of the last commit, before the space preallocated after it
        let on_disk = std::fs::metadata(root.join("gen-0001/wal.log"))
            .unwrap()
            .len();
        assert_eq!(wal.len, s.wal_bytes());
        assert!(on_disk > wal.len);
        assert_eq!(wal.len % super::super::WAL_REC as u64, 0);
        let p = c.file("prefixes.json").unwrap();
        let mut b = vec![0u8; p.len as usize];
        p.read_exact_at(0, &mut b).unwrap();
        let p: BTreeMap<String, String> = serde_json::from_slice(&b).unwrap();
        assert_eq!(p["ex"], "http://example.org/");

        // later writes and prefix changes do not reach the capture
        let lens: Vec<u64> = c.files.iter().map(|f| f.len).collect();
        ins(&s, 10);
        s.set_prefix("ex2", "http://example.org/2/").unwrap();
        assert_eq!(c.files.iter().map(|f| f.len).collect::<Vec<_>>(), lens);
        let out = dir.path().join("restored");
        restore_and_compare(&s, &c, &out);
        assert_eq!(dump_at(&s, 3).len(), 3);
        assert!(out.join("validation-shapes.ttl").exists());
        assert_eq!(
            std::fs::read(out.join("validation-schema.shex")).unwrap(),
            b"# schema\n"
        );
        assert!(out.join("validation-schema.json").exists());
        let r = Store::open(&out, StoreOptions::default()).unwrap();
        assert_eq!(r.prefixes(), p);
    }

    #[test]
    fn in_memory_stores_are_unsupported() {
        let s = Store::in_memory(StoreOptions::default());
        assert!(matches!(s.backup_capture("b"), Err(Error::Unsupported(_))));
    }

    /// An in-memory capture is a database with the store's id, head and content; its
    /// temporary directory goes with the lease.
    /// In-memory activation can race a memory capture's build. The capture writes
    /// the versions of the registry it captured, so it stays consistent and opens.
    #[test]
    fn a_memory_capture_racing_activation_keeps_its_captured_versions() {
        let tmp = tempfile::tempdir().unwrap();
        let store = Arc::new(Store::in_memory(StoreOptions::default()));
        update(
            &store,
            "INSERT DATA { <urn:s> <urn:p> 1 }",
            &QueryOptions::default(),
        )
        .unwrap();
        let racing = store.clone();
        let activated = Arc::new(AtomicBool::new(false));
        let once = activated.clone();
        let o = MemoryCaptureOptions {
            tmp_dir: tmp.path().join("scratch"),
            progress: Some(Arc::new(move |_, message: &str| {
                if message == "writing quads" && !once.swap(true, Ordering::SeqCst) {
                    let preparation =
                        crate::access::Preparation::new(crate::task::Control::none(), 64 << 20)
                            .unwrap();
                    racing
                        .activate_memory_configuration_history(&preparation)
                        .unwrap();
                }
            })),
            ..Default::default()
        };
        let capture = store.memory_backup_capture("race", &o).unwrap();
        assert!(activated.load(Ordering::SeqCst));
        let out = tmp.path().join("restored");
        capture.write_to(&out).unwrap();
        drop(capture);
        let json = |path: &str| -> serde_json::Value {
            serde_json::from_slice(&std::fs::read(out.join(path)).unwrap()).unwrap()
        };
        assert_eq!(json("gen-0001/commit.json")["format"], 1);
        let wal = std::fs::read(out.join("gen-0001/wal.log")).unwrap();
        assert_eq!(
            &wal[..super::super::WAL_REC],
            &super::super::wal::header_record()
        );
        assert!(json("dataset.json")["minimumReader"].as_u64().unwrap() < 8);
        assert!(!out.join("evidence-history").exists());
        drop(Store::open(&out, StoreOptions::default()).unwrap());
    }

    #[test]
    fn in_memory_captures_build_a_database() {
        let tmp = tempfile::tempdir().unwrap();
        let s = Store::in_memory(StoreOptions::default());
        ins(&s, 1);
        ins(&s, 2);
        update(
            &s,
            "INSERT DATA { GRAPH <urn:g> { _:b <urn:p> \"x\"@en } }",
            &QueryOptions::default(),
        )
        .unwrap();
        s.set_prefix("ex", "http://example.org/").unwrap();
        #[cfg(feature = "text")]
        s.enable_text(Default::default()).unwrap();
        let o = MemoryCaptureOptions {
            tmp_dir: tmp.path().join("scratch"),
            ..Default::default()
        };
        let c = s.memory_backup_capture("b", &o).unwrap();
        // the full-text index is rebuilt where the backup is restored
        #[cfg(feature = "text")]
        assert!(c.file("text.json").is_some());
        assert!(c.in_memory);
        assert_eq!(c.dataset_id, s.dataset_id());
        assert_eq!(c.commit.seq, s.head_commit().seq);
        assert_eq!(c.commit.quads, 3);
        assert_eq!(c.generation, "gen-0001");
        assert_eq!(c.lease.label(), "b");
        let paths: Vec<&str> = c.files.iter().map(|f| f.path.as_str()).collect();
        for p in [
            "gen-0001/wal.log",
            "gen-0001/commit.json",
            "commits.bin",
            "CURRENT",
            "dataset.json",
            "prefixes.json",
        ] {
            assert!(paths.contains(&p), "{p} in {paths:?}");
        }
        // a write after the capture is not in it
        ins(&s, 4);
        let out = tmp.path().join("db");
        c.write_to(&out).unwrap();
        let scratch: Vec<_> = std::fs::read_dir(tmp.path().join("scratch"))
            .unwrap()
            .collect();
        assert_eq!(scratch.len(), 1);
        drop(c);
        let scratch: Vec<_> = std::fs::read_dir(tmp.path().join("scratch"))
            .unwrap()
            .collect();
        assert!(scratch.is_empty());
        let report = crate::check::check(&out, &Default::default()).unwrap();
        // a missing full-text index is only a warning: it is rebuilt on open
        assert_eq!(report.errors, 0, "{report:?}");
        let r = Store::open(&out, StoreOptions::default()).unwrap();
        assert_eq!(r.dataset_id(), s.dataset_id());
        assert_eq!(r.head_commit().seq, s.head_commit().seq - 1);
        assert_eq!(r.snapshot().len(), 3);
        assert_eq!(
            r.prefixes().get("ex").map(String::as_str),
            Some("http://example.org/")
        );
        // the restored database goes on with the next commit, and new blank nodes do
        // not collide with the copied one
        ins(&r, 4);
        assert_eq!(r.head_commit().seq, s.head_commit().seq);
        update(
            &r,
            "INSERT DATA { GRAPH <urn:g> { _:c <urn:p> \"y\" } }",
            &QueryOptions::default(),
        )
        .unwrap();
        assert_eq!(r.snapshot().len(), 5);
    }

    #[test]
    fn in_memory_captures_cancel_and_clean_up() {
        let tmp = tempfile::tempdir().unwrap();
        let s = Store::in_memory(StoreOptions::default());
        let mut u = String::from("INSERT DATA {");
        for i in 0..70_000 {
            u.push_str(&format!(" <urn:s{i}> <urn:p> {i} ."));
        }
        u.push('}');
        update(&s, &u, &QueryOptions::default()).unwrap();
        let o = MemoryCaptureOptions {
            tmp_dir: tmp.path().to_path_buf(),
            cancel: Some(Arc::new(AtomicBool::new(true))),
            ..Default::default()
        };
        assert!(matches!(
            s.memory_backup_capture("b", &o),
            Err(Error::Cancelled)
        ));
        assert_eq!(std::fs::read_dir(tmp.path()).unwrap().count(), 0);
        // persistent stores use backup_capture
        let p = Store::open(&tmp.path().join("db"), StoreOptions::default()).unwrap();
        let o = MemoryCaptureOptions {
            tmp_dir: tmp.path().join("scratch"),
            ..Default::default()
        };
        assert!(matches!(
            p.memory_backup_capture("b", &o),
            Err(Error::Invalid(_))
        ));
    }

    #[test]
    fn restored_capture_continues_the_sequence() {
        let dir = tempfile::tempdir().unwrap();
        let s = Store::open(&dir.path().join("db"), StoreOptions::default()).unwrap();
        ins(&s, 1);
        ins(&s, 2);
        let c = s.backup_capture("b").unwrap();
        ins(&s, 3);
        let out = dir.path().join("r");
        c.write_to(&out).unwrap();
        assert!(c.write_to(&out).is_err());
        let r = Store::open(&out, StoreOptions::default()).unwrap();
        assert_eq!(r.head_commit().seq, 2);
        ins(&r, 7);
        assert_eq!(r.head_commit().seq, 3);
        let page = r.commits(crate::commit::CommitRange::Latest, 10);
        assert_eq!(page.commits.len(), 4);
    }

    #[test]
    fn lock_hold_is_measured() {
        let dir = tempfile::tempdir().unwrap();
        let s = Store::open(dir.path(), StoreOptions::default()).unwrap();
        s.set_failpoint(
            "backup-capture-locked",
            Some(Arc::new(|_: &Store| {
                std::thread::sleep(Duration::from_millis(30))
            })),
        );
        let c = s.backup_capture("b").unwrap();
        assert!(c.lock_hold >= Duration::from_millis(30));
        s.set_failpoint("backup-capture-locked", None);
        let c = s.backup_capture("b").unwrap();
        assert!(c.lock_hold < Duration::from_millis(30));
    }

    #[test]
    fn a_lagging_catalog_is_retryable() {
        let dir = tempfile::tempdir().unwrap();
        let s = Store::open(dir.path(), StoreOptions::default()).unwrap();
        ins(&s, 1);
        s.fail_catalog_writes(true);
        ins(&s, 2);
        match s.backup_capture("b") {
            Err(Error::Conflict(m)) => assert!(m.starts_with("catalog-lagging"), "{m}"),
            r => panic!("{r:?}"),
        }
        // no lease is left behind
        assert_eq!(leases(&s), 0);
        s.fail_catalog_writes(false);
        let c = s.backup_capture("b").unwrap();
        assert_eq!(c.commit.seq, 2);
        assert_eq!(c.file("commits.bin").unwrap().len, 4 * 64);
    }

    #[test]
    fn current_is_rechecked_once() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("db");
        let s = Store::open(&root, StoreOptions::default()).unwrap();
        ins(&s, 1);
        // a compaction right after the first attempt's lock: the second attempt captures
        // the new generation, and the first one's lease is gone with it
        let once = Arc::new(AtomicBool::new(true));
        let o = once.clone();
        s.set_failpoint(
            "backup-capture-unlocked",
            Some(Arc::new(move |s: &Store| {
                if o.swap(false, Ordering::SeqCst) {
                    s.compact().unwrap();
                }
            })),
        );
        let c = s.backup_capture("b").unwrap();
        assert_eq!(c.generation, "gen-0002");
        assert_eq!(c.commit.seq, 1);
        assert_eq!(gens(&root), ["gen-0002"]);
        assert_eq!(leases(&s), 1);
        restore_and_compare(&s, &c, &dir.path().join("r"));
        drop(c);
        // a compaction after every lock: give up after the retry
        s.set_failpoint(
            "backup-capture-unlocked",
            Some(Arc::new(|s: &Store| s.compact().unwrap())),
        );
        assert!(matches!(s.backup_capture("b"), Err(Error::Conflict(_))));
        s.set_failpoint("backup-capture-unlocked", None);
        assert_eq!(leases(&s), 0);
        assert_eq!(gens(&root), ["gen-0004"]);
    }

    /// Captures under a concurrent writer: every capture restores to exactly its
    /// commit, and the writer lock is held briefly.
    #[test]
    fn captures_are_consistent_under_concurrent_writes() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("db");
        let s = Arc::new(Store::open(&root, StoreOptions::default()).unwrap());
        ins(&s, 0);
        let stop = Arc::new(AtomicBool::new(false));
        let n = Arc::new(AtomicU64::new(1));
        let writer = {
            let (s, stop, n) = (s.clone(), stop.clone(), n.clone());
            std::thread::spawn(move || {
                while !stop.load(Ordering::SeqCst) {
                    let i = n.fetch_add(1, Ordering::SeqCst);
                    ins(&s, i);
                    if i % 3 == 0 {
                        let j = i - 1;
                        upd(
                            &s,
                            &format!("DELETE DATA {{ <urn:s{j}> <urn:p> \"v{j}\" }}"),
                        );
                    }
                }
            })
        };
        let mut holds = Vec::new();
        let mut captures = Vec::new();
        for k in 0..200 {
            let c = s.backup_capture(&format!("c{k}")).unwrap();
            holds.push(c.lock_hold);
            if k % 20 == 0 {
                captures.push(c);
            }
            std::thread::sleep(Duration::from_micros(500));
        }
        stop.store(true, Ordering::SeqCst);
        writer.join().unwrap();
        assert!(s.head_commit().seq > captures.last().unwrap().commit.seq);
        let seqs: Vec<u64> = captures.iter().map(|c| c.commit.seq).collect();
        assert!(seqs.windows(2).all(|w| w[0] <= w[1]), "{seqs:?}");
        for (i, c) in captures.iter().enumerate() {
            restore_and_compare(&s, c, &dir.path().join(format!("r{i}")));
        }
        holds.sort();
        let p99 = holds[holds.len() * 99 / 100 - 1];
        assert!(p99 < Duration::from_millis(5), "p99 lock hold {p99:?}");
    }

    /// A compaction and a bulk commit while a backup reads the generation: the lease
    /// keeps it (shown in the history status) until dropped, then it is collected.
    #[test]
    fn a_lease_keeps_the_generation_across_compaction() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("db");
        let opts = StoreOptions {
            bulk_threshold: 2,
            ..Default::default()
        };
        let s = Store::open(&root, opts).unwrap();
        ins(&s, 1);
        ins(&s, 2);
        let c = s.backup_capture("nightly").unwrap();
        let h = s.history();
        assert_eq!(
            h.generations[0].held_by,
            [Hold::Head, Hold::Lease("nightly".into())]
        );
        ins(&s, 3);
        s.compact().unwrap();
        assert_eq!(gens(&root), ["gen-0001", "gen-0002"]);
        let h = s.history();
        let g1 = h.generations.iter().find(|g| g.name == "gen-0001").unwrap();
        assert_eq!(g1.held_by, [Hold::Lease("nightly".into())]);
        assert_eq!(g1.held_by[0].to_string(), "backup:nightly");
        assert!(h.bytes > 0);
        // a bulk commit while leased
        let nt: String = (0..20)
            .map(|i| format!("<urn:x{i}> <urn:p> <urn:y{i}> .\n"))
            .collect();
        s.load(&[crate::io::Source::from_bytes(
            nt.into_bytes(),
            crate::io::RdfFormat::NTriples,
            None,
        )])
        .unwrap();
        assert_eq!(gens(&root), ["gen-0001", "gen-0003"]);
        restore_and_compare(&s, &c, &dir.path().join("r"));
        drop(c);
        assert_eq!(gens(&root), ["gen-0003"]);
        assert_eq!(s.history().bytes, 0);
    }

    #[test]
    fn a_lease_dropped_while_the_writer_is_busy_is_collected_later() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("db");
        let s = Store::open(&root, StoreOptions::default()).unwrap();
        ins(&s, 1);
        let c = s.backup_capture("b").unwrap();
        s.compact().unwrap();
        {
            let _txn = s.write();
            drop(c);
        }
        assert_eq!(gens(&root), ["gen-0001", "gen-0002"]);
        assert!(s.history().generations[0].held_by.is_empty());
        s.try_collect_history();
        assert_eq!(gens(&root), ["gen-0002"]);
    }

    #[test]
    fn a_lease_outliving_its_store_does_not_collect() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("db");
        let s = Store::open(&root, StoreOptions::default()).unwrap();
        ins(&s, 1);
        let c = s.backup_capture("b").unwrap();
        s.compact().unwrap();
        drop(s);
        drop(c);
        assert_eq!(gens(&root), ["gen-0001", "gen-0002"]);
        // the next open collects it
        let s = Store::open(&root, StoreOptions::default()).unwrap();
        assert_eq!(gens(&root), ["gen-0002"]);
        assert_eq!(s.head_commit().seq, 1);
    }

    #[test]
    fn an_owned_capture_keeps_the_store_open() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("db");
        let s = Store::open(&root, StoreOptions::default()).unwrap();
        ins(&s, 1);
        let want = dump(&s);
        let c = s.into_backup_capture("offline").unwrap();
        assert!(Store::open(&root, StoreOptions::default()).is_err());
        let out = dir.path().join("r");
        c.write_to(&out).unwrap();
        drop(c);
        let r = Store::open(&out, StoreOptions::default()).unwrap();
        assert_eq!(dump(&r), want);
        assert!(Store::open(&root, StoreOptions::default()).is_ok());
    }

    /// A restore under a new identity: same commits and data, a new dataset id, the
    /// source recorded as `forkedFrom`, and the sequence continuing at s + 1.
    #[test]
    fn reidentify_makes_a_new_lineage() {
        use crate::commit::{CommitRange, ForkedFrom, reidentify};
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("db");
        let s = Store::open(&root, StoreOptions::default()).unwrap();
        for i in 0..3 {
            ins(&s, i);
        }
        s.create_snapshot("v2", &At::Commit(2), None).unwrap();
        s.compact().unwrap();
        ins(&s, 3);
        let c = s.backup_capture("b").unwrap();
        let out = dir.path().join("r");
        c.write_to(&out).unwrap();
        // a leased history.json too (backups leave it out; reidentify handles it)
        std::fs::copy(root.join("history.json"), out.join("history.json")).unwrap();
        let old = s.dataset_id();
        let new = uuid::Uuid::new_v4();
        let from = ForkedFrom {
            id: old,
            seq: c.commit.seq,
        };
        reidentify(&out, new, from).unwrap();
        let (id, _) = crate::commit::read_catalog(&out.join("commits.bin"))
            .unwrap()
            .unwrap();
        assert_eq!(id, new);
        let opts = crate::check::CheckOptions { quick: false };
        let report = crate::check::check(&out, &opts).unwrap();
        assert_ne!(report.status, crate::check::Status::Error);
        let r = Store::open(&out, StoreOptions::default()).unwrap();
        assert_eq!(r.dataset_id(), new);
        assert_eq!(r.forked_from(), Some(from));
        assert_eq!(r.head_commit(), c.commit);
        assert_eq!(dump(&r), dump_at(&s, c.commit.seq));
        let page = r.commits(CommitRange::Latest, 100);
        assert_eq!(page.commits.len(), 5);
        assert!(page.complete);
        assert_eq!(page.commits, s.commits(CommitRange::Latest, 100).commits);
        assert_eq!(r.named_snapshot("v2").unwrap().seq, 2);
        let info: serde_json::Value =
            serde_json::from_slice(&std::fs::read(out.join("dataset.json")).unwrap()).unwrap();
        assert_eq!(info["origin"], "restore");
        assert_eq!(info["id"], new.to_string());
        ins(&r, 9);
        assert_eq!(r.head_commit().seq, c.commit.seq + 1);
        let receipt = update(
            &r,
            "INSERT DATA { <urn:z> <urn:p> 0 }",
            &QueryOptions::default(),
        );
        assert!(receipt.is_ok());
        assert_eq!(r.head_commit().seq, c.commit.seq + 2);
        drop(r);
        // and it opens again as the new lineage
        let r = Store::open(&out, StoreOptions::default()).unwrap();
        assert_eq!(r.dataset_id(), new);
        assert_eq!(r.head_commit().seq, c.commit.seq + 2);
    }

    #[test]
    fn reidentify_refuses_a_foreign_catalog() {
        let dir = tempfile::tempdir().unwrap();
        let a = dir.path().join("a");
        let b = dir.path().join("b");
        drop(Store::open(&a, StoreOptions::default()).unwrap());
        drop(Store::open(&b, StoreOptions::default()).unwrap());
        std::fs::copy(b.join("commits.bin"), a.join("commits.bin")).unwrap();
        let from = crate::commit::ForkedFrom {
            id: uuid::Uuid::new_v4(),
            seq: 0,
        };
        let e = crate::commit::reidentify(&a, uuid::Uuid::new_v4(), from).unwrap_err();
        assert!(matches!(e, Error::Corrupt(_)), "{e}");
        assert!(
            crate::commit::reidentify(&dir.path().join("none"), uuid::Uuid::new_v4(), from)
                .is_err()
        );
    }

    #[test]
    fn restored_from_round_trips() {
        use crate::commit::{RestoredFrom, read_restored_from, set_restored_from};
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("db");
        let s = Store::open(&root, StoreOptions::default()).unwrap();
        ins(&s, 1);
        assert_eq!(s.restored_from(), None);
        let id = s.dataset_id();
        drop(s);
        assert_eq!(read_restored_from(&root).unwrap(), None);
        let from = RestoredFrom {
            repository: "offsite".into(),
            backup: "wiki-20260930t140311z".into(),
            dataset_id: uuid::Uuid::new_v4(),
            seq: 41,
        };
        set_restored_from(&root, &from).unwrap();
        let info: serde_json::Value =
            serde_json::from_slice(&std::fs::read(root.join("dataset.json")).unwrap()).unwrap();
        assert_eq!(info["restoredFrom"]["backup"], "wiki-20260930t140311z");
        assert_eq!(
            info["restoredFrom"]["datasetId"],
            from.dataset_id.to_string()
        );
        assert_eq!(read_restored_from(&root).unwrap(), Some(from.clone()));
        // a new identity keeps it
        let fork = crate::commit::ForkedFrom { id, seq: 1 };
        crate::commit::reidentify(&root, uuid::Uuid::new_v4(), fork).unwrap();
        let s = Store::open(&root, StoreOptions::default()).unwrap();
        assert_eq!(s.restored_from(), Some(from));
        assert_eq!(s.forked_from(), Some(fork));
    }
    #[cfg(unix)]
    fn checked_member_handoff_refuses_mutation(member: &str) {
        let temporary = tempfile::tempdir().unwrap();
        let root = temporary.path().join("source");
        let store = history_store(&root);
        ins(&store, 1);
        let preparation = capture_preparation();
        let room = preparation.room();
        let checked = store
            .configuration_capture("checked-member-handoff", &Default::default(), &preparation)
            .unwrap()
            .checked_primary(&preparation)
            .unwrap();
        let path = root.join(&checked.capture.data.generation).join(member);
        let original = std::fs::read(&path).unwrap();
        let mut value: serde_json::Value = serde_json::from_slice(&original).unwrap();
        let quads = if member == "meta.json" {
            &mut value["quads"]
        } else {
            &mut value["commit"]["quads"]
        };
        let before = quads.as_u64().unwrap();
        assert!(
            before < 9,
            "keep this genuine valid mutation the same length"
        );
        *quads = (before + 1).into();
        let changed = serde_json::to_vec_pretty(&value).unwrap();
        assert_ne!(changed, original);
        assert_eq!(changed.len(), original.len());
        // This keeps the captured FD and extent; it changes a known typed value
        // after the actual producer/basis check and before descriptor hashing.
        std::fs::write(&path, changed).unwrap();
        let result = checked.describe_native(&preparation);
        std::fs::write(&path, &original).unwrap();
        let refused = result.as_ref().err().is_some_and(|error| {
            error.code() == "evidence-unavailable"
                && error
                    .to_string()
                    .contains("checked primary member changed before descriptor")
        });
        drop(result);
        assert_eq!(leases(&store), 0);
        assert_eq!(preparation.room(), room);
        assert!(
            refused,
            "a descriptor must not mint a new pin for changed checked {member} bytes"
        );
    }

    #[cfg(unix)]
    #[test]
    fn genuine_checked_metadata_pin_survives_until_source_descriptor() {
        checked_member_handoff_refuses_mutation("meta.json");
    }

    #[cfg(unix)]
    #[test]
    fn genuine_checked_commit_pin_survives_until_source_descriptor() {
        checked_member_handoff_refuses_mutation("commit.json");
    }
}
