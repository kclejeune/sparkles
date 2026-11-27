//! Startup recovery of interrupted restores, run before the registry's datasets are
//! opened (a crash between the two renames of an in-place restore leaves
//! `databases/<ds>` missing, which would stop the server):
//! * `databases/.restore-*` (an unfinished download or discarded rollback copy) is removed;
//! * `databases/.replaced-<ds>-<task>` carrying a `.restore-swap.json` marker is
//!   reconciled against the durable `config.json` revision (rolling back to the
//!   predecessor when the predecessor revision survived, retaining it when a
//!   committed swap also carries a validated `.keep-replaced-v1` marker, or
//!   removing `.replaced-*` when the restored revision or a later registration
//!   survived without retention);
//! * `databases/.replaced-<ds>-<task>` carrying a validated `.keep-replaced-v1`
//!   marker from a `keepReplaced` restore whose final rename did not finish
//!   synchronizes the predecessor directory, then moves it to `.kept-<ds>-<task>`
//!   under the store lock when that destination is free or retains it with a
//!   warning when occupied, without populating the live path;
//! * an unmarked legacy `databases/.replaced-<ds>-<task>` is reconciled against
//!   `config.json`: for a persistent row with a UUID, `databases` is synchronized
//!   under the live store lock before removing `.replaced-*` when `databases/<ds>`
//!   matches the UUID and has `CURRENT`, a sole matching predecessor is restored
//!   when it does not, and ambiguous identities fail closed; for a format 1
//!   persistent row without a UUID, roll-forward synchronizes `databases` under
//!   the live store lock before removing `.replaced-*` and a sole predecessor is
//!   restored with a WARN when `databases/<ds>` is absent; with no persistent
//!   registry row, obsolete `.replaced-*` copies are retired without populating
//!   `databases/<ds>`. Missing `config.json` fails closed whenever any
//!   `.replaced-*` entry is present;
//! * `tmp/verify-*` (the scratch directory of a `restore`-level verification) and
//!   `tmp/memory-backup-*` (the temporary copy of an in-memory dataset being backed
//!   up) are removed.

use super::delete;
use super::delete_fs::{Kind, RecoveryDir};
use super::{CONFIG_FILE, Registry};
use crate::{Error, Result};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::ffi::{OsStr, OsString};
use std::io::{ErrorKind, Read};
#[cfg(feature = "backup")]
use std::path::Path;
use uuid::Uuid;

/// Prefix of a restore's download directory: `.restore-<target>-<task>`.
pub const RESTORE_PREFIX: &str = ".restore-";
/// Prefix of the replaced directory of an in-place restore: `.replaced-<ds>-<task>`.
pub const REPLACED_PREFIX: &str = ".replaced-";
/// Prefix of a replaced directory kept with `keepReplaced`: `.kept-<ds>-<task>`.
#[cfg(feature = "backup")]
pub const KEPT_PREFIX: &str = super::KEPT_PREFIX;
/// Versioned marker stored inside `.replaced-<ds>-<task>` for registry-backed swaps.
pub(super) const SWAP_MARKER: &str = ".restore-swap.json";
const SWAP_MARKER_FORMAT: u32 = sparkles_core::formats::RESTORE_SWAP_MARKER.writes;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct SwapMarker {
    pub(super) format: u32,
    pub(super) dataset: String,
    pub(super) predecessor_id: Uuid,
    pub(super) predecessor_revision: Uuid,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) restored_id: Option<Uuid>,
    pub(super) restored_revision: Uuid,
}

#[cfg(feature = "backup")]
impl SwapMarker {
    pub(super) fn new(
        dataset: impl Into<String>,
        predecessor_id: Uuid,
        predecessor_revision: Uuid,
        restored_id: Option<Uuid>,
        restored_revision: Uuid,
    ) -> Self {
        Self {
            format: SWAP_MARKER_FORMAT,
            dataset: dataset.into(),
            predecessor_id,
            predecessor_revision,
            restored_id,
            restored_revision,
        }
    }
}

#[cfg(feature = "backup")]
pub(super) fn write_swap_marker(replaced: &Path, marker: &SwapMarker) -> Result<()> {
    let bytes = serde_json::to_vec(marker).map_err(|e| Error::invalid(e.to_string()))?;
    super::write_file_atomic(&replaced.join(SWAP_MARKER), &bytes)
}

#[cfg(all(feature = "backup", test))]
type MarkerRemoveHook = Box<dyn FnMut(&Path)>;
#[cfg(all(feature = "backup", test))]
thread_local! {
    pub(crate) static BEFORE_SWAP_MARKER_REMOVE: std::cell::RefCell<Option<MarkerRemoveHook>> =
        const { std::cell::RefCell::new(None) };
    pub(crate) static BEFORE_KEEP_MARKER_WRITE: std::cell::RefCell<Option<MarkerRemoveHook>> =
        const { std::cell::RefCell::new(None) };
}

