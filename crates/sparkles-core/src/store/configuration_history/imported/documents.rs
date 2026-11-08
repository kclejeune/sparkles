//! Closed original metadata, not authority to replay a foreign primary seal.
//! Retained documents use the caller's actual Preparation. A cold temporary loan
//! may use them synchronously only and must drop them before releasing its loan.
use super::*;
use crate::access::Reservation;
use std::fs::{File, OpenOptions};

#[cfg(all(test, unix))]
mod tests;

pub(super) struct Opened {
    file: File,
    _held: Reservation,
}

impl Opened {
    fn header(preparation: &Preparation) -> Result<Reservation> {
        preparation.reserve((std::mem::size_of::<Self>() as u64).saturating_add(256))
    }

    #[cfg(unix)]
    pub(super) fn root(path: &Path, preparation: &Preparation) -> Result<Self> {
        use std::os::unix::fs::OpenOptionsExt;
        let held = Self::header(preparation)?;
        let _path = preparation.reserve((path.as_os_str().len() as u64).saturating_add(1))?;
        let file = OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC)
            .open(path)?;
        if !file.metadata()?.is_dir() {
            return Err(managed_history::unavailable(
                "original metadata root is not a directory",
            ));
        }
        Ok(Self { file, _held: held })
    }

    #[cfg(not(unix))]
    pub(super) fn root(_: &Path, preparation: &Preparation) -> Result<Self> {
        preparation.check()?;
        Err(Error::Unsupported(
            "anchored imported metadata needs its platform reader".into(),
        ))
    }

    #[cfg(unix)]
    pub(super) fn child(
        &self,
        name: &str,
        directory: bool,
        preparation: &Preparation,
    ) -> Result<Self> {
        use std::os::fd::{AsRawFd, FromRawFd};
        if name.is_empty()
            || matches!(name, "." | "..")
            || !name
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-' | b'_'))
        {
            return Err(managed_history::unavailable(
                "invalid closed metadata component",
            ));
        }
        let held = Self::header(preparation)?;
        let _name = preparation.reserve((name.len() as u64).saturating_add(1))?;
        let name = std::ffi::CString::new(name)
            .map_err(|_| managed_history::unavailable("invalid metadata component"))?;
        let flags = libc::O_RDONLY
            | libc::O_NOFOLLOW
            | libc::O_NONBLOCK
            | libc::O_CLOEXEC
            | if directory { libc::O_DIRECTORY } else { 0 };
        // SAFETY: the borrowed directory FD and NUL-terminated CString remain
        // live for this syscall. No pointer or borrowed FD escapes the call.
        let fd = unsafe { libc::openat(self.file.as_raw_fd(), name.as_ptr(), flags) };
        if fd < 0 {
            return Err(std::io::Error::last_os_error().into());
        }
        // SAFETY: openat returned this new owned FD; File becomes its sole owner
        // and closes it exactly once on every success/refusal/error path.
        let file = unsafe { File::from_raw_fd(fd) };
        let metadata = file.metadata()?;
        if if directory {
            !metadata.is_dir()
        } else {
            !metadata.is_file()
        } {
            return Err(managed_history::unavailable(
                "original metadata member has the wrong kind",
            ));
        }
        Ok(Self { file, _held: held })
    }

    #[cfg(not(unix))]
    pub(super) fn child(&self, _: &str, _: bool, preparation: &Preparation) -> Result<Self> {
        preparation.check()?;
        Err(Error::Unsupported(
            "anchored imported metadata needs its platform reader".into(),
        ))
    }

    fn relative(&self, path: &str, directory: bool, preparation: &Preparation) -> Result<Self> {
        if path.is_empty() {
            return Err(managed_history::unavailable("empty original metadata path"));
        }
        let held = Self::header(preparation)?;
        let mut current = Self {
            file: self.file.try_clone()?,
            _held: held,
        };
        let mut components = path.split('/').peekable();
        while let Some(component) = components.next() {
            preparation.check()?;
            current = current.child(
                component,
                components.peek().is_some() || directory,
                preparation,
            )?;
        }
        Ok(current)
    }

    pub(super) fn bytes(
        self,
        length: Option<u64>,
        preparation: &Preparation,
    ) -> Result<managed_history::Bytes> {
        let Self { file, _held } = self;
        let result = managed_history::read_opened(file, length, preparation);
        drop(_held);
        result
    }
}

/// A candidate's original metadata closure. This cannot mint a bound view,
/// imported reference exemption or Store/capture authority.
pub(super) struct Documents {
    descriptor: managed_history::Owned<Descriptor>,
    origin: managed_history::Owned<Origin>,
    directory: Opened,
}

