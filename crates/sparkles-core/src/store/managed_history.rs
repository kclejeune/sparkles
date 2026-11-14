//! Primary managed-configuration generations. Digests identify publications; they
//! never make a caller-selected snapshot/configuration pair authoritative.

use crate::access::{Preparation, Reservation};
use crate::evidence::{Settings, Sha256Digest};
use crate::history::{ConfigurationBinding, ConfigurationIdentity};
use crate::managed::{Installed, ManagedError, ManagedErrorKind, OwnerEntry, OwnerKind};
use crate::{Error, Result};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use std::collections::BTreeMap;
use std::fs::OpenOptions;
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use uuid::Uuid;

pub(crate) const READER: u32 = 8;
pub(crate) const FORMAT: u32 = 1;

/// Whether the files at `root` record an activation of configuration history,
/// whatever its registry says. Activation raises the dataset reader of every
/// branch to [`READER`] and publishes the lineage under `evidence-history/`,
/// and it runs on a copy that replaces the root only once it is complete. A
/// root with either record is therefore activated, and a registry that is
/// missing or names no history root must refuse rather than read as legacy.
pub(crate) fn activation_recorded(root: &Path) -> Result<bool> {
    if reader_records_activation(root)? {
        return Ok(true);
    }
    match std::fs::symlink_metadata(root.join(DIRECTORY)) {
        Ok(_) => Ok(true),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error.into()),
    }
}

/// Whether the dataset reader at `root` records an activation: a reader of
/// [`READER`] or more that nothing else explains. Reader 9 belongs to the durable job
/// ledger ([`crate::jobs::READER`]), and an interrupted enablement of jobs leaves that
/// reader with no `jobs.json`, which C26 §7 says is safe to resume, so reader 9 never
/// records an activation by itself and only the history directory does. A reader this
/// build does not know still refuses as activated.
fn reader_records_activation(root: &Path) -> Result<bool> {
    let Some(versions) = crate::commit::read_dataset_versions(root)? else {
        return Ok(false);
    };
    let reader = versions.minimum_reader;
    Ok(reader >= READER && reader != crate::jobs::READER)
}

/// Refuse an activated root whose registry no longer names its history root.
pub(crate) fn check_activated_registry(
    root: &Path,
    state: Option<&crate::managed::ManagedState>,
) -> Result<()> {
    let anchored = match state {
        Some(crate::managed::ManagedState::Installed(installed)) => {
            installed.configuration_history.is_some()
        }
        // An unavailable registry refuses what depends on it, but only a readable
        // history root keeps an activated family from reading as legacy.
        Some(crate::managed::ManagedState::Unavailable { .. }) => {
            match std::fs::read(root.join(crate::managed::FILE)) {
                Ok(bytes) => matches!(crate::managed::recorded_history(&bytes), Ok(Some(_))),
                Err(_) => false,
            }
        }
        None => false,
    };
    if !anchored && activation_recorded(root)? {
        return Err(unavailable(
            "activated configuration history has no published managed registry root",
        ));
    }
    Ok(())
}

/// Whether a family has activated configuration history. Activation is a
/// closed-dataset migration that publishes the registry's history root and
/// raises every branch's dataset reader to [`READER`]. Only then does the store
/// write version 2 logs, generation `commit.json` format 2 and `managed.json`
/// format 3, so a store that never activates keeps the versions that older
/// releases read. A registry that cannot be enforced still records activation in
/// the family's dataset reader.
pub(crate) fn activated(
    state: Option<&crate::managed::ManagedState>,
    family_root: Option<&Path>,
) -> bool {
    match state {
        Some(crate::managed::ManagedState::Installed(installed)) => {
            installed.configuration_history.is_some()
        }
        Some(crate::managed::ManagedState::Unavailable { .. }) => {
            family_root.is_some_and(|root| reader_records_activation(root).unwrap_or(false))
        }
        None => false,
    }
}
pub(crate) const MAX_BYTES: usize = 64 << 20;
const PARSE_BYTES: u64 = 32;
const DIRECTORY: &str = "evidence-history";

pub(super) mod capture;

#[cfg(test)]
mod tests;

/// Only managed.json's durable publication makes this root authoritative.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct Root {
    pub(crate) format: u32,
    pub(crate) lineage_id: Uuid,
    pub(crate) coverage: Sha256Digest,
    pub(crate) current_generation: Sha256Digest,
}

impl Root {
    pub(crate) fn identity(&self, dataset_id: Uuid) -> ConfigurationIdentity {
        ConfigurationIdentity {
            dataset_id,
            lineage_id: self.lineage_id,
            generation: self.current_generation,
        }
    }