#[cfg(feature = "backup")]
pub(super) fn write_keep_marker(replaced: &Path, kept_name: &str) -> Result<()> {
    #[cfg(test)]
    BEFORE_KEEP_MARKER_WRITE.with_borrow_mut(|hook| {
        if let Some(hook) = hook {
            hook(replaced);
        }
    });
    super::write_file_atomic(
        &replaced.join(delete::KEEP_REPLACED_MARKER),
        kept_name.as_bytes(),
    )
}

#[cfg(feature = "backup")]
fn remove_regular_marker(replaced: &Path, name: &str) -> Result<()> {
    let path = replaced.join(name);
    match std::fs::symlink_metadata(&path) {
        Ok(meta) if meta.file_type().is_file() => {}
        Ok(_) => return Ok(()),
        Err(error) if error.kind() == ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error.into()),
    }
    match std::fs::remove_file(&path) {
        Ok(()) => {}
        Err(error) if error.kind() == ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error.into()),
    }
    super::sync_dir(replaced)
}

#[cfg(feature = "backup")]
pub(super) fn remove_keep_marker(replaced: &Path) -> Result<()> {
    remove_regular_marker(replaced, delete::KEEP_REPLACED_MARKER)
}

#[cfg(feature = "backup")]
pub(super) fn remove_swap_marker(replaced: &Path) -> Result<()> {
    #[cfg(test)]
    BEFORE_SWAP_MARKER_REMOVE.with_borrow_mut(|hook| {
        if let Some(hook) = hook {
            hook(replaced);
        }
    });
    remove_regular_marker(replaced, SWAP_MARKER)
}

fn read_swap_marker(replaced: &RecoveryDir, dataset: &str) -> Result<Option<SwapMarker>> {
    #[derive(Deserialize)]
    struct Header {
        format: serde_json::Value,
    }
    let corrupt = |error: &dyn std::fmt::Display| {
        Error::Corrupt(format!(
            "{}/{SWAP_MARKER}: {error}",
            replaced.path().display()
        ))
    };
    let mut file = match replaced.open_file(OsStr::new(SWAP_MARKER), false) {
        Ok(file) => file,
        // Sparkles only ever publishes regular files for `.restore-swap.json`.
        // Treat pre-existing non-regular entries (such as a directory or symlink)
        // as unmarked so already-staged roots can still roll back cleanly and
        // preserve them without aborting catalog startup for healthy siblings.
        Err(error) if matches!(error.kind(), ErrorKind::NotFound | ErrorKind::InvalidData) => {
            return Ok(None);
        }
        Err(error) => return Err(error.into()),
    };
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes)?;
    if bytes.iter().find(|b| !b.is_ascii_whitespace()) != Some(&b'{') {
        return Err(corrupt(&"expected a JSON object"));
    }
    let header: Header = serde_json::from_slice(&bytes).map_err(|e| corrupt(&e))?;
    let Some(format) = header.format.as_u64() else {
        return Err(corrupt(&"invalid format version"));
    };
    if format == 0 {
        return Err(corrupt(&"invalid format version 0"));
    }
    if format > u64::from(SWAP_MARKER_FORMAT) {
        return Err(Error::Unsupported(format!(
            "{}/{SWAP_MARKER} uses format {format}, which is newer than supported format {SWAP_MARKER_FORMAT}",
            replaced.path().display()
        )));
    }
    let marker: SwapMarker = serde_json::from_slice(&bytes).map_err(|e| corrupt(&e))?;
    if marker.format != SWAP_MARKER_FORMAT {
        return Err(corrupt(&"format version mismatch"));
    }
    if marker.dataset != dataset {
        return Err(corrupt(&format!(
            "dataset {:?} does not match directory for /{dataset}",
            marker.dataset
        )));
    }
    if marker.predecessor_revision == marker.restored_revision {
        return Err(corrupt(&"swap revisions must be distinct"));
    }
    Ok(Some(marker))
}

/// Whether `databases` contains any `.replaced-<ds>-<task>` directory
/// (requiring `config.json` for recovery). Validates swap markers on every
/// staged entry so a newer format still reports [`Error::Unsupported`] even
/// when an earlier sibling is malformed or unreadable.
pub(super) fn has_staged_swaps(databases: &RecoveryDir) -> Result<bool> {
    let mut found = false;
    let mut first_error: Option<Error> = None;
    for entry in databases.names()? {
        let name = entry?;
        let text = name.to_string_lossy();
        let Some(ds) = replaced_dataset(&text) else {
            continue;
        };
        found = true;
        let check = databases
            .child(&name)
            .map_err(Error::from)
            .and_then(|dir| read_swap_marker(&dir, ds).map(|_| ()));
        if let Err(error) = check {
            if matches!(error, Error::Unsupported(_)) {
                return Err(error);
            }
            first_error.get_or_insert(error);
        }
    }
    if let Some(error) = first_error {
        return Err(error);
    }
    Ok(found)
}