impl Documents {
    pub(super) fn open(
        root: &Path,
        source: Sha256Digest,
        preparation: &Preparation,
    ) -> Result<Self> {
        let destination = Opened::root(root, preparation)?;
        let native = destination
            .child("dataset.json", false, preparation)?
            .bytes(None, preparation)?;
        let dataset = dataset_id(&native.value, preparation)?;
        drop(native);
        let encoded = destination
            .relative(ROOT_FILE, false, preparation)?
            .bytes(None, preparation)?;
        let origin = Origin::decode(&encoded.value, preparation)?;
        drop(encoded);
        if origin.value.dataset_id != dataset
            || origin
                .value
                .sources
                .binary_search_by(|id| id.as_bytes().cmp(source.as_bytes()))
                .is_err()
        {
            return Err(managed_history::unavailable(
                "import source is not in this destination origin",
            ));
        }
        let imports = destination.relative("evidence-history/imports", true, preparation)?;
        let mut hex = [b'0'; 64];
        for (index, byte) in source.as_bytes().iter().enumerate() {
            hex[index * 2] = b"0123456789abcdef"[(byte >> 4) as usize];
            hex[index * 2 + 1] = b"0123456789abcdef"[(byte & 15) as usize];
        }
        let name = std::str::from_utf8(&hex).expect("lowercase ASCII hex");
        let directory = imports.child(name, true, preparation)?;
        let encoded = directory
            .child("origin.json", false, preparation)?
            .bytes(None, preparation)?;
        if Sha256Digest::of_bytes(&encoded.value, preparation.control())? != source {
            return Err(managed_history::unavailable(
                "original descriptor digest differs",
            ));
        }
        let descriptor = Descriptor::decode(&encoded.value, preparation)?;
        drop(encoded);
        if descriptor.value.sources.iter().any(|id| {
            origin
                .value
                .sources
                .binary_search_by(|value| value.as_bytes().cmp(id.as_bytes()))
                .is_err()
        }) {
            return Err(managed_history::unavailable(
                "original source closure is incomplete",
            ));
        }
        let documents = Self {
            descriptor,
            origin,
            directory,
        };
        for path in documents.descriptor.value.files.keys() {
            preparation.check()?;
            drop(documents.read(path, preparation)?);
        }
        documents.identities(preparation)?;
        Ok(documents)
    }

    /// Recheck the descriptor's exact length/hash on every original metadata use.
    fn read(&self, path: &str, preparation: &Preparation) -> Result<managed_history::Bytes> {
        let member = self
            .descriptor
            .value
            .files
            .get(path)
            .ok_or_else(|| managed_history::unavailable("original document is not declared"))?;
        if !source_member(path) {
            return Err(managed_history::unavailable(
                "invalid original document path",
            ));
        }
        let directory = self.directory.child("source", true, preparation)?;
        let bytes = directory
            .relative(path, false, preparation)?
            .bytes(Some(member.bytes), preparation)?;
        if Sha256Digest::of_bytes(&bytes.value, preparation.control())? != member.sha256 {
            return Err(managed_history::unavailable(
                "original document digest differs",
            ));
        }
        Ok(bytes)
    }

    fn identities(&self, preparation: &Preparation) -> Result<()> {
        let family = self.read("family-dataset.json", preparation)?;
        if dataset_id(&family.value, preparation)? != self.descriptor.value.family_id {
            return Err(managed_history::unavailable(
                "original publisher family differs",
            ));
        }
        drop(family);
        for (name, root) in &self.descriptor.value.roots {
            let prefix = if name == "main" { "" } else { "branches/" };
            let _names =
                preparation.reserve((name.len() as u64).saturating_mul(4).saturating_add(512))?;
            let base = if prefix.is_empty() {
                String::new()
            } else {
                format!("{prefix}{name}/")
            };
            let bytes = self.read(&format!("{base}dataset.json"), preparation)?;
            if dataset_id(&bytes.value, preparation)? != root.logical_id {
                return Err(managed_history::unavailable(
                    "original logical dataset differs",
                ));
            }
            drop(bytes);
            let current = self.read(&format!("{base}CURRENT"), preparation)?;
            if std::str::from_utf8(&current.value).ok().map(str::trim)
                != Some(root.generation.as_str())
            {
                return Err(managed_history::unavailable("original CURRENT differs"));
            }
            drop(current);
            if root.logical_id != self.descriptor.value.family_id {
                let branch = self.read(&format!("{base}branch.json"), preparation)?;
                branch_identity(
                    &branch.value,
                    self.descriptor.value.family_id,
                    root.logical_id,
                    root.head,
                    preparation,
                )?;
            }
        }
        preparation.check()
    }