    pub(crate) fn validate(&self) -> Result<()> {
        version(self.format)?;
        if self.lineage_id.is_nil() {
            return Err(unavailable("nil configuration lineage"));
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
enum EvidenceState {
    Absent,
    Disabled,
    Enabled,
}

/// Field order and UTF-8 JSON string encoding are part of generation format 1.
/// ownerSettings stores the exact settings bytes, without normalizing an old file.
#[derive(Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct Generation {
    format: u32,
    pub(crate) dataset_id: Uuid,
    pub(crate) lineage_id: Uuid,
    pub(crate) ordinal: u64,
    pub(crate) previous: Option<Sha256Digest>,
    pub(crate) registry_revision: u64,
    pub(crate) owners: BTreeMap<OwnerKind, OwnerEntry>,
    pub(crate) owner_settings: BTreeMap<OwnerKind, String>,
    evidence_state: EvidenceState,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct BranchCoverage {
    pub(crate) activation_head: u64,
    pub(crate) first_bound_own: u64,
}

#[derive(Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct Coverage {
    pub(crate) format: u32,
    pub(crate) dataset_id: Uuid,
    pub(crate) lineage_id: Uuid,
    pub(crate) activation_generation: Sha256Digest,
    pub(crate) branches: BTreeMap<Uuid, BranchCoverage>,
}

/// The decoded payload is destroyed before returning its preparation allowance.
pub(crate) struct Owned<T> {
    pub(crate) value: T,
    _held: Reservation,
}

pub(crate) struct Bytes {
    pub(crate) value: Vec<u8>,
    _held: Reservation,
}

pub(crate) struct Prepared {
    pub(crate) document: Owned<Generation>,
    pub(crate) bytes: Bytes,
    pub(crate) identity: ConfigurationIdentity,
}

/// Administrative loading must be able to reopen every valid published document,
/// including activation made with a larger caller preparation. This finite
/// bookkeeping allowance covers coverage plus one generation, raw/canonical
/// overlap and decoding. It does not preallocate those bytes. Controlled readers
/// and prepared mutations still use their caller's own, possibly smaller limit.
pub(crate) fn administrative_preparation() -> Result<Preparation> {
    administrative_preparation_with(crate::task::Control::none())
}

pub(crate) fn administrative_preparation_with(
    control: crate::task::Control,
) -> Result<Preparation> {
    let allowance = (MAX_BYTES as u64)
        .checked_mul(PARSE_BYTES * 2 + 8)
        .and_then(|bytes| bytes.checked_add(16 << 20))
        .ok_or_else(|| unavailable("configuration administrative allowance overflows"))?;
    Preparation::new(control, allowance)
}

/// Persistent linked ownership avoids copying every old generation on a change.
/// Every prepared payload retains its actual preparation credit through last drop.
pub(crate) struct MemoryHistory {
    pub(crate) prepared: Prepared,
    pub(crate) coverage: std::sync::Arc<Owned<Coverage>>,
    pub(crate) previous: Option<std::sync::Arc<MemoryHistory>>,
    pub(crate) retained_bytes: u64,
}

impl std::fmt::Debug for MemoryHistory {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MemoryHistory")
            .field("identity", &self.prepared.identity)
            .field("retained_bytes", &self.retained_bytes)
            .finish_non_exhaustive()
    }
}

impl Drop for MemoryHistory {
    fn drop(&mut self) {
        // Configuration changes are not bounded by the Rust stack depth. Shared
        // historical nodes remain with their actual holders, without recursive drop.
        let mut previous = self.previous.take();
        while let Some(node) = previous {
            match std::sync::Arc::try_unwrap(node) {
                Ok(mut node) => previous = node.previous.take(),
                Err(_) => break,
            }
        }
    }
}

impl MemoryHistory {
    pub(crate) fn initial(
        prepared: Prepared,
        coverage: Coverage,
        preparation: &Preparation,
    ) -> Result<(std::sync::Arc<Self>, Root)> {
        coverage.validate()?;
        let encoded = encode(&coverage, preparation)?;
        let digest = Sha256Digest::of_bytes(&encoded.value, preparation.control())?;
        let owned: Owned<Coverage> = decode(&encoded.value, preparation)?;
        let root = Root {
            format: FORMAT,
            lineage_id: prepared.identity.lineage_id,
            coverage: digest,
            current_generation: prepared.identity.generation,
        };
        let retained_bytes = prepared
            .document
            ._held
            .bytes()
            .checked_add(prepared.bytes._held.bytes())
            .and_then(|n| n.checked_add(owned._held.bytes()))
            .ok_or_else(|| unavailable("configuration memory size overflow"))?;
        Ok((
            std::sync::Arc::new(Self {
                prepared,
                coverage: std::sync::Arc::new(owned),
                previous: None,
                retained_bytes,
            }),
            root,
        ))
    }

    pub(crate) fn retained(&self) -> u64 {
        self.retained_bytes
    }

    pub(crate) fn next(
        prepared: Prepared,
        previous: std::sync::Arc<Self>,
    ) -> Result<std::sync::Arc<Self>> {
        if prepared.document.value.previous != Some(previous.prepared.identity.generation)
            || prepared.identity.dataset_id != previous.prepared.identity.dataset_id
            || prepared.identity.lineage_id != previous.prepared.identity.lineage_id
            || previous.prepared.document.value.ordinal.checked_add(1)
                != Some(prepared.document.value.ordinal)
            || previous
                .prepared
                .document
                .value
                .registry_revision
                .checked_add(1)
                != Some(prepared.document.value.registry_revision)
        {
            return Err(unavailable("in-memory configuration predecessor differs"));
        }
        let retained_bytes = previous
            .retained_bytes
            .checked_add(prepared.document._held.bytes())
            .and_then(|n| n.checked_add(prepared.bytes._held.bytes()))
            .ok_or_else(|| unavailable("configuration memory size overflow"))?;
        Ok(std::sync::Arc::new(Self {
            prepared,
            coverage: previous.coverage.clone(),
            previous: Some(previous),
            retained_bytes,
        }))
    }
}

pub(crate) fn unavailable(message: impl Into<String>) -> Error {
    ManagedError::error(ManagedErrorKind::EvidenceUnavailable, message)
}

fn version(format: u32) -> Result<()> {
    if format != FORMAT {
        return Err(Error::Unsupported(format!(
            "configuration history has format {format}; this release reads format {FORMAT}"
        )));
    }
    Ok(())
}

impl Generation {
    fn validate(&self, preparation: &Preparation) -> Result<()> {
        preparation.check()?;
        version(self.format)?;
        if self.dataset_id.is_nil()
            || self.lineage_id.is_nil()
            || self.ordinal == 0
            || (self.ordinal == 1) != self.previous.is_none()
            || self.owners.len() != self.owner_settings.len()
        {
            return Err(unavailable("incomplete configuration generation identity"));
        }
        let mut seen = std::collections::BTreeSet::new();
        for (owner, entry) in &self.owners {
            preparation.check()?;
            let settings = self
                .owner_settings
                .get(owner)
                .ok_or_else(|| unavailable("configuration owner settings missing"))?;
            if settings.len() > crate::managed::MAX_SETTINGS_BYTES
                || crate::managed::settings_digest(settings.as_bytes()) != entry.settings_digest
                || !owner.supports_profile(entry.profile)
                || entry.reserved().is_some()
            {
                return Err(unavailable("configuration owner settings differ"));
            }
            crate::managed::check_graphs(&entry.graphs)?;
            for graph in &entry.graphs {
                preparation.check()?;
                if !seen.insert(graph.as_str()) {
                    return Err(unavailable("configuration graph has duplicate owners"));
                }
            }
            for (graph, nonce) in &entry.protections {
                preparation.check()?;
                if nonce.is_nil() || !entry.graphs.contains(graph) {
                    return Err(unavailable(
                        "configuration protection does not name its graph",
                    ));
                }
            }
        }
        let observed = match self.owner_settings.get(&OwnerKind::Evidence) {
            None => EvidenceState::Absent,
            Some(bytes) => {
                let settings = Settings::from_slice(bytes.as_bytes(), preparation.control())?;
                if settings.enabled {
                    EvidenceState::Enabled
                } else {
                    EvidenceState::Disabled
                }
            }
        };
        if self.evidence_state != observed {
            return Err(unavailable("configuration evidence state differs"));
        }
        preparation.check()
    }