/// If `dir` is a `.replaced-<ds>-<task>` directory carrying a validated
/// `.keep-replaced-v1` marker matching `.kept-<ds>-<task>`, return that destination name.
fn keep_replaced_target(dir: &RecoveryDir) -> Result<Option<String>> {
    let Some(rest) = dir
        .path()
        .file_name()
        .and_then(OsStr::to_str)
        .and_then(|name| name.strip_prefix(REPLACED_PREFIX))
    else {
        return Ok(None);
    };
    let expected = format!("{}{rest}", super::KEPT_PREFIX);
    let file = match dir.open_file(OsStr::new(delete::KEEP_REPLACED_MARKER), false) {
        Ok(file) => file,
        Err(error) if matches!(error.kind(), ErrorKind::NotFound | ErrorKind::InvalidData) => {
            return Ok(None);
        }
        Err(error) => return Err(error.into()),
    };
    let mut marker = Vec::new();
    file.take(expected.len() as u64 + 1)
        .read_to_end(&mut marker)?;
    Ok((marker == expected.as_bytes()).then_some(expected))
}

/// Whether `databases/<name>` (a `.replaced-<ds>-<task>` directory) is provably
/// an intentionally retained `keepReplaced` copy rather than a rollback candidate
/// for `ds`. Returns `false` whenever the directory cannot be opened or its
/// markers or identity cannot be validated, keeping questionable candidates in
/// per-dataset conflict arbitration instead of failing catalog startup.
pub(super) fn is_retained_keep_replaced(
    databases: &RecoveryDir,
    name: &OsStr,
    ds: &str,
    registered: Option<&super::RegistryEntry>,
) -> bool {
    let Ok(dir) = databases.child(name) else {
        return false;
    };
    if !matches!(keep_replaced_target(&dir), Ok(Some(_))) {
        return false;
    }
    match read_swap_marker(&dir, ds) {
        Ok(None) => delete::dataset_id(&dir).is_ok(),
        Ok(Some(marker)) => {
            let committed = match registered {
                None => true,
                Some(row) => row
                    .revision
                    .is_some_and(|revision| revision != marker.predecessor_revision),
            };
            committed
                && matches!(
                    delete::dataset_id(&dir),
                    Ok(Some(id)) if id == marker.predecessor_id
                )
        }
        Err(_) => false,
    }
}

/// Whether `databases/<name>` (a `.replaced-<ds>-<task>` directory) is a readable,
/// valid rollback candidate for the registered persistent dataset `row` whose live
/// root is absent.
pub(super) fn is_verified_rollback_candidate(
    databases: &RecoveryDir,
    name: &OsStr,
    ds: &str,
    row: &super::RegistryEntry,
) -> bool {
    let Some(expected_id) = row.id else {
        return false;
    };
    let Ok(dir) = databases.child(name) else {
        return false;
    };
    if !matches!(delete::dataset_id(&dir), Ok(Some(id)) if id == expected_id) {
        return false;
    }
    match read_swap_marker(&dir, ds) {
        Ok(Some(marker)) => {
            row.revision == Some(marker.predecessor_revision)
                && marker.predecessor_id == expected_id
        }
        Ok(None) => matches!(keep_replaced_target(&dir), Ok(None)),
        Err(_) => false,
    }
}

/// Move a locked `keepReplaced` predecessor to `kept_name`, or retain it with a
/// warning when `kept_name` is already occupied. Callers must synchronize
/// `replaced_dir` first so the observed `.keep-replaced-v1` marker is durable on
/// every exit path.
fn rename_keep_replaced(
    databases: &RecoveryDir,
    ds: &str,
    mut replaced_dir: RecoveryDir,
    lock: sparkles_core::store::ProcessLock,
    kept_name: &str,
) -> Result<()> {
    let replaced_name = replaced_dir
        .path()
        .file_name()
        .expect("replaced directory name")
        .to_os_string();
    let kept_os = OsStr::new(kept_name);
    match databases.entry(kept_os) {
        Ok(_) => {
            drop(lock);
            delete::warn_retained_restore(
                databases,
                &replaced_name,
                Some(ds),
                format!("keepReplaced destination {kept_name} already exists"),
            );
            return Ok(());
        }
        Err(error) if error.kind() == ErrorKind::NotFound => {}
        Err(error) => return Err(error.into()),
    }
    replaced_dir.verify_lock(&lock)?;
    #[cfg(not(unix))]
    drop(lock);
    match databases.rename_child(&mut replaced_dir, kept_os) {
        Ok(()) => {
            databases.sync()?;
            #[cfg(unix)]
            drop(lock);
            tracing::info!(
                target: "sparkles::backup",
                path = %databases.path().join(kept_os).display(),
                dataset = ds,
                "finished retaining keepReplaced restore predecessor"
            );
            Ok(())
        }
        Err(error) if error.kind() == ErrorKind::AlreadyExists => {
            #[cfg(unix)]
            drop(lock);
            delete::warn_retained_restore(
                databases,
                &replaced_name,
                Some(ds),
                format!("keepReplaced destination {kept_name} already exists"),
            );
            Ok(())
        }
        Err(error) => Err(error.into()),
    }
}