    /// Authenticate a candidate's original publication through its declared
    /// small documents. This grants no physical/replay or capture authority.
    fn publication(
        &self,
        wanted: ConfigurationIdentity,
        preparation: &Preparation,
    ) -> Result<managed_history::Owned<managed_history::Generation>> {
        let registry = self.read(crate::managed::FILE, preparation)?;
        let history = primary::recorded_root_bytes(&registry.value, preparation)?
            .ok_or_else(|| managed_history::unavailable("original publication root is missing"))?;
        drop(registry);
        // The lineage UUID and digest have fixed canonical encodings. Hold the
        // temporary formatting/storage allowance before either path allocation.
        let _paths = preparation.reserve(1024)?;
        let lineage = format!("evidence-history/{}", history.lineage_id);
        managed_history::resolve_using(
            self.descriptor.value.family_id,
            &history,
            wanted,
            preparation,
            || {
                let bytes = self.read(&format!("{lineage}/coverage.json"), preparation)?;
                managed_history::coverage_bytes(
                    &bytes.value,
                    self.descriptor.value.family_id,
                    &history,
                    preparation,
                )
            },
            |identity| {
                let digest = identity.generation.to_string();
                let name = digest.strip_prefix("sha256:").expect("canonical digest");
                let bytes =
                    self.read(&format!("{lineage}/generations/{name}.json"), preparation)?;
                managed_history::generation_bytes(&bytes.value, identity, preparation)
            },
        )
    }
}

pub(super) fn dataset_id(bytes: &[u8], preparation: &Preparation) -> Result<Uuid> {
    let allowance = preparation.room();
    let _loan = preparation.reserve(allowance)?;
    let options = crate::history::HistoryOptions {
        cancel: Some(preparation.control().cancel.flag()),
        deadline: preparation.control().deadline,
    };
    let mut budget = crate::store::cold::ColdBudget::new(crate::history::ReadLimit {
        options: &options,
        memory: Some(allowance),
        held: 0,
    })?;
    crate::commit::dataset_id_bytes_controlled(bytes, &mut budget)
}

pub(super) fn branch_identity(
    bytes: &[u8],
    family: Uuid,
    logical: Uuid,
    head: u64,
    preparation: &Preparation,
) -> Result<()> {
    inspect_branch_identity(bytes, family, logical, head, preparation, |_| Ok(()))
}

/// Only a genuine supplier has the live immutable identity. Candidate documents
/// cannot turn schema-valid fork metadata into that authority.
pub(super) fn live_branch_identity(
    bytes: &[u8],
    family: Uuid,
    logical: Uuid,
    head: u64,
    preparation: &Preparation,
    actual: &crate::store::branching::Ident,
) -> Result<()> {
    inspect_branch_identity(bytes, family, logical, head, preparation, |branch| {
        if actual.dataset_id != family
            || branch.from.branch_id != actual.from.branch_id
            || branch.from.seq != actual.from.seq
            || branch.ordinal != actual.ordinal
            || branch.configuration_origin != actual.configuration_origin
        {
            return Err(managed_history::unavailable(
                "original branch identity differs from the live Store",
            ));
        }
        Ok(())
    })
}

// The callback returns unit synchronously: decoded heap data dies while its
// ColdBudget and the real Preparation loan are still held.
fn inspect_branch_identity(
    bytes: &[u8],
    family: Uuid,
    logical: Uuid,
    head: u64,
    preparation: &Preparation,
    inspect: impl FnOnce(&crate::store::branching::BranchFile) -> Result<()>,
) -> Result<()> {
    let _canonical =
        preparation.reserve((bytes.len() as u64).saturating_mul(2).saturating_add(4096))?;
    let allowance = preparation.room();
    let _loan = preparation.reserve(allowance)?;
    let options = crate::history::HistoryOptions {
        cancel: Some(preparation.control().cancel.flag()),
        deadline: preparation.control().deadline,
    };
    let mut budget = crate::store::cold::ColdBudget::new(crate::history::ReadLimit {
        options: &options,
        memory: Some(allowance),
        held: 0,
    })?;
    let mut raw: crate::store::cold::Loaded<serde_json::Value> = budget.json(bytes)?;
    crate::formats::check_settings("branch.json", &raw.value)?;
    if let Some(object) = raw.value.as_object_mut() {
        object.entry("format").or_insert(1.into());
    }
    let branch: crate::store::branching::BranchFile = budget.value(raw.value)?;
    branch.check_configuration(bytes)?;
    if branch.dataset_id != family || branch.id != logical || branch.from.seq > head {
        return Err(managed_history::unavailable(
            "original branch/family identity differs",
        ));
    }
    inspect(&branch)?;
    preparation.check()
}