    pub(crate) fn matches_installed(&self, installed: &Installed) -> bool {
        self.registry_revision == installed.graphs.revision
            && self.owners == installed.graphs.owners
            && self.owner_settings.len() == installed.settings.len()
            && self.owner_settings.iter().all(|(owner, bytes)| {
                installed
                    .settings
                    .get(owner)
                    .is_some_and(|found| found.as_ref() == bytes.as_bytes())
            })
    }
}

impl Coverage {
    pub(crate) fn validate(&self) -> Result<()> {
        version(self.format)?;
        if self.dataset_id.is_nil() || self.lineage_id.is_nil() || self.branches.is_empty() {
            return Err(unavailable("incomplete configuration coverage identity"));
        }
        for (branch, coverage) in &self.branches {
            if branch.is_nil()
                || coverage.activation_head.checked_add(1) != Some(coverage.first_bound_own)
            {
                return Err(unavailable("invalid configuration coverage cutoff"));
            }
        }
        Ok(())
    }
}

/// Reserve both source/container copies before constructing the immutable document.
pub(crate) fn prepare(
    dataset_id: Uuid,
    lineage_id: Uuid,
    ordinal: u64,
    previous: Option<Sha256Digest>,
    installed: &Installed,
    preparation: &Preparation,
) -> Result<Prepared> {
    let mut estimate = 4096u64;
    for (owner, entry) in &installed.graphs.owners {
        preparation.check()?;
        let settings = installed
            .settings
            .get(owner)
            .ok_or_else(|| unavailable("configuration settings missing"))?;
        estimate = estimate
            .checked_add((settings.len() as u64).saturating_mul(PARSE_BYTES))
            .ok_or_else(|| preparation.exhausted())?;
        for graph in &entry.graphs {
            estimate = estimate
                .checked_add((graph.len() as u64).saturating_mul(4).saturating_add(512))
                .ok_or_else(|| preparation.exhausted())?;
        }
    }
    let held = preparation.reserve(estimate)?;
    let mut owner_settings = BTreeMap::new();
    for (owner, bytes) in &installed.settings {
        preparation.check()?;
        let text = std::str::from_utf8(bytes)
            .map_err(|_| unavailable("configuration settings are not UTF-8"))?;
        owner_settings.insert(*owner, text.to_owned());
    }
    let evidence_state = match owner_settings.get(&OwnerKind::Evidence) {
        None => EvidenceState::Absent,
        Some(bytes) => {
            if Settings::from_slice(bytes.as_bytes(), preparation.control())?.enabled {
                EvidenceState::Enabled
            } else {
                EvidenceState::Disabled
            }
        }
    };
    let document = Owned {
        value: Generation {
            format: FORMAT,
            dataset_id,
            lineage_id,
            ordinal,
            previous,
            registry_revision: installed.graphs.revision,
            owners: installed.graphs.owners.clone(),
            owner_settings,
            evidence_state,
        },
        _held: held,
    };
    document.value.validate(preparation)?;
    let bytes = encode(&document.value, preparation)?;
    let identity = ConfigurationIdentity {
        dataset_id,
        lineage_id,
        generation: Sha256Digest::of_bytes(&bytes.value, preparation.control())?,
    };
    Ok(Prepared {
        document,
        bytes,
        identity,
    })
}

struct Encoder<'a> {
    bytes: Bytes,
    preparation: &'a Preparation,
    failure: Option<Error>,
}