fn finish_keep_replaced(
    databases: &RecoveryDir,
    ds: &str,
    replaced_dir: RecoveryDir,
    kept_name: &str,
) -> Result<()> {
    // Make the observed `.keep-replaced-v1` marker durable in its own directory
    // before reporting retention or moving the copy, including occupied destinations.
    replaced_dir.sync()?;
    let replaced_name = replaced_dir
        .path()
        .file_name()
        .expect("replaced directory name")
        .to_os_string();
    match databases.entry(OsStr::new(kept_name)) {
        Ok(_) => {
            delete::warn_retained_restore(
                databases,
                &replaced_name,
                Some(ds),
                format!("keepReplaced destination {kept_name} already exists"),
            );
            return Ok(());
        }
        Err(error) if error.kind() == ErrorKind::NotFound => {}
        Err(error) => return Err(error.into()),
    }
    let lock = match delete::dataset_id(&replaced_dir)? {
        Some(id) => delete::lock_candidate(&replaced_dir, id, false)?,
        None => delete::lock_dataset(&replaced_dir)?,
    };
    rename_keep_replaced(databases, ds, replaced_dir, lock, kept_name)
}

/// The dataset name of `.replaced-<ds>-<task>` (the task id is numeric).
pub fn replaced_dataset(file_name: &str) -> Option<&str> {
    let rest = file_name.strip_prefix(REPLACED_PREFIX)?;
    let (ds, task) = rest.rsplit_once('-')?;
    (!ds.is_empty() && !task.is_empty() && task.bytes().all(|b| b.is_ascii_digit())).then_some(ds)
}

