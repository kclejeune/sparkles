//! Capture the actual immutable publication closure, rather than following the
//! family's mutable settings paths after a backup has selected its data head.
use super::*;
use crate::guard::WriteOptions;
use crate::store::{CapturedFile, FileKind, FileSource};
use sha2::{Digest, Sha256};
use std::fs::File;
use std::io::{Seek, SeekFrom};

// Each history entry needs more than 256 bytes in the existing 16 MiB backup
// manifest. Refuse a closure that cannot fit it before retaining more handles.
const MAX_FILES: usize = (16 << 20) / 256;

fn controlled_preparation(control: &WriteOptions) -> Result<Preparation> {
    administrative_preparation_with(crate::task::Control {
        cancel: control
            .cancel
            .clone()
            .map(crate::task::Cancel::from_flag)
            .unwrap_or_default(),
        deadline: control.deadline,
        ..Default::default()
    })
}

/// Materialization already owns a temporary directory. Preserve the genuine
/// source publications there, without copying retained generation payloads into
/// public, independently cloneable byte buffers.
pub(in crate::store) fn memory_files(
    root: &Path,
    dataset_id: Uuid,
    installed: &Installed,
    control: &WriteOptions,
) -> Result<Vec<CapturedFile>> {
    control.check()?;
    let preparation = controlled_preparation(control)?;
    let memory = installed
        .history_memory
        .as_ref()
        .ok_or_else(|| unavailable("materialized configuration memory history is missing"))?;
    let mut node = Some(memory.as_ref());
    let mut count = 0usize;
    while let Some(current) = node {
        control.check()?;
        count += 1;
        if count >= MAX_FILES {
            return Err(unavailable(
                "configuration closure exceeds backup manifest limits",
            ));
        }
        persist(root, &current.prepared, &preparation)?;
        node = current.previous.as_deref();
    }
    let coverage = persist_coverage(root, &memory.coverage.value, &preparation)?;
    if installed
        .configuration_history
        .as_ref()
        .map(|history| history.coverage)
        != Some(coverage)
    {
        return Err(unavailable("materialized configuration coverage differs"));
    }
    let _registry = preparation.reserve(MAX_BYTES as u64)?;
    let registry = installed.file_bytes();
    crate::store::write_synced(&root.join(crate::managed::FILE), &registry)?;
    for (owner, bytes) in &installed.settings {
        control.check()?;
        crate::store::write_synced(&root.join(owner.settings_file()), bytes)?;
    }
    files(root, dataset_id, installed, control)
}