impl Write for Encoder<'_> {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        let result = (|| {
            self.preparation.check()?;
            let length = self
                .bytes
                .value
                .len()
                .checked_add(bytes.len())
                .filter(|&n| n <= MAX_BYTES)
                .ok_or_else(|| self.preparation.exhausted())?;
            if length > self.bytes.value.capacity() {
                let capacity = length
                    .max(self.bytes.value.capacity().saturating_mul(2))
                    .min(MAX_BYTES);
                self.bytes._held.grow_by(capacity as u64)?;
                self.bytes
                    .value
                    .try_reserve_exact(capacity - self.bytes.value.len())
                    .map_err(|_| self.preparation.exhausted())?;
                if self.bytes.value.capacity() != capacity {
                    return Err(self.preparation.exhausted());
                }
                self.bytes._held.release_to(capacity as u64);
            }
            self.bytes.value.extend_from_slice(bytes);
            Ok(())
        })();
        if let Err(error) = result {
            self.failure.get_or_insert(error);
            return Err(std::io::Error::other("configuration encoding refused"));
        }
        Ok(bytes.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

pub(crate) fn encode<T: Serialize>(value: &T, preparation: &Preparation) -> Result<Bytes> {
    encode_json(value, preparation, false)
}

pub(crate) fn encode_pretty<T: Serialize>(value: &T, preparation: &Preparation) -> Result<Bytes> {
    encode_json(value, preparation, true)
}

fn encode_json<T: Serialize>(value: &T, preparation: &Preparation, pretty: bool) -> Result<Bytes> {
    let mut writer = Encoder {
        bytes: Bytes {
            value: Vec::new(),
            _held: preparation.reserve(0)?,
        },
        preparation,
        failure: None,
    };
    let result = if pretty {
        serde_json::to_writer_pretty(&mut writer, value)
    } else {
        serde_json::to_writer(&mut writer, value)
    };
    if let Some(error) = writer.failure {
        return Err(error);
    }
    result.map_err(|_| unavailable("configuration metadata does not encode"))?;
    preparation.check()?;
    Ok(writer.bytes)
}

struct Decoder<'a> {
    bytes: &'a [u8],
    preparation: &'a Preparation,
    failure: Option<Error>,
}
impl Read for Decoder<'_> {
    fn read(&mut self, output: &mut [u8]) -> std::io::Result<usize> {
        if let Err(error) = self.preparation.check() {
            self.failure.get_or_insert(error);
            return Err(std::io::Error::other("configuration decoding refused"));
        }
        let length = output.len().min(self.bytes.len()).min(64 << 10);
        output[..length].copy_from_slice(&self.bytes[..length]);
        self.bytes = &self.bytes[length..];
        Ok(length)
    }
}

/// Canonical re-encoding also rejects duplicates inside nested map fields.
pub(crate) fn decode<T: DeserializeOwned + Serialize>(
    bytes: &[u8],
    preparation: &Preparation,
) -> Result<Owned<T>> {
    decode_with_version(bytes, preparation, version)
}

/// Keep each document type's version refusal ahead of body decoding. This
/// factors only the canonical admitted codec; it supplies no publication or
/// source authority. Existing generation/coverage callers still accept only1.
pub(crate) fn decode_with_version<T: DeserializeOwned + Serialize>(
    bytes: &[u8],
    preparation: &Preparation,
    validate_version: fn(u32) -> Result<()>,
) -> Result<Owned<T>> {
    if bytes.len() > MAX_BYTES {
        return Err(preparation.exhausted());
    }
    let amount = (bytes.len() as u64)
        .checked_mul(PARSE_BYTES)
        .and_then(|n| n.checked_add(4096))
        .ok_or_else(|| preparation.exhausted())?;
    let held = preparation.reserve(amount)?;
    #[derive(Deserialize)]
    struct Header {
        format: u32,
    }
    let mut header_reader = Decoder {
        bytes,
        preparation,
        failure: None,
    };
    let header: std::result::Result<Header, _> = serde_json::from_reader(&mut header_reader);
    if let Some(error) = header_reader.failure {
        return Err(error);
    }
    validate_version(
        header
            .map_err(|_| unavailable("configuration format is not a version"))?
            .format,
    )?;
    let mut reader = Decoder {
        bytes,
        preparation,
        failure: None,
    };
    let result = serde_json::from_reader(&mut reader);
    if let Some(error) = reader.failure {
        return Err(error);
    }
    let value: T = result.map_err(|_| unavailable("invalid configuration metadata"))?;
    let canonical = encode(&value, preparation)?;
    if canonical.value != bytes {
        return Err(unavailable(
            "configuration metadata is not its unique canonical encoding",
        ));
    }
    Ok(Owned { value, _held: held })
}

fn checked_directory(path: &Path) -> Result<()> {
    let metadata = std::fs::symlink_metadata(path)?;
    if !metadata.is_dir() || metadata.file_type().is_symlink() {
        return Err(unavailable(
            "configuration directory is not an ordinary directory",
        ));
    }
    Ok(())
}

pub(crate) fn directory(root: &Path, lineage: Uuid) -> Result<PathBuf> {
    let base = root.join(DIRECTORY);
    checked_directory(&base)?;
    let path = base.join(lineage.to_string());
    checked_directory(&path)?;
    Ok(path)
}

fn digest_name(digest: Sha256Digest) -> String {
    format!("{}.json", &digest.to_string()[7..])
}

/// Read one already anchored ordinary metadata FD. The caller owns directory
/// traversal and content-address authentication; this never reopens a name.
pub(crate) fn read_opened(
    mut file: std::fs::File,
    expected_length: Option<u64>,
    preparation: &Preparation,
) -> Result<Bytes> {
    preparation.check()?;
    let before = file.metadata()?;
    if !before.is_file()
        || before.len() > MAX_BYTES as u64
        || expected_length.is_some_and(|length| length != before.len())
    {
        return Err(unavailable("invalid original configuration metadata FD"));
    }
    file.seek(SeekFrom::Start(0))?;
    let length = usize::try_from(before.len()).map_err(|_| preparation.exhausted())?;
    let held = preparation.reserve(length as u64)?;
    let mut value = Vec::new();
    value
        .try_reserve_exact(length)
        .map_err(|_| preparation.exhausted())?;
    if value.capacity() != length {
        return Err(preparation.exhausted());
    }
    while value.len() < length {
        preparation.check()?;
        let start = value.len();
        value.resize((start + (64 << 10)).min(length), 0);
        file.read_exact(&mut value[start..])?;
    }
    let after = file.metadata()?;
    if before.len() != after.len() || before.modified()? != after.modified()? {
        return Err(unavailable(
            "original metadata changed while reading its FD",
        ));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if before.dev() != after.dev()
            || before.ino() != after.ino()
            || before.ctime() != after.ctime()
            || before.ctime_nsec() != after.ctime_nsec()
        {
            return Err(unavailable("original metadata FD identity changed"));
        }
    }
    preparation.check()?;
    Ok(Bytes { value, _held: held })
}