/// Recover `<data_dir>/databases` after the catalog lock is held and before opening
/// any registered datasets.
pub(super) fn recover(
    catalog: &RecoveryDir,
    databases: &RecoveryDir,
    registry: &Registry,
    protected: &BTreeSet<String>,
) -> Result<()> {
    let mut restore_leftovers = Vec::new();
    let mut replaced_entries: BTreeMap<String, Vec<(RecoveryDir, Option<SwapMarker>)>> =
        BTreeMap::new();
    let mut first_error: Option<Error> = None;
    for entry in databases.names()? {
        let name = entry?;
        let text = name.to_string_lossy().into_owned();
        if text.starts_with(RESTORE_PREFIX) {
            restore_leftovers.push(databases.path().join(&name));
        } else if let Some(ds) = replaced_dataset(&text) {
            if protected.contains(ds) {
                continue;
            }
            let opened = databases.child(&name).map_err(Error::from).and_then(|dir| {
                let marker = read_swap_marker(&dir, ds)?;
                Ok((dir, marker))
            });
            match opened {
                Ok((dir, marker)) => replaced_entries
                    .entry(ds.to_string())
                    .or_default()
                    .push((dir, marker)),
                Err(error @ Error::Unsupported(_)) => return Err(error),
                Err(error) => {
                    first_error.get_or_insert(error);
                }
            }
        }
    }
    if let Some(error) = first_error {
        return Err(error);
    }

    if !restore_leftovers.is_empty() {
        for path in restore_leftovers {
            tracing::info!(
                target: "sparkles::backup",
                "removing unfinished restore {}",
                path.display()
            );
            std::fs::remove_dir_all(&path)?;
        }
        databases.sync()?;
    }

    // Make the observed registry durable before any swap removes either copy.
    if !replaced_entries.is_empty() {
        catalog.sync()?;
    }

    for (ds, entries) in replaced_entries {
        let mut rollbacks = Vec::new();
        let mut completions = Vec::new();
        let mut kept_replaced = Vec::new();
        let mut unmarked = Vec::new();
        let registered = delete::registered_dataset(registry, &ds);
        for (dir, marker) in entries {
            let Some(marker) = marker else {
                if let Some(kept_name) = keep_replaced_target(&dir)? {
                    kept_replaced.push((dir, kept_name));
                } else {
                    unmarked.push(dir);
                }
                continue;
            };
            match registered {
                Some(row) if row.revision.is_none() => {
                    return Err(Error::Corrupt(format!(
                        "{CONFIG_FILE} is missing the swap revision for /{ds}"
                    )));
                }
                Some(row) if row.revision == Some(marker.predecessor_revision) => {
                    rollbacks.push((dir, marker, row));
                }
                Some(row) if row.revision == Some(marker.restored_revision) => {
                    completions.push((dir, marker, Some(row)));
                }
                _ => {
                    completions.push((dir, marker, None));
                }
            }
        }
        if rollbacks.len() > 1 {
            return Err(Error::Corrupt(format!(
                "multiple staged restore swaps match the registered revision of /{ds}"
            )));
        }

        // Roll back any uncommitted swap before cleaning up older completed swap copies.
        for (replaced_dir, marker, row) in rollbacks {
            if row.id.is_some_and(|id| id != marker.predecessor_id) {
                return Err(Error::Corrupt(format!(
                    "{CONFIG_FILE} identity {:?} disagrees with staged restore predecessor {}",
                    row.id, marker.predecessor_id
                )));
            }
            let mut replaced = delete::LockedDeleteDir::open(replaced_dir, marker.predecessor_id)?;
            let discard =
                if let Some(mut live) = delete::child_if_present(databases, OsStr::new(&ds))? {
                    if let Some(expected_restored) = marker.restored_id
                        && let Ok(Some(found)) = delete::dataset_id(&live)
                        && found != expected_restored
                        && found != marker.predecessor_id
                    {
                        return Err(Error::Corrupt(format!(
                            "live dataset /{ds} identity {found} does not match staged restore swap"
                        )));
                    }
                    let live_lock = delete::lock_dataset(&live)?;
                    #[cfg(not(unix))]
                    let live_lock = {
                        drop(live_lock);
                        None
                    };
                    #[cfg(unix)]
                    let live_lock = Some(live_lock);
                    let discard_name =
                        OsString::from(format!("{RESTORE_PREFIX}{ds}-rollback-{}", Uuid::new_v4()));
                    databases.rename_child(&mut live, &discard_name)?;
                    Some((live, live_lock))
                } else {
                    None
                };
            // Establish that `live` is durably absent before clearing the swap marker,
            // even when an earlier failed live rollback left it visibly missing without a sync.
            databases.sync()?;
            tracing::warn!(
                target: "sparkles::backup",
                "an in-place restore of /{ds} did not commit: putting back {}",
                replaced.path().display()
            );
            // Clear any pre-published keep marker and sync before unlinking the
            // swap marker so power loss cannot leave an orphan keep marker.
            // Preserve unexpected non-regular entries (such as a directory at
            // `.keep-replaced-v1`) because `keep_replaced_target` only recognizes
            // regular files.
            match replaced
                .dir()
                .entry(OsStr::new(delete::KEEP_REPLACED_MARKER))
            {
                Ok(entry) if entry.kind == Kind::File => {
                    match replaced
                        .dir()
                        .remove_file(OsStr::new(delete::KEEP_REPLACED_MARKER))
                    {
                        Ok(()) => {}
                        Err(error) if error.kind() == ErrorKind::NotFound => {}
                        Err(error) => return Err(error.into()),
                    }
                }
                Ok(_) => {}
                Err(error) if error.kind() == ErrorKind::NotFound => {}
                Err(error) => return Err(error.into()),
            }
            replaced.dir().sync()?;
            match replaced.dir().remove_file(OsStr::new(SWAP_MARKER)) {
                Ok(()) => {}
                Err(error) if error.kind() == ErrorKind::NotFound => {}
                Err(error) => return Err(error.into()),
            }
            replaced.dir().sync()?;
            replaced.rename(databases, &ds)?;
            databases.sync()?;
            drop(replaced);
            if let Some((discard_dir, live_lock)) = discard {
                let lock = match live_lock {
                    Some(lock) => lock,
                    None => delete::lock_dataset(&discard_dir)?,
                };
                databases.remove_child(&discard_dir, lock)?;
                databases.sync()?;
            }
        }

        for (replaced_dir, marker, committed_row) in completions {
            if let Some(row) = committed_row
                && let Some(expected_id) = row.id
                && let Some(live) = delete::child_if_present(databases, OsStr::new(&ds))?
            {
                let _live_lock = delete::lock_candidate(&live, expected_id, false)?;
            }
            if let Some(kept_name) = keep_replaced_target(&replaced_dir)? {
                let lock = delete::lock_candidate(&replaced_dir, marker.predecessor_id, false)?;
                // Make the observed `.keep-replaced-v1` marker durable before clearing the
                // swap marker, so power loss cannot drop the keep marker and leave an
                // unmarked staging copy.
                replaced_dir.sync()?;
                match replaced_dir.remove_file(OsStr::new(SWAP_MARKER)) {
                    Ok(()) => {}
                    Err(error) if error.kind() == ErrorKind::NotFound => {}
                    Err(error) => return Err(error.into()),
                }
                replaced_dir.sync()?;
                rename_keep_replaced(databases, &ds, replaced_dir, lock, &kept_name)?;
                continue;
            }
            tracing::info!(
                target: "sparkles::backup",
                "removing {} (the restore of /{ds} had committed)",
                replaced_dir.path().display()
            );
            let lock = delete::lock_candidate(&replaced_dir, marker.predecessor_id, true)?;
            databases.remove_child(&replaced_dir, lock)?;
            databases.sync()?;
        }

        for (replaced_dir, kept_name) in kept_replaced {
            finish_keep_replaced(databases, &ds, replaced_dir, &kept_name)?;
        }

        if !unmarked.is_empty() {
            match registered {
                Some(row) if row.id.is_some() => {
                    let reg_id = row.id.expect("checked above");
                    let live = delete::child_if_present(databases, OsStr::new(&ds))?;
                    let live_id = match &live {
                        Some(dir) => delete::dataset_id(dir)?,
                        None => None,
                    };
                    let mut candidates = Vec::with_capacity(unmarked.len());
                    for dir in unmarked {
                        let id = delete::dataset_id(&dir)?;
                        candidates.push((dir, id));
                    }
                    match (live, live_id) {
                        (Some(live_dir), Some(id)) if id == reg_id => {
                            if !live_dir
                                .entry(OsStr::new("CURRENT"))
                                .is_ok_and(|entry| entry.kind == Kind::File)
                            {
                                return Err(Error::Corrupt(format!(
                                    "live dataset /{ds} is missing CURRENT while unmarked restore copies remain"
                                )));
                            }
                            let _live_lock = delete::lock_candidate(&live_dir, reg_id, false)?;
                            // Make the selected live directory name durable before touching any predecessor.
                            databases.sync()?;
                            for (replaced_dir, rep_id) in candidates {
                                tracing::info!(
                                    target: "sparkles::backup",
                                    "removing {} (the restore of /{ds} had finished)",
                                    replaced_dir.path().display()
                                );
                                let lock = match rep_id {
                                    Some(id) => delete::lock_candidate(&replaced_dir, id, true)?,
                                    None => delete::lock_dataset(&replaced_dir)?,
                                };
                                databases.remove_child(&replaced_dir, lock)?;
                                databases.sync()?;
                            }
                        }
                        (live, _) => {
                            if candidates.len() != 1 || candidates[0].1 != Some(reg_id) {
                                return Err(Error::Corrupt(format!(
                                    "unmarked restore swap for /{ds} does not match registered identity {reg_id}"
                                )));
                            }
                            let (replaced_dir, _) = candidates.pop().expect("single candidate");
                            let mut replaced = delete::LockedDeleteDir::open(replaced_dir, reg_id)?;
                            let discard = if let Some(mut live_dir) = live {
                                let live_lock = delete::lock_dataset(&live_dir)?;
                                #[cfg(not(unix))]
                                let live_lock = {
                                    drop(live_lock);
                                    None
                                };
                                #[cfg(unix)]
                                let live_lock = Some(live_lock);
                                let discard_name = OsString::from(format!(
                                    "{RESTORE_PREFIX}{ds}-rollback-{}",
                                    Uuid::new_v4()
                                ));
                                databases.rename_child(&mut live_dir, &discard_name)?;
                                databases.sync()?;
                                Some((live_dir, live_lock))
                            } else {
                                None
                            };
                            tracing::warn!(
                                target: "sparkles::backup",
                                "an unmarked in-place restore of /{ds} did not commit: putting back {}",
                                replaced.path().display()
                            );
                            replaced.rename(databases, &ds)?;
                            databases.sync()?;
                            drop(replaced);
                            if let Some((discard_dir, live_lock)) = discard {
                                let lock = match live_lock {
                                    Some(lock) => lock,
                                    None => delete::lock_dataset(&discard_dir)?,
                                };
                                databases.remove_child(&discard_dir, lock)?;
                                databases.sync()?;
                            }
                        }
                    }
                }
                Some(_) => {
                    // Legacy format 1 persistent row without a recorded UUID.
                    if let Some(live_dir) = delete::child_if_present(databases, OsStr::new(&ds))? {
                        let _live_lock = delete::lock_dataset(&live_dir)?;
                        databases.sync()?;
                        for replaced_dir in unmarked {
                            tracing::info!(
                                target: "sparkles::backup",
                                "removing {} (the restore of /{ds} had finished)",
                                replaced_dir.path().display()
                            );
                            let lock = delete::lock_dataset(&replaced_dir)?;
                            databases.remove_child(&replaced_dir, lock)?;
                            databases.sync()?;
                        }
                    } else {
                        if unmarked.len() != 1 {
                            return Err(Error::Corrupt(format!(
                                "multiple unmarked restore copies remain for legacy dataset /{ds}"
                            )));
                        }
                        let mut replaced_dir = unmarked.pop().expect("single candidate");
                        let lock = delete::lock_dataset(&replaced_dir)?;
                        #[cfg(not(unix))]
                        drop(lock);
                        tracing::warn!(
                            target: "sparkles::backup",
                            "an in-place restore of /{ds} was interrupted: putting back {}",
                            replaced_dir.path().display()
                        );
                        databases.rename_child(&mut replaced_dir, OsStr::new(&ds))?;
                        databases.sync()?;
                        #[cfg(unix)]
                        drop(lock);
                    }
                }
                None => {
                    // No persistent registry row owns this alias: retire obsolete staged
                    // copies without populating the live path.
                    databases.sync()?;
                    for replaced_dir in unmarked {
                        tracing::info!(
                            target: "sparkles::backup",
                            "removing obsolete {} (/{ds} is not a registered persistent dataset)",
                            replaced_dir.path().display()
                        );
                        let lock = delete::lock_dataset(&replaced_dir)?;
                        databases.remove_child(&replaced_dir, lock)?;
                        databases.sync()?;
                    }
                }
            }
        }
    }

    if let Ok(entries) = std::fs::read_dir(catalog.path().join("tmp")) {
        for e in entries.flatten() {
            let name = e.file_name().to_string_lossy().into_owned();
            if name.starts_with("verify-") || name.starts_with(crate::store::MEMORY_CAPTURE_PREFIX)
            {
                let _ = std::fs::remove_dir_all(e.path());
            }
        }
    }
    Ok(())
}