/// All payloads are descriptor-backed. No request-credit sidecar can be dropped
/// while a caller retains a copied public FileSource::Bytes payload.
pub(in crate::store) fn files(
    root: &Path,
    dataset_id: Uuid,
    installed: &Installed,
    control: &WriteOptions,
) -> Result<Vec<CapturedFile>> {
    control.check()?;
    let preparation = controlled_preparation(control)?;
    let history = installed
        .configuration_history
        .as_ref()
        .ok_or_else(|| unavailable("configuration capture has no published root"))?;
    let coverage = read_coverage(root, dataset_id, history, &preparation)?;
    let directory = directory(root, history.lineage_id)?;
    checked_directory(&directory.join("generations"))?;
    let mut files = Vec::new();
    let mut retain = |path: PathBuf, digest| -> Result<()> {
        control.check()?;
        if files.len() >= MAX_FILES {
            return Err(unavailable(
                "configuration closure exceeds backup manifest limits",
            ));
        }
        files.try_reserve(1).map_err(|_| preparation.exhausted())?;
        let relative = path
            .strip_prefix(root)
            .map_err(|_| unavailable("configuration capture escaped its root"))?;
        let path_name = relative
            .to_str()
            .ok_or_else(|| unavailable("configuration capture path is not UTF-8"))?
            .replace(std::path::MAIN_SEPARATOR, "/");
        let file = pinned_file(&path, digest, &preparation, control)?;
        files.push(CapturedFile {
            path: path_name,
            kind: FileKind::Immutable,
            len: file.metadata()?.len(),
            src: FileSource::File(file),
        });
        Ok(())
    };
    retain(directory.join("coverage.json"), history.coverage)?;
    let mut current = history.identity(dataset_id);
    let mut expected = None;
    loop {
        control.check()?;
        let generation = read_generation(root, current, &preparation)?;
        if current.generation == history.current_generation
            && !generation.value.matches_installed(installed)
        {
            return Err(unavailable(
                "captured registry differs from its published generation",
            ));
        }
        if expected.is_some_and(|(ordinal, revision)| {
            generation.value.ordinal != ordinal || generation.value.registry_revision != revision
        }) {
            return Err(unavailable("captured configuration predecessor differs"));
        }
        retain(
            directory
                .join("generations")
                .join(digest_name(current.generation)),
            current.generation,
        )?;
        match generation.value.previous {
            Some(previous) => {
                expected =
                    Some((
                        generation.value.ordinal.checked_sub(1).ok_or_else(|| {
                            unavailable("captured configuration ordinal underflows")
                        })?,
                        generation
                            .value
                            .registry_revision
                            .checked_sub(1)
                            .ok_or_else(|| {
                                unavailable("captured configuration registry revision underflows")
                            })?,
                    ));
                current.generation = previous;
            }
            None => {
                if generation.value.ordinal != 1
                    || current.generation != coverage.value.activation_generation
                {
                    return Err(unavailable("captured configuration genesis differs"));
                }
                break;
            }
        }
    }
    drop(retain);
    // Registry encoding is bounded by the already validated generation. Its
    // temporary bytes are admitted, hashed and discarded; callers retain the FD.
    let _registry = preparation.reserve(MAX_BYTES as u64)?;
    let registry = installed.file_bytes();
    let digest = Sha256Digest::of_bytes(&registry, preparation.control())?;
    let file = pinned_file(
        &root.join(crate::managed::FILE),
        digest,
        &preparation,
        control,
    )?;
    files.push(CapturedFile {
        path: crate::managed::FILE.into(),
        kind: FileKind::Meta,
        len: file.metadata()?.len(),
        src: FileSource::File(file),
    });
    for (owner, bytes) in &installed.settings {
        control.check()?;
        let file = pinned_file(
            &root.join(owner.settings_file()),
            Sha256Digest::of_bytes(bytes, preparation.control())?,
            &preparation,
            control,
        )?;
        files.push(CapturedFile {
            path: owner.settings_file().into(),
            kind: FileKind::Meta,
            len: file.metadata()?.len(),
            src: FileSource::File(file),
        });
    }
    control.check()?;
    Ok(files)
}

/// Verify the descriptor that the public capture will actually read. A later
/// atomic settings replacement may unlink it without changing its captured bytes.
pub(in crate::store) fn pinned_file(
    path: &Path,
    expected: Sha256Digest,
    preparation: &Preparation,
    control: &WriteOptions,
) -> Result<File> {
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
        return Err(unavailable("invalid captured configuration file"));
    }
    let mut digest = Sha256::new();
    let _buffer = preparation.reserve(64 << 10)?;
    let mut buffer = [0; 64 << 10];
    loop {
        control.check()?;
        preparation.check()?;
        let length = file.read(&mut buffer)?;
        if length == 0 {
            break;
        }
        digest.update(&buffer[..length]);
    }
    let after = file.metadata()?;
    if before.len() != after.len()
        || before.modified()? != after.modified()?
        || Sha256Digest::from_digest_bytes(digest.finalize().into()) != expected
    {
        return Err(unavailable(
            "captured configuration bytes changed or differ",
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
            return Err(unavailable("captured configuration descriptor changed"));
        }
    }
    file.seek(SeekFrom::Start(0))?;
    Ok(file)
}