pub(crate) fn read_bytes(path: &Path, preparation: &Preparation) -> Result<Bytes> {
    read_bytes_synced(path, preparation, false)
}

fn read_bytes_synced(path: &Path, preparation: &Preparation, sync: bool) -> Result<Bytes> {
    preparation.check()?;
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
    }
    let mut file = options.open(path)?;
    let before = file.metadata()?;
    if !before.is_file() || before.len() > MAX_BYTES as u64 {
        return Err(unavailable("invalid configuration metadata file"));
    }
    let bytes = read_bytes_from(&mut file, before.len(), preparation)?;
    if sync {
        file.sync_all()?;
    }
    let after = file.metadata()?;
    let named = std::fs::symlink_metadata(path)?;
    if !named.is_file()
        || before.len() != after.len()
        || before.modified()? != after.modified()?
        || named.len() != after.len()
        || named.modified()? != after.modified()?
    {
        return Err(unavailable("configuration metadata changed while read"));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if before.dev() != after.dev()
            || before.ino() != after.ino()
            || before.ctime() != after.ctime()
            || before.ctime_nsec() != after.ctime_nsec()
            || named.dev() != after.dev()
            || named.ino() != after.ino()
        {
            return Err(unavailable(
                "configuration metadata FD/path identity changed",
            ));
        }
    }
    preparation.check()?;
    Ok(bytes)
}

/// Read a caller-selected already-owned source prefix into its real receiving
/// preparation. This byte owner alone confers no file or publication identity.
pub(crate) fn read_bytes_from<R: Read>(
    file: &mut R,
    length: u64,
    preparation: &Preparation,
) -> Result<Bytes> {
    preparation.check()?;
    if length > MAX_BYTES as u64 {
        return Err(unavailable("invalid configuration metadata extent"));
    }
    let length = usize::try_from(length).map_err(|_| preparation.exhausted())?;
    let held = preparation.reserve(length as u64)?;
    let mut value = Vec::new();
    value
        .try_reserve_exact(length)
        .map_err(|_| preparation.exhausted())?;
    if value.capacity() != length {
        return Err(preparation.exhausted());
    }
    while value.len() < length {
        preparation.check()?;
        let start = value.len();
        value.resize((start + (64 << 10)).min(length), 0);
        file.read_exact(&mut value[start..])?;
    }
    preparation.check()?;
    Ok(Bytes { value, _held: held })
}

pub(crate) fn read_generation(
    root: &Path,
    identity: ConfigurationIdentity,
    preparation: &Preparation,
) -> Result<Owned<Generation>> {
    let directory = directory(root, identity.lineage_id)?;
    let path = directory
        .join("generations")
        .join(digest_name(identity.generation));
    checked_directory(&directory.join("generations"))?;
    let bytes = read_bytes(&path, preparation)?;
    generation_bytes(&bytes.value, identity, preparation)
}

/// Shared validation for already admitted original metadata. This establishes
/// publication syntax and identity, not authority to replay an imported seal.
pub(crate) fn generation_bytes(
    bytes: &[u8],
    identity: ConfigurationIdentity,
    preparation: &Preparation,
) -> Result<Owned<Generation>> {
    if Sha256Digest::of_bytes(bytes, preparation.control())? != identity.generation {
        return Err(unavailable("configuration generation hash differs"));
    }
    let document: Owned<Generation> = decode(bytes, preparation)?;
    document.value.validate(preparation)?;
    if document.value.dataset_id != identity.dataset_id
        || document.value.lineage_id != identity.lineage_id
    {
        return Err(unavailable(
            "configuration generation belongs to another lineage",
        ));
    }
    Ok(document)
}

fn immutable_file(path: &Path, bytes: &[u8], preparation: &Preparation) -> Result<()> {
    let mut options = OpenOptions::new();
    options.create_new(true).write(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600).custom_flags(libc::O_NOFOLLOW);
    }
    match options.open(path) {
        Ok(mut file) => {
            for chunk in bytes.chunks(64 << 10) {
                preparation.check()?;
                file.write_all(chunk)?;
            }
            file.sync_all()?;
            super::sync_dir(path.parent().expect("configuration file has a parent"))?;
        }
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            // An earlier preparation may have failed before its sync. Read,
            // sync and verify the same nonblocking, no-follow regular-file FD;
            // never reopen an unchecked replacement after proving equality.
            if read_bytes_synced(path, preparation, true)?.value != bytes {
                return Err(unavailable("immutable configuration file already differs"));
            }
            super::sync_dir(path.parent().expect("configuration file has a parent"))?;
        }
        Err(error) => return Err(error.into()),
    }
    preparation.check()
}

pub(crate) fn create_directory(root: &Path, lineage: Uuid) -> Result<PathBuf> {
    let mut parent = root.to_path_buf();
    for part in [
        DIRECTORY.to_string(),
        lineage.to_string(),
        "generations".to_string(),
    ] {
        let next = parent.join(part);
        match std::fs::create_dir(&next) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                checked_directory(&next)?
            }
            Err(error) => return Err(error.into()),
        }
        // Also closes an earlier failed preparation's unsynced directory creation.
        super::sync_dir(&parent)?;
        parent = next;
    }
    Ok(parent.parent().expect("lineage directory").to_path_buf())
}

pub(crate) fn persist(root: &Path, prepared: &Prepared, preparation: &Preparation) -> Result<()> {
    let directory = create_directory(root, prepared.identity.lineage_id)?;
    immutable_file(
        &directory
            .join("generations")
            .join(digest_name(prepared.identity.generation)),
        &prepared.bytes.value,
        preparation,
    )
}