#[cfg(feature = "backup")]
impl super::Catalog {
    /// Publish a downloaded restore under a reservation made before queuing work.
    #[doc(hidden)]
    pub fn publish_restored(
        &self,
        reservation: super::Reservation,
        restored: &Path,
    ) -> crate::Result<crate::Dataset> {
        // A close callback's refusal keeps the staged directory and its error kind.
        self.check_manageable()?;
        super::swap::publish_new(self, reservation, restored).map_err(crate::backup::error)
    }
    #[doc(hidden)]
    pub fn replace_restored_reserved(
        &self,
        reservation: super::Reservation,
        restored: &Path,
        task: &str,
        keep: bool,
    ) -> crate::Result<crate::Dataset> {
        self.replace_restored_reserved_observed(reservation, restored, task, keep, |_, _| {})
    }

    /// Notify the server after the predecessor's final drain and committed close,
    /// while its counters are still readable and before publishing a successor.
    #[doc(hidden)]
    pub fn replace_restored_reserved_observed(
        &self,
        reservation: super::Reservation,
        restored: &Path,
        task: &str,
        keep: bool,
        on_closed: impl FnMut(u64, &crate::store::Store),
    ) -> crate::Result<crate::Dataset> {
        // A close callback's refusal comes before any drain or error translation.
        self.check_manageable()?;
        if !std::sync::Arc::ptr_eq(&self.inner, &reservation.catalog.inner)
            || self.restoring_by(reservation.name()).as_deref() != Some(task)
        {
            return Err(crate::Error::invalid("invalid restore reservation"));
        }
        super::swap::replace_reserved(self, reservation, restored, task, keep, on_closed)
            .map_err(crate::backup::error)
    }
    #[doc(hidden)]
    #[allow(clippy::too_many_arguments)]
    pub fn download_restore(
        &self,
        repo: &crate::backup::Repository,
        backup: &str,
        target: &str,
        req: &crate::backup::RestoreRequest,
        tmp: &Path,
        store_opts: crate::store::StoreOptions,
        ctl: &crate::task::Control,
    ) -> crate::Result<crate::backup::RestoreReport> {
        use crate::backup::{BackupError, Code, Identity, RestoreOptions, block_on, error};
        ctl.check()?;
        let manifest = block_on(repo.manifest(backup))?.map_err(error)?;
        if req.identity == Identity::Keep
            && self
                .list()
                .iter()
                .any(|d| d.id == manifest.dataset.id && !(req.replace && d.name == target))
        {
            return Err(error(BackupError::new(
                Code::DuplicateDatasetId,
                "another dataset has the backup's identity",
            )));
        }
        let in_place_head = self
            .get(target)
            .filter(|d| req.replace && d.dataset_id() == manifest.dataset.id)
            .map(|d| d.head_commit().seq);
        let cat = self.clone();
        let o = RestoreOptions {
            identity: req.identity,
            check: req.check,
            id_in_use: std::sync::Arc::new(move |id| cat.get_by_id(id).is_some()),
            in_place_head,
            store_opts,
            ctl: ctl.into(),
        };
        block_on(repo.restore(backup, tmp, &o))?.map_err(error)
    }
    /// Download, verify and publish a repository backup. For an in-place restore,
    /// existing handles must be released before the old store can be closed.
    pub fn restore(
        &self,
        repo: &crate::backup::Repository,
        backup: &str,
        req: &crate::backup::RestoreRequest,
        ctl: &crate::task::Control,
    ) -> crate::Result<crate::Dataset> {
        self.restore_report(repo, backup, req, ctl)
            .map(|(ds, _)| ds)
    }
    #[doc(hidden)]
    pub fn restore_report(
        &self,
        repo: &crate::backup::Repository,
        backup: &str,
        req: &crate::backup::RestoreRequest,
        ctl: &crate::task::Control,
    ) -> crate::Result<(crate::Dataset, crate::backup::RestoreReport)> {
        use crate::backup::{BackupError, Code, Identity, block_on, error};
        ctl.check()?;
        let manifest = block_on(repo.manifest(backup))?.map_err(error)?;
        let target = req.target.as_deref().unwrap_or(&manifest.dataset.name);
        super::check_name(target)?;
        let existing = self.get(target);
        if req.replace {
            let info = self
                .info(target)
                .ok_or_else(|| crate::Error::NotFound(format!("no dataset /{target}")))?;
            if info.kind != super::DatasetKind::Persistent || info.attached {
                return Err(error(BackupError::new(
                    Code::NotManaged,
                    "only managed persistent datasets can be replaced",
                )));
            }
        } else if existing.is_some() {
            return Err(crate::Error::Conflict(format!(
                "dataset /{target} is already registered"
            )));
        }
        if req.identity == Identity::Keep
            && self
                .list()
                .iter()
                .any(|d| d.id == manifest.dataset.id && !(req.replace && d.name == target))
        {
            return Err(error(BackupError::new(
                Code::DuplicateDatasetId,
                "another dataset has the backup's identity",
            )));
        }
        drop(existing);
        let holder = format!(
            "{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos()
        );
        let reservation = self.reserve(target, super::ReservationKind::Restore, &holder)?;
        let tmp = self.database_dir(&format!(".restore-{target}-{holder}"))?;
        let result = (|| {
            let report = self.download_restore(
                repo,
                backup,
                target,
                req,
                &tmp,
                self.inner.options.read().dataset.store.clone(),
                &ctl.part(0.0, 0.9),
            )?;
            ctl.check()?;
            let ds = if req.replace {
                self.replace_restored_reserved(reservation, &tmp, &holder, req.keep_replaced)?
            } else {
                self.publish_restored(reservation, &tmp)?
            };
            ctl.progress.report(1.0, "restored");
            Ok((ds, report))
        })();
        if result.is_err() {
            let _ = std::fs::remove_dir_all(&tmp);
        }
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names() {
        assert_eq!(replaced_dataset(".replaced-ds-12"), Some("ds"));
        assert_eq!(replaced_dataset(".replaced-my-ds.v2-3"), Some("my-ds.v2"));
        assert_eq!(replaced_dataset(".replaced-ds-x"), None);
        assert_eq!(replaced_dataset(".replaced--1"), None);
        assert_eq!(replaced_dataset(".kept-ds-1"), None);
        assert_eq!(replaced_dataset("ds"), None);
    }

    #[test]
    fn leftovers_are_cleaned_up() {
        use super::super::{DatasetKind, RegistryEntry};

        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("databases");
        for d in [
            ".restore-x-4",
            ".replaced-a-5",
            "a",
            ".replaced-b-6",
            ".replaced-z-8",
            ".kept-c-7",
        ] {
            std::fs::create_dir_all(db.join(d)).unwrap();
        }
        std::fs::write(db.join(".replaced-b-6").join("marker"), b"old b").unwrap();
        std::fs::create_dir_all(dir.path().join("tmp").join("verify-9")).unwrap();
        std::fs::create_dir_all(dir.path().join("tmp").join("memory-backup-x1")).unwrap();
        let catalog = RecoveryDir::open(dir.path()).unwrap();
        let databases = delete::open_databases(&catalog).unwrap();
        let legacy_registry = Registry {
            datasets: vec![
                RegistryEntry {
                    name: "a".into(),
                    kind: DatasetKind::Persistent,
                    id: None,
                    revision: None,
                    reasoning: None,
                },
                RegistryEntry {
                    name: "b".into(),
                    kind: DatasetKind::Persistent,
                    id: None,
                    revision: None,
                    reasoning: None,
                },
            ],
        };
        recover(&catalog, &databases, &legacy_registry, &Default::default()).unwrap();
        let mut left: Vec<String> = std::fs::read_dir(&db)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        left.sort();
        assert_eq!(left, [".kept-c-7", "a", "b"]);
        assert_eq!(
            std::fs::read(db.join("b").join("marker")).unwrap(),
            b"old b"
        );
        assert!(!dir.path().join("tmp").join("verify-9").exists());
        assert!(!dir.path().join("tmp").join("memory-backup-x1").exists());
        // nothing to do: fine on an empty databases directory
        recover(
            &catalog,
            &databases,
            &Registry::default(),
            &Default::default(),
        )
        .unwrap();
    }
}