pub(crate) fn persist_coverage(
    root: &Path,
    coverage: &Coverage,
    preparation: &Preparation,
) -> Result<Sha256Digest> {
    coverage.validate()?;
    let directory = create_directory(root, coverage.lineage_id)?;
    let encoded = encode(coverage, preparation)?;
    let digest = Sha256Digest::of_bytes(&encoded.value, preparation.control())?;
    immutable_file(
        &directory.join("coverage.json"),
        &encoded.value,
        preparation,
    )?;
    Ok(digest)
}

pub(crate) fn read_coverage(
    root: &Path,
    dataset_id: Uuid,
    history: &Root,
    preparation: &Preparation,
) -> Result<Owned<Coverage>> {
    history.validate()?;
    let bytes = read_bytes(
        &directory(root, history.lineage_id)?.join("coverage.json"),
        preparation,
    )?;
    coverage_bytes(&bytes.value, dataset_id, history, preparation)
}

pub(crate) fn coverage_bytes(
    bytes: &[u8],
    dataset_id: Uuid,
    history: &Root,
    preparation: &Preparation,
) -> Result<Owned<Coverage>> {
    history.validate()?;
    if Sha256Digest::of_bytes(bytes, preparation.control())? != history.coverage {
        return Err(unavailable("configuration coverage hash differs"));
    }
    let coverage: Owned<Coverage> = decode(bytes, preparation)?;
    coverage.value.validate()?;
    if coverage.value.dataset_id != dataset_id || coverage.value.lineage_id != history.lineage_id {
        return Err(unavailable(
            "configuration coverage belongs to another lineage",
        ));
    }
    Ok(coverage)
}

/// Verify the entire published chain, not an orphan filename or the newest ordinal.
pub(crate) fn resolve(
    root: &Path,
    dataset_id: Uuid,
    history: &Root,
    wanted: ConfigurationIdentity,
    preparation: &Preparation,
) -> Result<Owned<Generation>> {
    resolve_using(
        dataset_id,
        history,
        wanted,
        preparation,
        || read_coverage(root, dataset_id, history, preparation),
        |identity| read_generation(root, identity, preparation),
    )
}

/// Both loaders must validate the exact bytes they return using the helpers
/// above. They borrow the caller's actual Preparation synchronously; no source
/// or physical/replay authority is conferred by this common chain walk.
pub(crate) fn resolve_using(
    dataset_id: Uuid,
    history: &Root,
    wanted: ConfigurationIdentity,
    preparation: &Preparation,
    mut coverage: impl FnMut() -> Result<Owned<Coverage>>,
    mut generation: impl FnMut(ConfigurationIdentity) -> Result<Owned<Generation>>,
) -> Result<Owned<Generation>> {
    if wanted.dataset_id != dataset_id || wanted.lineage_id != history.lineage_id {
        return Err(unavailable("foreign configuration identity"));
    }
    let coverage = coverage()?;
    let mut current = history.identity(dataset_id);
    let mut expected_ordinal = None;
    let mut expected_revision = None;
    let mut found = false;
    loop {
        preparation.check()?;
        let document = generation(current)?;
        if expected_ordinal.is_some_and(|ordinal| ordinal != document.value.ordinal) {
            return Err(unavailable("configuration predecessor ordinal differs"));
        }
        if expected_revision.is_some_and(|revision| revision != document.value.registry_revision) {
            return Err(unavailable(
                "configuration predecessor registry revision differs",
            ));
        }
        let previous = document.value.previous;
        let ordinal = document.value.ordinal;
        if current == wanted {
            found = true;
        }
        match previous {
            Some(previous) => {
                expected_ordinal = Some(ordinal - 1);
                expected_revision = Some(
                    document
                        .value
                        .registry_revision
                        .checked_sub(1)
                        .ok_or_else(|| unavailable("configuration registry revision underflow"))?,
                );
                current.generation = previous;
            }
            None => {
                if current.generation != coverage.value.activation_generation || ordinal != 1 {
                    return Err(unavailable("configuration genesis differs from activation"));
                }
                break;
            }
        }
    }
    if !found {
        return Err(unavailable(
            "configuration was not published in this lineage",
        ));
    }
    // Hold at most one decoded predecessor during verification. The immutable
    // selected body is hash-checked again rather than retaining a whole second
    // generation across the walk.
    generation(wanted)
}

pub(crate) fn binding(identity: ConfigurationIdentity) -> ConfigurationBinding {
    ConfigurationBinding::Known {
        configuration: identity,
    }
}

/// Decode only the selected in-memory publication into the requesting owner.
/// Retaining a view does not accidentally retain every predecessor payload after
/// its Store is dropped; the selected body keeps the caller's own preparation.
pub(crate) fn resolve_memory(
    published: &MemoryHistory,
    identity: ConfigurationIdentity,
    preparation: &Preparation,
) -> Result<Owned<Generation>> {
    let mut node = Some(published);
    loop {
        preparation.check()?;
        let current =
            node.ok_or_else(|| unavailable("configuration was not published in memory"))?;
        if current.prepared.identity == identity {
            if Sha256Digest::of_bytes(&current.prepared.bytes.value, preparation.control())?
                != identity.generation
            {
                return Err(unavailable("in-memory configuration bytes differ"));
            }
            let copied: Owned<Generation> = decode(&current.prepared.bytes.value, preparation)?;
            copied.value.validate(preparation)?;
            if copied.value.dataset_id != identity.dataset_id
                || copied.value.lineage_id != identity.lineage_id
            {
                return Err(unavailable(
                    "in-memory configuration belongs to another lineage",
                ));
            }
            return Ok(copied);
        }
        node = current.previous.as_deref();
    }
}

/// A closed-copy activation input. Each entry owns its path storage allowance.
pub(crate) struct ActivationBranch {
    pub(crate) root: PathBuf,
    pub(crate) id: Uuid,
    pub(crate) head: u64,
    hot: PathBuf,
    _held: Reservation,
}

pub(crate) fn activation_needed(root: &Path, preparation: &Preparation) -> Result<bool> {
    preparation.check()?;
    match super::managed::load_dir(root) {
        Some(crate::managed::ManagedState::Installed(installed)) => {
            Ok(installed.configuration_history.is_none())
        }
        Some(crate::managed::ManagedState::Unavailable { reason, .. }) => Err(unavailable(reason)),
        None => Ok(true),
    }
}

fn activation_branch(root: &Path, preparation: &Preparation) -> Result<ActivationBranch> {
    let held = preparation.reserve(
        (root.as_os_str().len() as u64)
            .saturating_mul(16)
            .saturating_add(8192),
    )?;
    let current = read_bytes(&root.join("CURRENT"), preparation)?;
    if current.value.len() > 256 {
        return Err(unavailable("activation CURRENT is too long"));
    }
    let name = std::str::from_utf8(&current.value)
        .map_err(|_| unavailable("activation CURRENT is not UTF-8"))?
        .trim();
    if !name
        .strip_prefix("gen-")
        .is_some_and(|digits| !digits.is_empty() && digits.bytes().all(|b| b.is_ascii_digit()))
    {
        return Err(unavailable(
            "activation CURRENT is not a generation component",
        ));
    }
    let generation = root.join(name);
    checked_directory(&generation)?;
    let hot = generation.join("wal.log");
    drop(current);
    // Activation never rewrites a live headerless stream or its original offsets.
    // The supported compaction path must first give it a framed current log.
    let version = super::wal::file_version(&hot)?;
    if version == 0 {
        return Err(Error::Unsupported("configuration-history activation needs a framed current WAL; compact the closed dataset's store normally first".into()));
    }
    let allowance = preparation.room();
    let _decode = preparation.reserve(allowance)?;
    let options = crate::history::HistoryOptions {
        cancel: Some(preparation.control().cancel.flag()),
        deadline: preparation.control().deadline,
    };
    let limit = crate::history::ReadLimit {
        options: &options,
        memory: Some(allowance),
        held: 0,
    };
    let mut budget = super::cold::ColdBudget::new(limit)?;
    let id = crate::commit::read_dataset_file_controlled(root, &mut budget)?
        .ok_or_else(|| unavailable("activation branch has no dataset identity"))?;
    let physical = crate::commit::read_gen_commit_controlled(&generation, &mut budget)?
        .ok_or_else(|| unavailable("activation current generation has no commit identity"))?;
    if physical.value.dataset_id != id {
        return Err(unavailable("activation generation has a foreign identity"));
    }
    if physical.value.configuration != ConfigurationBinding::LegacyUnbound {
        return Err(unavailable(
            "activation found an unpublished generation binding",
        ));
    }
    let mut head = physical.value.commit.seq;
    let from = super::wal::WalPoint {
        seq: head,
        offset: 0,
    };
    let cursor_limit = budget.owned_limit()?;
    let mut cursor = super::wal::WalCursor::open_limited(&hot, from, &cursor_limit, 0)?;
    while let Some((seq, _, _)) = cursor.next_limited(&cursor_limit, 0)? {
        if cursor.configuration().is_some() {
            return Err(unavailable(
                "activation found an unpublished WAL configuration binding",
            ));
        }
        head = seq;
    }
    head.checked_add(1)
        .ok_or_else(|| unavailable("activation cutoff overflows"))?;
    // Activation covers the physical branch, including pinned/retired
    // generations. It must not bless a previously unanchored reference merely
    // because compaction moved that reference out of CURRENT.
    drop(cursor);
    drop(physical);
    drop(_decode);
    activation_retained_generations(root, id, preparation)?;
    Ok(ActivationBranch {
        root: root.to_path_buf(),
        id,
        head,
        hot,
        _held: held,
    })
}

fn activation_retained_generations(root: &Path, id: Uuid, preparation: &Preparation) -> Result<()> {
    use super::{WAL_COMMIT, WAL_CONFIGURATION, WAL_HEADER, WAL_REC};
    use std::io::{BufReader, Seek, SeekFrom};

    for entry in std::fs::read_dir(root)? {
        preparation.check()?;
        let entry = entry?;
        let name = entry.file_name();
        let Some(name) = name.to_str() else { continue };
        if !name.strip_prefix("gen-").is_some_and(|digits| {
            !digits.is_empty() && digits.bytes().all(|byte| byte.is_ascii_digit())
        }) {
            continue;
        }
        if !entry.file_type()?.is_dir() {
            return Err(unavailable("activation generation is not a directory"));
        }
        let _scratch = preparation.reserve(
            (root.as_os_str().len() as u64)
                .saturating_add(name.len() as u64)
                .saturating_mul(8)
                .saturating_add((64 << 10) + 4096),
        )?;
        let directory = entry.path();
        let allowance = preparation.room();
        {
            let _decode = preparation.reserve(allowance)?;
            let options = crate::history::HistoryOptions {
                cancel: Some(preparation.control().cancel.flag()),
                deadline: preparation.control().deadline,
            };
            let mut budget = super::cold::ColdBudget::new(crate::history::ReadLimit {
                options: &options,
                memory: Some(allowance),
                held: 0,
            })?;
            let physical = crate::commit::read_gen_commit_controlled(&directory, &mut budget)?;
            if let Some(physical) = &physical
                && (physical.value.dataset_id != id
                    || physical.value.configuration != ConfigurationBinding::LegacyUnbound)
            {
                return Err(unavailable(
                    "activation found a foreign or unpublished retained generation binding",
                ));
            }
        }
        let path = directory.join("wal.log");
        let mut options = OpenOptions::new();
        options.read(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
        }
        let mut file = match options.open(&path) {
            Ok(file) => file,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => return Err(error.into()),
        };
        if !file.metadata()?.is_file() {
            return Err(unavailable("activation retained WAL is not a regular file"));
        }
        // Reuse the version and seal decoders. This is a negative preflight,
        // not a replacement recovery parser or a reconstructed commit catalog.
        super::wal::descriptor_version(&mut file, &path)?;
        file.seek(SeekFrom::Start(0))?;
        let mut reader = BufReader::with_capacity(64 << 10, file);
        let mut crc = flate2::Crc::new();
        let mut record = [0; WAL_REC];
        let mut first = true;
        loop {
            preparation.check()?;
            match reader.read_exact(&mut record) {
                Ok(()) => {}
                Err(error) if error.kind() == std::io::ErrorKind::UnexpectedEof => break,
                Err(error) => return Err(error.into()),
            }
            if record[0] == WAL_CONFIGURATION {
                return Err(unavailable(
                    "activation found unpublished retained WAL configuration data",
                ));
            }
            if record[0] == WAL_COMMIT {
                if let Some(Ok((_, _, _, flags))) =
                    crate::commit::open_wal_commit_with_crc(&record, crc)
                    && flags & crate::commit::WAL_FLAG_CONFIGURATION != 0
                {
                    return Err(unavailable(
                        "activation found an unpublished retained WAL configuration flag",
                    ));
                }
                crc = flate2::Crc::new();
            } else if !(first && record[0] == WAL_HEADER) {
                crc.update(&record);
            }
            first = false;
        }
    }
    preparation.check()
}

/// Inventory all physical branch roots, including retired roots. This reads only;
/// publication invokes it again under the migration and original database locks.
pub(crate) fn activation_inventory(
    root: &Path,
    preparation: &Preparation,
) -> Result<crate::access::PreparationBuffer<ActivationBranch>> {
    let mut output = crate::access::PreparationBuffer::new(preparation)?;
    output.push(activation_branch(root, preparation)?)?;
    let entries = match std::fs::read_dir(root.join("branches")) {
        Ok(entries) => Some(entries),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
        Err(error) => return Err(error.into()),
    };
    if let Some(entries) = entries {
        for entry in entries {
            preparation.check()?;
            let entry = entry?;
            let kind = entry.file_type()?;
            if kind.is_symlink() {
                return Err(unavailable("activation branch tree contains a symlink"));
            }
            if !kind.is_dir() {
                continue;
            }
            let name = entry.file_name();
            // Use the same ordinary physical identity test as the format walker;
            // unfinished branch scratch is not a published dataset.
            if crate::formats::transient_branch_directory(&name.to_string_lossy()) {
                continue;
            }
            let _path = preparation.reserve(
                (root.as_os_str().len() as u64)
                    .saturating_mul(4)
                    .saturating_add(4096),
            )?;
            output.push(activation_branch(&entry.path(), preparation)?)?;
        }
    }
    Ok(output)
}

/// Called only on the migration copy. The old root remains locked and untouched
/// until this copy passes ordinary check and the existing recovery mark is durable.
pub(crate) fn activate_copy(
    root: &Path,
    preparation: &Preparation,
    hook: &mut dyn FnMut(&str) -> Result<()>,
) -> Result<()> {
    let state = super::managed::load_dir(root);
    let mut installed = match state {
        Some(crate::managed::ManagedState::Installed(installed)) => installed,
        Some(crate::managed::ManagedState::Unavailable { reason, .. }) => {
            return Err(unavailable(reason));
        }
        None => Installed::new(
            crate::managed::ManagedGraphs {
                revision: 0,
                owners: BTreeMap::new(),
            },
            BTreeMap::new(),
        )?,
    };
    if installed.configuration_history.is_some() {
        return Ok(());
    }
    let branches = activation_inventory(root, preparation)?;
    let id = branches[0].id;
    let prepared = prepare(id, Uuid::new_v4(), 1, None, &installed, preparation)?;
    let _nodes = preparation.reserve((branches.len() as u64).saturating_mul(256))?;
    let coverage = Coverage {
        format: FORMAT,
        dataset_id: id,
        lineage_id: prepared.identity.lineage_id,
        activation_generation: prepared.identity.generation,
        branches: branches
            .iter()
            .map(|branch| {
                (
                    branch.id,
                    BranchCoverage {
                        activation_head: branch.head,
                        first_bound_own: branch.head + 1,
                    },
                )
            })
            .collect(),
    };
    if coverage.branches.len() != branches.len() {
        return Err(unavailable("activation repeats a branch identity"));
    }
    for branch in branches.iter() {
        crate::commit::require_reader(&branch.root, READER)?;
    }
    persist(root, &prepared, preparation)?;
    hook("configuration-generation-written")?;
    let digest = persist_coverage(root, &coverage, preparation)?;
    hook("configuration-coverage-written")?;
    for branch in branches.iter() {
        use std::io::{Seek, SeekFrom};
        preparation.check()?;
        let mut options = OpenOptions::new();
        options.read(true).write(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.custom_flags(libc::O_NOFOLLOW);
        }
        let mut file = options.open(&branch.hot)?;
        let version = super::wal::descriptor_version(&mut file, &branch.hot)?;
        if version == 1 {
            file.seek(SeekFrom::Start(0))?;
            file.write_all(&super::wal::configuration_header_record())?;
            file.sync_all()?;
        } else if version != crate::formats::WAL_VERSION
            && version != crate::formats::WAL_CONTROL_VERSION
        {
            // version 3 already carries configuration records (spec C26 §7)
            return Err(unavailable("activation WAL version changed"));
        }
    }
    hook("configuration-current-logs-upgraded")?;
    installed.configuration_history = Some(Root {
        format: FORMAT,
        lineage_id: prepared.identity.lineage_id,
        coverage: digest,
        current_generation: prepared.identity.generation,
    });
    let _registry = preparation.reserve(
        (prepared.bytes.value.len() as u64)
            .saturating_mul(4)
            .saturating_add(4096),
    )?;
    super::write_atomic(&root.join(crate::managed::FILE), &installed.file_bytes())?;
    super::sync_dir(root)?;
    hook("configuration-root-written")?;
    Ok(())
}
