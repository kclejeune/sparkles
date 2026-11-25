//! Recover managed persistent deletes under catalog and dataset process exclusion.
//! The source is staged and synchronized before removing its registry row. The
//! durable registry decides whether startup restores the source or reclaims it.
//! Before reusing the same registration identity, the catalog records that decision.

use super::delete_fs::{Kind, RecoveryDir};
use super::{
    DatasetKind, KEPT_PREFIX, Refusal, Registry, RegistryEntry, TRASH_PREFIX, check_name,
    read_registry, read_registry_at, sync_dir, write_registry,
};
use crate::{Error, Result};
use serde::{Deserialize, Serialize};
use sparkles_core::store::ProcessLock;
use std::collections::{BTreeMap, BTreeSet};
use std::ffi::{OsStr, OsString};
use std::io::{self, ErrorKind, Read};
use std::path::Path;
use uuid::Uuid;

pub(super) const INTENT_FILE: &str = "delete.json";
pub(super) const CRASH_AFTER_INTENT: u8 = 1;
pub(super) const CRASH_AFTER_SAVE: u8 = 2;
#[cfg(test)]
pub(super) const CRASH_AFTER_KEPT_RENAME: u8 = 3;
#[cfg(test)]
pub(super) const CRASH_AFTER_FAILED_FORWARD_MOVE: u8 = 4;
#[cfg(test)]
pub(super) const CRASH_AFTER_FAILED_ROLLBACK: u8 = 5;
#[cfg(test)]
pub(super) const CRASH_AFTER_ROLLBACK_MOVE_SYNC: u8 = 6;
#[cfg(test)]
pub(super) const CRASH_BEFORE_REGISTRY_REPLACE: u8 = 7;

#[cfg(test)]
thread_local! {
    static CRASH_AFTER: std::cell::Cell<u8> = const { std::cell::Cell::new(0) };
    static FAIL_JOURNAL_SYNC: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    static RECOVERY_STOP: std::cell::Cell<RecoveryStop> = const { std::cell::Cell::new(RecoveryStop::None) };
}

#[cfg(test)]
#[derive(Clone, Copy, PartialEq, Eq)]
enum RecoveryStop {
    None,
    AfterMetadataRemoval,
    BeforeRemovalSync,
}

pub(super) fn crash_point(_stage: u8) -> Result<()> {
    #[cfg(test)]
    if CRASH_AFTER.get() == _stage {
        return Err(Error::invalid(format!(
            "interrupted delete at stage {_stage}"
        )));
    }
    Ok(())
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub(super) enum IntentState {
    Prepared,
    PreparedV3,
    Committed,
    Legacy,
}

impl IntentState {
    pub(super) fn is_prepared(self) -> bool {
        matches!(self, Self::Prepared | Self::PreparedV3)
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct DeleteIntent {
    pub(super) name: String,
    pub(super) id: Uuid,
    pub(super) token: Uuid,
    pub(super) state: IntentState,
}

impl DeleteIntent {
    pub(super) fn kept_name(&self) -> String {
        format!("{KEPT_PREFIX}{}-{}", self.name, self.token)
    }

    pub(super) fn trash_name(&self) -> String {
        format!("{TRASH_PREFIX}{}", self.token)
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct LegacyIntent {
    name: String,
    id: Uuid,
    token: Uuid,
}

impl From<LegacyIntent> for DeleteIntent {
    fn from(intent: LegacyIntent) -> Self {
        Self {
            name: intent.name,
            id: intent.id,
            token: intent.token,
            state: IntentState::Legacy,
        }
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "kebab-case")]
enum LegacyV2State {
    Deleting,
    RollingBack,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct LegacyV2Intent {
    name: String,
    id: Uuid,
    token: Uuid,
    #[serde(rename = "state")]
    _state: LegacyV2State,
    #[serde(rename = "reasoning", default)]
    _reasoning: Option<crate::reasoning::ReasoningRecord>,
}

impl From<LegacyV2Intent> for DeleteIntent {
    fn from(intent: LegacyV2Intent) -> Self {
        LegacyIntent {
            name: intent.name,
            id: intent.id,
            token: intent.token,
        }
        .into()
    }
}

#[derive(Deserialize)]
struct IntentHeader {
    format: serde_json::Value,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct IntentFile {
    #[serde(rename = "format")]
    _format: u32,
    intents: Vec<Box<serde_json::value::RawValue>>,
}

fn corrupt(error: impl std::fmt::Display) -> Error {
    Error::Corrupt(format!("{INTENT_FILE}: {error}"))
}

/// Check the header before deserializing the schema. Separate typed reads reject
/// repeated keys while allowing a newer schema to receive the version error.
pub(super) fn read_intents(dir: &Path) -> Result<Vec<DeleteIntent>> {
    read_intents_at(&RecoveryDir::open(dir)?)
}

pub(super) fn read_intents_at(catalog: &RecoveryDir) -> Result<Vec<DeleteIntent>> {
    let mut file = match catalog.open_file(OsStr::new(INTENT_FILE), false) {
        Ok(file) => file,
        Err(error) if error.kind() == ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) if error.kind() == ErrorKind::InvalidData => return Err(corrupt(error)),
        Err(error) => return Err(error.into()),
    };
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes)?;
    let header: IntentHeader = parse_object(&bytes)?;
    if header.format.is_null() {
        return Err(corrupt("missing format version"));
    }
    let format = sparkles_core::formats::check_settings(
        INTENT_FILE,
        &serde_json::json!({ "format": header.format }),
    )?;
    let file: IntentFile = parse_object(&bytes)?;
    let mut tokens = BTreeSet::new();
    let mut intents = Vec::with_capacity(file.intents.len());
    for record in file.intents {
        let bytes = record.get().as_bytes();
        let mut intent: DeleteIntent = match format {
            1 => parse_object::<LegacyIntent>(bytes)?.into(),
            2 => parse_object::<LegacyV2Intent>(bytes)?.into(),
            _ => parse_object(bytes)?,
        };
        if format == 3 {
            intent.state = match intent.state {
                IntentState::Prepared => IntentState::PreparedV3,
                IntentState::Legacy => IntentState::Legacy,
                _ => return Err(corrupt("delete state requires format 4")),
            };
        }
        check_name(&intent.name).map_err(corrupt)?;
        if !tokens.insert(intent.token) {
            return Err(corrupt(format!("duplicate delete token {}", intent.token)));
        }
        intents.push(intent);
    }
    Ok(intents)
}

fn parse_object<'a, T: Deserialize<'a>>(bytes: &'a [u8]) -> Result<T> {
    if bytes.iter().find(|byte| !byte.is_ascii_whitespace()) != Some(&b'{') {
        return Err(corrupt("expected a JSON object"));
    }
    serde_json::from_slice(bytes).map_err(corrupt)
}

fn write_intents(dir: &Path, intents: &[DeleteIntent]) -> Result<()> {
    let path = dir.join(INTENT_FILE);
    if intents.is_empty() {
        match std::fs::remove_file(&path) {
            Ok(()) => {}
            Err(error) if error.kind() == ErrorKind::NotFound => return Ok(()),
            Err(error) => return Err(error.into()),
        }
    } else {
        #[derive(Serialize)]
        struct Records<'a> {
            intents: &'a [DeleteIntent],
        }
        let bytes = sparkles_core::formats::settings_to_vec(INTENT_FILE, &Records { intents })?;
        let tmp = path.with_extension(format!("tmp-{}", Uuid::new_v4()));
        sparkles_core::fsutil::replace_via(&path, &tmp, &bytes)?;
    }
    #[cfg(test)]
    if FAIL_JOURNAL_SYNC.replace(false) {
        return Err(io::Error::other("injected delete journal sync failure").into());
    }
    sync_dir(dir)
}

pub(super) fn check_prepare(dir: &Path) -> Result<()> {
    read_intents(dir).map(|_| ())
}

/// Older registries have no identity. Persist it before staging so startup
/// can distinguish this row from later registrations with different UUIDs.
pub(super) fn bind_registry_identity(dir: &Path, name: &str, id: Uuid) -> Result<()> {
    let mut registry = read_registry(dir)?;
    let entry = registry
        .datasets
        .iter_mut()
        .find(|entry| entry.name == name && entry.kind == DatasetKind::Persistent)
        .ok_or_else(|| Error::Corrupt(format!("dataset /{name} has no persistent registry row")))?;
    if entry.id != Some(id) {
        entry.id = Some(id);
        settle_reused_identities(dir, &registry)?;
        write_registry(dir, &registry)?;
    }
    Ok(())
}

/// A restore or rename can register the same alias and UUID as a prior delete.
/// Once the old registry proves the delete committed, persist that decision
/// before a registry save can make the prepared record ambiguous again.
pub(super) fn settle_reused_identities(dir: &Path, next: &Registry) -> Result<()> {
    let catalog = RecoveryDir::open(dir)?;
    let mut intents = read_intents_at(&catalog)?;
    let mut reused = intents
        .iter_mut()
        .filter(|intent| {
            intent.state != IntentState::Legacy
                && registered_dataset(next, &intent.name)
                    .is_some_and(|row| row.id == Some(intent.id))
        })
        .peekable();
    // A failed sync after journal unlink or rewrite can hide a record that
    // would reappear after a crash. Flush even the absence of a matching
    // record before a registry replacement can publish a reused identity.
    if reused.peek().is_none() {
        return catalog.sync();
    }

    let current = read_registry_at(&catalog)?.ok_or_else(|| {
        Error::Corrupt("config.json is missing while a delete identity is being reused".into())
    })?;
    catalog.sync()?;
    let mut changed = false;
    for intent in reused.filter(|intent| intent.state.is_prepared()) {
        match registered_dataset(&current, &intent.name) {
            Some(row) if row.id == Some(intent.id) => {}
            Some(row) if row.id.is_none() => {
                return Err(corrupt(format!(
                    "cannot establish the prior registration of /{}",
                    intent.name
                )));
            }
            _ => {
                intent.state = IntentState::Committed;
                changed = true;
            }
        }
    }
    if changed {
        write_intents(dir, &intents)?;
    }
    Ok(())
}

pub(super) fn registered_dataset<'a>(
    registry: &'a Registry,
    name: &str,
) -> Option<&'a RegistryEntry> {
    registry
        .datasets
        .iter()
        .find(|entry| entry.name == name && entry.kind == DatasetKind::Persistent)
}

pub(super) fn record(dir: &Path, intent: &DeleteIntent) -> Result<()> {
    check_name(&intent.name)?;
    let mut intents = read_intents(dir)?;
    if intents.iter().any(|entry| entry.token == intent.token) {
        return Err(corrupt(format!("duplicate delete token {}", intent.token)));
    }
    intents.push(intent.clone());
    write_intents(dir, &intents)
}

pub(super) fn clear(dir: &Path, token: Uuid) -> Result<()> {
    let mut intents = read_intents(dir)?;
    let before = intents.len();
    intents.retain(|intent| intent.token != token);
    if intents.len() != before {
        write_intents(dir, &intents)?;
    }
    Ok(())
}

/// Establish the database directory before rename, restore and delete recovery.
pub(super) fn open_databases(catalog: &RecoveryDir) -> Result<RecoveryDir> {
    let dir = catalog.path();
    let name = OsStr::new("databases");
    match catalog.entry(name) {
        Err(error) if error.kind() == ErrorKind::NotFound => {
            std::fs::create_dir(dir.join(name))?;
        }
        Err(error) => return Err(error.into()),
        Ok(entry) if entry.kind == Kind::Directory => {}
        Ok(_) => {
            return Err(Error::Corrupt(format!(
                "{} is not a real directory",
                dir.join(name).display()
            )));
        }
    }
    Ok(catalog.child(name)?)
}

pub(super) fn dataset_id(dir: &RecoveryDir) -> Result<Option<Uuid>> {
    #[derive(Deserialize)]
    struct Identity {
        id: Uuid,
    }
    let file = match dir.open_file(OsStr::new("dataset.json"), false) {
        Ok(file) => file,
        Err(error) if error.kind() == ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.into()),
    };
    let metadata: Identity = serde_json::from_reader(file).map_err(|error| {
        if let Some(kind) = error.io_error_kind() {
            Error::from(io::Error::new(kind, error))
        } else {
            Error::Corrupt(format!("{}/dataset.json: {error}", dir.path().display()))
        }
    })?;
    Ok(Some(metadata.id))
}

pub(super) fn child_if_present(
    parent: &RecoveryDir,
    name: &OsStr,
) -> io::Result<Option<RecoveryDir>> {
    match parent.child(name) {
        Ok(dir) => Ok(Some(dir)),
        Err(error) if error.kind() == ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error),
    }
}

pub(super) fn lock_dataset(dir: &RecoveryDir) -> Result<ProcessLock> {
    let lock = dir.open_file(OsStr::new("sparkles.lock"), true)?;
    match lock.try_lock() {
        Ok(()) => {
            let lock = ProcessLock::from_locked_file(lock);
            dir.verify_lock(&lock)?;
            Ok(lock)
        }
        Err(std::fs::TryLockError::WouldBlock) => Err(Error::Locked {
            path: dir.path().into(),
            pid: None,
        }),
        Err(std::fs::TryLockError::Error(error)) => Err(error.into()),
    }
}

#[derive(Default)]
struct Bytes {
    count: u64,
    incomplete: bool,
}

fn retained_bytes(parent: &RecoveryDir, name: &OsStr) -> Bytes {
    fn count(parent: &RecoveryDir, name: &OsStr, total: &mut Bytes) -> io::Result<()> {
        let entry = parent.entry(name)?;
        match entry.kind {
            Kind::File => total.count = total.count.saturating_add(entry.bytes),
            Kind::Directory => {
                let dir = parent.child(name)?;
                for entry in dir.names()? {
                    total.incomplete |= entry.and_then(|name| count(&dir, &name, total)).is_err();
                }
            }
            Kind::Other => {}
        }
        Ok(())
    }
    let mut bytes = Bytes::default();
    bytes.incomplete |= count(parent, name, &mut bytes).is_err();
    bytes
}

fn warn_retained_with(
    parent: &RecoveryDir,
    name: &OsStr,
    dataset: Option<&str>,
    reason: impl std::fmt::Display,
    message: &str,
) {
    let bytes = retained_bytes(parent, name);
    tracing::warn!(
        target: "sparkles::catalog",
        path = %parent.path().join(name).display(),
        dataset,
        retained_bytes = bytes.count,
        retained_bytes_complete = !bytes.incomplete,
        %reason,
        "{message}"
    );
}

fn warn_retained(
    parent: &RecoveryDir,
    name: &OsStr,
    dataset: Option<&str>,
    reason: impl std::fmt::Display,
) {
    warn_retained_with(
        parent,
        name,
        dataset,
        reason,
        "startup delete recovery retained this entry",
    );
}

pub(super) fn warn_retained_restore(
    parent: &RecoveryDir,
    name: &OsStr,
    dataset: Option<&str>,
    reason: impl std::fmt::Display,
) {
    warn_retained_with(
        parent,
        name,
        dataset,
        reason,
        "startup restore recovery retained this entry",
    );
}

fn require_identity(candidate: &RecoveryDir, expected: Uuid, allow_missing: bool) -> Result<()> {
    match dataset_id(candidate)? {
        Some(id) if id == expected => Ok(()),
        None if allow_missing => Ok(()),
        found => Err(Error::Corrupt(format!(
            "dataset identity {found:?} does not match delete intent {expected}"
        ))),
    }
}

/// Verify before creating a lock file and recheck under the lease. A missing
/// identity is possible only after a staged directory has begun removal.
pub(super) fn lock_candidate(
    candidate: &RecoveryDir,
    id: Uuid,
    staged: bool,
) -> Result<ProcessLock> {
    require_identity(candidate, id, staged)?;
    let lock = lock_dataset(candidate)?;
    require_identity(candidate, id, staged)?;
    Ok(lock)
}

/// The verified directory and its process lock travel together across renames.
pub(super) struct LockedDeleteDir {
    dir: RecoveryDir,
    lock: Option<ProcessLock>,
}

impl LockedDeleteDir {
    pub(super) fn open(dir: RecoveryDir, id: Uuid) -> Result<Self> {
        let lock = Some(lock_candidate(&dir, id, false)?);
        Ok(Self { dir, lock })
    }

    pub(super) fn dir(&self) -> &RecoveryDir {
        &self.dir
    }

    pub(super) fn path(&self) -> &Path {
        self.dir.path()
    }

    pub(super) fn ensure_locked(&mut self, id: Uuid) -> Result<()> {
        if self.lock.is_none() {
            self.lock = Some(lock_candidate(&self.dir, id, false)?);
        }
        Ok(())
    }

    pub(super) fn rename(&mut self, parent: &RecoveryDir, to: &str) -> Result<()> {
        let lock = self
            .lock
            .as_ref()
            .expect("delete rename holds the store lock");
        self.dir.verify_lock(lock)?;
        // On Windows, the directory cannot be renamed with a child handle open.
        // Reacquire the lock at the new name before committing or removing data.
        #[cfg(not(unix))]
        drop(self.lock.take());
        Ok(parent.rename_child(&mut self.dir, OsStr::new(to))?)
    }

    pub(super) fn remove(mut self, parent: &RecoveryDir, id: Uuid) -> Result<()> {
        self.ensure_locked(id)?;
        let lock = self.lock.expect("delete removal holds the store lock");
        self.dir.verify_lock(&lock)?;
        Ok(parent.remove_child(&self.dir, lock)?)
    }
}

pub(super) fn move_verified(
    parent: &RecoveryDir,
    from: &str,
    to: &str,
    id: Uuid,
) -> Result<LockedDeleteDir> {
    let mut candidate = LockedDeleteDir::open(parent.child(OsStr::new(from))?, id)?;
    candidate.rename(parent, to)?;
    Ok(candidate)
}

fn path_present(parent: &RecoveryDir, name: &OsStr) -> bool {
    !parent
        .entry(name)
        .is_err_and(|error| error.kind() == ErrorKind::NotFound)
}

fn staged_name(databases: &RecoveryDir, intent: &DeleteIntent) -> Option<OsString> {
    [intent.kept_name(), intent.trash_name()]
        .into_iter()
        .map(OsString::from)
        .find(|name| path_present(databases, name))
}

/// Format 3 moved `.kept-<name>-<token>` to `.deleted-<token>` only after durable
/// registry removal. A sole verified trash copy proves that delete committed.
fn is_committed_v3_trash(databases: &RecoveryDir, intent: &DeleteIntent) -> bool {
    if intent.state != IntentState::PreparedV3 {
        return false;
    }
    let kept = intent.kept_name();
    if path_present(databases, OsStr::new(&kept)) {
        return false;
    }
    let trash = intent.trash_name();
    matches!(
        child_if_present(databases, OsStr::new(&trash)),
        Ok(Some(dir)) if require_identity(&dir, intent.id, true).is_ok()
    )
}

/// Isolate competing candidates for a missing registered root. A prepared
/// delete of a prior UUID cannot block a verified rollback of its replacement.
pub(super) fn restore_conflicts(
    databases: &RecoveryDir,
    registry: &Registry,
    intents: &[DeleteIntent],
) -> Result<BTreeSet<String>> {
    let uncertain: Vec<_> = intents
        .iter()
        .filter(|intent| {
            intent.state != IntentState::Committed
                && !is_committed_v3_trash(databases, intent)
                && !databases
                    .entry(OsStr::new(&intent.name))
                    .is_ok_and(|entry| entry.kind == Kind::Directory)
                && staged_name(databases, intent).is_some()
        })
        .collect();
    let mut conflicts = BTreeSet::new();
    if uncertain.is_empty() {
        return Ok(conflicts);
    }
    let uncertain_names: BTreeSet<&str> = uncertain
        .iter()
        .map(|intent| intent.name.as_str())
        .collect();
    let mut replaced = BTreeMap::<String, Vec<OsString>>::new();
    for name in databases.names()? {
        let name = name?;
        let Some(dataset) = name.to_str().and_then(super::restore::replaced_dataset) else {
            continue;
        };
        if !uncertain_names.contains(dataset) {
            continue;
        }
        let registered = registered_dataset(registry, dataset);
        if super::restore::is_retained_keep_replaced(databases, &name, dataset, registered) {
            continue;
        }
        replaced.entry(dataset.into()).or_default().push(name);
    }
    for intent in uncertain {
        let Some(candidates) = replaced.get(&intent.name) else {
            continue;
        };
        let registered = registered_dataset(registry, &intent.name);
        let distinct = match (intent.state, registered, candidates.as_slice()) {
            (state, Some(row), [replaced])
                if state.is_prepared()
                    && row.id.is_some_and(|id| id != intent.id)
                    && !path_present(databases, OsStr::new(&intent.name)) =>
            {
                let kept = intent.kept_name();
                let trash = intent.trash_name();
                let staged = match (
                    path_present(databases, OsStr::new(&kept)),
                    path_present(databases, OsStr::new(&trash)),
                ) {
                    (true, false) => Some(kept),
                    (false, true) => Some(trash),
                    _ => None,
                };
                let matches = |name: &OsStr, id, allow_missing| {
                    databases
                        .child(name)
                        .is_ok_and(|dir| require_identity(&dir, id, allow_missing).is_ok())
                };
                staged
                    .as_deref()
                    .is_some_and(|name| matches(OsStr::new(name), intent.id, true))
                    && super::restore::is_verified_rollback_candidate(
                        databases,
                        replaced,
                        &intent.name,
                        row,
                    )
            }
            _ => false,
        };
        if !distinct {
            conflicts.insert(intent.name.clone());
        }
    }
    Ok(conflicts)
}

fn finish_delete(
    databases: &RecoveryDir,
    intent: &mut DeleteIntent,
    registered: Option<&RegistryEntry>,
    warning_name: &mut OsString,
) -> Result<()> {
    let kept = intent.kept_name();
    let trash = intent.trash_name();
    let name = OsStr::new(&intent.name);
    let kept_name = OsStr::new(&kept);
    let trash_name = OsStr::new(&trash);
    let kept_dir = child_if_present(databases, kept_name)?;
    *warning_name = trash_name.into();
    let trash_dir = child_if_present(databases, trash_name)?;
    let (staged, candidate) = match (kept_dir, trash_dir) {
        (Some(_), Some(_)) => return Err(Error::Corrupt("both staged delete paths exist".into())),
        (Some(candidate), None) => (kept_name, Some(candidate)),
        (None, trash_dir) => (trash_name, trash_dir),
    };

    if candidate.is_some() {
        *warning_name = staged.into();
    } else {
        *warning_name = name.into();
    }
    if intent.state == IntentState::Legacy && candidate.is_some() {
        return Err(Error::Corrupt(
            "legacy delete journal cannot establish the commit outcome".into(),
        ));
    }

    let candidate = if let Some(registered) =
        registered.filter(|_| intent.state != IntentState::Committed)
    {
        let live = child_if_present(databases, name)?;
        // A replacement recorded after this delete is authoritative, even
        // if its root is now missing. A live root must also agree under lock.
        if let Some(id) = registered.id.filter(|id| *id != intent.id) {
            if let Some(live) = live {
                let _live_lock = lock_candidate(&live, id, false)?;
            }
            candidate
        } else {
            match (registered.id, live, candidate) {
                (_, Some(live), None) => {
                    let _lock = lock_candidate(&live, intent.id, false)?;
                    return Ok(());
                }
                (Some(_), None, Some(kept))
                    if staged == kept_name && intent.state == IntentState::Prepared =>
                {
                    let mut kept = LockedDeleteDir::open(kept, intent.id)?;
                    kept.rename(databases, &intent.name)?;
                    *warning_name = name.into();
                    databases.sync()?;
                    return Ok(());
                }
                // Format 3 renamed to `.deleted-<token>` only after durable registry
                // removal. A sole verified trash copy proves that delete committed.
                (Some(id), live, Some(trash))
                    if staged == trash_name && intent.state == IntentState::PreparedV3 =>
                {
                    if let Some(live) = live {
                        let _live_lock = lock_candidate(&live, id, false)?;
                    }
                    require_identity(&trash, intent.id, true)?;
                    intent.state = IntentState::Committed;
                    Some(trash)
                }
                _ if intent.state == IntentState::PreparedV3 => {
                    return Err(Error::Corrupt(
                        "format 3 delete intent cannot distinguish this registration from a restore that reused its identity".into(),
                    ));
                }
                _ => {
                    return Err(Error::Corrupt(
                        "could not verify one original root for the registered delete".into(),
                    ));
                }
            }
        }
    } else {
        candidate
    };

    let Some(candidate) = candidate else {
        if intent.state != IntentState::Committed
            && registered.is_none()
            && child_if_present(databases, name)?.is_some()
        {
            return Err(Error::Corrupt(
                "an unstaged directory remains after registry removal".into(),
            ));
        }
        return Ok(());
    };
    let lock = lock_candidate(&candidate, intent.id, true)?;
    // A restart may see the trash rename before its parent sync completed.
    databases.sync()?;
    #[cfg(test)]
    if RECOVERY_STOP.get() == RecoveryStop::AfterMetadataRemoval {
        candidate.remove_file(OsStr::new("dataset.json"))?;
        return Err(Error::invalid(
            "interrupted recovery after removing dataset.json",
        ));
    }
    let bytes = retained_bytes(databases, staged);
    databases.remove_child(&candidate, lock)?;
    tracing::info!(
        target: "sparkles::catalog",
        path = %databases.path().join(staged).display(),
        dataset = %intent.name,
        reclaimed_bytes = bytes.count,
        "removed interrupted delete directory on startup"
    );
    Ok(())
}

fn clean_temps(catalog: &RecoveryDir) -> Result<()> {
    let mut changed = false;
    for name in catalog.names()? {
        let name = name?;
        if !name
            .to_str()
            .and_then(|name| name.strip_prefix("delete.tmp-"))
            .is_some_and(|id| {
                Uuid::parse_str(id).is_ok_and(|uuid| uuid.hyphenated().to_string() == id)
            })
            || !catalog
                .entry(&name)
                .is_ok_and(|entry| entry.kind == Kind::File)
        {
            continue;
        }
        match catalog.remove_file(&name) {
            Ok(()) => changed = true,
            Err(error) => warn_retained(catalog, &name, None, error),
        }
    }
    if changed {
        catalog.sync()?;
    }
    Ok(())
}

/// A marker written by the restore path before publishing the retained copy.
pub(super) const KEEP_REPLACED_MARKER: &str = ".keep-replaced-v1";

fn intentional_backup(databases: &RecoveryDir, name: &OsStr) -> bool {
    let Some(text) = name.to_str() else {
        return false;
    };
    let Ok(dir) = databases.child(name) else {
        return false;
    };
    let Ok(file) = dir.open_file(OsStr::new(KEEP_REPLACED_MARKER), false) else {
        return false;
    };
    let mut marker = Vec::new();
    file.take(text.len() as u64 + 1)
        .read_to_end(&mut marker)
        .is_ok()
        && marker == text.as_bytes()
}

pub(super) fn recover(
    catalog: &RecoveryDir,
    databases: &RecoveryDir,
    registry: &Registry,
    intents: Vec<DeleteIntent>,
    restore_conflicts: &BTreeSet<String>,
) -> Result<BTreeMap<String, Refusal>> {
    clean_temps(catalog)?;
    // A prior process may have made a registry rename visible without syncing it.
    // Make the registry decision durable before it can authorize removing bytes.
    if !intents.is_empty() {
        catalog.sync()?;
    }
    let mut retained = Vec::new();
    let mut refusals = BTreeMap::new();
    let mut warned = BTreeSet::new();
    let tracked_trash: BTreeSet<_> = intents.iter().map(DeleteIntent::trash_name).collect();
    let count = intents.len();
    // A retired rollback may have left an unstaged record for this same
    // registration. Restore a staged root before checking those older records.
    let (staged, unstaged): (Vec<_>, Vec<_>) = intents
        .into_iter()
        .map(|intent| {
            let name = staged_name(databases, &intent);
            (intent, name)
        })
        .partition(|(_, staged)| staged.is_some());
    let mut pending_roots = BTreeSet::new();
    let mut ambiguous_roots = BTreeMap::new();
    for (intent, _) in &staged {
        if intent.state.is_prepared()
            && !is_committed_v3_trash(databases, intent)
            && !pending_roots.insert((intent.name.as_str(), intent.id))
        {
            ambiguous_roots
                .entry(intent.name.clone())
                .or_insert_with(BTreeSet::new)
                .insert(intent.id);
        }
    }
    let mut journal_changed = false;
    for (mut intent, staged_name) in staged.into_iter().chain(unstaged) {
        let mut warning_name = staged_name
            .clone()
            .unwrap_or_else(|| intent.kept_name().into());
        let registered = registered_dataset(registry, &intent.name);
        let initial_state = intent.state;
        let committed_v3 = is_committed_v3_trash(databases, &intent);
        let conflict = intent.state != IntentState::Committed
            && !committed_v3
            && staged_name.is_some()
            && restore_conflicts.contains(&intent.name);
        let result = if conflict {
            Err(Error::Corrupt(
                "both a staged restore and a staged delete may own the missing root".into(),
            ))
        } else if intent.state.is_prepared()
            && !committed_v3
            && staged_name.is_some()
            && registered.is_some_and(|row| row.id == Some(intent.id))
            && ambiguous_roots
                .get(&intent.name)
                .is_some_and(|ids| ids.contains(&intent.id))
        {
            Err(Error::Corrupt(
                "multiple staged deletes may own the registered root".into(),
            ))
        } else {
            finish_delete(databases, &mut intent, registered, &mut warning_name)
        };
        if let Err(error) = result {
            journal_changed |= intent.state != initial_state;
            if registered.is_some_and(|entry| {
                conflict || (intent.state.is_prepared() && entry.id == Some(intent.id))
            }) {
                refusals.insert(
                    intent.name.clone(),
                    Refusal {
                        message: format!(
                            "{}: {error}",
                            databases.path().join(&warning_name).display()
                        ),
                        kind: "recovery",
                    },
                );
            }
            warn_retained(databases, &warning_name, Some(&intent.name), error);
            warned.insert(warning_name);
            retained.push(intent);
        }
    }
    #[cfg(test)]
    if RECOVERY_STOP.get() == RecoveryStop::BeforeRemovalSync {
        return Err(Error::invalid(
            "interrupted recovery before syncing removal",
        ));
    }
    if retained.len() != count || journal_changed {
        // Retiring even an already absent cleanup path requires its parent sync.
        databases.sync()?;
        write_intents(catalog.path(), &retained)?;
    }

    for name in databases.names()? {
        let name = name?;
        let text = name.to_string_lossy();
        if text.starts_with(TRASH_PREFIX) && !tracked_trash.contains(text.as_ref()) {
            let result = (|| -> Result<()> {
                let Some(candidate) = child_if_present(databases, &name)? else {
                    return Ok(());
                };
                let lock = lock_dataset(&candidate)?;
                databases.remove_child(&candidate, lock)?;
                Ok(())
            })();
            if let Err(error) = result {
                warn_retained(databases, &name, None, error);
            }
        } else if text.starts_with(KEPT_PREFIX)
            && !warned.contains(&name)
            && !intentional_backup(databases, &name)
        {
            warn_retained(databases, &name, None, "unrecognized kept entry");
        }
    }
    Ok(refusals)
}
#[cfg(test)]
mod tests {
    use super::super::write_file_atomic;
    use super::*;
    use crate::Catalog;
    use std::collections::BTreeMap;
    use std::sync::{Arc, Mutex};
    use tracing::field::{Field, Visit};
    use tracing::{Event, Level, Metadata, Subscriber, span};

    #[derive(Clone, Debug)]
    struct CapturedEvent {
        level: Level,
        target: String,
        fields: BTreeMap<String, String>,
    }

    #[derive(Default)]
    struct FieldCollector(BTreeMap<String, String>);

    impl Visit for FieldCollector {
        fn record_u64(&mut self, field: &Field, value: u64) {
            self.0.insert(field.name().to_string(), value.to_string());
        }
        fn record_str(&mut self, field: &Field, value: &str) {
            self.0.insert(field.name().to_string(), value.to_string());
        }
        fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
            self.0
                .insert(field.name().to_string(), format!("{value:?}"));
        }
    }

    #[derive(Clone, Default)]
    struct EventCapture(Arc<Mutex<Vec<CapturedEvent>>>);

    impl EventCapture {
        fn warnings(&self) -> Vec<CapturedEvent> {
            self.0
                .lock()
                .unwrap()
                .iter()
                .filter(|e| e.level == Level::WARN && e.target == "sparkles::catalog")
                .cloned()
                .collect()
        }
    }

    impl Subscriber for EventCapture {
        fn enabled(&self, metadata: &Metadata<'_>) -> bool {
            metadata.target().starts_with("sparkles::catalog")
        }
        fn new_span(&self, _span: &span::Attributes<'_>) -> span::Id {
            span::Id::from_u64(1)
        }
        fn record(&self, _span: &span::Id, _values: &span::Record<'_>) {}
        fn record_follows_from(&self, _span: &span::Id, _follows: &span::Id) {}
        fn event(&self, event: &Event<'_>) {
            let mut collector = FieldCollector::default();
            event.record(&mut collector);
            self.0.lock().unwrap().push(CapturedEvent {
                level: *event.metadata().level(),
                target: event.metadata().target().to_string(),
                fields: collector.0,
            });
        }
        fn enter(&self, _span: &span::Id) {}
        fn exit(&self, _span: &span::Id) {}
    }

    fn kept_entries(dir: &Path) -> Vec<std::path::PathBuf> {
        let mut out: Vec<_> = std::fs::read_dir(dir.join("databases"))
            .unwrap()
            .map(|e| e.unwrap().path())
            .filter(|p| {
                p.file_name()
                    .unwrap()
                    .to_string_lossy()
                    .starts_with(KEPT_PREFIX)
            })
            .collect();
        out.sort();
        out
    }

    #[test]
    fn interrupted_delete_after_kept_rename_is_removed_on_startup() {
        let dir = tempfile::tempdir().unwrap();
        let cat = Catalog::open(dir.path(), Default::default()).unwrap();
        let active = cat.create("active", &Default::default()).unwrap();
        active.update("INSERT DATA { <urn:a> <urn:p> 1 }").unwrap();
        let active_id = active.dataset_id();
        drop(active);
        let doomed = cat.create("my-doomed", &Default::default()).unwrap();
        doomed.update("INSERT DATA { <urn:d> <urn:p> 2 }").unwrap();
        drop(doomed);

        CRASH_AFTER.set(CRASH_AFTER_SAVE);
        let err = cat.delete("my-doomed");
        CRASH_AFTER.set(0);
        assert!(err.is_err());
        drop(cat);

        // Verify that the journal and staged bytes survived removal from the registry.
        let intent_raw: serde_json::Value =
            serde_json::from_slice(&std::fs::read(dir.path().join(INTENT_FILE)).unwrap()).unwrap();
        assert_eq!(intent_raw["format"], 4);
        let kept = kept_entries(dir.path());
        assert_eq!(kept.len(), 1);
        assert!(
            kept[0]
                .file_name()
                .unwrap()
                .to_string_lossy()
                .starts_with(".kept-my-doomed-")
        );
        assert!(
            retained_bytes(
                &open_databases(&RecoveryDir::open(dir.path()).unwrap()).unwrap(),
                kept[0].file_name().unwrap()
            )
            .count
                > 0
        );
        assert!(!dir.path().join("databases/my-doomed").exists());

        // Next startup finishes removing the kept directory and clears delete.json.
        let capture = EventCapture::default();
        let reopened = tracing::subscriber::with_default(capture.clone(), || {
            Catalog::open(dir.path(), Default::default()).unwrap()
        });
        assert!(kept_entries(dir.path()).is_empty());
        assert!(!dir.path().join(INTENT_FILE).exists());
        assert!(capture.warnings().is_empty(), "{:?}", capture.warnings());
        assert!(reopened.get("my-doomed").is_none());
        let active = reopened.get("active").unwrap();
        assert_eq!(active.dataset_id(), active_id);
        assert_eq!(active.len(), 1);
        drop(active);
        // The deleted name is free for fresh creation.
        drop(reopened.create("my-doomed", &Default::default()).unwrap());
    }

    #[test]
    fn legacy_unmoved_root_has_no_safe_delete_decision() {
        let dir = tempfile::tempdir().unwrap();
        let cat = Catalog::open(dir.path(), Default::default()).unwrap();
        let active = cat.create("active", &Default::default()).unwrap();
        let active_id = active.dataset_id();
        drop(active);
        let doomed = cat.create("doomed", &Default::default()).unwrap();
        doomed.update("INSERT DATA { <urn:d> <urn:p> 2 }").unwrap();
        drop(doomed);

        CRASH_AFTER.set(CRASH_AFTER_SAVE);
        assert!(cat.delete("doomed").is_err());
        CRASH_AFTER.set(0);
        drop(cat);

        let intent = read_intents(dir.path()).unwrap().pop().unwrap();
        let databases = dir.path().join("databases");
        let root = databases.join("doomed");
        std::fs::rename(databases.join(intent.kept_name()), &root).unwrap();
        let journal = dir.path().join(INTENT_FILE);
        std::fs::write(&journal, serde_json::to_vec(&serde_json::json!({
            "format": 1, "intents": [{"name": intent.name, "id": intent.id, "token": intent.token}]
        })).unwrap()).unwrap();

        let capture = EventCapture::default();
        let reopened = tracing::subscriber::with_default(capture.clone(), || {
            Catalog::open(dir.path(), Default::default()).unwrap()
        });
        assert!(root.join("dataset.json").exists());
        assert!(journal.exists());
        assert_eq!(reopened.get("active").unwrap().dataset_id(), active_id);
        assert!(capture.warnings().iter().any(|event| {
            event.fields["path"].ends_with("databases/doomed")
                && event.fields.contains_key("retained_bytes")
        }));
    }

    #[test]
    fn interrupted_delete_before_save_preserves_registered_dataset() {
        let dir = tempfile::tempdir().unwrap();
        let cat = Catalog::open(dir.path(), Default::default()).unwrap();
        let ds = cat.create("keepme", &Default::default()).unwrap();
        ds.update("INSERT DATA { <urn:k> <urn:p> 9 }").unwrap();
        let id = ds.dataset_id();
        drop(ds);

        CRASH_AFTER.set(CRASH_AFTER_INTENT);
        assert!(cat.delete("keepme").is_err());
        CRASH_AFTER.set(0);
        drop(cat);

        assert!(dir.path().join(INTENT_FILE).exists());
        let reopened = Catalog::open(dir.path(), Default::default()).unwrap();
        assert!(!dir.path().join(INTENT_FILE).exists());
        let ds = reopened
            .get("keepme")
            .expect("registered dataset must survive");
        assert_eq!(ds.dataset_id(), id);
        assert_eq!(ds.len(), 1);
    }

    #[test]
    fn recreated_dataset_with_same_name_stays_intact_while_old_kept_dir_is_removed() {
        let dir = tempfile::tempdir().unwrap();
        let cat = Catalog::open(dir.path(), Default::default()).unwrap();
        let old = cat.create("ds", &Default::default()).unwrap();
        old.update("INSERT DATA { <urn:old> <urn:p> 1 }").unwrap();
        let old_id = old.dataset_id();
        drop(old);

        CRASH_AFTER.set(CRASH_AFTER_SAVE);
        assert!(cat.delete("ds").is_err());
        CRASH_AFTER.set(0);
        // Simulate a new dataset created under the same name before restart.
        let replacement = cat.create("ds", &Default::default()).unwrap();
        replacement
            .update("INSERT DATA { <urn:new> <urn:p> 2 }")
            .unwrap();
        let new_id = replacement.dataset_id();
        assert_ne!(old_id, new_id);
        drop(replacement);
        drop(cat);

        assert_eq!(kept_entries(dir.path()).len(), 1);
        assert!(dir.path().join(INTENT_FILE).exists());

        let reopened = Catalog::open(dir.path(), Default::default()).unwrap();
        assert!(kept_entries(dir.path()).is_empty());
        assert!(!dir.path().join(INTENT_FILE).exists());
        let live = reopened
            .get("ds")
            .expect("replacement dataset must survive");
        assert_eq!(live.dataset_id(), new_id);
        assert_eq!(live.len(), 1);
    }

    #[test]
    fn normal_delete_and_trash_rename_fallback_leave_no_markers_behind() {
        for fail_mask in [
            0u16,
            1 << super::super::DELETE_TRASH_RENAME,
            1 << super::super::DELETE_TRASH_SYNC,
        ] {
            let dir = tempfile::tempdir().unwrap();
            let cat = Catalog::open(dir.path(), Default::default()).unwrap();
            let ds = cat.create("temp", &Default::default()).unwrap();
            ds.update("INSERT DATA { <urn:t> <urn:p> 1 }").unwrap();
            drop(ds);

            let journal = dir.path().join(INTENT_FILE);
            super::super::DELETE_HOOK.set(Some(Box::new(move || {
                assert_eq!(journal.exists(), fail_mask != 0);
            })));
            super::super::DELETE_FAIL.set(fail_mask);
            let ok = cat.delete("temp");
            super::super::DELETE_FAIL.set(0);
            super::super::DELETE_HOOK.set(None);
            assert!(ok.unwrap(), "fail_mask={fail_mask}");
            assert!(kept_entries(dir.path()).is_empty(), "fail_mask={fail_mask}");
            assert!(
                !dir.path().join(INTENT_FILE).exists(),
                "fail_mask={fail_mask}"
            );
            let db_entries: Vec<_> = std::fs::read_dir(dir.path().join("databases"))
                .unwrap()
                .collect();
            assert!(db_entries.is_empty(), "fail_mask={fail_mask}");
        }
    }

    #[test]
    fn unrecognized_foreign_kept_directory_logs_warning_and_is_not_removed() {
        let dir = tempfile::tempdir().unwrap();
        let cat = Catalog::open(dir.path(), Default::default()).unwrap();
        let active = cat.create("active", &Default::default()).unwrap();
        let active_id = active.dataset_id();
        drop(active);
        drop(cat);

        let foreign = dir.path().join("databases/.kept-foreign-manual");
        std::fs::create_dir_all(foreign.join("nested")).unwrap();
        std::fs::write(foreign.join("nested/payload.bin"), vec![0xAB; 2048]).unwrap();

        let capture = EventCapture::default();
        let reopened = tracing::subscriber::with_default(capture.clone(), || {
            Catalog::open(dir.path(), Default::default()).unwrap()
        });
        assert_eq!(reopened.get("active").unwrap().dataset_id(), active_id);
        assert!(foreign.join("nested/payload.bin").exists());
        let warnings = capture.warnings();
        assert_eq!(warnings.len(), 1, "{warnings:?}");
        assert!(
            warnings[0]
                .fields
                .get("path")
                .is_some_and(|p| p.contains(".kept-foreign-manual")),
            "{warnings:?}"
        );
        assert_eq!(
            warnings[0].fields.get("retained_bytes").map(String::as_str),
            Some("2048"),
            "{warnings:?}"
        );
    }

    #[cfg(unix)]
    #[test]
    fn unrecognized_kept_names_with_non_utf8_bytes_are_reported() {
        use std::os::unix::ffi::OsStrExt;

        let dir = tempfile::tempdir().unwrap();
        drop(Catalog::open(dir.path(), Default::default()).unwrap());
        let name = OsStr::from_bytes(b".kept-foreign-\xff");
        let kept = dir.path().join("databases").join(name);
        std::fs::create_dir(&kept).unwrap();
        std::fs::write(kept.join("file"), b"retained").unwrap();

        let capture = EventCapture::default();
        drop(tracing::subscriber::with_default(capture.clone(), || {
            Catalog::open(dir.path(), Default::default()).unwrap()
        }));
        assert!(kept.exists());
        let warnings = capture.warnings();
        assert_eq!(warnings.len(), 1, "{warnings:?}");
        assert_eq!(warnings[0].fields["retained_bytes"], "8");
    }

    #[test]
    fn unknown_numbered_copy_of_live_dataset_is_preserved_and_warned() {
        let dir = tempfile::tempdir().unwrap();
        let cat = Catalog::open(dir.path(), Default::default()).unwrap();
        drop(cat.create("ds", &Default::default()).unwrap());
        drop(cat);

        let kept = dir.path().join("databases/.kept-ds-42");
        std::fs::create_dir_all(&kept).unwrap();
        std::fs::write(kept.join("marker"), b"old copy").unwrap();

        let capture = EventCapture::default();
        let reopened = tracing::subscriber::with_default(capture.clone(), || {
            Catalog::open(dir.path(), Default::default()).unwrap()
        });
        assert!(reopened.get("ds").is_some());
        assert!(kept.join("marker").exists());
        assert_eq!(capture.warnings().len(), 1, "{:?}", capture.warnings());
    }

    #[cfg(unix)]
    #[test]
    fn symlinks_in_or_at_kept_directories_never_affect_external_targets() {
        use std::os::unix::fs::symlink;

        let outside = tempfile::tempdir().unwrap();
        let sentinel = outside.path().join("sentinel.txt");
        std::fs::write(&sentinel, vec![b'S'; 1024]).unwrap();

        // 1. Top-level symlink without delete intent: untouched, warned with 0 bytes.
        let dir = tempfile::tempdir().unwrap();
        let cat = Catalog::open(dir.path(), Default::default()).unwrap();
        drop(cat);
        let top_link = dir.path().join("databases/.kept-escape-1");
        symlink(outside.path(), &top_link).unwrap();

        let capture = EventCapture::default();
        drop(tracing::subscriber::with_default(capture.clone(), || {
            Catalog::open(dir.path(), Default::default()).unwrap()
        }));
        assert!(
            std::fs::symlink_metadata(&top_link)
                .unwrap()
                .file_type()
                .is_symlink()
        );
        assert_eq!(std::fs::read(&sentinel).unwrap().len(), 1024);
        let warnings = capture.warnings();
        assert_eq!(warnings.len(), 1, "{warnings:?}");
        assert_eq!(
            warnings[0].fields.get("retained_bytes").map(String::as_str),
            Some("0"),
            "{warnings:?}"
        );

        // 2. Top-level symlink even WITH a matching delete intent: never followed or removed.
        write_registry(dir.path(), &Registry::default()).unwrap();
        let token = Uuid::new_v4();
        let intent_link = dir.path().join(format!("databases/.kept-evil-{token}"));
        symlink(outside.path(), &intent_link).unwrap();
        record(
            dir.path(),
            &DeleteIntent {
                name: "evil".into(),
                id: Uuid::new_v4(),
                token,
                state: IntentState::Prepared,
            },
        )
        .unwrap();
        let capture = EventCapture::default();
        drop(tracing::subscriber::with_default(capture.clone(), || {
            Catalog::open(dir.path(), Default::default()).unwrap()
        }));
        assert!(
            std::fs::symlink_metadata(&intent_link)
                .unwrap()
                .file_type()
                .is_symlink()
        );
        assert_eq!(std::fs::read(&sentinel).unwrap().len(), 1024);
        assert!(
            capture.warnings().iter().any(|w| w
                .fields
                .get("path")
                .is_some_and(|p| p.contains(&token.to_string()))),
            "{:?}",
            capture.warnings()
        );

        // 3. Symlink nested INSIDE a real intent-backed kept directory: kept dir removed, external target untouched.
        let dir2 = tempfile::tempdir().unwrap();
        let cat = Catalog::open(dir2.path(), Default::default()).unwrap();
        drop(cat.create("doomed", &Default::default()).unwrap());
        CRASH_AFTER.set(CRASH_AFTER_SAVE);
        assert!(cat.delete("doomed").is_err());
        CRASH_AFTER.set(0);
        drop(cat);
        let kept = kept_entries(dir2.path());
        assert_eq!(kept.len(), 1);
        symlink(outside.path(), kept[0].join("nested-link")).unwrap();
        drop(Catalog::open(dir2.path(), Default::default()).unwrap());
        assert!(kept_entries(dir2.path()).is_empty());
        assert!(!dir2.path().join(INTENT_FILE).exists());
        assert_eq!(std::fs::read(&sentinel).unwrap().len(), 1024);
    }

    #[cfg(unix)]
    #[test]
    fn unremovable_kept_directory_warns_without_breaking_startup_and_retries_later() {
        use std::os::unix::fs::PermissionsExt;

        // Root bypasses POSIX directory write permission checks.
        if unsafe { libc::geteuid() } == 0 {
            return;
        }

        let dir = tempfile::tempdir().unwrap();
        let cat = Catalog::open(dir.path(), Default::default()).unwrap();
        let active = cat.create("active", &Default::default()).unwrap();
        let active_id = active.dataset_id();
        drop(active);
        let doomed = cat.create("doomed", &Default::default()).unwrap();
        doomed.update("INSERT DATA { <urn:d> <urn:p> 1 }").unwrap();
        drop(doomed);

        CRASH_AFTER.set(CRASH_AFTER_SAVE);
        assert!(cat.delete("doomed").is_err());
        CRASH_AFTER.set(0);
        drop(cat);

        let kept = kept_entries(dir.path());
        assert_eq!(kept.len(), 1);
        let locked_dir = kept[0].join("locked");
        std::fs::create_dir_all(&locked_dir).unwrap();
        std::fs::write(locked_dir.join("file.bin"), vec![1u8; 512]).unwrap();
        std::fs::set_permissions(&locked_dir, std::fs::Permissions::from_mode(0o500)).unwrap();

        let journal = std::fs::read(dir.path().join(INTENT_FILE)).unwrap();
        std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o500)).unwrap();
        let capture = EventCapture::default();
        let reopened = tracing::subscriber::with_default(capture.clone(), || {
            Catalog::open(dir.path(), Default::default()).unwrap()
        });
        // Startup succeeds and serves healthy datasets despite the permission failure.
        assert_eq!(reopened.get("active").unwrap().dataset_id(), active_id);
        assert!(kept[0].exists());
        assert!(dir.path().join(INTENT_FILE).exists());
        let warnings = capture.warnings();
        assert_eq!(warnings.len(), 1, "{warnings:?}");
        assert!(
            warnings[0]
                .fields
                .get("path")
                .is_some_and(|p| p.contains(".kept-doomed-")),
            "{warnings:?}"
        );
        let reported_bytes: u64 = warnings[0]
            .fields
            .get("retained_bytes")
            .expect("retained_bytes field")
            .parse()
            .unwrap();
        assert!(reported_bytes >= 512, "reported_bytes={reported_bytes}");
        drop(reopened);

        assert_eq!(
            std::fs::read(dir.path().join(INTENT_FILE)).unwrap(),
            journal
        );
        std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        // Restore permissions: next startup retries and finishes cleanup.
        std::fs::set_permissions(&locked_dir, std::fs::Permissions::from_mode(0o700)).unwrap();
        let reopened = Catalog::open(dir.path(), Default::default()).unwrap();
        assert!(kept_entries(dir.path()).is_empty());
        assert!(!dir.path().join(INTENT_FILE).exists());
        assert_eq!(reopened.get("active").unwrap().dataset_id(), active_id);
    }

    #[test]
    fn unknown_intent_version_and_corrupt_intents_are_refused_cleanly() {
        let dir = tempfile::tempdir().unwrap();
        let cat = Catalog::open(dir.path(), Default::default()).unwrap();
        drop(cat.create("ds", &Default::default()).unwrap());
        drop(cat);
        let kept = dir.path().join("databases/.kept-ghost-1");
        std::fs::create_dir_all(&kept).unwrap();

        // Newer version is refused with Unsupported.
        std::fs::write(
            dir.path().join(INTENT_FILE),
            br#"{"format": 5, "future": true}"#,
        )
        .unwrap();
        assert!(matches!(
            Catalog::open(dir.path(), Default::default()),
            Err(Error::Unsupported(_))
        ));
        assert!(kept.exists());

        // Missing format is refused with Corrupt.
        std::fs::write(dir.path().join(INTENT_FILE), br#"{"intents": []}"#).unwrap();
        assert!(matches!(
            Catalog::open(dir.path(), Default::default()),
            Err(Error::Corrupt(_))
        ));

        // Unknown field is refused with Corrupt.
        std::fs::write(
            dir.path().join(INTENT_FILE),
            br#"{"format": 1, "intents": [], "extra": true}"#,
        )
        .unwrap();
        assert!(matches!(
            Catalog::open(dir.path(), Default::default()),
            Err(Error::Corrupt(_))
        ));

        // Delete refuses before closing a dataset if delete.json has an unknown version.
        std::fs::remove_file(dir.path().join(INTENT_FILE)).unwrap();
        let cat = Catalog::open(dir.path(), Default::default()).unwrap();
        std::fs::write(
            dir.path().join(INTENT_FILE),
            br#"{"format": 5, "future": true}"#,
        )
        .unwrap();
        assert!(matches!(cat.delete("ds"), Err(Error::Unsupported(_))));
        assert!(
            cat.get("ds").is_some(),
            "dataset must stay open on preflight refusal"
        );
    }

    #[test]
    fn mismatched_uuid_in_kept_dir_refuses_auto_removal_and_warns() {
        let dir = tempfile::tempdir().unwrap();
        let cat = Catalog::open(dir.path(), Default::default()).unwrap();
        drop(cat.create("doomed", &Default::default()).unwrap());
        CRASH_AFTER.set(CRASH_AFTER_SAVE);
        assert!(cat.delete("doomed").is_err());
        CRASH_AFTER.set(0);
        drop(cat);

        // Tamper with dataset.json UUID inside the kept directory so it mismatches delete.json.
        let kept = kept_entries(dir.path());
        assert_eq!(kept.len(), 1);
        let ds_json = kept[0].join("dataset.json");
        let mut val: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&ds_json).unwrap()).unwrap();
        val["id"] = Uuid::new_v4().to_string().into();
        std::fs::write(&ds_json, serde_json::to_vec(&val).unwrap()).unwrap();

        let capture = EventCapture::default();
        drop(tracing::subscriber::with_default(capture.clone(), || {
            Catalog::open(dir.path(), Default::default()).unwrap()
        }));
        assert!(kept[0].exists(), "mismatched kept dir must not be deleted");
        let warnings = capture.warnings();
        assert_eq!(warnings.len(), 1, "{warnings:?}");
        assert!(
            warnings[0]
                .fields
                .get("path")
                .is_some_and(|p| p.contains(".kept-doomed-")),
            "{warnings:?}"
        );
    }
    fn interrupted_delete(dir: &Path, stage: u8) -> DeleteIntent {
        let catalog = Catalog::open(dir, Default::default()).unwrap();
        let dataset = catalog.create("doomed", &Default::default()).unwrap();
        dataset.update("INSERT DATA { <urn:d> <urn:p> 2 }").unwrap();
        drop(dataset);
        CRASH_AFTER.set(stage);
        assert!(catalog.delete("doomed").is_err());
        CRASH_AFTER.set(0);
        read_intents(dir).unwrap().pop().unwrap()
    }

    #[test]
    fn r2_missing_registry_preserves_the_precommit_copy() {
        let dir = tempfile::tempdir().unwrap();
        let intent = interrupted_delete(dir.path(), CRASH_AFTER_KEPT_RENAME);
        let config = dir.path().join(super::super::CONFIG_FILE);
        let backup = dir.path().join("original-config");
        let kept = dir.path().join("databases").join(intent.kept_name());
        let journal = dir.path().join(INTENT_FILE);
        let bytes = std::fs::read(&journal).unwrap();
        std::fs::rename(&config, &backup).unwrap();

        let opened = Catalog::open(dir.path(), Default::default());
        assert!(kept.join("dataset.json").exists());
        assert!(matches!(opened, Err(Error::Corrupt(_))));
        assert_eq!(std::fs::read(&journal).unwrap(), bytes);

        std::fs::rename(backup, config).unwrap();
        let catalog = Catalog::open(dir.path(), Default::default()).unwrap();
        let data = catalog.get("doomed").unwrap();
        assert_eq!((data.dataset_id(), data.len()), (intent.id, 1));
        assert!(!kept.exists() && !journal.exists());
    }

    #[cfg(unix)]
    #[test]
    fn r2_linked_registry_preserves_the_precommit_copy() {
        use std::os::unix::fs::symlink;

        for dangling in [true, false] {
            let dir = tempfile::tempdir().unwrap();
            let intent = interrupted_delete(dir.path(), CRASH_AFTER_KEPT_RENAME);
            let config = dir.path().join(super::super::CONFIG_FILE);
            let backup = dir.path().join("original-config");
            let external = tempfile::tempdir().unwrap();
            let target = external.path().join("redirected");
            let empty = br#"{"format":2,"datasets":[]}"#;
            if !dangling {
                std::fs::write(&target, empty).unwrap();
            }
            let kept = dir.path().join("databases").join(intent.kept_name());
            let journal = dir.path().join(INTENT_FILE);
            let bytes = std::fs::read(&journal).unwrap();
            std::fs::rename(&config, &backup).unwrap();
            symlink(&target, &config).unwrap();

            let opened = Catalog::open(dir.path(), Default::default());
            assert!(kept.join("dataset.json").exists(), "dangling={dangling}");
            assert!(matches!(opened, Err(Error::Corrupt(_))));
            assert_eq!(std::fs::read(&journal).unwrap(), bytes);
            if !dangling {
                assert_eq!(std::fs::read(&target).unwrap(), empty);
            }

            std::fs::remove_file(&config).unwrap();
            std::fs::rename(backup, config).unwrap();
            let catalog = Catalog::open(dir.path(), Default::default()).unwrap();
            let data = catalog.get("doomed").unwrap();
            assert_eq!((data.dataset_id(), data.len()), (intent.id, 1));
        }
    }

    fn copy_store(from: &Path, to: &Path) {
        std::fs::create_dir(to).unwrap();
        for entry in std::fs::read_dir(from).unwrap() {
            let entry = entry.unwrap();
            let name = entry.file_name();
            if name == "sparkles.lock" {
                continue;
            }
            let dst = to.join(name);
            if entry.file_type().unwrap().is_dir() {
                copy_store(&entry.path(), &dst);
            } else {
                std::fs::copy(entry.path(), dst).unwrap();
            }
        }
    }

    #[cfg(feature = "backup")]
    #[test]
    fn r2_same_uuid_restore_supersedes_the_retained_delete() {
        use crate::catalog::{ReservationKind, swap};
        use crate::store::Store;

        for missing in [false, true] {
            let dir = tempfile::tempdir().unwrap();
            let intent = interrupted_delete(dir.path(), CRASH_AFTER_SAVE);
            let db = dir.path().join("databases");
            let kept = db.join(intent.kept_name());
            let root = db.join("doomed");
            let busy = Store::open_existing(&kept, Default::default(), None).unwrap();
            let old_head = busy.head_commit();
            let catalog = Catalog::open(dir.path(), Default::default()).unwrap();
            assert!(catalog.get("doomed").is_none() && kept.exists());

            let staged = db.join(".restore-doomed-42");
            copy_store(&kept, &staged);
            let copy = Store::open_existing(&staged, Default::default(), None).unwrap();
            crate::sparql::update::update(
                &copy,
                "INSERT DATA { <urn:replacement> <urn:p> 3 }",
                &Default::default(),
            )
            .unwrap();
            let new_head = copy.head_commit();
            assert_ne!(new_head, old_head);
            drop(copy);
            let reservation = catalog
                .reserve("doomed", ReservationKind::Restore, "42")
                .unwrap();
            let restored = swap::publish_new(&catalog, reservation, &staged).unwrap();
            assert_eq!(
                (restored.dataset_id(), restored.head_commit()),
                (intent.id, new_head)
            );
            drop((restored, catalog, busy));
            if missing {
                std::fs::remove_dir_all(&root).unwrap();
            }

            let reopened = Catalog::open(dir.path(), Default::default()).unwrap();
            if missing {
                assert!(reopened.get("doomed").is_none());
                assert_eq!(reopened.refusal("doomed").unwrap().kind, "missing");
                assert!(!root.exists());
            } else {
                let restored = reopened.get("doomed").unwrap();
                assert_eq!(
                    (
                        restored.dataset_id(),
                        restored.head_commit(),
                        restored.len()
                    ),
                    (intent.id, new_head, 2)
                );
            }
            assert!(!kept.exists() && !dir.path().join(INTENT_FILE).exists());
        }
    }

    #[cfg(feature = "backup")]
    #[test]
    fn r2_older_delete_allows_a_later_restore_to_roll_back() {
        use crate::catalog::swap;
        use crate::store::Store;

        let dir = tempfile::tempdir().unwrap();
        let intent = interrupted_delete(dir.path(), CRASH_AFTER_SAVE);
        let db = dir.path().join("databases");
        let kept = db.join(intent.kept_name());
        let busy = Store::open_existing(&kept, Default::default(), None).unwrap();
        let catalog = Catalog::open(dir.path(), Default::default()).unwrap();
        let newer = catalog.create("doomed", &Default::default()).unwrap();
        newer
            .update("INSERT DATA { <urn:newer> <urn:p> 3 }")
            .unwrap();
        let id = newer.dataset_id();
        let head = newer.head_commit();
        assert_ne!(id, intent.id);
        drop(newer);
        let staged = db.join(".restore-doomed-42");
        drop(Store::open(&staged, Default::default()).unwrap());

        swap::CRASH_AFTER.set(1);
        let result = swap::replace_in_place(&catalog, "doomed", &staged, "42", false);
        swap::CRASH_AFTER.set(0);
        assert!(result.is_err());
        drop((catalog, busy));
        let replaced = db.join(".replaced-doomed-42");
        assert!(replaced.exists() && !db.join("doomed").exists());

        let reopened = Catalog::open(dir.path(), Default::default()).unwrap();
        let newer = reopened.get("doomed").unwrap();
        assert_eq!(
            (newer.dataset_id(), newer.head_commit(), newer.len()),
            (id, head, 1)
        );
        assert!(!replaced.exists() && !kept.exists());
    }

    #[cfg(unix)]
    #[test]
    fn r2_repointed_parent_cannot_commit_a_staged_delete() {
        use super::super::DELETE_STAGE_HOOK;
        use std::os::unix::fs::symlink;

        let dir = tempfile::tempdir().unwrap();
        let catalog = Catalog::open(dir.path(), Default::default()).unwrap();
        let data = catalog.create("doomed", &Default::default()).unwrap();
        data.update("INSERT DATA { <urn:d> <urn:p> 2 }").unwrap();
        let id = data.dataset_id();
        drop(data);
        let parent = dir.path().join("databases");
        let original = dir.path().join("original-databases");
        let external = tempfile::tempdir().unwrap();
        let target = external.path().to_path_buf();
        let swap = (parent.clone(), original.clone());
        DELETE_STAGE_HOOK.set(Some(Box::new(move || {
            std::fs::rename(&swap.0, &swap.1).unwrap();
            symlink(&target, &swap.0).unwrap();
        })));
        let deleted = catalog.delete("doomed");
        DELETE_STAGE_HOOK.set(None);

        let intents = read_intents(dir.path()).unwrap();
        assert!(deleted.is_err());
        assert_eq!(intents.len(), 1);
        assert!(
            original
                .join(intents[0].kept_name())
                .join("CURRENT")
                .exists()
        );
        assert!(external.path().read_dir().unwrap().next().is_none());
        assert_eq!(read_registry(dir.path()).unwrap().datasets[0].id, Some(id));
        drop(catalog);
        std::fs::remove_file(&parent).unwrap();
        std::fs::rename(original, parent).unwrap();
        let reopened = Catalog::open(dir.path(), Default::default()).unwrap();
        let data = reopened.get("doomed").unwrap();
        assert_eq!((data.dataset_id(), data.len()), (id, 1));
    }

    #[cfg(unix)]
    #[test]
    fn r2_live_cleanup_uses_the_parent_it_published_to() {
        use super::super::DELETE_HOOK;
        use std::os::unix::fs::symlink;

        let dir = tempfile::tempdir().unwrap();
        let catalog = Catalog::open(dir.path(), Default::default()).unwrap();
        drop(catalog.create("doomed", &Default::default()).unwrap());
        let parent = dir.path().join("databases");
        let original = dir.path().join("original-databases");
        let external = tempfile::tempdir().unwrap();
        let target = external.path().to_path_buf();
        let old = original.clone();
        DELETE_HOOK.set(Some(Box::new(move || {
            let basename = parent
                .read_dir()
                .unwrap()
                .next()
                .unwrap()
                .unwrap()
                .file_name();
            let sentinel = target.join(basename);
            std::fs::create_dir(&sentinel).unwrap();
            std::fs::write(sentinel.join("sentinel"), b"keep").unwrap();
            std::fs::rename(&parent, &old).unwrap();
            symlink(&target, &parent).unwrap();
        })));
        let deleted = catalog.delete("doomed");
        DELETE_HOOK.set(None);

        assert!(deleted.unwrap());
        let sentinel = external.path().read_dir().unwrap().next().unwrap().unwrap();
        assert_eq!(
            std::fs::read(sentinel.path().join("sentinel")).unwrap(),
            b"keep"
        );
        assert!(original.read_dir().unwrap().next().is_none());
    }

    #[cfg(unix)]
    #[test]
    fn r2_live_removal_excludes_openers_during_traversal() {
        use super::super::delete_fs::DURING_REMOVAL;
        use crate::store::Store;

        for (fail, prefix) in [
            (0, TRASH_PREFIX),
            (1 << super::super::DELETE_TRASH_RENAME, KEPT_PREFIX),
        ] {
            let dir = tempfile::tempdir().unwrap();
            let catalog = Catalog::open(dir.path(), Default::default()).unwrap();
            let data = catalog.create("doomed", &Default::default()).unwrap();
            let root = data.store().root().unwrap();
            std::fs::write(root.join("unrelated"), b"scratch").unwrap();
            drop(data);
            let seen = std::rc::Rc::new(std::cell::Cell::new(false));
            let observed = seen.clone();
            DURING_REMOVAL.set(Some(Box::new(move |root| {
                assert!(
                    root.file_name()
                        .unwrap()
                        .to_str()
                        .unwrap()
                        .starts_with(prefix)
                );
                std::fs::remove_file(root.join("unrelated")).unwrap();
                assert!(root.join("CURRENT").is_file() && root.join("dataset.json").is_file());
                assert!(matches!(
                    Store::open_existing(root, Default::default(), None),
                    Err(Error::Locked { .. })
                ));
                observed.set(true);
            })));
            super::super::DELETE_FAIL.set(fail);
            let result = catalog.delete("doomed");
            super::super::DELETE_FAIL.set(0);
            DURING_REMOVAL.set(None);
            assert!(result.unwrap());
            assert!(seen.get());
            assert!(
                dir.path()
                    .join("databases")
                    .read_dir()
                    .unwrap()
                    .next()
                    .is_none()
            );
        }
    }

    #[test]
    fn r2_wrong_replacement_identity_is_checked_before_creating_its_lock() {
        let dir = tempfile::tempdir().unwrap();
        let intent = interrupted_delete(dir.path(), CRASH_AFTER_SAVE);
        let db = dir.path().join("databases");
        let busy = crate::store::Store::open_existing(
            &db.join(intent.kept_name()),
            Default::default(),
            None,
        )
        .unwrap();
        let catalog = Catalog::open(dir.path(), Default::default()).unwrap();
        drop(catalog.create("doomed", &Default::default()).unwrap());
        drop((catalog, busy));
        let mut registry = read_registry(dir.path()).unwrap();
        registry.datasets[0].id = Some(Uuid::new_v4());
        write_registry(dir.path(), &registry).unwrap();
        let lock = db.join("doomed/sparkles.lock");
        std::fs::remove_file(&lock).unwrap();

        let (_catalog, pending) = Catalog::open_deferred(dir.path(), Default::default()).unwrap();
        assert_eq!(pending, ["doomed"]);
        assert!(!lock.exists());
        assert!(db.join(intent.kept_name()).exists());
        assert!(dir.path().join(INTENT_FILE).exists());
    }

    #[cfg(feature = "backup")]
    #[test]
    fn r2_conflict_reports_the_extant_trash_copy() {
        let dir = tempfile::tempdir().unwrap();
        let intent = interrupted_delete(dir.path(), CRASH_AFTER_KEPT_RENAME);
        let db = dir.path().join("databases");
        let trash = db.join(intent.trash_name());
        std::fs::rename(db.join(intent.kept_name()), &trash).unwrap();
        let replaced = db.join(".replaced-doomed-42");
        drop(crate::store::Store::open(&replaced, Default::default()).unwrap());

        let capture = EventCapture::default();
        let catalog = tracing::subscriber::with_default(capture.clone(), || {
            Catalog::open(dir.path(), Default::default()).unwrap()
        });
        assert_eq!(catalog.refusal("doomed").unwrap().kind, "recovery");
        assert!(trash.exists() && replaced.exists());
        let warnings = capture.warnings();
        assert_eq!(warnings.len(), 1, "{warnings:?}");
        assert_eq!(warnings[0].fields["path"], trash.display().to_string());
        assert!(warnings[0].fields["retained_bytes"].parse::<u64>().unwrap() > 0);
    }

    #[test]
    fn r2_retry_rolls_back_before_checking_an_older_unstaged_intent() {
        let dir = tempfile::tempdir().unwrap();
        let newer = interrupted_delete(dir.path(), CRASH_AFTER_KEPT_RENAME);
        let older = DeleteIntent {
            token: Uuid::new_v4(),
            ..newer.clone()
        };
        write_intents(dir.path(), &[older, newer.clone()]).unwrap();

        let catalog = Catalog::open(dir.path(), Default::default()).unwrap();
        assert!(
            catalog.refusal("doomed").is_none(),
            "{:?}",
            catalog.refusals()
        );
        let data = catalog.get("doomed").unwrap();
        assert_eq!((data.dataset_id(), data.len()), (newer.id, 1));
        assert!(!dir.path().join(INTENT_FILE).exists());
    }

    #[cfg(feature = "backup")]
    #[test]
    fn r2_consult_retirement_has_a_barrier_before_registry_publication() {
        use super::super::{DELETE_HOOK, DELETE_STAGE_HOOK, take_catalog_dir_syncs};
        use crate::catalog::{ReservationKind, swap};
        use crate::store::Store;

        for rewrite in [false, true] {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().to_path_buf();
            let catalog = Catalog::open(&path, Default::default()).unwrap();
            let old = catalog.create("doomed", &Default::default()).unwrap();
            old.update("INSERT DATA { <urn:old> <urn:p> 1 }").unwrap();
            let old_head = old.head_commit();
            let id = old.dataset_id();
            drop(old);

            let root = path.join("databases/doomed");
            let staged = path.join("databases/.restore-doomed-42");
            copy_store(&root, &staged);
            let copy = Store::open_existing(&staged, Default::default(), None).unwrap();
            crate::sparql::update::update(
                &copy,
                "INSERT DATA { <urn:new> <urn:p> 2 }",
                &Default::default(),
            )
            .unwrap();
            let new_head = copy.head_commit();
            assert_ne!(new_head, old_head);
            drop(copy);
            if rewrite {
                record(
                    &path,
                    &DeleteIntent {
                        name: "unrelated".into(),
                        id: Uuid::new_v4(),
                        token: Uuid::new_v4(),
                        state: IntentState::Prepared,
                    },
                )
                .unwrap();
            }

            let captured = std::rc::Rc::new(std::cell::RefCell::new(None));
            let original = captured.clone();
            let stage_path = path.clone();
            DELETE_STAGE_HOOK.set(Some(Box::new(move || {
                *original.borrow_mut() = read_intents(&stage_path).unwrap().pop();
                FAIL_JOURNAL_SYNC.set(true);
            })));
            let restore_catalog = catalog.clone();
            DELETE_HOOK.set(Some(Box::new(move || {
                let intent = captured.borrow().clone().unwrap();
                assert!(path.join("databases").join(intent.trash_name()).exists());
                let observed = read_intents(&path).unwrap();
                assert_eq!(observed.len(), usize::from(rewrite));
                assert!(observed.iter().all(|record| record.token != intent.token));

                let reservation = restore_catalog
                    .reserve("doomed", ReservationKind::Restore, "42")
                    .unwrap();
                take_catalog_dir_syncs();
                let new = swap::publish_new(&restore_catalog, reservation, &staged).unwrap();
                assert_eq!((new.dataset_id(), new.head_commit()), (id, new_head));
                let syncs = take_catalog_dir_syncs();
                // The first catalog sync makes the journal unlink or rewrite durable.
                // The second follows publication of the replacement registry.
                assert_eq!(
                    syncs.iter().filter(|dir| *dir == &path).count(),
                    2,
                    "{syncs:?}"
                );
            })));
            let result = catalog.delete("doomed");
            DELETE_STAGE_HOOK.set(None);
            DELETE_HOOK.set(None);
            assert!(result.unwrap());
        }
    }

    #[test]
    fn r2_consult_v3_ambiguity_survives_a_journal_rewrite() {
        use crate::store::Store;

        let dir = tempfile::tempdir().unwrap();
        let intent = interrupted_delete(dir.path(), CRASH_AFTER_KEPT_RENAME);
        let old = dir.path().join("databases").join(intent.kept_name());
        let root = dir.path().join("databases/doomed");
        let store = Store::open_existing(&old, Default::default(), None).unwrap();
        let old_head = store.head_commit();
        drop(store);

        let unrelated = DeleteIntent {
            name: "unrelated".into(),
            id: Uuid::new_v4(),
            token: Uuid::new_v4(),
            state: IntentState::Prepared,
        };
        let journal = dir.path().join(INTENT_FILE);
        let bytes = serde_json::to_vec(&serde_json::json!({
            "format": 3,
            "intents": [intent, unrelated],
        }))
        .unwrap();
        std::fs::write(&journal, bytes).unwrap();

        for _ in 0..2 {
            let catalog = Catalog::open(dir.path(), Default::default()).unwrap();
            assert!(catalog.get("doomed").is_none());
            assert_eq!(catalog.refusal("doomed").unwrap().kind, "recovery");
            assert!(!root.exists());
            assert!(old.exists());
            let rewritten: serde_json::Value =
                serde_json::from_slice(&std::fs::read(&journal).unwrap()).unwrap();
            assert_eq!(rewritten["format"], 4);
            assert_eq!(rewritten["intents"].as_array().unwrap().len(), 1);
            assert_eq!(rewritten["intents"][0]["state"], "prepared-v3");
            drop(catalog);
            let store = Store::open_existing(&old, Default::default(), None).unwrap();
            assert_eq!(store.head_commit(), old_head);
        }
    }

    #[cfg(feature = "backup")]
    #[test]
    fn r2_consult_adoption_preserves_bytes_after_registry_sync_error() {
        use super::super::FAIL_RESTORE_REGISTRY_SYNC;
        use crate::catalog::{ReservationKind, swap};
        use crate::store::Store;

        let dir = tempfile::tempdir().unwrap();
        let intent = interrupted_delete(dir.path(), CRASH_AFTER_SAVE);
        let db = dir.path().join("databases");
        let kept = db.join(intent.kept_name());
        let busy = Store::open_existing(&kept, Default::default(), None).unwrap();
        let catalog = Catalog::open(dir.path(), Default::default()).unwrap();
        let staged = db.join(".restore-doomed-42");
        copy_store(&kept, &staged);
        let copy = Store::open_existing(&staged, Default::default(), None).unwrap();
        crate::sparql::update::update(
            &copy,
            "INSERT DATA { <urn:new> <urn:p> 2 }",
            &Default::default(),
        )
        .unwrap();
        let head = copy.head_commit();
        drop(copy);

        let reservation = catalog
            .reserve("doomed", ReservationKind::Restore, "42")
            .unwrap();
        FAIL_RESTORE_REGISTRY_SYNC.set(true);
        let result = swap::publish_new(&catalog, reservation, &staged);
        FAIL_RESTORE_REGISTRY_SYNC.set(false);
        assert!(result.is_err());
        let root = db.join("doomed");
        assert!(root.join("CURRENT").exists());
        assert_eq!(
            read_registry(dir.path()).unwrap().datasets[0].id,
            Some(intent.id)
        );
        assert_eq!(
            read_intents(dir.path()).unwrap()[0].state,
            IntentState::Committed
        );
        assert!(catalog.get("doomed").is_none());
        assert!(
            catalog
                .reserve("doomed", ReservationKind::Restore, "43")
                .is_err()
        );
        drop((catalog, busy));

        let reopened = Catalog::open(dir.path(), Default::default()).unwrap();
        let restored = reopened.get("doomed").unwrap();
        assert_eq!(
            (restored.dataset_id(), restored.head_commit()),
            (intent.id, head)
        );
        assert!(!kept.exists());
    }

    #[cfg(feature = "backup")]
    #[test]
    fn r2_consult_in_place_journal_failure_reopens_the_predecessor() {
        assert_in_place_publish_failure(false);
    }

    #[cfg(feature = "backup")]
    #[test]
    fn r2_consult_in_place_registry_sync_keeps_both_copies() {
        assert_in_place_publish_failure(true);
    }

    #[cfg(feature = "backup")]
    fn assert_in_place_publish_failure(after_registry_replace: bool) {
        use super::super::FAIL_RESTORE_REGISTRY_SYNC;
        use crate::catalog::swap;
        use crate::store::Store;

        let dir = tempfile::tempdir().unwrap();
        let intent = interrupted_delete(dir.path(), CRASH_AFTER_SAVE);
        let db = dir.path().join("databases");
        let kept = db.join(intent.kept_name());
        let root = db.join("doomed");
        let replaced = db.join(".replaced-doomed-42");
        let busy = Store::open_existing(&kept, Default::default(), None).unwrap();
        let catalog = Catalog::open(dir.path(), Default::default()).unwrap();
        let predecessor = catalog.create("doomed", &Default::default()).unwrap();
        predecessor
            .update("INSERT DATA { <urn:predecessor> <urn:p> 1 }")
            .unwrap();
        let previous_id = predecessor.dataset_id();
        let previous_head = predecessor.head_commit();
        drop(predecessor);
        let staged = db.join(".restore-doomed-42");
        copy_store(&kept, &staged);
        let copy = Store::open_existing(&staged, Default::default(), None).unwrap();
        crate::sparql::update::update(
            &copy,
            "INSERT DATA { <urn:new> <urn:p> 2 }",
            &Default::default(),
        )
        .unwrap();
        let head = copy.head_commit();
        drop(copy);

        FAIL_JOURNAL_SYNC.set(!after_registry_replace);
        FAIL_RESTORE_REGISTRY_SYNC.set(after_registry_replace);
        let result = swap::replace_in_place(&catalog, "doomed", &staged, "42", false);
        assert!(!FAIL_JOURNAL_SYNC.replace(false));
        assert!(!FAIL_RESTORE_REGISTRY_SYNC.replace(false));
        assert!(result.is_err());
        assert!(root.join("CURRENT").exists() && kept.exists());
        let expected = if after_registry_replace {
            assert!(replaced.join("CURRENT").exists());
            assert!(catalog.get("doomed").is_none());
            assert!(catalog.rename("doomed", "elsewhere").is_err());
            (intent.id, head)
        } else {
            assert!(!replaced.exists());
            let active = catalog.get("doomed").unwrap();
            assert_eq!(
                (active.dataset_id(), active.head_commit()),
                (previous_id, previous_head)
            );
            (previous_id, previous_head)
        };
        assert_eq!(
            read_registry(dir.path()).unwrap().datasets[0].id,
            Some(expected.0)
        );
        drop((catalog, busy));

        let reopened = Catalog::open(dir.path(), Default::default()).unwrap();
        let active = reopened.get("doomed").unwrap();
        assert_eq!((active.dataset_id(), active.head_commit()), expected);
        assert!(!replaced.exists() && !kept.exists());
    }

    #[cfg(feature = "backup")]
    #[test]
    fn r3_indeterminate_in_place_restore_reconciles_both_registry_images() {
        use super::super::{CONFIG_FILE, FAIL_RESTORE_REGISTRY_SYNC};
        use crate::catalog::swap;
        use crate::store::Store;

        for replacement_survives in [false, true] {
            let dir = tempfile::tempdir().unwrap();
            let intent = interrupted_delete(dir.path(), CRASH_AFTER_SAVE);
            let db = dir.path().join("databases");
            let kept = db.join(intent.kept_name());
            let replaced = db.join(".replaced-doomed-42");
            let busy = Store::open_existing(&kept, Default::default(), None).unwrap();
            let catalog = Catalog::open(dir.path(), Default::default()).unwrap();
            let predecessor = catalog.create("doomed", &Default::default()).unwrap();
            predecessor
                .update("INSERT DATA { <urn:predecessor> <urn:p> 1 }")
                .unwrap();
            let previous_id = predecessor.dataset_id();
            let previous_head = predecessor.head_commit();
            drop(predecessor);

            let config = dir.path().join(CONFIG_FILE);
            let old_config = std::fs::read(&config).unwrap();

            let staged = db.join(".restore-doomed-42");
            copy_store(&kept, &staged);
            let copy = Store::open_existing(&staged, Default::default(), None).unwrap();
            crate::sparql::update::update(
                &copy,
                "INSERT DATA { <urn:new> <urn:p> 2 }",
                &Default::default(),
            )
            .unwrap();
            let new_head = copy.head_commit();
            drop(copy);

            FAIL_RESTORE_REGISTRY_SYNC.set(true);
            let result = swap::replace_in_place(&catalog, "doomed", &staged, "42", false);
            assert!(!FAIL_RESTORE_REGISTRY_SYNC.replace(false));
            assert!(result.is_err());
            assert!(db.join("doomed/CURRENT").exists() && replaced.join("CURRENT").exists());
            assert_eq!(
                read_intents(dir.path()).unwrap()[0].state,
                IntentState::Committed
            );
            drop(catalog);

            if !replacement_survives {
                std::fs::write(&config, &old_config).unwrap();
            }

            let expected = if replacement_survives {
                (intent.id, new_head)
            } else {
                (previous_id, previous_head)
            };

            // First reopen while the older committed delete copy is still held busy.
            let reopened = Catalog::open(dir.path(), Default::default()).unwrap();
            let active = reopened.get("doomed").unwrap();
            assert_eq!(
                (active.dataset_id(), active.head_commit()),
                expected,
                "replacement_survives={replacement_survives}"
            );
            assert_eq!(
                read_registry(dir.path()).unwrap().datasets[0].id,
                Some(expected.0)
            );
            assert!(!replaced.exists() && kept.exists());
            drop(active);
            reopened.save().unwrap();
            assert_eq!(
                read_registry(dir.path()).unwrap().datasets[0].id,
                Some(expected.0)
            );
            drop((reopened, busy));

            // Second reopen once the older copy is free reclaims only the older copy.
            let final_open = Catalog::open(dir.path(), Default::default()).unwrap();
            let active = final_open.get("doomed").unwrap();
            assert_eq!((active.dataset_id(), active.head_commit()), expected);
            assert_eq!(
                read_registry(dir.path()).unwrap().datasets[0].id,
                Some(expected.0)
            );
            assert!(!replaced.exists() && !kept.exists());
            assert!(!dir.path().join(INTENT_FILE).exists());
        }
    }

    #[cfg(feature = "backup")]
    #[test]
    fn r3_indeterminate_same_uuid_in_place_restore_reconciles_by_revision() {
        use super::super::{CONFIG_FILE, FAIL_RESTORE_REGISTRY_SYNC};
        use crate::catalog::swap;
        use crate::store::Store;

        for replacement_survives in [false, true] {
            let dir = tempfile::tempdir().unwrap();
            let catalog = Catalog::open(dir.path(), Default::default()).unwrap();
            let predecessor = catalog.create("ds", &Default::default()).unwrap();
            predecessor
                .update("INSERT DATA { <urn:old> <urn:p> 1 }")
                .unwrap();
            let id = predecessor.dataset_id();
            let old_head = predecessor.head_commit();
            drop(predecessor);

            let db = dir.path().join("databases");
            let staged = db.join(".restore-ds-42");
            copy_store(&db.join("ds"), &staged);
            let copy = Store::open_existing(&staged, Default::default(), None).unwrap();
            crate::sparql::update::update(
                &copy,
                "INSERT DATA { <urn:new> <urn:p> 2 }",
                &Default::default(),
            )
            .unwrap();
            let new_head = copy.head_commit();
            assert_ne!(old_head, new_head);
            drop(copy);

            let config = dir.path().join(CONFIG_FILE);
            let old_config = std::fs::read(&config).unwrap();

            FAIL_RESTORE_REGISTRY_SYNC.set(true);
            let result = swap::replace_in_place(&catalog, "ds", &staged, "42", false);
            assert!(!FAIL_RESTORE_REGISTRY_SYNC.replace(false));
            assert!(result.is_err());
            drop(catalog);

            if !replacement_survives {
                std::fs::write(&config, &old_config).unwrap();
            }

            let expected_head = if replacement_survives {
                new_head
            } else {
                old_head
            };
            let reopened = Catalog::open(dir.path(), Default::default()).unwrap();
            let active = reopened.get("ds").unwrap();
            assert_eq!(
                (active.dataset_id(), active.head_commit()),
                (id, expected_head),
                "replacement_survives={replacement_survives}"
            );
            assert!(!db.join(".replaced-ds-42").exists());
        }
    }

    #[cfg(feature = "backup")]
    #[test]
    fn r3_pre_replacement_error_with_failed_rollback_rename_recovers_predecessor() {
        use crate::catalog::swap;
        use crate::store::Store;

        for corrupt_restored in [false, true] {
            let dir = tempfile::tempdir().unwrap();
            let intent = interrupted_delete(dir.path(), CRASH_AFTER_SAVE);
            let db = dir.path().join("databases");
            let kept = db.join(intent.kept_name());
            let replaced = db.join(".replaced-doomed-42");
            let busy = Store::open_existing(&kept, Default::default(), None).unwrap();
            let catalog = Catalog::open(dir.path(), Default::default()).unwrap();
            let predecessor = catalog.create("doomed", &Default::default()).unwrap();
            predecessor
                .update("INSERT DATA { <urn:predecessor> <urn:p> 1 }")
                .unwrap();
            let previous_id = predecessor.dataset_id();
            let previous_head = predecessor.head_commit();
            drop(predecessor);

            let staged = db.join(".restore-doomed-42");
            if corrupt_restored {
                std::fs::create_dir_all(&staged).unwrap();
                std::fs::write(staged.join("CURRENT"), b"gen-0042\n").unwrap();
            } else {
                copy_store(&kept, &staged);
                let copy = Store::open_existing(&staged, Default::default(), None).unwrap();
                crate::sparql::update::update(
                    &copy,
                    "INSERT DATA { <urn:new> <urn:p> 2 }",
                    &Default::default(),
                )
                .unwrap();
                drop(copy);
                FAIL_JOURNAL_SYNC.set(true);
            }

            swap::FAIL_ROLLBACK_RENAME.set(true);
            let result = swap::replace_in_place(&catalog, "doomed", &staged, "42", false);
            assert!(!swap::FAIL_ROLLBACK_RENAME.replace(false));
            assert!(!FAIL_JOURNAL_SYNC.replace(false));
            let error = result.err().unwrap().to_string();
            assert!(error.contains("the next start repairs it"), "{error}");
            assert!(db.join("doomed").exists() && replaced.join("CURRENT").exists());
            assert_eq!(
                read_registry(dir.path()).unwrap().datasets[0].id,
                Some(previous_id)
            );
            drop((catalog, busy));

            let reopened = Catalog::open(dir.path(), Default::default()).unwrap();
            let active = reopened.get("doomed").unwrap();
            assert_eq!(
                (active.dataset_id(), active.head_commit()),
                (previous_id, previous_head),
                "corrupt_restored={corrupt_restored}"
            );
            assert_eq!(
                read_registry(dir.path()).unwrap().datasets[0].id,
                Some(previous_id)
            );
            assert!(!replaced.exists() && !kept.exists());
            assert!(!dir.path().join(INTENT_FILE).exists());
        }
    }

    #[test]
    fn r3_format3_sole_trash_copy_allows_same_uuid_replacement() {
        use crate::store::Store;

        let dir = tempfile::tempdir().unwrap();
        let intent = interrupted_delete(dir.path(), CRASH_AFTER_SAVE);
        let db = dir.path().join("databases");
        let kept = db.join(intent.kept_name());
        let trash = db.join(intent.trash_name());
        let root = db.join("doomed");
        std::fs::rename(&kept, &trash).unwrap();

        // Build a same-UUID replacement with a distinguishable head.
        copy_store(&trash, &root);
        let replacement = Store::open_existing(&root, Default::default(), Some(intent.id)).unwrap();
        let old_head = replacement.head_commit();
        crate::sparql::update::update(
            &replacement,
            "INSERT DATA { <urn:replacement> <urn:p> 9 }",
            &Default::default(),
        )
        .unwrap();
        let new_head = replacement.head_commit();
        assert_ne!(new_head, old_head);
        drop(replacement);

        // Register the replacement in config.json and write a format 3 prepared delete journal.
        let mut registry = read_registry(dir.path()).unwrap();
        registry.datasets.push(RegistryEntry {
            name: "doomed".into(),
            kind: DatasetKind::Persistent,
            id: Some(intent.id),
            revision: None,
            reasoning: None,
        });
        write_registry(dir.path(), &registry).unwrap();
        let journal = dir.path().join(INTENT_FILE);
        std::fs::write(
            &journal,
            serde_json::to_vec(&serde_json::json!({
                "format": 3,
                "intents": [{
                    "name": intent.name,
                    "id": intent.id,
                    "token": intent.token,
                    "state": "prepared",
                }],
            }))
            .unwrap(),
        )
        .unwrap();

        // Even while the old trash copy is held busy, the healthy replacement opens.
        let busy = Store::open_existing(&trash, Default::default(), Some(intent.id)).unwrap();
        let capture = EventCapture::default();
        let catalog = tracing::subscriber::with_default(capture.clone(), || {
            Catalog::open(dir.path(), Default::default()).unwrap()
        });
        assert!(
            catalog.refusal("doomed").is_none(),
            "{:?}",
            catalog.refusals()
        );
        let active = catalog.get("doomed").unwrap();
        assert_eq!(
            (active.dataset_id(), active.head_commit()),
            (intent.id, new_head)
        );
        assert!(trash.exists() && !kept.exists());
        assert!(!capture.warnings().is_empty());
        // Verify that the format 3 trash promotion was durably persisted as format 4 Committed.
        let persisted: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&journal).unwrap()).unwrap();
        assert_eq!(persisted["format"], 4, "{persisted}");
        let intents_arr = persisted["intents"].as_array().unwrap();
        assert_eq!(intents_arr.len(), 1, "{persisted}");
        assert_eq!(intents_arr[0]["token"], intent.token.to_string());
        assert_eq!(intents_arr[0]["state"], "committed");
        drop((active, catalog, busy));

        // Once the old trash copy is free, startup opens the replacement and reclaims only trash.
        let reopened = Catalog::open(dir.path(), Default::default()).unwrap();
        assert!(
            reopened.refusal("doomed").is_none(),
            "{:?}",
            reopened.refusals()
        );
        let active = reopened.get("doomed").unwrap();
        assert_eq!(
            (active.dataset_id(), active.head_commit()),
            (intent.id, new_head)
        );
        assert!(!trash.exists() && !kept.exists());
        assert!(!journal.exists());
    }

    #[test]
    fn r3_format3_rejects_format4_only_states() {
        let dir = tempfile::tempdir().unwrap();
        let intent = interrupted_delete(dir.path(), CRASH_AFTER_SAVE);
        let kept = dir.path().join("databases").join(intent.kept_name());
        let journal = dir.path().join(INTENT_FILE);

        for state in ["committed", "prepared-v3"] {
            let bytes = format!(
                r#"{{"format":3,"intents":[{{"name":"doomed","id":"{}","token":"{}","state":"{state}"}}]}}"#,
                intent.id, intent.token
            );
            std::fs::write(&journal, &bytes).unwrap();
            let err = Catalog::open(dir.path(), Default::default())
                .err()
                .expect("format 3 must reject format-4-only states");
            assert!(
                matches!(err, Error::Corrupt(ref msg) if msg.contains("requires format 4")),
                "state={state}: {err:?}"
            );
            assert!(kept.exists(), "state={state}");
            assert_eq!(std::fs::read_to_string(&journal).unwrap(), bytes);
        }
    }

    #[cfg(feature = "backup")]
    #[test]
    fn r4_live_rollback_syncs_databases_before_removing_swap_marker() {
        use super::super::{FAIL_NEXT_DIR_SYNC, FAIL_REGISTRY_REPLACE, take_catalog_dir_syncs};
        use crate::catalog::{restore, swap};
        use crate::store::Store;

        // Subtest A: normal BeforeReplace rollback syncs `databases` before removing `.restore-swap.json`.
        {
            let dir = tempfile::tempdir().unwrap();
            let catalog = Catalog::open(dir.path(), Default::default()).unwrap();
            let predecessor = catalog.create("ds", &Default::default()).unwrap();
            predecessor
                .update("INSERT DATA { <urn:old> <urn:p> 1 }")
                .unwrap();
            let id = predecessor.dataset_id();
            let old_head = predecessor.head_commit();
            drop(predecessor);

            let db = dir.path().join("databases");
            let staged = db.join(".restore-ds-42");
            copy_store(&db.join("ds"), &staged);
            let copy = Store::open_existing(&staged, Default::default(), Some(id)).unwrap();
            crate::sparql::update::update(
                &copy,
                "INSERT DATA { <urn:new> <urn:p> 2 }",
                &Default::default(),
            )
            .unwrap();
            assert_ne!(copy.head_commit(), old_head);
            drop(copy);

            let syncs_before_marker = std::rc::Rc::new(std::cell::RefCell::new(None));
            let recorded = syncs_before_marker.clone();
            swap::ROLLBACK_HOOK.set(Some(Box::new(move || {
                let _ = take_catalog_dir_syncs();
            })));
            restore::BEFORE_SWAP_MARKER_REMOVE.set(Some(Box::new(move |_replaced| {
                *recorded.borrow_mut() = Some(take_catalog_dir_syncs());
            })));
            FAIL_REGISTRY_REPLACE.set(1);
            let err = swap::replace_in_place(&catalog, "ds", &staged, "42", false)
                .err()
                .expect("injected BeforeReplace registry error must roll back");
            FAIL_REGISTRY_REPLACE.set(0);
            swap::ROLLBACK_HOOK.set(None);
            restore::BEFORE_SWAP_MARKER_REMOVE.set(None);
            assert!(
                err.to_string()
                    .contains("injected registry replace failure"),
                "{err}"
            );

            let syncs = syncs_before_marker
                .borrow_mut()
                .take()
                .expect("remove_swap_marker must run on live rollback");
            assert!(
                syncs.contains(&db),
                "databases dir must be synced after moving live aside and before removing swap marker; saw {syncs:?}"
            );
            let active = catalog.get("ds").unwrap();
            assert_eq!((active.dataset_id(), active.head_commit()), (id, old_head));
        }

        // Subtest B: failing the pre-marker databases sync preserves `.restore-swap.json`,
        // so power loss reverting the unsynced rename recovers predecessor B (not same-UUID C).
        {
            let dir = tempfile::tempdir().unwrap();
            let catalog = Catalog::open(dir.path(), Default::default()).unwrap();
            let predecessor = catalog.create("ds", &Default::default()).unwrap();
            predecessor
                .update("INSERT DATA { <urn:old> <urn:p> 1 }")
                .unwrap();
            let id = predecessor.dataset_id();
            let old_head = predecessor.head_commit();
            drop(predecessor);

            let db = dir.path().join("databases");
            let staged = db.join(".restore-ds-42");
            let replaced = db.join(".replaced-ds-42");
            copy_store(&db.join("ds"), &staged);
            let copy = Store::open_existing(&staged, Default::default(), Some(id)).unwrap();
            crate::sparql::update::update(
                &copy,
                "INSERT DATA { <urn:new> <urn:p> 2 }",
                &Default::default(),
            )
            .unwrap();
            assert_ne!(copy.head_commit(), old_head);
            drop(copy);

            let db_for_hook = db.clone();
            swap::ROLLBACK_HOOK.set(Some(Box::new(move || {
                FAIL_NEXT_DIR_SYNC.set(Some(db_for_hook.clone()));
            })));
            FAIL_REGISTRY_REPLACE.set(1);
            let err = swap::replace_in_place(&catalog, "ds", &staged, "42", false)
                .err()
                .expect("rollback pre-marker sync failure must fail swap");
            FAIL_REGISTRY_REPLACE.set(0);
            FAIL_NEXT_DIR_SYNC.set(None);
            swap::ROLLBACK_HOOK.set(None);
            assert!(
                err.to_string().contains("the next start repairs it"),
                "{err}"
            );
            assert!(
                replaced.join(".restore-swap.json").exists(),
                "swap marker must survive when pre-marker databases sync fails"
            );
            assert_eq!(catalog.refusal("ds").map(|r| r.kind), Some("recovery"));
            drop(catalog);

            // Simulate power loss losing the unsynced rename(&root, &failed): C reappears at live `ds`.
            assert!(staged.exists() && !db.join("ds").exists());
            std::fs::rename(&staged, db.join("ds")).unwrap();

            let reopened = Catalog::open(dir.path(), Default::default()).unwrap();
            let active = reopened.get("ds").unwrap();
            assert_eq!((active.dataset_id(), active.head_commit()), (id, old_head));
            assert!(!replaced.exists());
        }
    }

    #[cfg(feature = "backup")]
    #[test]
    fn r4_startup_rollback_syncs_databases_before_marker_unlink_when_live_absent() {
        use super::super::take_catalog_dir_syncs;
        use crate::catalog::swap;
        use crate::store::Store;

        let dir = tempfile::tempdir().unwrap();
        let catalog = Catalog::open(dir.path(), Default::default()).unwrap();
        let predecessor = catalog.create("ds", &Default::default()).unwrap();
        predecessor
            .update("INSERT DATA { <urn:old> <urn:p> 1 }")
            .unwrap();
        let id = predecessor.dataset_id();
        let old_head = predecessor.head_commit();
        drop(predecessor);

        let db = dir.path().join("databases");
        let staged = db.join(".restore-ds-42");
        let replaced = db.join(".replaced-ds-42");
        copy_store(&db.join("ds"), &staged);
        let copy = Store::open_existing(&staged, Default::default(), Some(id)).unwrap();
        crate::sparql::update::update(
            &copy,
            "INSERT DATA { <urn:new> <urn:p> 2 }",
            &Default::default(),
        )
        .unwrap();
        drop(copy);

        swap::CRASH_AFTER.set(2);
        assert!(swap::replace_in_place(&catalog, "ds", &staged, "42", false).is_err());
        swap::CRASH_AFTER.set(0);
        drop(catalog);

        // Simulate a failed live rollback + caller cleanup that left `live` and `.restore-*`
        // visibly absent without syncing `databases`.
        std::fs::remove_dir_all(db.join("ds")).unwrap();
        assert!(!staged.exists() && replaced.join(".restore-swap.json").exists());

        let _ = take_catalog_dir_syncs();
        let reopened = Catalog::open(dir.path(), Default::default()).unwrap();
        let syncs = take_catalog_dir_syncs();
        let db_pos = syncs
            .iter()
            .position(|p| p == &db)
            .expect("databases must be synced");
        let replaced_pos = syncs
            .iter()
            .position(|p| p == &replaced)
            .expect("replaced must be synced after unlinking swap marker");
        assert!(
            db_pos < replaced_pos,
            "databases must be synced before unlinking swap marker even when live is absent: {syncs:?}"
        );

        let active = reopened.get("ds").unwrap();
        assert_eq!((active.dataset_id(), active.head_commit()), (id, old_head));
        assert!(!replaced.exists());
    }

    #[test]
    fn r4_unmarked_format2_two_rename_reconciles_by_registered_uuid() {
        use super::super::CONFIG_FILE;
        use crate::store::Store;

        // Case 1: Pre-commit unmarked format 2 two-rename crash with distinct UUIDs:
        // config.json names B, live holds C, unmarked `.replaced-ds-5` holds B -> must restore B.
        {
            let dir = tempfile::tempdir().unwrap();
            let catalog = Catalog::open(dir.path(), Default::default()).unwrap();
            let predecessor = catalog.create("ds", &Default::default()).unwrap();
            predecessor
                .update("INSERT DATA { <urn:old> <urn:p> 1 }")
                .unwrap();
            let id_b = predecessor.dataset_id();
            let head_b = predecessor.head_commit();
            drop(predecessor);
            drop(catalog);

            let db = dir.path().join("databases");
            let replaced = db.join(".replaced-ds-5");
            std::fs::rename(db.join("ds"), &replaced).unwrap();
            let restored = Store::open(&db.join("ds"), Default::default()).unwrap();
            crate::sparql::update::update(
                &restored,
                "INSERT DATA { <urn:new> <urn:p> 2 }",
                &Default::default(),
            )
            .unwrap();
            let id_c = restored.dataset_id();
            assert_ne!(id_b, id_c);
            drop(restored);

            // Base format 2 registry row with persistent UUID B and no revision.
            std::fs::write(
                dir.path().join(CONFIG_FILE),
                serde_json::to_vec(&serde_json::json!({
                    "format": 2,
                    "datasets": [{
                        "name": "ds",
                        "type": "persistent",
                        "id": id_b,
                    }]
                }))
                .unwrap(),
            )
            .unwrap();

            let reopened = Catalog::open(dir.path(), Default::default()).unwrap();
            assert!(
                reopened.refusal("ds").is_none(),
                "{:?}",
                reopened.refusals()
            );
            let active = reopened.get("ds").unwrap();
            assert_eq!((active.dataset_id(), active.head_commit()), (id_b, head_b));
            assert!(!replaced.exists());
        }

        // Case 2: Post-commit unmarked format 2 crash (config.json names C, live holds C,
        // unmarked `.replaced-ds-5` holds B) -> must keep C and remove B.
        {
            let dir = tempfile::tempdir().unwrap();
            let catalog = Catalog::open(dir.path(), Default::default()).unwrap();
            let predecessor = catalog.create("ds", &Default::default()).unwrap();
            predecessor
                .update("INSERT DATA { <urn:old> <urn:p> 1 }")
                .unwrap();
            let id_b = predecessor.dataset_id();
            drop(predecessor);
            drop(catalog);

            let db = dir.path().join("databases");
            let replaced = db.join(".replaced-ds-5");
            std::fs::rename(db.join("ds"), &replaced).unwrap();
            let restored = Store::open(&db.join("ds"), Default::default()).unwrap();
            crate::sparql::update::update(
                &restored,
                "INSERT DATA { <urn:new> <urn:p> 2 }",
                &Default::default(),
            )
            .unwrap();
            let id_c = restored.dataset_id();
            let head_c = restored.head_commit();
            assert_ne!(id_b, id_c);
            drop(restored);

            std::fs::write(
                dir.path().join(CONFIG_FILE),
                serde_json::to_vec(&serde_json::json!({
                    "format": 2,
                    "datasets": [{
                        "name": "ds",
                        "type": "persistent",
                        "id": id_c,
                    }]
                }))
                .unwrap(),
            )
            .unwrap();

            let reopened = Catalog::open(dir.path(), Default::default()).unwrap();
            let active = reopened.get("ds").unwrap();
            assert_eq!((active.dataset_id(), active.head_commit()), (id_c, head_c));
            assert!(!replaced.exists());
        }

        // Case 3: Ambiguous/mismatched identities (config.json names D, live holds C,
        // unmarked `.replaced-ds-5` holds B) -> must fail closed and preserve both copies.
        {
            let dir = tempfile::tempdir().unwrap();
            let catalog = Catalog::open(dir.path(), Default::default()).unwrap();
            let predecessor = catalog.create("ds", &Default::default()).unwrap();
            predecessor
                .update("INSERT DATA { <urn:old> <urn:p> 1 }")
                .unwrap();
            drop(predecessor);
            drop(catalog);

            let db = dir.path().join("databases");
            let replaced = db.join(".replaced-ds-5");
            std::fs::rename(db.join("ds"), &replaced).unwrap();
            drop(Store::open(&db.join("ds"), Default::default()).unwrap());

            std::fs::write(
                dir.path().join(CONFIG_FILE),
                serde_json::to_vec(&serde_json::json!({
                    "format": 2,
                    "datasets": [{
                        "name": "ds",
                        "type": "persistent",
                        "id": Uuid::new_v4(),
                    }]
                }))
                .unwrap(),
            )
            .unwrap();

            let err = Catalog::open(dir.path(), Default::default())
                .err()
                .expect("mismatched unmarked format 2 pair must fail closed");
            assert!(matches!(err, Error::Corrupt(_)), "{err:?}");
            assert!(db.join("ds/CURRENT").exists() && replaced.join("CURRENT").exists());
        }
    }

    #[test]
    fn r4_marked_same_uuid_swap_recovers_without_backup_feature() {
        use super::super::CONFIG_FILE;
        use crate::store::Store;

        let dir = tempfile::tempdir().unwrap();
        let catalog = Catalog::open(dir.path(), Default::default()).unwrap();
        let predecessor = catalog.create("ds", &Default::default()).unwrap();
        predecessor
            .update("INSERT DATA { <urn:old> <urn:p> 1 }")
            .unwrap();
        let id = predecessor.dataset_id();
        let old_head = predecessor.head_commit();
        drop(predecessor);
        drop(catalog);

        // Verify new catalogs write format 3 config.json to fence format-2-only readers.
        let config_path = dir.path().join(CONFIG_FILE);
        let config_json: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&config_path).unwrap()).unwrap();
        assert_eq!(config_json["format"], 3, "{config_json}");

        let pred_rev = Uuid::parse_str(
            config_json["datasets"][0]["revision"]
                .as_str()
                .expect("created persistent dataset must record a swap revision"),
        )
        .unwrap();
        let rest_rev = Uuid::new_v4();

        // Construct a marked two-rename same-UUID crash image (pre-commit):
        // live `ds` holds C (same UUID, newer head), `.replaced-ds-42` holds B + `.restore-swap.json`.
        let db = dir.path().join("databases");
        let replaced = db.join(".replaced-ds-42");
        copy_store(&db.join("ds"), &replaced);
        let live = Store::open_existing(&db.join("ds"), Default::default(), Some(id)).unwrap();
        crate::sparql::update::update(
            &live,
            "INSERT DATA { <urn:new> <urn:p> 2 }",
            &Default::default(),
        )
        .unwrap();
        assert_ne!(live.head_commit(), old_head);
        drop(live);

        std::fs::write(
            replaced.join(".restore-swap.json"),
            serde_json::to_vec(&serde_json::json!({
                "format": 1,
                "dataset": "ds",
                "predecessor_id": id,
                "predecessor_revision": pred_rev,
                "restored_id": id,
                "restored_revision": rest_rev,
            }))
            .unwrap(),
        )
        .unwrap();

        // Format 2 config.json carrying `revision` must be rejected as corrupt,
        // and format 4 must be rejected as Unsupported, preserving both copies.
        let saved_config = std::fs::read(&config_path).unwrap();
        let mut bad_v2 = config_json.clone();
        bad_v2["format"] = 2.into();
        std::fs::write(&config_path, serde_json::to_vec(&bad_v2).unwrap()).unwrap();
        assert!(matches!(
            Catalog::open(dir.path(), Default::default()),
            Err(Error::Corrupt(_))
        ));
        assert!(db.join("ds/CURRENT").exists() && replaced.join("CURRENT").exists());

        let mut future_v4 = config_json;
        future_v4["format"] = 4.into();
        std::fs::write(&config_path, serde_json::to_vec(&future_v4).unwrap()).unwrap();
        assert!(matches!(
            Catalog::open(dir.path(), Default::default()),
            Err(Error::Unsupported(_))
        ));
        assert!(db.join("ds/CURRENT").exists() && replaced.join("CURRENT").exists());
        std::fs::write(&config_path, &saved_config).unwrap();

        // Opening in any build (with or without `backup`) must reconcile the marked swap to B.
        let reopened = Catalog::open(dir.path(), Default::default()).unwrap();
        let active = reopened.get("ds").unwrap();
        assert_eq!((active.dataset_id(), active.head_commit()), (id, old_head));
        assert!(!replaced.exists());
        assert!(!db.join("ds/.restore-swap.json").exists());
    }

    #[cfg(feature = "backup")]
    #[test]
    fn r4_failed_bind_swap_revision_and_reopen_save_still_persists_on_retry() {
        use super::super::{CONFIG_FILE, FAIL_REGISTRY_REPLACE};
        use crate::catalog::swap;
        use crate::store::Store;

        let dir = tempfile::tempdir().unwrap();
        let catalog = Catalog::open(dir.path(), Default::default()).unwrap();
        let predecessor = catalog.create("ds", &Default::default()).unwrap();
        predecessor
            .update("INSERT DATA { <urn:old> <urn:p> 1 }")
            .unwrap();
        let id = predecessor.dataset_id();
        let old_head = predecessor.head_commit();
        drop(predecessor);
        drop(catalog);

        // Rewrite config.json as a revision-less format 2 row.
        let config_path = dir.path().join(CONFIG_FILE);
        std::fs::write(
            &config_path,
            serde_json::to_vec(&serde_json::json!({
                "format": 2,
                "datasets": [{
                    "name": "ds",
                    "type": "persistent",
                    "id": id,
                }]
            }))
            .unwrap(),
        )
        .unwrap();

        let catalog = Catalog::open(dir.path(), Default::default()).unwrap();
        let db = dir.path().join("databases");
        let staged = db.join(".restore-ds-42");
        copy_store(&db.join("ds"), &staged);
        let copy = Store::open_existing(&staged, Default::default(), Some(id)).unwrap();
        crate::sparql::update::update(
            &copy,
            "INSERT DATA { <urn:new> <urn:p> 2 }",
            &Default::default(),
        )
        .unwrap();
        drop(copy);

        // Fail both bind_swap_revision's save AND reopen_after's st.save().
        FAIL_REGISTRY_REPLACE.set(2);
        let err = swap::replace_in_place(&catalog, "ds", &staged, "42", false)
            .err()
            .expect("first bind_swap_revision must fail");
        assert_eq!(
            FAIL_REGISTRY_REPLACE.replace(0),
            0,
            "both bind_swap_revision and reopen_after save should have attempted persistence: {err}"
        );
        // Predecessor was reopened and config.json on disk still lacks a revision.
        assert!(catalog.get("ds").is_some());
        assert!(
            read_registry(dir.path()).unwrap().datasets[0]
                .revision
                .is_none()
        );

        // Retry swap and crash after rename 2 (before commit_restored_dataset).
        swap::CRASH_AFTER.set(2);
        assert!(swap::replace_in_place(&catalog, "ds", &staged, "42", false).is_err());
        swap::CRASH_AFTER.set(0);
        drop(catalog);

        let on_disk_reg = read_registry(dir.path()).unwrap();
        let on_disk_rev = on_disk_reg.datasets[0]
            .revision
            .expect("retry must have persisted predecessor revision to config.json");
        let marker: serde_json::Value = serde_json::from_slice(
            &std::fs::read(db.join(".replaced-ds-42/.restore-swap.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(marker["predecessor_revision"], on_disk_rev.to_string());

        let reopened = Catalog::open(dir.path(), Default::default()).unwrap();
        let active = reopened.get("ds").unwrap();
        assert_eq!((active.dataset_id(), active.head_commit()), (id, old_head));
        assert!(!db.join(".replaced-ds-42").exists());
    }

    #[cfg(feature = "backup")]
    #[test]
    fn r4_bind_swap_revision_syncs_catalog_dir_when_revision_cached() {
        use super::super::take_catalog_dir_syncs;

        let dir = tempfile::tempdir().unwrap();
        let catalog = Catalog::open(dir.path(), Default::default()).unwrap();
        let ds = catalog.create("ds", &Default::default()).unwrap();
        let id = ds.dataset_id();
        drop(ds);

        let detached = catalog.detach_for_swap("ds").unwrap().unwrap();
        drop(detached);
        let _ = take_catalog_dir_syncs();
        let _rev = catalog.bind_swap_revision("ds", id).unwrap();
        let syncs = take_catalog_dir_syncs();
        let canonical = std::path::absolute(dir.path()).unwrap();
        assert!(
            syncs.iter().any(|p| p == &canonical || p == dir.path()),
            "bind_swap_revision must sync catalog dir even when revision is already cached: {syncs:?}"
        );
    }

    #[test]
    fn r4_format3_sole_trash_does_not_block_missing_live_legacy_predecessor() {
        use super::super::CONFIG_FILE;
        use crate::store::Store;

        let dir = tempfile::tempdir().unwrap();
        let intent = interrupted_delete(dir.path(), CRASH_AFTER_SAVE);
        let db = dir.path().join("databases");
        let kept = db.join(intent.kept_name());
        let trash = db.join(intent.trash_name());
        std::fs::rename(&kept, &trash).unwrap();

        // Build a verified legacy unmarked predecessor at `.replaced-doomed-9` with the same UUID
        // and a distinct head, leaving live `databases/doomed` missing.
        let replaced = db.join(".replaced-doomed-9");
        copy_store(&trash, &replaced);
        let pred = Store::open_existing(&replaced, Default::default(), Some(intent.id)).unwrap();
        crate::sparql::update::update(
            &pred,
            "INSERT DATA { <urn:pred> <urn:p> 7 }",
            &Default::default(),
        )
        .unwrap();
        let pred_head = pred.head_commit();
        drop(pred);

        std::fs::write(
            dir.path().join(CONFIG_FILE),
            serde_json::to_vec(&serde_json::json!({
                "format": 2,
                "datasets": [{
                    "name": "doomed",
                    "type": "persistent",
                    "id": intent.id,
                }]
            }))
            .unwrap(),
        )
        .unwrap();
        let journal = dir.path().join(INTENT_FILE);
        std::fs::write(
            &journal,
            serde_json::to_vec(&serde_json::json!({
                "format": 3,
                "intents": [{
                    "name": intent.name,
                    "id": intent.id,
                    "token": intent.token,
                    "state": "prepared",
                }],
            }))
            .unwrap(),
        )
        .unwrap();

        // Even while the old format 3 trash copy is held busy, `.replaced-doomed-9` is restored.
        let busy = Store::open_existing(&trash, Default::default(), Some(intent.id)).unwrap();
        let catalog = Catalog::open(dir.path(), Default::default()).unwrap();
        assert!(
            catalog.refusal("doomed").is_none(),
            "{:?}",
            catalog.refusals()
        );
        let active = catalog.get("doomed").unwrap();
        assert_eq!(
            (active.dataset_id(), active.head_commit()),
            (intent.id, pred_head)
        );
        assert!(!replaced.exists() && trash.exists());
        let persisted: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&journal).unwrap()).unwrap();
        assert_eq!(persisted["format"], 4);
        assert_eq!(persisted["intents"][0]["state"], "committed");
        drop((active, catalog, busy));

        // Once the trash copy is free, only the trash copy is reclaimed.
        let reopened = Catalog::open(dir.path(), Default::default()).unwrap();
        let active = reopened.get("doomed").unwrap();
        assert_eq!(
            (active.dataset_id(), active.head_commit()),
            (intent.id, pred_head)
        );
        assert!(!trash.exists() && !journal.exists());
    }

    #[test]
    fn r4_multiple_format3_sole_trash_tokens_do_not_block_live_replacement() {
        use super::super::CONFIG_FILE;
        use crate::store::Store;

        // Subtest A: two independently retained exact format 3 trash tokens beside a live replacement.
        {
            let dir = tempfile::tempdir().unwrap();
            let intent1 = interrupted_delete(dir.path(), CRASH_AFTER_SAVE);
            let db = dir.path().join("databases");
            let kept1 = db.join(intent1.kept_name());
            let trash1 = db.join(intent1.trash_name());
            std::fs::rename(&kept1, &trash1).unwrap();

            let token2 = Uuid::new_v4();
            let trash2 = db.join(format!("{TRASH_PREFIX}{token2}"));
            copy_store(&trash1, &trash2);

            let live_dir = db.join("doomed");
            copy_store(&trash1, &live_dir);
            let live =
                Store::open_existing(&live_dir, Default::default(), Some(intent1.id)).unwrap();
            crate::sparql::update::update(
                &live,
                "INSERT DATA { <urn:live> <urn:p> 11 }",
                &Default::default(),
            )
            .unwrap();
            let live_head = live.head_commit();
            drop(live);

            std::fs::write(
                dir.path().join(CONFIG_FILE),
                serde_json::to_vec(&serde_json::json!({
                    "format": 2,
                    "datasets": [{
                        "name": "doomed",
                        "type": "persistent",
                        "id": intent1.id,
                    }]
                }))
                .unwrap(),
            )
            .unwrap();
            let journal = dir.path().join(INTENT_FILE);
            std::fs::write(
                &journal,
                serde_json::to_vec(&serde_json::json!({
                    "format": 3,
                    "intents": [
                        {
                            "name": "doomed",
                            "id": intent1.id,
                            "token": intent1.token,
                            "state": "prepared",
                        },
                        {
                            "name": "doomed",
                            "id": intent1.id,
                            "token": token2,
                            "state": "prepared",
                        }
                    ],
                }))
                .unwrap(),
            )
            .unwrap();

            let reopened = Catalog::open(dir.path(), Default::default()).unwrap();
            assert!(
                reopened.refusal("doomed").is_none(),
                "{:?}",
                reopened.refusals()
            );
            let active = reopened.get("doomed").unwrap();
            assert_eq!(
                (active.dataset_id(), active.head_commit()),
                (intent1.id, live_head)
            );
            assert!(!trash1.exists() && !trash2.exists() && !journal.exists());
        }

        // Subtest B: conservative control — if a second token has a kept path (or both paths),
        // only the sole verified trash copy is eligible for cleanup and the dataset stays refused.
        {
            let dir = tempfile::tempdir().unwrap();
            let intent1 = interrupted_delete(dir.path(), CRASH_AFTER_SAVE);
            let db = dir.path().join("databases");
            let kept1 = db.join(intent1.kept_name());
            let trash1 = db.join(intent1.trash_name());
            std::fs::rename(&kept1, &trash1).unwrap();

            let token2 = Uuid::new_v4();
            let kept2 = db.join(format!("{KEPT_PREFIX}doomed-{token2}"));
            let trash2 = db.join(format!("{TRASH_PREFIX}{token2}"));
            copy_store(&trash1, &kept2);
            copy_store(&trash1, &trash2);

            let live_dir = db.join("doomed");
            copy_store(&trash1, &live_dir);

            std::fs::write(
                dir.path().join(CONFIG_FILE),
                serde_json::to_vec(&serde_json::json!({
                    "format": 2,
                    "datasets": [{
                        "name": "doomed",
                        "type": "persistent",
                        "id": intent1.id,
                    }]
                }))
                .unwrap(),
            )
            .unwrap();
            std::fs::write(
                dir.path().join(INTENT_FILE),
                serde_json::to_vec(&serde_json::json!({
                    "format": 3,
                    "intents": [
                        {
                            "name": "doomed",
                            "id": intent1.id,
                            "token": intent1.token,
                            "state": "prepared",
                        },
                        {
                            "name": "doomed",
                            "id": intent1.id,
                            "token": token2,
                            "state": "prepared",
                        }
                    ],
                }))
                .unwrap(),
            )
            .unwrap();

            let reopened = Catalog::open(dir.path(), Default::default()).unwrap();
            assert_eq!(reopened.refusal("doomed").map(|r| r.kind), Some("recovery"));
            assert!(
                !trash1.exists(),
                "sole verified trash copy is eligible for cleanup"
            );
            assert!(
                kept2.exists() && trash2.exists(),
                "dual-path token must remain preserved"
            );
        }
    }

    #[test]
    fn r5_unmarked_roll_forward_syncs_databases_before_removing_predecessor() {
        use super::super::{CONFIG_FILE, FAIL_NEXT_DIR_SYNC, take_catalog_dir_syncs};
        use crate::store::Store;

        for format_version in [2u32, 1u32] {
            let dir = tempfile::tempdir().unwrap();
            let catalog = Catalog::open(dir.path(), Default::default()).unwrap();
            let predecessor = catalog.create("ds", &Default::default()).unwrap();
            predecessor
                .update("INSERT DATA { <urn:old> <urn:p> 1 }")
                .unwrap();
            let id = predecessor.dataset_id();
            let old_head = predecessor.head_commit();
            drop(predecessor);
            drop(catalog);

            let db = std::path::absolute(dir.path()).unwrap().join("databases");
            let replaced = db.join(".replaced-ds-5");
            copy_store(&db.join("ds"), &replaced);
            let live = Store::open_existing(&db.join("ds"), Default::default(), Some(id)).unwrap();
            crate::sparql::update::update(
                &live,
                "INSERT DATA { <urn:new> <urn:p> 2 }",
                &Default::default(),
            )
            .unwrap();
            let new_head = live.head_commit();
            assert_ne!(old_head, new_head);
            drop(live);

            assert!(
                !std::fs::read_dir(&db)
                    .unwrap()
                    .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
                    .any(|n| n.starts_with(".restore-")),
                "fixture must have no .restore-* leftover"
            );

            let config = if format_version == 2 {
                serde_json::json!({
                    "format": 2,
                    "datasets": [{
                        "name": "ds",
                        "type": "persistent",
                        "id": id,
                    }]
                })
            } else {
                serde_json::json!({
                    "format": 1,
                    "datasets": [{
                        "name": "ds",
                        "type": "persistent",
                    }]
                })
            };
            std::fs::write(
                dir.path().join(CONFIG_FILE),
                serde_json::to_vec(&config).unwrap(),
            )
            .unwrap();

            // Injecting a failure on the pre-cleanup `databases` sync must preserve both copies.
            FAIL_NEXT_DIR_SYNC.set(Some(db.clone()));
            let err = Catalog::open(dir.path(), Default::default())
                .err()
                .expect("pre-cleanup databases sync failure must abort recovery");
            assert!(
                FAIL_NEXT_DIR_SYNC.replace(None).is_none(),
                "format {format_version}: databases sync must be attempted before cleanup: {err}"
            );
            assert!(
                err.to_string().contains("injected dir sync failure"),
                "format {format_version}: {err}"
            );
            assert!(
                db.join("ds/CURRENT").exists() && replaced.join("CURRENT").exists(),
                "format {format_version}: failed pre-cleanup sync must preserve both live and predecessor"
            );
            let live_check =
                Store::open_existing(&db.join("ds"), Default::default(), Some(id)).unwrap();
            assert_eq!(live_check.head_commit(), new_head);
            drop(live_check);
            let pred_check = Store::open_existing(&replaced, Default::default(), Some(id)).unwrap();
            assert_eq!(pred_check.head_commit(), old_head);
            drop(pred_check);

            // Clean open must sync `databases` before unlinking `.replaced-ds-5`, then keep live.
            let _ = take_catalog_dir_syncs();
            #[cfg(unix)]
            let syncs_before_unlink = {
                let captured = std::rc::Rc::new(std::cell::RefCell::new(None));
                let slot = captured.clone();
                let target = replaced.clone();
                super::super::delete_fs::DURING_REMOVAL.set(Some(Box::new(move |path| {
                    if path == target {
                        *slot.borrow_mut() = Some(take_catalog_dir_syncs());
                    }
                })));
                captured
            };
            let reopened = Catalog::open(dir.path(), Default::default()).unwrap();
            #[cfg(unix)]
            {
                super::super::delete_fs::DURING_REMOVAL.set(None);
                let syncs = syncs_before_unlink
                    .borrow_mut()
                    .take()
                    .expect("predecessor removal must run through RecoveryDir::remove_child");
                assert!(
                    syncs.contains(&db),
                    "format {format_version}: databases must sync before unlinking predecessor; saw {syncs:?}"
                );
            }
            assert!(
                reopened.refusal("ds").is_none(),
                "{:?}",
                reopened.refusals()
            );
            let active = reopened.get("ds").unwrap();
            assert_eq!(
                (active.dataset_id(), active.head_commit()),
                (id, new_head),
                "format {format_version}"
            );
            assert!(!replaced.exists());
        }
    }

    #[test]
    fn r5_empty_registry_stale_unmarked_predecessor_and_format3_trash_reuses_alias() {
        use super::super::CONFIG_FILE;
        use crate::store::Store;

        // Subtest A: empty format 2 registry + stale unmarked predecessor + committed format 3 trash.
        // Retiring the journal on open 1 must not allow open 2 to roll back the stale predecessor,
        // and the alias must be cleanly reusable.
        {
            let dir = tempfile::tempdir().unwrap();
            let intent = interrupted_delete(dir.path(), CRASH_AFTER_SAVE);
            let db = dir.path().join("databases");
            let kept = db.join(intent.kept_name());
            let trash = db.join(intent.trash_name());
            std::fs::rename(&kept, &trash).unwrap();

            let replaced = db.join(".replaced-doomed-9");
            let stale = Store::open(&replaced, Default::default()).unwrap();
            crate::sparql::update::update(
                &stale,
                "INSERT DATA { <urn:stale> <urn:p> 99 }",
                &Default::default(),
            )
            .unwrap();
            let stale_id = stale.dataset_id();
            drop(stale);

            std::fs::write(
                dir.path().join(CONFIG_FILE),
                serde_json::to_vec(&serde_json::json!({
                    "format": 2,
                    "datasets": [],
                }))
                .unwrap(),
            )
            .unwrap();
            let journal = dir.path().join(INTENT_FILE);
            std::fs::write(
                &journal,
                serde_json::to_vec(&serde_json::json!({
                    "format": 3,
                    "intents": [{
                        "name": intent.name,
                        "id": intent.id,
                        "token": intent.token,
                        "state": "prepared",
                    }],
                }))
                .unwrap(),
            )
            .unwrap();

            assert!(!db.join("doomed").exists());
            assert!(replaced.join("CURRENT").exists() && trash.join("CURRENT").exists());

            // Open 1: trash is cleaned and journal retired; stale predecessor must not populate live.
            let first = Catalog::open(dir.path(), Default::default()).unwrap();
            assert!(first.get("doomed").is_none());
            assert!(first.refusal("doomed").is_none());
            assert!(
                !db.join("doomed").exists(),
                "stale unmarked predecessor must not repopulate empty live alias"
            );
            assert!(!replaced.exists());
            assert!(!trash.exists() && !journal.exists());
            drop(first);

            // Open 2: with the journal gone, the alias remains free and reusable.
            let second = Catalog::open(dir.path(), Default::default()).unwrap();
            assert!(second.get("doomed").is_none());
            assert!(second.refusal("doomed").is_none());
            assert!(!db.join("doomed").exists());
            let fresh = second.create("doomed", &Default::default()).unwrap();
            assert_ne!(fresh.dataset_id(), stale_id);
            assert_eq!(fresh.len(), 0);
            fresh
                .update("INSERT DATA { <urn:fresh> <urn:p> 1 }")
                .unwrap();
            let fresh_id = fresh.dataset_id();
            let fresh_head = fresh.head_commit();
            drop((fresh, second));

            let third = Catalog::open(dir.path(), Default::default()).unwrap();
            let active = third.get("doomed").unwrap();
            assert_eq!(
                (active.dataset_id(), active.head_commit()),
                (fresh_id, fresh_head)
            );
        }

        // Subtest B: an existing format 1 persistent row without a UUID retains legacy rollback.
        {
            let dir = tempfile::tempdir().unwrap();
            let catalog = Catalog::open(dir.path(), Default::default()).unwrap();
            let ds = catalog.create("legacy", &Default::default()).unwrap();
            ds.update("INSERT DATA { <urn:legacy> <urn:p> 5 }").unwrap();
            let legacy_id = ds.dataset_id();
            let legacy_head = ds.head_commit();
            drop((ds, catalog));

            let db = dir.path().join("databases");
            let replaced = db.join(".replaced-legacy-4");
            std::fs::rename(db.join("legacy"), &replaced).unwrap();
            std::fs::write(
                dir.path().join(CONFIG_FILE),
                serde_json::to_vec(&serde_json::json!({
                    "format": 1,
                    "datasets": [{
                        "name": "legacy",
                        "type": "persistent",
                    }]
                }))
                .unwrap(),
            )
            .unwrap();

            let reopened = Catalog::open(dir.path(), Default::default()).unwrap();
            let active = reopened.get("legacy").unwrap();
            assert_eq!(
                (active.dataset_id(), active.head_commit()),
                (legacy_id, legacy_head)
            );
            assert!(!replaced.exists());
        }
    }

    #[test]
    fn r6_keep_replaced_marked_predecessor_is_preserved_across_alias_reuse() {
        use super::super::{CONFIG_FILE, take_catalog_dir_syncs};
        use crate::store::Store;

        // Subtest A: committed format 3 trash + empty format 2 registry + `.replaced-doomed-1`
        // carrying a validated `.keep-replaced-v1` marker with a free `.kept-doomed-1` destination.
        {
            let dir = tempfile::tempdir().unwrap();
            let intent = interrupted_delete(dir.path(), CRASH_AFTER_SAVE);
            let db = std::path::absolute(dir.path()).unwrap().join("databases");
            let trash = db.join(intent.trash_name());
            std::fs::rename(db.join(intent.kept_name()), &trash).unwrap();

            let replaced = db.join(".replaced-doomed-1");
            let kept_dst = db.join(".kept-doomed-1");
            let pred = Store::open(&replaced, Default::default()).unwrap();
            crate::sparql::update::update(
                &pred,
                "INSERT DATA { <urn:pred-a> <urn:p> 10 . <urn:pred-a> <urn:p> 11 }",
                &Default::default(),
            )
            .unwrap();
            let pred_id = pred.dataset_id();
            let pred_head = pred.head_commit();
            drop(pred);
            write_file_atomic(&replaced.join(KEEP_REPLACED_MARKER), b".kept-doomed-1").unwrap();

            std::fs::write(
                dir.path().join(CONFIG_FILE),
                serde_json::to_vec(&serde_json::json!({
                    "format": 2,
                    "datasets": [],
                }))
                .unwrap(),
            )
            .unwrap();
            let journal = dir.path().join(INTENT_FILE);
            std::fs::write(
                &journal,
                serde_json::to_vec(&serde_json::json!({
                    "format": 3,
                    "intents": [{
                        "name": intent.name,
                        "id": intent.id,
                        "token": intent.token,
                        "state": "prepared",
                    }],
                }))
                .unwrap(),
            )
            .unwrap();

            let _ = take_catalog_dir_syncs();
            let capture = EventCapture::default();
            let first = tracing::subscriber::with_default(capture.clone(), || {
                Catalog::open(dir.path(), Default::default()).unwrap()
            });
            let syncs = take_catalog_dir_syncs();
            assert!(
                syncs.contains(&replaced),
                "finishing keepReplaced move must sync predecessor dir {replaced:?}; saw {syncs:?}"
            );
            assert!(
                syncs.contains(&db),
                "finishing keepReplaced move must sync databases; saw {syncs:?}"
            );
            assert!(first.get("doomed").is_none());
            assert!(first.refusal("doomed").is_none());
            assert!(!db.join("doomed").exists());
            assert!(!trash.exists() && !journal.exists());
            assert!(!replaced.exists() && kept_dst.join("CURRENT").exists());
            assert!(
                capture.warnings().is_empty(),
                "unexpected warnings: {:?}",
                capture.warnings()
            );
            let kept_check =
                Store::open_existing(&kept_dst, Default::default(), Some(pred_id)).unwrap();
            assert_eq!(kept_check.head_commit(), pred_head);
            drop((kept_check, first));

            // Alias remains free across reopen, accepts a fresh dataset, and keeps the predecessor.
            let second = Catalog::open(dir.path(), Default::default()).unwrap();
            assert!(second.get("doomed").is_none());
            assert!(!db.join("doomed").exists());
            let fresh = second.create("doomed", &Default::default()).unwrap();
            assert_ne!(fresh.dataset_id(), pred_id);
            fresh
                .update("INSERT DATA { <urn:fresh-a> <urn:p> 1 }")
                .unwrap();
            let fresh_id = fresh.dataset_id();
            let fresh_head = fresh.head_commit();
            drop((fresh, second));

            let third = Catalog::open(dir.path(), Default::default()).unwrap();
            let active = third.get("doomed").unwrap();
            assert_eq!(
                (active.dataset_id(), active.head_commit()),
                (fresh_id, fresh_head)
            );
            let kept_check =
                Store::open_existing(&kept_dst, Default::default(), Some(pred_id)).unwrap();
            assert_eq!(kept_check.head_commit(), pred_head);
        }

        // Subtest B: same committed format 3 trash setup, but `.kept-doomed-1` is already
        // occupied by an earlier kept copy with a distinct head. Recovery must preserve both
        // kept copies, report the retained `.replaced-doomed-1`, keep the alias free across
        // reuse and reopen, and not poison a later interrupted delete of the recreated alias.
        {
            let dir = tempfile::tempdir().unwrap();
            let intent = interrupted_delete(dir.path(), CRASH_AFTER_SAVE);
            let db = std::path::absolute(dir.path()).unwrap().join("databases");
            let trash = db.join(intent.trash_name());
            std::fs::rename(db.join(intent.kept_name()), &trash).unwrap();

            let kept_dst = db.join(".kept-doomed-1");
            let earlier = Store::open(&kept_dst, Default::default()).unwrap();
            crate::sparql::update::update(
                &earlier,
                "INSERT DATA { <urn:earlier> <urn:p> 1 }",
                &Default::default(),
            )
            .unwrap();
            let earlier_id = earlier.dataset_id();
            let earlier_head = earlier.head_commit();
            drop(earlier);
            write_file_atomic(&kept_dst.join(KEEP_REPLACED_MARKER), b".kept-doomed-1").unwrap();

            let replaced = db.join(".replaced-doomed-1");
            let second_pred = Store::open(&replaced, Default::default()).unwrap();
            crate::sparql::update::update(
                &second_pred,
                "INSERT DATA { <urn:second> <urn:p> 2 . <urn:second> <urn:p> 3 }",
                &Default::default(),
            )
            .unwrap();
            let second_id = second_pred.dataset_id();
            let second_head = second_pred.head_commit();
            assert_ne!(earlier_id, second_id);
            assert_ne!(earlier_head, second_head);
            drop(second_pred);
            write_file_atomic(&replaced.join(KEEP_REPLACED_MARKER), b".kept-doomed-1").unwrap();

            std::fs::write(
                dir.path().join(CONFIG_FILE),
                serde_json::to_vec(&serde_json::json!({
                    "format": 2,
                    "datasets": [],
                }))
                .unwrap(),
            )
            .unwrap();
            let journal = dir.path().join(INTENT_FILE);
            std::fs::write(
                &journal,
                serde_json::to_vec(&serde_json::json!({
                    "format": 3,
                    "intents": [{
                        "name": intent.name,
                        "id": intent.id,
                        "token": intent.token,
                        "state": "prepared",
                    }],
                }))
                .unwrap(),
            )
            .unwrap();

            let _ = take_catalog_dir_syncs();
            let capture = EventCapture::default();
            let first = tracing::subscriber::with_default(capture.clone(), || {
                Catalog::open(dir.path(), Default::default()).unwrap()
            });
            let syncs = take_catalog_dir_syncs();
            assert!(
                syncs.contains(&replaced),
                "occupied keepReplaced must sync predecessor dir {replaced:?}; saw {syncs:?}"
            );
            assert!(first.get("doomed").is_none());
            assert!(first.refusal("doomed").is_none());
            assert!(!db.join("doomed").exists());
            assert!(!trash.exists() && !journal.exists());
            assert!(kept_dst.join("CURRENT").exists() && replaced.join("CURRENT").exists());

            let warnings = capture.warnings();
            assert!(
                warnings.iter().any(|event| {
                    event
                        .fields
                        .get("path")
                        .is_some_and(|p| p.ends_with(".replaced-doomed-1"))
                        && event
                            .fields
                            .get("retained_bytes")
                            .and_then(|v| v.parse::<u64>().ok())
                            .is_some_and(|b| b > 0)
                        && event
                            .fields
                            .get("retained_bytes_complete")
                            .map(String::as_str)
                            == Some("true")
                        && event
                            .fields
                            .get("reason")
                            .is_some_and(|r| r.contains(".kept-doomed-1"))
                }),
                "expected structured retention warning for .replaced-doomed-1, got {warnings:?}"
            );

            let earlier_check =
                Store::open_existing(&kept_dst, Default::default(), Some(earlier_id)).unwrap();
            assert_eq!(earlier_check.head_commit(), earlier_head);
            let second_check =
                Store::open_existing(&replaced, Default::default(), Some(second_id)).unwrap();
            assert_eq!(second_check.head_commit(), second_head);
            drop((earlier_check, second_check, first));

            // Recreate the alias, reopen while registered, and verify both copies remain intact.
            let second = Catalog::open(dir.path(), Default::default()).unwrap();
            assert!(second.get("doomed").is_none());
            assert!(!db.join("doomed").exists());
            let fresh = second.create("doomed", &Default::default()).unwrap();
            fresh
                .update("INSERT DATA { <urn:fresh-b> <urn:p> 9 }")
                .unwrap();
            let fresh_id = fresh.dataset_id();
            let fresh_head = fresh.head_commit();
            drop((fresh, second));

            let third = Catalog::open(dir.path(), Default::default()).unwrap();
            let active = third.get("doomed").unwrap();
            assert_eq!(
                (active.dataset_id(), active.head_commit()),
                (fresh_id, fresh_head)
            );
            drop(active);

            // A later interrupted delete of the recreated alias must not conflict with retained `.replaced-doomed-1`.
            CRASH_AFTER.set(CRASH_AFTER_KEPT_RENAME);
            let err = third.delete("doomed");
            CRASH_AFTER.set(0);
            assert!(err.is_err());
            drop(third);

            let fourth = Catalog::open(dir.path(), Default::default()).unwrap();
            assert!(
                fourth.refusal("doomed").is_none(),
                "{:?}",
                fourth.refusals()
            );
            let active = fourth.get("doomed").unwrap();
            assert_eq!(
                (active.dataset_id(), active.head_commit()),
                (fresh_id, fresh_head)
            );
            let earlier_check =
                Store::open_existing(&kept_dst, Default::default(), Some(earlier_id)).unwrap();
            assert_eq!(earlier_check.head_commit(), earlier_head);
            let second_check =
                Store::open_existing(&replaced, Default::default(), Some(second_id)).unwrap();
            assert_eq!(second_check.head_commit(), second_head);
        }

        // Subtest C: active registered format 2 dataset with a live root and a `.replaced-ds-1`
        // carrying a validated `.keep-replaced-v1` marker must preserve it at `.kept-ds-1`
        // instead of deleting it in the roll-forward arm.
        {
            let dir = tempfile::tempdir().unwrap();
            let catalog = Catalog::open(dir.path(), Default::default()).unwrap();
            let ds = catalog.create("ds", &Default::default()).unwrap();
            ds.update("INSERT DATA { <urn:live> <urn:p> 1 }").unwrap();
            let live_id = ds.dataset_id();
            let live_head = ds.head_commit();
            drop((ds, catalog));

            let db = dir.path().join("databases");
            let replaced = db.join(".replaced-ds-1");
            let kept_dst = db.join(".kept-ds-1");
            let pred = Store::open(&replaced, Default::default()).unwrap();
            crate::sparql::update::update(
                &pred,
                "INSERT DATA { <urn:pred-c> <urn:p> 2 }",
                &Default::default(),
            )
            .unwrap();
            let pred_id = pred.dataset_id();
            let pred_head = pred.head_commit();
            drop(pred);
            write_file_atomic(&replaced.join(KEEP_REPLACED_MARKER), b".kept-ds-1").unwrap();

            let reopened = Catalog::open(dir.path(), Default::default()).unwrap();
            let active = reopened.get("ds").unwrap();
            assert_eq!(
                (active.dataset_id(), active.head_commit()),
                (live_id, live_head)
            );
            assert!(!replaced.exists() && kept_dst.join("CURRENT").exists());
            let kept_check =
                Store::open_existing(&kept_dst, Default::default(), Some(pred_id)).unwrap();
            assert_eq!(kept_check.head_commit(), pred_head);
        }
    }

    #[test]
    fn r6_missing_config_fails_closed_on_any_replaced_entry() {
        use super::super::CONFIG_FILE;
        use crate::store::Store;

        let dir = tempfile::tempdir().unwrap();
        let catalog = Catalog::open(dir.path(), Default::default()).unwrap();
        let ds = catalog.create("ds", &Default::default()).unwrap();
        ds.update("INSERT DATA { <urn:orig> <urn:p> 1 }").unwrap();
        let orig_id = ds.dataset_id();
        let orig_head = ds.head_commit();
        drop((ds, catalog));

        let db = dir.path().join("databases");
        let replaced = db.join(".replaced-ds-5");
        std::fs::rename(db.join("ds"), &replaced).unwrap();
        let config = dir.path().join(CONFIG_FILE);
        std::fs::remove_file(&config).unwrap();

        // Missing config.json with an unmarked `.replaced-*` must fail closed and keep it intact.
        let err = Catalog::open(dir.path(), Default::default())
            .err()
            .expect("missing config.json with unmarked .replaced-* must fail closed");
        assert!(matches!(err, Error::Corrupt(_)), "{err:?}");
        assert!(!db.join("ds").exists());
        assert!(replaced.join("CURRENT").exists());
        let check = Store::open_existing(&replaced, Default::default(), Some(orig_id)).unwrap();
        assert_eq!(check.head_commit(), orig_head);
        drop(check);

        // A future swap marker on another `.replaced-*` entry must still report Unsupported.
        let future = db.join(".replaced-other-9");
        std::fs::create_dir_all(&future).unwrap();
        std::fs::write(
            future.join(super::super::restore::SWAP_MARKER),
            br#"{"format":99,"dataset":"other"}"#,
        )
        .unwrap();
        let err = Catalog::open(dir.path(), Default::default())
            .err()
            .expect("future swap marker must report Unsupported even with unmarked sibling");
        assert!(matches!(err, Error::Unsupported(_)), "{err:?}");
        assert!(replaced.join("CURRENT").exists() && future.exists());
        std::fs::remove_dir_all(&future).unwrap();

        // Restoring an explicit empty config.json allows retiring the obsolete unmarked copy.
        std::fs::write(
            &config,
            serde_json::to_vec(&serde_json::json!({
                "format": 2,
                "datasets": [],
            }))
            .unwrap(),
        )
        .unwrap();
        let reopened = Catalog::open(dir.path(), Default::default()).unwrap();
        assert!(reopened.get("ds").is_none());
        assert!(!db.join("ds").exists() && !replaced.exists());
    }

    #[test]
    fn r7_occupied_keep_replaced_syncs_predecessor_dir_and_fails_closed_on_sync_error() {
        use super::super::{CONFIG_FILE, FAIL_NEXT_DIR_SYNC, take_catalog_dir_syncs};
        use crate::store::Store;

        // Subtest A: unmarked `.keep-replaced-v1` on an occupied destination must sync the
        // predecessor directory before reporting retention, and fail closed if that sync fails.
        {
            let dir = tempfile::tempdir().unwrap();
            let intent = interrupted_delete(dir.path(), CRASH_AFTER_SAVE);
            let db = std::path::absolute(dir.path()).unwrap().join("databases");
            let trash = db.join(intent.trash_name());
            std::fs::rename(db.join(intent.kept_name()), &trash).unwrap();

            let kept_dst = db.join(".kept-doomed-1");
            let earlier = Store::open(&kept_dst, Default::default()).unwrap();
            crate::sparql::update::update(
                &earlier,
                "INSERT DATA { <urn:r7-earlier> <urn:p> 1 }",
                &Default::default(),
            )
            .unwrap();
            let earlier_id = earlier.dataset_id();
            let earlier_head = earlier.head_commit();
            drop(earlier);
            write_file_atomic(&kept_dst.join(KEEP_REPLACED_MARKER), b".kept-doomed-1").unwrap();

            let replaced = db.join(".replaced-doomed-1");
            let second_pred = Store::open(&replaced, Default::default()).unwrap();
            crate::sparql::update::update(
                &second_pred,
                "INSERT DATA { <urn:r7-second> <urn:p> 2 . <urn:r7-second> <urn:p> 3 }",
                &Default::default(),
            )
            .unwrap();
            let second_id = second_pred.dataset_id();
            let second_head = second_pred.head_commit();
            drop(second_pred);
            write_file_atomic(&replaced.join(KEEP_REPLACED_MARKER), b".kept-doomed-1").unwrap();

            std::fs::write(
                dir.path().join(CONFIG_FILE),
                serde_json::to_vec(&serde_json::json!({
                    "format": 2,
                    "datasets": [],
                }))
                .unwrap(),
            )
            .unwrap();
            let journal = dir.path().join(INTENT_FILE);
            std::fs::write(
                &journal,
                serde_json::to_vec(&serde_json::json!({
                    "format": 3,
                    "intents": [{
                        "name": intent.name,
                        "id": intent.id,
                        "token": intent.token,
                        "state": "prepared",
                    }],
                }))
                .unwrap(),
            )
            .unwrap();

            // Inject a sync failure on the predecessor directory itself.
            let _ = take_catalog_dir_syncs();
            FAIL_NEXT_DIR_SYNC.set(Some(replaced.clone()));
            let capture = EventCapture::default();
            let failed = tracing::subscriber::with_default(capture.clone(), || {
                Catalog::open(dir.path(), Default::default())
            });
            let remaining = FAIL_NEXT_DIR_SYNC.replace(None);
            assert!(
                failed.is_err(),
                "occupied keepReplaced must fail closed if predecessor dir sync fails"
            );
            assert!(
                remaining.is_none(),
                "startup must attempt to sync the occupied predecessor directory {replaced:?}"
            );
            assert!(
                capture.warnings().is_empty(),
                "retention warning must not fire before predecessor sync succeeds: {:?}",
                capture.warnings()
            );
            assert!(trash.exists() && journal.exists());
            let earlier_check =
                Store::open_existing(&kept_dst, Default::default(), Some(earlier_id)).unwrap();
            assert_eq!(earlier_check.head_commit(), earlier_head);
            let second_check =
                Store::open_existing(&replaced, Default::default(), Some(second_id)).unwrap();
            assert_eq!(second_check.head_commit(), second_head);
            drop((earlier_check, second_check));

            // Retry succeeds, records the predecessor directory sync, warns, and keeps both heads.
            let _ = take_catalog_dir_syncs();
            let capture = EventCapture::default();
            let opened = tracing::subscriber::with_default(capture.clone(), || {
                Catalog::open(dir.path(), Default::default()).unwrap()
            });
            let syncs = take_catalog_dir_syncs();
            assert!(
                syncs.contains(&replaced),
                "occupied keepReplaced retry must sync predecessor dir {replaced:?}; saw {syncs:?}"
            );
            assert!(!trash.exists() && !journal.exists());
            assert!(capture.warnings().iter().any(|event| {
                event
                    .fields
                    .get("path")
                    .is_some_and(|p| p.ends_with(".replaced-doomed-1"))
            }));
            drop(opened);
            let earlier_check =
                Store::open_existing(&kept_dst, Default::default(), Some(earlier_id)).unwrap();
            assert_eq!(earlier_check.head_commit(), earlier_head);
            let second_check =
                Store::open_existing(&replaced, Default::default(), Some(second_id)).unwrap();
            assert_eq!(second_check.head_commit(), second_head);
        }

        // Subtest B: dual-marked committed predecessor on an occupied destination must sync
        // `.keep-replaced-v1` before unlinking `.restore-swap.json`, leaving both markers and
        // both heads intact if that sync fails.
        {
            let dir = tempfile::tempdir().unwrap();
            let catalog = Catalog::open(dir.path(), Default::default()).unwrap();
            let active = catalog.create("ds", &Default::default()).unwrap();
            active
                .update("INSERT DATA { <urn:r7-live> <urn:p> 1 }")
                .unwrap();
            let active_id = active.dataset_id();
            let active_head = active.head_commit();
            drop((active, catalog));

            let restored_rev = read_registry(dir.path()).unwrap().datasets[0]
                .revision
                .expect("created dataset has swap revision");
            let db = std::path::absolute(dir.path()).unwrap().join("databases");

            let kept_dst = db.join(".kept-ds-1");
            let earlier = Store::open(&kept_dst, Default::default()).unwrap();
            crate::sparql::update::update(
                &earlier,
                "INSERT DATA { <urn:r7-earlier-b> <urn:p> 1 }",
                &Default::default(),
            )
            .unwrap();
            let earlier_id = earlier.dataset_id();
            let earlier_head = earlier.head_commit();
            drop(earlier);
            write_file_atomic(&kept_dst.join(KEEP_REPLACED_MARKER), b".kept-ds-1").unwrap();

            let replaced = db.join(".replaced-ds-1");
            let pred = Store::open(&replaced, Default::default()).unwrap();
            crate::sparql::update::update(
                &pred,
                "INSERT DATA { <urn:r7-pred-b> <urn:p> 2 }",
                &Default::default(),
            )
            .unwrap();
            let pred_id = pred.dataset_id();
            let pred_head = pred.head_commit();
            drop(pred);
            write_file_atomic(&replaced.join(KEEP_REPLACED_MARKER), b".kept-ds-1").unwrap();
            let swap_marker = replaced.join(super::super::restore::SWAP_MARKER);
            write_file_atomic(
                &swap_marker,
                &serde_json::to_vec(&serde_json::json!({
                    "format": 1,
                    "dataset": "ds",
                    "predecessor_id": pred_id,
                    "predecessor_revision": Uuid::new_v4(),
                    "restored_id": active_id,
                    "restored_revision": restored_rev,
                }))
                .unwrap(),
            )
            .unwrap();

            FAIL_NEXT_DIR_SYNC.set(Some(replaced.clone()));
            let failed = Catalog::open(dir.path(), Default::default());
            assert!(FAIL_NEXT_DIR_SYNC.replace(None).is_none());
            assert!(failed.is_err());
            // Swap marker must still be present because pre-unlink sync failed.
            assert!(
                swap_marker.exists(),
                "swap marker must not be unlinked before keep marker is durably synced"
            );
            assert!(replaced.join(KEEP_REPLACED_MARKER).exists());

            let reopened = Catalog::open(dir.path(), Default::default()).unwrap();
            let ds = reopened.get("ds").unwrap();
            assert_eq!(
                (ds.dataset_id(), ds.head_commit()),
                (active_id, active_head)
            );
            assert!(!swap_marker.exists());
            assert!(replaced.join(KEEP_REPLACED_MARKER).exists());
            let earlier_check =
                Store::open_existing(&kept_dst, Default::default(), Some(earlier_id)).unwrap();
            assert_eq!(earlier_check.head_commit(), earlier_head);
            let pred_check =
                Store::open_existing(&replaced, Default::default(), Some(pred_id)).unwrap();
            assert_eq!(pred_check.head_commit(), pred_head);
        }
    }

    #[test]
    fn r7_dual_marked_committed_keep_replaced_allows_interrupted_delete_recovery() {
        use super::super::take_catalog_dir_syncs;
        use crate::store::Store;

        // Subtest A: prepared format 4 delete with restored revision still registered and an
        // occupied predecessor carrying both `.restore-swap.json` and `.keep-replaced-v1`.
        {
            let dir = tempfile::tempdir().unwrap();
            let catalog = Catalog::open(dir.path(), Default::default()).unwrap();
            let active = catalog.create("ds", &Default::default()).unwrap();
            active
                .update("INSERT DATA { <urn:r7-active> <urn:p> 1 }")
                .unwrap();
            let active_id = active.dataset_id();
            let active_head = active.head_commit();
            drop(active);

            let restored_rev = read_registry(dir.path()).unwrap().datasets[0]
                .revision
                .expect("created persistent dataset records swap revision");

            CRASH_AFTER.set(CRASH_AFTER_KEPT_RENAME);
            assert!(catalog.delete("ds").is_err());
            CRASH_AFTER.set(0);
            drop(catalog);

            let intent = read_intents(dir.path()).unwrap().pop().unwrap();
            assert_eq!(intent.state, IntentState::Prepared);
            assert_eq!(intent.id, active_id);

            let db = std::path::absolute(dir.path()).unwrap().join("databases");
            let kept_dst = db.join(".kept-ds-1");
            let earlier = Store::open(&kept_dst, Default::default()).unwrap();
            crate::sparql::update::update(
                &earlier,
                "INSERT DATA { <urn:r7-earlier> <urn:p> 10 }",
                &Default::default(),
            )
            .unwrap();
            let earlier_id = earlier.dataset_id();
            let earlier_head = earlier.head_commit();
            drop(earlier);
            write_file_atomic(&kept_dst.join(KEEP_REPLACED_MARKER), b".kept-ds-1").unwrap();

            let replaced = db.join(".replaced-ds-1");
            let pred = Store::open(&replaced, Default::default()).unwrap();
            crate::sparql::update::update(
                &pred,
                "INSERT DATA { <urn:r7-pred> <urn:p> 20 . <urn:r7-pred> <urn:p> 21 }",
                &Default::default(),
            )
            .unwrap();
            let pred_id = pred.dataset_id();
            let pred_head = pred.head_commit();
            drop(pred);
            write_file_atomic(&replaced.join(KEEP_REPLACED_MARKER), b".kept-ds-1").unwrap();
            let swap_marker = replaced.join(super::super::restore::SWAP_MARKER);
            write_file_atomic(
                &swap_marker,
                &serde_json::to_vec(&serde_json::json!({
                    "format": 1,
                    "dataset": "ds",
                    "predecessor_id": pred_id,
                    "predecessor_revision": Uuid::new_v4(),
                    "restored_id": active_id,
                    "restored_revision": restored_rev,
                }))
                .unwrap(),
            )
            .unwrap();

            let _ = take_catalog_dir_syncs();
            let capture = EventCapture::default();
            let first = tracing::subscriber::with_default(capture.clone(), || {
                Catalog::open(dir.path(), Default::default()).unwrap()
            });
            let syncs = take_catalog_dir_syncs();
            assert!(
                syncs.contains(&replaced),
                "occupied keepReplaced must sync predecessor dir {replaced:?}; saw {syncs:?}"
            );
            assert!(
                first.refusal("ds").is_none(),
                "committed dual-marked keepReplaced copy must not block delete recovery: {:?}",
                first.refusals()
            );
            let live = first.get("ds").expect("active dataset must be restored");
            assert_eq!(
                (live.dataset_id(), live.head_commit()),
                (active_id, active_head)
            );
            assert!(!dir.path().join(INTENT_FILE).exists());
            assert!(!db.join(intent.kept_name()).exists());
            assert!(!swap_marker.exists());
            assert!(replaced.join(KEEP_REPLACED_MARKER).exists());
            assert!(capture.warnings().iter().any(|event| {
                event
                    .fields
                    .get("path")
                    .is_some_and(|p| p.ends_with(".replaced-ds-1"))
            }));
            drop((live, first));

            // Reopen confirms active head and both kept heads survive.
            let second = Catalog::open(dir.path(), Default::default()).unwrap();
            assert!(second.refusal("ds").is_none());
            let live = second.get("ds").unwrap();
            assert_eq!(
                (live.dataset_id(), live.head_commit()),
                (active_id, active_head)
            );
            let earlier_check =
                Store::open_existing(&kept_dst, Default::default(), Some(earlier_id)).unwrap();
            assert_eq!(earlier_check.head_commit(), earlier_head);
            let pred_check =
                Store::open_existing(&replaced, Default::default(), Some(pred_id)).unwrap();
            assert_eq!(pred_check.head_commit(), pred_head);
        }

        // Subtest B: a dual-marked candidate matching the registered predecessor revision is a
        // possible rollback source and must still trigger a per-dataset conflict refusal.
        {
            let dir = tempfile::tempdir().unwrap();
            let catalog = Catalog::open(dir.path(), Default::default()).unwrap();
            let active = catalog.create("ds", &Default::default()).unwrap();
            active
                .update("INSERT DATA { <urn:r7-rollback> <urn:p> 1 }")
                .unwrap();
            let active_id = active.dataset_id();
            drop(active);

            let registered_rev = read_registry(dir.path()).unwrap().datasets[0]
                .revision
                .expect("created persistent dataset records swap revision");

            CRASH_AFTER.set(CRASH_AFTER_KEPT_RENAME);
            assert!(catalog.delete("ds").is_err());
            CRASH_AFTER.set(0);
            drop(catalog);

            let intent = read_intents(dir.path()).unwrap().pop().unwrap();
            let db = std::path::absolute(dir.path()).unwrap().join("databases");
            let replaced = db.join(".replaced-ds-1");
            let pred = Store::open(&replaced, Default::default()).unwrap();
            let pred_id = pred.dataset_id();
            drop(pred);
            write_file_atomic(&replaced.join(KEEP_REPLACED_MARKER), b".kept-ds-1").unwrap();
            let swap_marker = replaced.join(super::super::restore::SWAP_MARKER);
            write_file_atomic(
                &swap_marker,
                &serde_json::to_vec(&serde_json::json!({
                    "format": 1,
                    "dataset": "ds",
                    "predecessor_id": pred_id,
                    "predecessor_revision": registered_rev,
                    "restored_id": active_id,
                    "restored_revision": Uuid::new_v4(),
                }))
                .unwrap(),
            )
            .unwrap();

            let reopened = Catalog::open(dir.path(), Default::default()).unwrap();
            assert_eq!(
                reopened.refusal("ds").map(|r| r.kind),
                Some("recovery"),
                "predecessor-revision match must stay refused"
            );
            assert!(reopened.get("ds").is_none());
            assert!(!db.join("ds").exists());
            assert!(db.join(intent.kept_name()).join("dataset.json").exists());
            assert!(replaced.join("dataset.json").exists());
            assert!(swap_marker.exists());
            assert!(dir.path().join(INTENT_FILE).exists());
        }
    }

    #[test]
    fn r7_conflict_discovery_isolates_malformed_and_unreadable_candidates() {
        use crate::store::Store;

        let dir = tempfile::tempdir().unwrap();
        let catalog = Catalog::open(dir.path(), Default::default()).unwrap();
        let healthy = catalog.create("healthy", &Default::default()).unwrap();
        healthy
            .update("INSERT DATA { <urn:r7-healthy> <urn:p> 1 }")
            .unwrap();
        let healthy_id = healthy.dataset_id();
        let healthy_head = healthy.head_commit();
        drop(healthy);
        let doomed = catalog.create("doomed", &Default::default()).unwrap();
        doomed
            .update("INSERT DATA { <urn:r7-doomed> <urn:p> 2 }")
            .unwrap();
        drop(doomed);

        CRASH_AFTER.set(CRASH_AFTER_KEPT_RENAME);
        assert!(catalog.delete("doomed").is_err());
        CRASH_AFTER.set(0);
        drop(catalog);

        let intent = read_intents(dir.path()).unwrap().pop().unwrap();
        let db = dir.path().join("databases");
        let kept = db.join(intent.kept_name());
        let replaced = db.join(".replaced-doomed-42");
        drop(Store::open(&replaced, Default::default()).unwrap());
        let swap_marker = replaced.join(super::super::restore::SWAP_MARKER);

        // Truncated swap marker (without and with `.keep-replaced-v1`) must keep the conflict
        // scoped to `/doomed` while `/healthy` still opens cleanly.
        for with_keep_marker in [false, true] {
            if with_keep_marker {
                write_file_atomic(&replaced.join(KEEP_REPLACED_MARKER), b".kept-doomed-42")
                    .unwrap();
            }
            std::fs::write(&swap_marker, b"{\"format\":1,").unwrap();
            let reopened = Catalog::open(dir.path(), Default::default()).unwrap();
            let h = reopened
                .get("healthy")
                .expect("healthy sibling must open despite competing malformed swap marker");
            assert_eq!(
                (h.dataset_id(), h.head_commit()),
                (healthy_id, healthy_head)
            );
            assert_eq!(reopened.refusal("doomed").map(|r| r.kind), Some("recovery"));
            assert!(reopened.get("doomed").is_none());
            assert!(!db.join("doomed").exists());
            assert!(kept.join("dataset.json").exists());
            assert!(replaced.join("dataset.json").exists());
            assert!(dir.path().join(INTENT_FILE).exists());
        }

        // Unreadable competing `.replaced-*` directory must also stay scoped to `/doomed`.
        #[cfg(unix)]
        if unsafe { libc::geteuid() } != 0 {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&replaced, std::fs::Permissions::from_mode(0o000)).unwrap();
            let opened = Catalog::open(dir.path(), Default::default());
            std::fs::set_permissions(&replaced, std::fs::Permissions::from_mode(0o700)).unwrap();
            let reopened = opened.expect(
                "healthy sibling must open when competing .replaced-* directory is unreadable",
            );
            let h = reopened.get("healthy").unwrap();
            assert_eq!(
                (h.dataset_id(), h.head_commit()),
                (healthy_id, healthy_head)
            );
            assert_eq!(reopened.refusal("doomed").map(|r| r.kind), Some("recovery"));
            assert!(kept.join("dataset.json").exists());
            assert!(replaced.join("dataset.json").exists());
            assert!(dir.path().join(INTENT_FILE).exists());
        }
    }

    #[test]
    fn r7_missing_config_reports_unsupported_swap_before_prepared_delete_or_malformed_sibling() {
        use super::super::CONFIG_FILE;

        let dir = tempfile::tempdir().unwrap();
        let intent = interrupted_delete(dir.path(), CRASH_AFTER_KEPT_RENAME);
        let db = dir.path().join("databases");
        let kept = db.join(intent.kept_name());
        let journal = dir.path().join(INTENT_FILE);
        std::fs::remove_file(dir.path().join(CONFIG_FILE)).unwrap();

        let future = db.join(".replaced-other-9");
        std::fs::create_dir_all(&future).unwrap();
        std::fs::write(
            future.join(super::super::restore::SWAP_MARKER),
            br#"{"format":99,"dataset":"other"}"#,
        )
        .unwrap();

        // Missing config.json + prepared delete + future swap marker on another alias -> Unsupported.
        let err = Catalog::open(dir.path(), Default::default())
            .err()
            .expect("future swap marker must report Unsupported even beside a prepared delete");
        assert!(matches!(err, Error::Unsupported(_)), "{err:?}");
        assert!(kept.join("dataset.json").exists());
        assert!(journal.exists());
        assert!(future.exists());

        // Malformed siblings on both sides of `other` must not mask the Unsupported format.
        let malformed_a = db.join(".replaced-aaa-1");
        let malformed_z = db.join(".replaced-zzz-2");
        for malformed in [&malformed_a, &malformed_z] {
            std::fs::create_dir_all(malformed).unwrap();
            std::fs::write(
                malformed.join(super::super::restore::SWAP_MARKER),
                b"{\"format\":1,",
            )
            .unwrap();
        }
        let err = Catalog::open(dir.path(), Default::default())
            .err()
            .expect("malformed siblings must not mask future swap marker");
        assert!(matches!(err, Error::Unsupported(_)), "{err:?}");

        // Without the prepared delete, malformed siblings still must not mask Unsupported.
        let saved_journal = std::fs::read(&journal).unwrap();
        std::fs::remove_file(&journal).unwrap();
        let err = Catalog::open(dir.path(), Default::default())
            .err()
            .expect("malformed siblings without prepared delete must not mask Unsupported");
        assert!(matches!(err, Error::Unsupported(_)), "{err:?}");

        // Once the future swap marker is removed, the malformed marker reports Corrupt.
        std::fs::remove_dir_all(&future).unwrap();
        let err = Catalog::open(dir.path(), Default::default())
            .err()
            .expect("malformed marker alone must fail closed with Corrupt");
        assert!(matches!(err, Error::Corrupt(_)), "{err:?}");

        // With the prepared delete restored and no future swap marker, missing registry reports Corrupt.
        std::fs::write(&journal, &saved_journal).unwrap();
        let err = Catalog::open(dir.path(), Default::default())
            .err()
            .expect("prepared delete with missing config.json must still report Corrupt");
        assert!(matches!(err, Error::Corrupt(_)), "{err:?}");
        assert!(kept.join("dataset.json").exists());
        assert!(journal.exists());
    }

    #[cfg(feature = "backup")]
    #[test]
    fn r8_live_keep_replaced_requires_durable_keep_marker_before_commit() {
        use super::super::{FAIL_NEXT_DIR_SYNC, FAIL_REGISTRY_REPLACE, take_catalog_dir_syncs};
        use crate::catalog::{restore, swap};
        use crate::store::Store;

        // Subtest A: occupied destination with injected keep-marker sync failure and blocked
        // rollback must not acknowledge success, must keep `.restore-swap.json` intact, and
        // must preserve the predecessor on a restart image where the failed marker is absent.
        {
            let dir = tempfile::tempdir().unwrap();
            let catalog = Catalog::open(dir.path(), Default::default()).unwrap();
            let pred = catalog.create("ds", &Default::default()).unwrap();
            pred.update("INSERT DATA { <urn:r8-pred-a> <urn:p> 1 }")
                .unwrap();
            let pred_id = pred.dataset_id();
            let pred_head = pred.head_commit();
            drop(pred);

            let db = std::path::absolute(dir.path()).unwrap().join("databases");
            let kept_dst = db.join(".kept-ds-7");
            let earlier = Store::open(&kept_dst, Default::default()).unwrap();
            crate::sparql::update::update(
                &earlier,
                "INSERT DATA { <urn:r8-earlier-a> <urn:p> 10 }",
                &Default::default(),
            )
            .unwrap();
            let earlier_id = earlier.dataset_id();
            let earlier_head = earlier.head_commit();
            drop(earlier);
            write_file_atomic(&kept_dst.join(KEEP_REPLACED_MARKER), b".kept-ds-7").unwrap();

            let staged = db.join(".restore-ds-7");
            let restored = Store::open(&staged, Default::default()).unwrap();
            crate::sparql::update::update(
                &restored,
                "INSERT DATA { <urn:r8-restored-a> <urn:p> 20 . <urn:r8-restored-a> <urn:p> 21 }",
                &Default::default(),
            )
            .unwrap();
            drop(restored);

            let replaced = db.join(".replaced-ds-7");
            let replaced_for_hook = replaced.clone();
            restore::BEFORE_KEEP_MARKER_WRITE.set(Some(Box::new(move |_| {
                FAIL_NEXT_DIR_SYNC.set(Some(replaced_for_hook.clone()));
            })));
            swap::FAIL_ROLLBACK_RENAME.set(true);
            let res = swap::replace_in_place(&catalog, "ds", &staged, "7", true);
            restore::BEFORE_KEEP_MARKER_WRITE.set(None);
            swap::FAIL_ROLLBACK_RENAME.set(false);
            let remaining = FAIL_NEXT_DIR_SYNC.replace(None);

            assert!(
                remaining.is_none(),
                "keep-marker sync failure must fire on {replaced:?}"
            );
            assert!(
                res.is_err(),
                "keepReplaced swap must not acknowledge success when keep-marker sync fails"
            );
            assert!(
                replaced.join(restore::SWAP_MARKER).exists(),
                "swap marker must remain intact when keep-marker sync fails"
            );
            assert!(
                replaced.join(KEEP_REPLACED_MARKER).exists(),
                "unsynced keep marker rename should be visible before simulated power loss"
            );
            drop(catalog);

            // Simulate power loss dropping the unsynced `.keep-replaced-v1` publication.
            std::fs::remove_file(replaced.join(KEEP_REPLACED_MARKER)).unwrap();

            let reopened = Catalog::open(dir.path(), Default::default()).unwrap();
            let active = reopened
                .get("ds")
                .expect("predecessor must be recovered instead of deleted on restart");
            assert_eq!(
                (active.dataset_id(), active.head_commit()),
                (pred_id, pred_head)
            );
            assert!(!replaced.exists());
            assert!(!db.join("ds").join(KEEP_REPLACED_MARKER).exists());
            assert!(!db.join("ds").join(restore::SWAP_MARKER).exists());
            let earlier_check =
                Store::open_existing(&kept_dst, Default::default(), Some(earlier_id)).unwrap();
            assert_eq!(earlier_check.head_commit(), earlier_head);
        }

        // Subtest B: occupied destination with injected keep-marker sync failure and normal live
        // rollback restores the predecessor cleanly without acknowledging retention.
        {
            let dir = tempfile::tempdir().unwrap();
            let catalog = Catalog::open(dir.path(), Default::default()).unwrap();
            let pred = catalog.create("ds", &Default::default()).unwrap();
            pred.update("INSERT DATA { <urn:r8-pred-b> <urn:p> 1 }")
                .unwrap();
            let pred_id = pred.dataset_id();
            let pred_head = pred.head_commit();
            drop(pred);

            let db = std::path::absolute(dir.path()).unwrap().join("databases");
            let kept_dst = db.join(".kept-ds-8");
            let earlier = Store::open(&kept_dst, Default::default()).unwrap();
            let earlier_id = earlier.dataset_id();
            let earlier_head = earlier.head_commit();
            drop(earlier);
            write_file_atomic(&kept_dst.join(KEEP_REPLACED_MARKER), b".kept-ds-8").unwrap();

            let staged = db.join(".restore-ds-8");
            drop(Store::open(&staged, Default::default()).unwrap());

            let replaced = db.join(".replaced-ds-8");
            let replaced_for_hook = replaced.clone();
            restore::BEFORE_KEEP_MARKER_WRITE.set(Some(Box::new(move |_| {
                FAIL_NEXT_DIR_SYNC.set(Some(replaced_for_hook.clone()));
            })));
            let res = swap::replace_in_place(&catalog, "ds", &staged, "8", true);
            restore::BEFORE_KEEP_MARKER_WRITE.set(None);
            assert!(FAIL_NEXT_DIR_SYNC.replace(None).is_none());
            assert!(res.is_err());

            let active = catalog.get("ds").expect("live rollback must reopen ds");
            assert_eq!(
                (active.dataset_id(), active.head_commit()),
                (pred_id, pred_head)
            );
            assert!(!replaced.exists());
            assert!(!db.join("ds").join(KEEP_REPLACED_MARKER).exists());
            assert!(!db.join("ds").join(restore::SWAP_MARKER).exists());
            let earlier_check =
                Store::open_existing(&kept_dst, Default::default(), Some(earlier_id)).unwrap();
            assert_eq!(earlier_check.head_commit(), earlier_head);
        }

        // Subtest C: occupied destination with durable keep-marker sync keeps all three heads
        // and proves `.keep-replaced-v1` is synced before `.restore-swap.json` is cleared.
        {
            let dir = tempfile::tempdir().unwrap();
            let catalog = Catalog::open(dir.path(), Default::default()).unwrap();
            let pred = catalog.create("ds", &Default::default()).unwrap();
            pred.update("INSERT DATA { <urn:r8-pred-c> <urn:p> 1 }")
                .unwrap();
            let pred_id = pred.dataset_id();
            let pred_head = pred.head_commit();
            drop(pred);

            let db = std::path::absolute(dir.path()).unwrap().join("databases");
            let kept_dst = db.join(".kept-ds-9");
            let earlier = Store::open(&kept_dst, Default::default()).unwrap();
            crate::sparql::update::update(
                &earlier,
                "INSERT DATA { <urn:r8-earlier-c> <urn:p> 10 }",
                &Default::default(),
            )
            .unwrap();
            let earlier_id = earlier.dataset_id();
            let earlier_head = earlier.head_commit();
            drop(earlier);
            write_file_atomic(&kept_dst.join(KEEP_REPLACED_MARKER), b".kept-ds-9").unwrap();

            let staged = db.join(".restore-ds-9");
            let restored = Store::open(&staged, Default::default()).unwrap();
            crate::sparql::update::update(
                &restored,
                "INSERT DATA { <urn:r8-restored-c> <urn:p> 20 . <urn:r8-restored-c> <urn:p> 21 }",
                &Default::default(),
            )
            .unwrap();
            let restored_id = restored.dataset_id();
            let restored_head = restored.head_commit();
            drop(restored);

            let replaced = db.join(".replaced-ds-9");
            let observed = std::rc::Rc::new(std::cell::RefCell::new(None));
            let observed_in_hook = observed.clone();
            let _ = take_catalog_dir_syncs();
            restore::BEFORE_SWAP_MARKER_REMOVE.set(Some(Box::new(move |dir| {
                let keep_present = dir.join(KEEP_REPLACED_MARKER).exists();
                let syncs = take_catalog_dir_syncs();
                *observed_in_hook.borrow_mut() = Some((keep_present, syncs));
            })));
            let ds = swap::replace_in_place(&catalog, "ds", &staged, "9", true).unwrap();
            restore::BEFORE_SWAP_MARKER_REMOVE.set(None);
            assert_eq!(
                (ds.dataset_id(), ds.head_commit()),
                (restored_id, restored_head)
            );
            let (keep_present, syncs) = observed
                .borrow_mut()
                .take()
                .expect("remove_swap_marker must run post-commit");
            assert!(
                keep_present,
                "keep marker must exist before clearing swap marker"
            );
            assert!(
                syncs.contains(&replaced),
                "predecessor dir {replaced:?} must be synced before clearing swap marker; saw {syncs:?}"
            );
            assert!(replaced.join(KEEP_REPLACED_MARKER).exists());
            assert!(!replaced.join(restore::SWAP_MARKER).exists());
            drop((ds, catalog));

            let reopened = Catalog::open(dir.path(), Default::default()).unwrap();
            let live = reopened.get("ds").unwrap();
            assert_eq!(
                (live.dataset_id(), live.head_commit()),
                (restored_id, restored_head)
            );
            let earlier_check =
                Store::open_existing(&kept_dst, Default::default(), Some(earlier_id)).unwrap();
            assert_eq!(earlier_check.head_commit(), earlier_head);
            let pred_check =
                Store::open_existing(&replaced, Default::default(), Some(pred_id)).unwrap();
            assert_eq!(pred_check.head_commit(), pred_head);
        }

        // Subtest D: BeforeReplace rollback of a `keepReplaced` swap clears and syncs
        // `.keep-replaced-v1` before clearing `.restore-swap.json`.
        {
            let dir = tempfile::tempdir().unwrap();
            let catalog = Catalog::open(dir.path(), Default::default()).unwrap();
            let pred = catalog.create("ds", &Default::default()).unwrap();
            pred.update("INSERT DATA { <urn:r8-pred-d> <urn:p> 1 }")
                .unwrap();
            let pred_id = pred.dataset_id();
            let pred_head = pred.head_commit();
            drop(pred);

            let db = std::path::absolute(dir.path()).unwrap().join("databases");
            let staged = db.join(".restore-ds-10");
            drop(Store::open(&staged, Default::default()).unwrap());
            let replaced = db.join(".replaced-ds-10");

            let observed = std::rc::Rc::new(std::cell::RefCell::new(None));
            let observed_in_hook = observed.clone();
            swap::ROLLBACK_HOOK.set(Some(Box::new(move || {
                let _ = take_catalog_dir_syncs();
            })));
            restore::BEFORE_SWAP_MARKER_REMOVE.set(Some(Box::new(move |dir| {
                let keep_present = dir.join(KEEP_REPLACED_MARKER).exists();
                let syncs = take_catalog_dir_syncs();
                *observed_in_hook.borrow_mut() = Some((keep_present, syncs));
            })));
            FAIL_REGISTRY_REPLACE.set(1);
            let res = swap::replace_in_place(&catalog, "ds", &staged, "10", true);
            FAIL_REGISTRY_REPLACE.set(0);
            swap::ROLLBACK_HOOK.set(None);
            restore::BEFORE_SWAP_MARKER_REMOVE.set(None);
            assert!(res.is_err());

            let (keep_present, syncs) = observed
                .borrow_mut()
                .take()
                .expect("remove_swap_marker must run on BeforeReplace rollback");
            assert!(
                !keep_present,
                "rollback must remove keep marker before removing swap marker"
            );
            assert!(
                syncs.contains(&replaced),
                "rollback must sync predecessor dir {replaced:?} after removing keep marker; saw {syncs:?}"
            );
            let active = catalog.get("ds").unwrap();
            assert_eq!(
                (active.dataset_id(), active.head_commit()),
                (pred_id, pred_head)
            );
            assert!(!db.join("ds").join(KEEP_REPLACED_MARKER).exists());
            assert!(!db.join("ds").join(restore::SWAP_MARKER).exists());
        }
    }

    #[test]
    fn r8_distinct_uuid_conflict_requires_valid_swap_classification() {
        use super::super::CONFIG_FILE;
        use crate::store::Store;

        let dir = tempfile::tempdir().unwrap();
        let catalog = Catalog::open(dir.path(), Default::default()).unwrap();
        let healthy = catalog.create("healthy", &Default::default()).unwrap();
        healthy
            .update("INSERT DATA { <urn:r8-healthy> <urn:p> 1 }")
            .unwrap();
        let healthy_id = healthy.dataset_id();
        let healthy_head = healthy.head_commit();
        drop(healthy);
        let doomed = catalog.create("doomed", &Default::default()).unwrap();
        doomed
            .update("INSERT DATA { <urn:r8-doomed-a> <urn:p> 2 }")
            .unwrap();
        drop(doomed);

        CRASH_AFTER.set(CRASH_AFTER_KEPT_RENAME);
        assert!(catalog.delete("doomed").is_err());
        CRASH_AFTER.set(0);
        drop(catalog);

        let intent = read_intents(dir.path()).unwrap().pop().unwrap();
        assert_eq!(intent.state, IntentState::Prepared);
        let db = dir.path().join("databases");
        let kept = db.join(intent.kept_name());
        let journal = dir.path().join(INTENT_FILE);

        // Build a distinct registered replacement (UUID B != intent UUID A) whose live path is
        // missing and whose sole `.replaced-doomed-42` carries readable `dataset.json` with UUID B.
        let replaced = db.join(".replaced-doomed-42");
        let replacement = Store::open(&replaced, Default::default()).unwrap();
        crate::sparql::update::update(
            &replacement,
            "INSERT DATA { <urn:r8-doomed-b> <urn:p> 3 . <urn:r8-doomed-b> <urn:p> 4 }",
            &Default::default(),
        )
        .unwrap();
        let replacement_id = replacement.dataset_id();
        let replacement_head = replacement.head_commit();
        assert_ne!(replacement_id, intent.id);
        drop(replacement);

        let replacement_rev = Uuid::new_v4();
        let mut reg = read_registry(dir.path()).unwrap();
        let row = reg
            .datasets
            .iter_mut()
            .find(|entry| entry.name == "doomed")
            .expect("doomed row must exist");
        row.id = Some(replacement_id);
        row.revision = Some(replacement_rev);
        std::fs::write(
            dir.path().join(CONFIG_FILE),
            sparkles_core::formats::settings_to_vec(CONFIG_FILE, &reg).unwrap(),
        )
        .unwrap();
        assert!(!db.join("doomed").exists());

        let swap_marker = replaced.join(super::super::restore::SWAP_MARKER);

        // Case 1: truncated swap marker (with and without `.keep-replaced-v1`) must NOT use the
        // distinct-UUID exception; healthy sibling opens, doomed stays closed, and all staged
        // copies + delete journal survive.
        for with_keep_marker in [false, true] {
            if with_keep_marker {
                write_file_atomic(&replaced.join(KEEP_REPLACED_MARKER), b".kept-doomed-42")
                    .unwrap();
            }
            std::fs::write(&swap_marker, b"{\"format\":1,").unwrap();
            let reopened = Catalog::open(dir.path(), Default::default()).unwrap();
            let h = reopened
                .get("healthy")
                .expect("healthy sibling must open despite distinct-UUID malformed swap marker");
            assert_eq!(
                (h.dataset_id(), h.head_commit()),
                (healthy_id, healthy_head)
            );
            assert_eq!(reopened.refusal("doomed").map(|r| r.kind), Some("recovery"));
            assert!(reopened.get("doomed").is_none());
            assert!(!db.join("doomed").exists());
            assert!(kept.join("dataset.json").exists());
            assert!(replaced.join("dataset.json").exists());
            assert!(journal.exists());
        }
        std::fs::remove_file(replaced.join(KEEP_REPLACED_MARKER)).unwrap();

        // Case 2: valid JSON swap marker whose predecessor_revision does NOT match the registered
        // revision is not a verified rollback of B and must also stay isolated to `/doomed`.
        std::fs::write(
            &swap_marker,
            serde_json::to_vec(&serde_json::json!({
                "format": 1,
                "dataset": "doomed",
                "predecessor_id": replacement_id,
                "predecessor_revision": Uuid::new_v4(),
                "restored_id": Uuid::new_v4(),
                "restored_revision": Uuid::new_v4(),
            }))
            .unwrap(),
        )
        .unwrap();
        {
            let reopened = Catalog::open(dir.path(), Default::default()).unwrap();
            let h = reopened.get("healthy").unwrap();
            assert_eq!(
                (h.dataset_id(), h.head_commit()),
                (healthy_id, healthy_head)
            );
            assert_eq!(reopened.refusal("doomed").map(|r| r.kind), Some("recovery"));
            assert!(reopened.get("doomed").is_none());
            assert!(!db.join("doomed").exists());
            assert!(kept.join("dataset.json").exists());
            assert!(replaced.join("dataset.json").exists());
            assert!(journal.exists());
        }

        // Prepare a verified rollback swap marker matching UUID B and `replacement_rev`.
        let valid_rollback_marker = serde_json::to_vec(&serde_json::json!({
            "format": 1,
            "dataset": "doomed",
            "predecessor_id": replacement_id,
            "predecessor_revision": replacement_rev,
            "restored_id": Uuid::new_v4(),
            "restored_revision": Uuid::new_v4(),
        }))
        .unwrap();
        std::fs::write(&swap_marker, &valid_rollback_marker).unwrap();

        // Case 3: unreadable swap marker must stay isolated to `/doomed` while `/healthy` opens.
        #[cfg(unix)]
        if unsafe { libc::geteuid() } != 0 {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&swap_marker, std::fs::Permissions::from_mode(0o000)).unwrap();
            let opened = Catalog::open(dir.path(), Default::default());
            std::fs::set_permissions(&swap_marker, std::fs::Permissions::from_mode(0o600)).unwrap();
            let reopened = opened
                .expect("healthy sibling must open when distinct-UUID swap marker is unreadable");
            let h = reopened.get("healthy").unwrap();
            assert_eq!(
                (h.dataset_id(), h.head_commit()),
                (healthy_id, healthy_head)
            );
            assert_eq!(reopened.refusal("doomed").map(|r| r.kind), Some("recovery"));
            assert!(reopened.get("doomed").is_none());
            assert!(!db.join("doomed").exists());
            assert!(kept.join("dataset.json").exists());
            assert!(replaced.join("dataset.json").exists());
            assert!(journal.exists());
        }

        // Case 4 (positive control): once the swap marker is readable and valid for B's rollback,
        // the distinct-UUID exception rolls back `/doomed` to B and retires A's delete copy.
        let reopened = Catalog::open(dir.path(), Default::default()).unwrap();
        let h = reopened.get("healthy").unwrap();
        assert_eq!(
            (h.dataset_id(), h.head_commit()),
            (healthy_id, healthy_head)
        );
        assert!(
            reopened.refusal("doomed").is_none(),
            "{:?}",
            reopened.refusals()
        );
        let restored_b = reopened.get("doomed").unwrap();
        assert_eq!(
            (restored_b.dataset_id(), restored_b.head_commit()),
            (replacement_id, replacement_head)
        );
        assert!(!replaced.exists());
        assert!(!kept.exists());
        assert!(!journal.exists());
    }

    #[cfg(feature = "backup")]
    #[test]
    fn r9_live_keep_replaced_preserves_keep_marker_directory_on_rollback() {
        use crate::catalog::{restore, swap};
        use crate::store::Store;

        // Subtest A: an unexpected `.keep-replaced-v1` directory in a healthy predecessor
        // must fail live `keepReplaced` restore cleanly, roll back the predecessor with its
        // directory preserved, and keep healthy siblings online.
        {
            let dir = tempfile::tempdir().unwrap();
            let catalog = Catalog::open(dir.path(), Default::default()).unwrap();
            let healthy = catalog.create("healthy", &Default::default()).unwrap();
            healthy
                .update("INSERT DATA { <urn:r9-healthy-a> <urn:p> 1 }")
                .unwrap();
            let healthy_id = healthy.dataset_id();
            let healthy_head = healthy.head_commit();
            drop(healthy);

            let pred = catalog.create("ds", &Default::default()).unwrap();
            pred.update("INSERT DATA { <urn:r9-pred-a> <urn:p> 2 }")
                .unwrap();
            let pred_id = pred.dataset_id();
            let pred_head = pred.head_commit();
            drop(pred);

            let db = std::path::absolute(dir.path()).unwrap().join("databases");
            let keep_dir = db.join("ds").join(KEEP_REPLACED_MARKER);

            let staged = db.join(".restore-ds-11");
            let restored = Store::open(&staged, Default::default()).unwrap();
            crate::sparql::update::update(
                &restored,
                "INSERT DATA { <urn:r9-restored-a> <urn:p> 3 . <urn:r9-restored-a> <urn:p> 4 }",
                &Default::default(),
            )
            .unwrap();
            drop(restored);

            // Inject the `.keep-replaced-v1` directory after rename 1 and swap-marker
            // publication (bypassing live preflight) so live rollback exercises the
            // post-rename-1 rollback path.
            restore::BEFORE_KEEP_MARKER_WRITE.set(Some(Box::new(|replaced| {
                let staged_keep_dir = replaced.join(KEEP_REPLACED_MARKER);
                std::fs::create_dir(&staged_keep_dir).unwrap();
                std::fs::write(staged_keep_dir.join("sentinel.txt"), b"keep-dir-live").unwrap();
            })));
            let res = swap::replace_in_place(&catalog, "ds", &staged, "11", true);
            restore::BEFORE_KEEP_MARKER_WRITE.set(None);
            assert!(
                res.is_err(),
                "keepReplaced restore must fail when .keep-replaced-v1 is a directory"
            );

            let active = catalog
                .get("ds")
                .expect("live rollback must reopen ds when .keep-replaced-v1 is a directory");
            assert_eq!(
                (active.dataset_id(), active.head_commit()),
                (pred_id, pred_head)
            );
            let h = catalog
                .get("healthy")
                .expect("healthy sibling must stay online");
            assert_eq!(
                (h.dataset_id(), h.head_commit()),
                (healthy_id, healthy_head)
            );
            assert!(!db.join(".replaced-ds-11").exists());
            assert!(!db.join("ds").join(restore::SWAP_MARKER).exists());
            assert!(keep_dir.is_dir());
            assert_eq!(
                std::fs::read(keep_dir.join("sentinel.txt")).unwrap(),
                b"keep-dir-live"
            );
            drop((active, h, catalog));

            let reopened = Catalog::open(dir.path(), Default::default()).unwrap();
            let h = reopened.get("healthy").unwrap();
            assert_eq!(
                (h.dataset_id(), h.head_commit()),
                (healthy_id, healthy_head)
            );
            let active = reopened.get("ds").unwrap();
            assert_eq!(
                (active.dataset_id(), active.head_commit()),
                (pred_id, pred_head)
            );
            assert_eq!(
                std::fs::read(keep_dir.join("sentinel.txt")).unwrap(),
                b"keep-dir-live"
            );
        }

        // Subtest B: when live rollback is also interrupted, startup rollback must recover
        // the predecessor cleanly, preserve its `.keep-replaced-v1` directory, and keep
        // healthy siblings online.
        {
            let dir = tempfile::tempdir().unwrap();
            let catalog = Catalog::open(dir.path(), Default::default()).unwrap();
            let healthy = catalog.create("healthy", &Default::default()).unwrap();
            healthy
                .update("INSERT DATA { <urn:r9-healthy-b> <urn:p> 1 }")
                .unwrap();
            let healthy_id = healthy.dataset_id();
            let healthy_head = healthy.head_commit();
            drop(healthy);

            let pred = catalog.create("ds", &Default::default()).unwrap();
            pred.update("INSERT DATA { <urn:r9-pred-b> <urn:p> 2 }")
                .unwrap();
            let pred_id = pred.dataset_id();
            let pred_head = pred.head_commit();
            drop(pred);

            let db = std::path::absolute(dir.path()).unwrap().join("databases");
            let keep_dir = db.join("ds").join(KEEP_REPLACED_MARKER);

            let staged = db.join(".restore-ds-12");
            let restored = Store::open(&staged, Default::default()).unwrap();
            crate::sparql::update::update(
                &restored,
                "INSERT DATA { <urn:r9-restored-b> <urn:p> 3 . <urn:r9-restored-b> <urn:p> 4 }",
                &Default::default(),
            )
            .unwrap();
            drop(restored);

            // Inject the `.keep-replaced-v1` directory after rename 1 and swap-marker
            // publication (bypassing live preflight) so live rollback fails and startup
            // recovery exercises an already-staged root carrying both markers.
            restore::BEFORE_KEEP_MARKER_WRITE.set(Some(Box::new(|replaced| {
                let staged_keep_dir = replaced.join(KEEP_REPLACED_MARKER);
                std::fs::create_dir(&staged_keep_dir).unwrap();
                std::fs::write(staged_keep_dir.join("sentinel.txt"), b"keep-dir-startup").unwrap();
            })));
            swap::FAIL_ROLLBACK_RENAME.set(true);
            let res = swap::replace_in_place(&catalog, "ds", &staged, "12", true);
            swap::FAIL_ROLLBACK_RENAME.set(false);
            restore::BEFORE_KEEP_MARKER_WRITE.set(None);
            assert!(res.is_err());

            let replaced = db.join(".replaced-ds-12");
            assert!(replaced.join(restore::SWAP_MARKER).exists());
            assert!(replaced.join(KEEP_REPLACED_MARKER).is_dir());
            drop(catalog);

            let reopened = Catalog::open(dir.path(), Default::default()).expect(
                "startup rollback must preserve .keep-replaced-v1 directory and open catalog",
            );
            let h = reopened
                .get("healthy")
                .expect("healthy sibling must stay online after startup rollback");
            assert_eq!(
                (h.dataset_id(), h.head_commit()),
                (healthy_id, healthy_head)
            );
            let active = reopened
                .get("ds")
                .expect("predecessor must be rolled back on startup");
            assert_eq!(
                (active.dataset_id(), active.head_commit()),
                (pred_id, pred_head)
            );
            assert!(!replaced.exists());
            assert!(!db.join("ds").join(restore::SWAP_MARKER).exists());
            assert!(keep_dir.is_dir());
            assert_eq!(
                std::fs::read(keep_dir.join("sentinel.txt")).unwrap(),
                b"keep-dir-startup"
            );
        }
    }

    #[cfg(feature = "backup")]
    #[test]
    fn r10_live_swap_refuses_non_regular_markers_before_rename() {
        use crate::catalog::{restore, swap};
        use crate::store::Store;

        // Subtest A: pre-existing `.restore-swap.json` directory is refused BEFORE rename 1.
        {
            let dir = tempfile::tempdir().unwrap();
            let catalog = Catalog::open(dir.path(), Default::default()).unwrap();
            let healthy = catalog.create("healthy", &Default::default()).unwrap();
            healthy
                .update("INSERT DATA { <urn:r10-healthy-a> <urn:p> 1 }")
                .unwrap();
            let healthy_id = healthy.dataset_id();
            let healthy_head = healthy.head_commit();
            drop(healthy);

            let pred = catalog.create("ds", &Default::default()).unwrap();
            pred.update("INSERT DATA { <urn:r10-pred-a> <urn:p> 2 }")
                .unwrap();
            let pred_id = pred.dataset_id();
            let pred_head = pred.head_commit();
            drop(pred);

            let db = std::path::absolute(dir.path()).unwrap().join("databases");
            let swap_dir = db.join("ds").join(restore::SWAP_MARKER);
            std::fs::create_dir(&swap_dir).unwrap();
            std::fs::write(swap_dir.join("sentinel.txt"), b"swap-dir-live").unwrap();

            let staged = db.join(".restore-ds-13");
            let restored = Store::open(&staged, Default::default()).unwrap();
            crate::sparql::update::update(
                &restored,
                "INSERT DATA { <urn:r10-restored-a> <urn:p> 3 . <urn:r10-restored-a> <urn:p> 4 }",
                &Default::default(),
            )
            .unwrap();
            drop(restored);

            // Arm CRASH_AFTER(1): if rename 1 were ever attempted, `ds` would be moved
            // aside to `.replaced-ds-13` and left detached.
            swap::CRASH_AFTER.set(1);
            let res = swap::replace_in_place(&catalog, "ds", &staged, "13", false);
            swap::CRASH_AFTER.set(0);
            assert!(
                res.is_err(),
                "swap must fail when .restore-swap.json is a directory"
            );

            let active = catalog
                .get("ds")
                .expect("preflight must refuse before rename 1 and keep ds open");
            assert_eq!(
                (active.dataset_id(), active.head_commit()),
                (pred_id, pred_head)
            );
            let h = catalog
                .get("healthy")
                .expect("healthy sibling must stay online");
            assert_eq!(
                (h.dataset_id(), h.head_commit()),
                (healthy_id, healthy_head)
            );
            assert!(!db.join(".replaced-ds-13").exists());
            assert!(swap_dir.is_dir());
            assert_eq!(
                std::fs::read(swap_dir.join("sentinel.txt")).unwrap(),
                b"swap-dir-live"
            );
            drop((active, h, catalog));

            let reopened = Catalog::open(dir.path(), Default::default()).unwrap();
            let h = reopened.get("healthy").unwrap();
            assert_eq!(
                (h.dataset_id(), h.head_commit()),
                (healthy_id, healthy_head)
            );
            let active = reopened.get("ds").unwrap();
            assert_eq!(
                (active.dataset_id(), active.head_commit()),
                (pred_id, pred_head)
            );
            assert_eq!(
                std::fs::read(swap_dir.join("sentinel.txt")).unwrap(),
                b"swap-dir-live"
            );
        }

        // Subtest B: pre-existing `.keep-replaced-v1` directory with `keepReplaced` is refused
        // BEFORE rename 1.
        {
            let dir = tempfile::tempdir().unwrap();
            let catalog = Catalog::open(dir.path(), Default::default()).unwrap();
            let pred = catalog.create("ds", &Default::default()).unwrap();
            pred.update("INSERT DATA { <urn:r10-pred-b> <urn:p> 2 }")
                .unwrap();
            let pred_id = pred.dataset_id();
            let pred_head = pred.head_commit();
            drop(pred);

            let db = std::path::absolute(dir.path()).unwrap().join("databases");
            let keep_dir = db.join("ds").join(KEEP_REPLACED_MARKER);
            std::fs::create_dir(&keep_dir).unwrap();
            std::fs::write(keep_dir.join("sentinel.txt"), b"keep-dir-preflight").unwrap();

            let staged = db.join(".restore-ds-14");
            drop(Store::open(&staged, Default::default()).unwrap());

            swap::CRASH_AFTER.set(1);
            let res = swap::replace_in_place(&catalog, "ds", &staged, "14", true);
            swap::CRASH_AFTER.set(0);
            assert!(res.is_err());

            let active = catalog
                .get("ds")
                .expect("keepReplaced preflight must refuse before rename 1");
            assert_eq!(
                (active.dataset_id(), active.head_commit()),
                (pred_id, pred_head)
            );
            assert!(!db.join(".replaced-ds-14").exists());
            assert_eq!(
                std::fs::read(keep_dir.join("sentinel.txt")).unwrap(),
                b"keep-dir-preflight"
            );
        }

        // Subtest C (Unix): pre-existing symlink at `.restore-swap.json` is refused BEFORE rename 1.
        #[cfg(unix)]
        {
            let dir = tempfile::tempdir().unwrap();
            let catalog = Catalog::open(dir.path(), Default::default()).unwrap();
            let pred = catalog.create("ds", &Default::default()).unwrap();
            let pred_id = pred.dataset_id();
            drop(pred);

            let db = std::path::absolute(dir.path()).unwrap().join("databases");
            let swap_link = db.join("ds").join(restore::SWAP_MARKER);
            std::os::unix::fs::symlink(dir.path().join("nonexistent-target"), &swap_link).unwrap();

            let staged = db.join(".restore-ds-15");
            drop(Store::open(&staged, Default::default()).unwrap());

            swap::CRASH_AFTER.set(1);
            let res = swap::replace_in_place(&catalog, "ds", &staged, "15", false);
            swap::CRASH_AFTER.set(0);
            assert!(res.is_err());

            assert_eq!(catalog.get("ds").unwrap().dataset_id(), pred_id);
            assert!(!db.join(".replaced-ds-15").exists());
            assert!(std::fs::symlink_metadata(&swap_link).unwrap().is_symlink());
        }
    }

    #[test]
    fn r10_startup_rollback_preserves_swap_marker_directory_and_sibling() {
        use super::super::CONFIG_FILE;
        use crate::catalog::restore;
        use crate::store::Store;

        // Subtest A: already-staged predecessor carrying a sentinel directory at
        // `.restore-swap.json` rolls back cleanly on startup, preserves the sentinel,
        // and keeps healthy siblings online.
        {
            let dir = tempfile::tempdir().unwrap();
            let catalog = Catalog::open(dir.path(), Default::default()).unwrap();
            let healthy = catalog.create("healthy", &Default::default()).unwrap();
            healthy
                .update("INSERT DATA { <urn:r10-startup-healthy> <urn:p> 1 }")
                .unwrap();
            let healthy_id = healthy.dataset_id();
            let healthy_head = healthy.head_commit();
            drop(healthy);

            let pred = catalog.create("ds", &Default::default()).unwrap();
            pred.update("INSERT DATA { <urn:r10-startup-pred> <urn:p> 2 }")
                .unwrap();
            let pred_id = pred.dataset_id();
            let pred_head = pred.head_commit();
            drop((pred, catalog));

            let db = std::path::absolute(dir.path()).unwrap().join("databases");
            let replaced = db.join(".replaced-ds-14");
            std::fs::rename(db.join("ds"), &replaced).unwrap();
            let swap_dir = replaced.join(restore::SWAP_MARKER);
            std::fs::create_dir(&swap_dir).unwrap();
            std::fs::write(swap_dir.join("sentinel.txt"), b"swap-dir-startup").unwrap();

            let reopened = Catalog::open(dir.path(), Default::default()).expect(
                "startup rollback must preserve .restore-swap.json directory and open catalog",
            );
            let h = reopened
                .get("healthy")
                .expect("healthy sibling must remain available");
            assert_eq!(
                (h.dataset_id(), h.head_commit()),
                (healthy_id, healthy_head)
            );
            let active = reopened
                .get("ds")
                .expect("predecessor must roll back on startup");
            assert_eq!(
                (active.dataset_id(), active.head_commit()),
                (pred_id, pred_head)
            );
            assert!(!replaced.exists());
            let live_swap_dir = db.join("ds").join(restore::SWAP_MARKER);
            assert!(live_swap_dir.is_dir());
            assert_eq!(
                std::fs::read(live_swap_dir.join("sentinel.txt")).unwrap(),
                b"swap-dir-startup"
            );
        }

        // Subtest B: distinct-UUID rollback when staged replacement UUID B has a sentinel
        // directory at `.restore-swap.json` still reinstates B, preserves the sentinel,
        // keeps healthy siblings online, and retires prior UUID A.
        {
            let dir = tempfile::tempdir().unwrap();
            let catalog = Catalog::open(dir.path(), Default::default()).unwrap();
            let healthy = catalog.create("healthy", &Default::default()).unwrap();
            healthy
                .update("INSERT DATA { <urn:r10-distinct-healthy> <urn:p> 1 }")
                .unwrap();
            let healthy_id = healthy.dataset_id();
            let healthy_head = healthy.head_commit();
            drop(healthy);

            let doomed = catalog.create("doomed", &Default::default()).unwrap();
            doomed
                .update("INSERT DATA { <urn:r10-distinct-a> <urn:p> 2 }")
                .unwrap();
            drop(doomed);

            CRASH_AFTER.set(CRASH_AFTER_KEPT_RENAME);
            assert!(catalog.delete("doomed").is_err());
            CRASH_AFTER.set(0);
            drop(catalog);

            let intent = read_intents(dir.path()).unwrap().pop().unwrap();
            let db = dir.path().join("databases");
            let kept = db.join(intent.kept_name());
            let journal = dir.path().join(INTENT_FILE);

            let replaced = db.join(".replaced-doomed-43");
            let replacement = Store::open(&replaced, Default::default()).unwrap();
            crate::sparql::update::update(
                &replacement,
                "INSERT DATA { <urn:r10-distinct-b> <urn:p> 3 . <urn:r10-distinct-b> <urn:p> 4 }",
                &Default::default(),
            )
            .unwrap();
            let replacement_id = replacement.dataset_id();
            let replacement_head = replacement.head_commit();
            assert_ne!(replacement_id, intent.id);
            drop(replacement);

            let mut reg = read_registry(dir.path()).unwrap();
            let row = reg
                .datasets
                .iter_mut()
                .find(|entry| entry.name == "doomed")
                .expect("doomed row must exist");
            row.id = Some(replacement_id);
            row.revision = Some(Uuid::new_v4());
            std::fs::write(
                dir.path().join(CONFIG_FILE),
                sparkles_core::formats::settings_to_vec(CONFIG_FILE, &reg).unwrap(),
            )
            .unwrap();

            let swap_dir = replaced.join(restore::SWAP_MARKER);
            std::fs::create_dir(&swap_dir).unwrap();
            std::fs::write(swap_dir.join("sentinel.txt"), b"distinct-swap-dir").unwrap();

            let reopened = Catalog::open(dir.path(), Default::default())
                .expect("distinct-UUID rollback with .restore-swap.json directory must succeed");
            let h = reopened.get("healthy").unwrap();
            assert_eq!(
                (h.dataset_id(), h.head_commit()),
                (healthy_id, healthy_head)
            );
            let restored_b = reopened.get("doomed").unwrap();
            assert_eq!(
                (restored_b.dataset_id(), restored_b.head_commit()),
                (replacement_id, replacement_head)
            );
            let live_swap_dir = db.join("doomed").join(restore::SWAP_MARKER);
            assert!(live_swap_dir.is_dir());
            assert_eq!(
                std::fs::read(live_swap_dir.join("sentinel.txt")).unwrap(),
                b"distinct-swap-dir"
            );
            assert!(!replaced.exists());
            assert!(!kept.exists());
            assert!(!journal.exists());
        }

        // Subtest C: malformed and unsupported regular `.restore-swap.json` files remain
        // rejected with Corrupt and Unsupported respectively.
        {
            let dir = tempfile::tempdir().unwrap();
            let catalog = Catalog::open(dir.path(), Default::default()).unwrap();
            drop(catalog.create("ds", &Default::default()).unwrap());
            drop(catalog);

            let db = dir.path().join("databases");
            let replaced = db.join(".replaced-ds-15");
            std::fs::rename(db.join("ds"), &replaced).unwrap();
            let marker = replaced.join(restore::SWAP_MARKER);

            std::fs::write(&marker, b"{\"format\":1,").unwrap();
            let err = Catalog::open(dir.path(), Default::default())
                .err()
                .expect("malformed regular swap marker must still fail closed");
            assert!(matches!(err, Error::Corrupt(_)), "{err:?}");
            assert!(replaced.exists() && !db.join("ds").exists());

            std::fs::write(&marker, br#"{"format":99,"dataset":"ds"}"#).unwrap();
            let err = Catalog::open(dir.path(), Default::default())
                .err()
                .expect("future regular swap marker must still report Unsupported");
            assert!(matches!(err, Error::Unsupported(_)), "{err:?}");
            assert!(replaced.exists() && !db.join("ds").exists());
        }
    }

    #[test]
    fn r9_distinct_uuid_rollback_preserves_keep_marker_directory_and_sibling() {
        use super::super::CONFIG_FILE;
        use crate::store::Store;

        for keep_marker_is_dir in [true, false] {
            let dir = tempfile::tempdir().unwrap();
            let catalog = Catalog::open(dir.path(), Default::default()).unwrap();
            let healthy = catalog.create("healthy", &Default::default()).unwrap();
            healthy
                .update("INSERT DATA { <urn:r9-distinct-healthy> <urn:p> 1 }")
                .unwrap();
            let healthy_id = healthy.dataset_id();
            let healthy_head = healthy.head_commit();
            drop(healthy);

            let doomed = catalog.create("doomed", &Default::default()).unwrap();
            doomed
                .update("INSERT DATA { <urn:r9-distinct-a> <urn:p> 2 }")
                .unwrap();
            drop(doomed);

            CRASH_AFTER.set(CRASH_AFTER_KEPT_RENAME);
            assert!(catalog.delete("doomed").is_err());
            CRASH_AFTER.set(0);
            drop(catalog);

            let intent = read_intents(dir.path()).unwrap().pop().unwrap();
            assert_eq!(intent.state, IntentState::Prepared);
            let db = dir.path().join("databases");
            let kept = db.join(intent.kept_name());
            let journal = dir.path().join(INTENT_FILE);

            let replaced = db.join(".replaced-doomed-42");
            let replacement = Store::open(&replaced, Default::default()).unwrap();
            crate::sparql::update::update(
                &replacement,
                "INSERT DATA { <urn:r9-distinct-b> <urn:p> 3 . <urn:r9-distinct-b> <urn:p> 4 }",
                &Default::default(),
            )
            .unwrap();
            let replacement_id = replacement.dataset_id();
            let replacement_head = replacement.head_commit();
            assert_ne!(replacement_id, intent.id);
            drop(replacement);

            let replacement_rev = Uuid::new_v4();
            let mut reg = read_registry(dir.path()).unwrap();
            let row = reg
                .datasets
                .iter_mut()
                .find(|entry| entry.name == "doomed")
                .expect("doomed row must exist");
            row.id = Some(replacement_id);
            row.revision = Some(replacement_rev);
            std::fs::write(
                dir.path().join(CONFIG_FILE),
                sparkles_core::formats::settings_to_vec(CONFIG_FILE, &reg).unwrap(),
            )
            .unwrap();
            assert!(!db.join("doomed").exists());

            std::fs::write(
                replaced.join(super::super::restore::SWAP_MARKER),
                serde_json::to_vec(&serde_json::json!({
                    "format": 1,
                    "dataset": "doomed",
                    "predecessor_id": replacement_id,
                    "predecessor_revision": replacement_rev,
                    "restored_id": Uuid::new_v4(),
                    "restored_revision": Uuid::new_v4(),
                }))
                .unwrap(),
            )
            .unwrap();

            let keep_path = replaced.join(KEEP_REPLACED_MARKER);
            if keep_marker_is_dir {
                std::fs::create_dir(&keep_path).unwrap();
                std::fs::write(keep_path.join("sentinel.txt"), b"distinct-keep-dir").unwrap();
            } else {
                write_file_atomic(&keep_path, b".kept-doomed-42").unwrap();
            }

            let reopened = Catalog::open(dir.path(), Default::default()).expect(
                "verified distinct-UUID rollback must not fail catalog open regardless of keep marker kind",
            );
            let h = reopened
                .get("healthy")
                .expect("healthy sibling must remain available");
            assert_eq!(
                (h.dataset_id(), h.head_commit()),
                (healthy_id, healthy_head)
            );
            assert!(
                reopened.refusal("doomed").is_none(),
                "{:?}",
                reopened.refusals()
            );
            let restored_b = reopened.get("doomed").unwrap();
            assert_eq!(
                (restored_b.dataset_id(), restored_b.head_commit()),
                (replacement_id, replacement_head)
            );
            let live_keep = db.join("doomed").join(KEEP_REPLACED_MARKER);
            if keep_marker_is_dir {
                assert!(live_keep.is_dir());
                assert_eq!(
                    std::fs::read(live_keep.join("sentinel.txt")).unwrap(),
                    b"distinct-keep-dir"
                );
            } else {
                assert!(!live_keep.exists());
            }
            assert!(
                !db.join("doomed")
                    .join(super::super::restore::SWAP_MARKER)
                    .exists()
            );
            assert!(!replaced.exists());
            assert!(!kept.exists());
            assert!(!journal.exists());
        }
    }

    #[test]
    fn staging_failures_and_precommit_crashes_restore_the_registered_dataset() {
        use super::super::{
            DELETE_BEFORE_MOVE, DELETE_FAIL, DELETE_ROLLBACK_MOVE, DELETE_ROLLBACK_SYNC,
            DELETE_SAVE_BEFORE_REPLACE, DELETE_STAGE_SYNC,
        };
        for (failure, stage, kept) in [
            (
                (1 << DELETE_BEFORE_MOVE) | (1 << DELETE_SAVE_BEFORE_REPLACE),
                CRASH_AFTER_FAILED_FORWARD_MOVE,
                false,
            ),
            (
                (1 << DELETE_STAGE_SYNC) | (1 << DELETE_ROLLBACK_MOVE),
                CRASH_AFTER_FAILED_ROLLBACK,
                true,
            ),
            (
                (1 << DELETE_STAGE_SYNC) | (1 << DELETE_ROLLBACK_SYNC),
                CRASH_AFTER_FAILED_ROLLBACK,
                false,
            ),
            (
                1 << DELETE_STAGE_SYNC,
                CRASH_AFTER_ROLLBACK_MOVE_SYNC,
                false,
            ),
            (
                1 << DELETE_SAVE_BEFORE_REPLACE,
                CRASH_BEFORE_REGISTRY_REPLACE,
                true,
            ),
            (0, CRASH_AFTER_KEPT_RENAME, true),
        ] {
            let dir = tempfile::tempdir().unwrap();
            DELETE_FAIL.set(failure);
            let intent = interrupted_delete(dir.path(), stage);
            DELETE_FAIL.set(0);
            assert_eq!(intent.state, IntentState::Prepared);
            let registry = read_registry(dir.path()).unwrap();
            assert_eq!(registry.datasets[0].id, Some(intent.id));
            assert_eq!(kept_entries(dir.path()).len(), usize::from(kept));

            let catalog = Catalog::open(dir.path(), Default::default()).unwrap();
            let dataset = catalog.get("doomed").unwrap();
            assert_eq!(dataset.dataset_id(), intent.id);
            assert_eq!(dataset.len(), 1);
            assert!(!dir.path().join(INTENT_FILE).exists());
        }
    }

    #[test]
    fn registry_replace_sync_failure_follows_only_the_surviving_registry() {
        use super::super::{DELETE_FAIL, DELETE_SAVE_SYNC};

        for replacement_survives in [false, true] {
            let dir = tempfile::tempdir().unwrap();
            let catalog = Catalog::open(dir.path(), Default::default()).unwrap();
            let doomed = catalog.create("doomed", &Default::default()).unwrap();
            doomed.update("INSERT DATA { <urn:d> <urn:p> 2 }").unwrap();
            let id = doomed.dataset_id();
            drop(doomed);
            let config = dir.path().join(super::super::CONFIG_FILE);
            let original = std::fs::read(&config).unwrap();
            DELETE_FAIL.set(1 << DELETE_SAVE_SYNC);
            let result = catalog.delete("doomed");
            DELETE_FAIL.set(0);
            let error = result.unwrap_err().to_string();
            let (_, retained) = error.split_once("indeterminate commit:").unwrap();
            let (_, path) = retained.split_once("; ").unwrap();
            assert_eq!(path, path.trim_start());
            assert!(catalog.get("doomed").is_none());
            assert!(catalog.create("doomed", &Default::default()).is_err());
            assert!(
                catalog
                    .attach("doomed", crate::catalog::Attach::Memory)
                    .is_err()
            );
            assert!(catalog.delete("doomed").is_err());
            assert!(catalog.rename("doomed", "elsewhere").is_err());
            assert_eq!(kept_entries(dir.path()).len(), 1);
            assert!(read_registry(dir.path()).unwrap().datasets.is_empty());
            assert!(!dir.path().join("databases/doomed").exists());
            drop(catalog);

            if !replacement_survives {
                write_file_atomic(&config, &original).unwrap();
            }
            let catalog = Catalog::open(dir.path(), Default::default()).unwrap();
            if replacement_survives {
                assert!(catalog.get("doomed").is_none());
                drop(catalog.create("doomed", &Default::default()).unwrap());
            } else {
                let dataset = catalog.get("doomed").unwrap();
                assert_eq!(dataset.dataset_id(), id);
                assert_eq!(dataset.len(), 1);
            }
            assert!(!dir.path().join(INTENT_FILE).exists());
            assert!(kept_entries(dir.path()).is_empty());
        }
    }

    #[test]
    fn prior_commit_cannot_restore_the_old_id_over_a_missing_replacement() {
        let dir = tempfile::tempdir().unwrap();
        let catalog = Catalog::open(dir.path(), Default::default()).unwrap();
        drop(catalog.create("doomed", &Default::default()).unwrap());
        CRASH_AFTER.set(CRASH_AFTER_SAVE);
        assert!(catalog.delete("doomed").is_err());
        CRASH_AFTER.set(0);
        let replacement = catalog.create("doomed", &Default::default()).unwrap();
        let replacement_id = replacement.dataset_id();
        drop(replacement);
        drop(catalog);
        std::fs::remove_dir_all(dir.path().join("databases/doomed")).unwrap();

        let reopened = Catalog::open(dir.path(), Default::default()).unwrap();
        assert!(reopened.refusal("doomed").is_some());
        assert!(reopened.get("doomed").is_none());
        assert_eq!(
            read_registry(dir.path()).unwrap().datasets[0].id,
            Some(replacement_id)
        );
        assert!(!dir.path().join(INTENT_FILE).exists());
        assert!(kept_entries(dir.path()).is_empty());
    }

    #[test]
    fn legacy_registry_id_is_bound_before_the_first_stage() {
        let dir = tempfile::tempdir().unwrap();
        let catalog = Catalog::open(dir.path(), Default::default()).unwrap();
        let dataset = catalog.create("old", &Default::default()).unwrap();
        let id = dataset.dataset_id();
        drop(dataset);
        let config = dir.path().join(super::super::CONFIG_FILE);
        std::fs::write(
            &config,
            br#"{"format":1,"datasets":[{"name":"old","type":"persistent"}]}"#,
        )
        .unwrap();
        CRASH_AFTER.set(CRASH_AFTER_KEPT_RENAME);
        assert!(catalog.delete("old").is_err());
        CRASH_AFTER.set(0);
        drop(catalog);

        assert_eq!(read_registry(dir.path()).unwrap().datasets[0].id, Some(id));
        let catalog = Catalog::open(dir.path(), Default::default()).unwrap();
        assert_eq!(catalog.get("old").unwrap().dataset_id(), id);
    }

    #[test]
    fn staged_restore_and_delete_conflict_keeps_both_copies() {
        let dir = tempfile::tempdir().unwrap();
        let catalog = Catalog::open(dir.path(), Default::default()).unwrap();
        let healthy = catalog.create("healthy", &Default::default()).unwrap();
        healthy
            .update("INSERT DATA { <urn:healthy> <urn:p> 1 }")
            .unwrap();
        let healthy_id = healthy.dataset_id();
        drop(healthy);
        let doomed = catalog.create("doomed", &Default::default()).unwrap();
        doomed.update("INSERT DATA { <urn:d> <urn:p> 2 }").unwrap();
        drop(doomed);
        CRASH_AFTER.set(CRASH_AFTER_KEPT_RENAME);
        assert!(catalog.delete("doomed").is_err());
        CRASH_AFTER.set(0);
        drop(catalog);

        let intent = read_intents(dir.path()).unwrap().pop().unwrap();
        let databases = dir.path().join("databases");
        let replaced = databases.join(".replaced-doomed-42");
        drop(crate::store::Store::open(&replaced, Default::default()).unwrap());
        std::fs::write(
            replaced.join(super::super::restore::SWAP_MARKER),
            b"{\"format\":1,",
        )
        .unwrap();
        let kept = databases.join(intent.kept_name());
        let catalog = Catalog::open(dir.path(), Default::default()).unwrap();
        assert_eq!(
            catalog
                .get("healthy")
                .expect("healthy sibling must remain available")
                .dataset_id(),
            healthy_id
        );
        assert!(catalog.refusal("doomed").is_some());
        assert!(catalog.get("doomed").is_none());
        assert!(!databases.join("doomed").exists());
        assert!(kept.join("dataset.json").exists());
        assert!(replaced.join("dataset.json").exists());
        assert!(dir.path().join(INTENT_FILE).exists());
    }

    #[test]
    fn stale_forward_intent_cannot_reclaim_registered_datasets_only_copy() {
        let dir = tempfile::tempdir().unwrap();
        let intent = interrupted_delete(dir.path(), CRASH_AFTER_SAVE);
        super::super::write_registry(
            dir.path(),
            &super::super::Registry {
                datasets: vec![RegistryEntry {
                    name: intent.name.clone(),
                    kind: DatasetKind::Persistent,
                    id: None,
                    revision: None,
                    reasoning: None,
                }],
            },
        )
        .unwrap();
        let capture = EventCapture::default();
        let catalog = tracing::subscriber::with_default(capture.clone(), || {
            Catalog::open(dir.path(), Default::default()).unwrap()
        });
        assert!(catalog.refusal("doomed").is_some());
        assert!(
            dir.path()
                .join("databases")
                .join(intent.kept_name())
                .exists()
        );
        assert!(dir.path().join(INTENT_FILE).exists());
        assert!(capture.warnings().iter().any(|event| {
            event
                .fields
                .get("path")
                .is_some_and(|p| p.contains(&intent.kept_name()))
                && event.fields.contains_key("retained_bytes")
        }));
    }

    #[test]
    fn standalone_store_lease_blocks_startup_cleanup_until_released() {
        use crate::store::Store;

        for stage in [CRASH_AFTER_SAVE, CRASH_AFTER_KEPT_RENAME] {
            let dir = tempfile::tempdir().unwrap();
            let intent = interrupted_delete(dir.path(), stage);
            let name = intent.kept_name();
            let root = dir.path().join("databases").join(&name);
            let store = Store::open(&root, Default::default()).unwrap();
            assert_eq!(store.dataset_id(), intent.id);
            let capture = EventCapture::default();
            drop(tracing::subscriber::with_default(capture.clone(), || {
                Catalog::open(dir.path(), Default::default()).unwrap()
            }));
            assert!(root.join("dataset.json").exists());
            assert_eq!(store.snapshot().len(), 1);
            assert_eq!(read_intents(dir.path()).unwrap()[0].token, intent.token);
            assert!(
                capture.warnings().iter().any(|event| {
                    event.fields.get("path").is_some_and(|p| p.ends_with(&name))
                        && event
                            .fields
                            .get("reason")
                            .is_some_and(|reason| reason.contains("in use by another opener"))
                }),
                "{:?}",
                capture.warnings()
            );
            drop(store);
            let reopened = Catalog::open(dir.path(), Default::default()).unwrap();
            if stage == CRASH_AFTER_KEPT_RENAME {
                assert_eq!(reopened.get("doomed").unwrap().dataset_id(), intent.id);
            }
            assert!(!root.exists());
            assert!(!dir.path().join(INTENT_FILE).exists());
        }
    }

    #[test]
    fn staged_delete_releases_the_original_alias_before_partial_removal() {
        let dir = tempfile::tempdir().unwrap();
        let intent = interrupted_delete(dir.path(), CRASH_AFTER_SAVE);
        let databases = dir.path().join("databases");
        let kept = databases.join(intent.kept_name());
        RECOVERY_STOP.set(RecoveryStop::AfterMetadataRemoval);
        let capture = EventCapture::default();
        let catalog = tracing::subscriber::with_default(capture.clone(), || {
            Catalog::open(dir.path(), Default::default()).unwrap()
        });
        RECOVERY_STOP.set(RecoveryStop::None);
        assert!(!databases.join("doomed").exists());
        assert!(kept.exists() && !kept.join("dataset.json").exists());
        assert!(dir.path().join(INTENT_FILE).exists());
        assert!(capture.warnings().iter().any(|event| {
            event
                .fields
                .get("path")
                .is_some_and(|p| p.ends_with(&intent.kept_name()))
                && event.fields["retained_bytes"].parse::<u64>().unwrap() > 0
        }));
        let replacement = catalog.create("doomed", &Default::default()).unwrap();
        let replacement_id = replacement.dataset_id();
        drop(replacement);
        drop(catalog);

        let catalog = Catalog::open(dir.path(), Default::default()).unwrap();
        assert_eq!(catalog.get("doomed").unwrap().dataset_id(), replacement_id);
        assert!(!kept.exists());
        assert!(!dir.path().join(INTENT_FILE).exists());
    }

    #[test]
    fn retry_syncs_the_already_absent_cleanup_path_before_retiring_its_intent() {
        use super::super::take_catalog_dir_syncs;

        let dir = tempfile::tempdir().unwrap();
        let intent = interrupted_delete(dir.path(), CRASH_AFTER_SAVE);
        RECOVERY_STOP.set(RecoveryStop::BeforeRemovalSync);
        assert!(Catalog::open(dir.path(), Default::default()).is_err());
        RECOVERY_STOP.set(RecoveryStop::None);
        let databases = dir.path().join("databases");
        assert!(!databases.join(intent.kept_name()).exists());
        assert!(dir.path().join(INTENT_FILE).exists());

        take_catalog_dir_syncs();
        drop(Catalog::open(dir.path(), Default::default()).unwrap());
        assert!(!dir.path().join(INTENT_FILE).exists());
        let synced = take_catalog_dir_syncs();
        assert_eq!(
            synced,
            [
                dir.path().to_path_buf(),
                databases,
                dir.path().to_path_buf()
            ]
        );
    }

    #[cfg(unix)]
    #[test]
    fn unreadable_identity_is_retried_at_both_original_and_staged_paths() {
        use std::os::unix::fs::PermissionsExt;

        if unsafe { libc::geteuid() } == 0 {
            return;
        }

        for stage in [CRASH_AFTER_INTENT, CRASH_AFTER_SAVE] {
            let dir = tempfile::tempdir().unwrap();
            let intent = interrupted_delete(dir.path(), stage);
            let name = if stage == CRASH_AFTER_INTENT {
                intent.name.clone()
            } else {
                intent.kept_name()
            };
            let root = dir.path().join("databases").join(&name);
            let metadata = root.join("dataset.json");
            std::fs::set_permissions(&metadata, std::fs::Permissions::from_mode(0o000)).unwrap();
            let capture = EventCapture::default();
            drop(tracing::subscriber::with_default(capture.clone(), || {
                Catalog::open(dir.path(), Default::default()).unwrap()
            }));
            assert_eq!(read_intents(dir.path()).unwrap()[0].token, intent.token);
            assert!(capture.warnings().iter().any(|event| {
                event.fields.get("path").is_some_and(|p| p.ends_with(&name))
                    && event.fields["retained_bytes"].parse::<u64>().unwrap() > 0
            }));
            std::fs::set_permissions(&metadata, std::fs::Permissions::from_mode(0o600)).unwrap();
            let reopened = Catalog::open(dir.path(), Default::default()).unwrap();
            if stage == CRASH_AFTER_INTENT {
                assert_eq!(reopened.get("doomed").unwrap().dataset_id(), intent.id);
            } else {
                assert!(!root.exists());
            }
            assert!(!dir.path().join(INTENT_FILE).exists());
        }
    }

    #[cfg(unix)]
    #[test]
    fn incomplete_byte_counts_are_reported_for_inaccessible_directories() {
        use std::os::unix::fs::PermissionsExt;

        if unsafe { libc::geteuid() } == 0 {
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let intent = interrupted_delete(dir.path(), CRASH_AFTER_SAVE);
        let name = intent.kept_name();
        let root = dir.path().join("databases").join(&name);
        std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o000)).unwrap();
        let capture = EventCapture::default();
        let opened = tracing::subscriber::with_default(capture.clone(), || {
            Catalog::open(dir.path(), Default::default())
        });
        std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700)).unwrap();
        drop(opened.unwrap());
        assert_eq!(read_intents(dir.path()).unwrap()[0].token, intent.token);
        assert!(capture.warnings().iter().any(|event| {
            event.fields.get("path").is_some_and(|p| p.ends_with(&name))
                && event.fields["retained_bytes"] == "0"
                && event.fields["retained_bytes_complete"] == "false"
        }));
    }

    #[cfg(unix)]
    #[test]
    fn journal_and_database_parent_symlinks_are_refused_before_recovery() {
        use std::os::unix::fs::symlink;

        let dir = tempfile::tempdir().unwrap();
        let intent = interrupted_delete(dir.path(), CRASH_AFTER_INTENT);
        let journal = dir.path().join(INTENT_FILE);
        let other = tempfile::tempdir().unwrap();
        let target = other.path().join(INTENT_FILE);
        std::fs::rename(&journal, &target).unwrap();
        symlink(&target, &journal).unwrap();
        for dangling in [false, true] {
            if dangling {
                std::fs::rename(&target, target.with_extension("saved")).unwrap();
            }
            assert!(matches!(
                Catalog::open(dir.path(), Default::default()),
                Err(Error::Corrupt(_))
            ));
            assert!(dir.path().join("databases/doomed").exists());
            assert!(std::fs::symlink_metadata(&journal).unwrap().is_symlink());
        }
        std::fs::remove_file(&journal).unwrap();
        std::fs::rename(target.with_extension("saved"), &journal).unwrap();

        let databases = dir.path().join("databases");
        let external_db = other.path().join("databases");
        std::fs::rename(&databases, &external_db).unwrap();
        symlink(&external_db, &databases).unwrap();
        assert!(matches!(
            Catalog::open(dir.path(), Default::default()),
            Err(Error::Corrupt(_))
        ));
        assert!(external_db.join("doomed/dataset.json").exists());
        assert_eq!(read_intents(dir.path()).unwrap()[0].token, intent.token);
    }

    #[cfg(unix)]
    #[test]
    fn recovery_keeps_using_its_open_directory_after_path_replacement() {
        use std::os::unix::fs::symlink;

        let dir = tempfile::tempdir().unwrap();
        let intent = interrupted_delete(dir.path(), CRASH_AFTER_SAVE);
        let kept = intent.kept_name();
        let intents = read_intents(dir.path()).unwrap();
        let databases = open_databases(&RecoveryDir::open(dir.path()).unwrap()).unwrap();
        let original = dir.path().join("original");
        let alternate = dir.path().join("alternate");
        std::fs::rename(databases.path(), &original).unwrap();
        std::fs::create_dir_all(alternate.join(&kept)).unwrap();
        std::fs::copy(
            original.join(&kept).join("dataset.json"),
            alternate.join(&kept).join("dataset.json"),
        )
        .unwrap();
        symlink(&alternate, databases.path()).unwrap();

        let registry = read_registry(dir.path()).unwrap();
        recover(
            &RecoveryDir::open(dir.path()).unwrap(),
            &databases,
            &registry,
            intents,
            &Default::default(),
        )
        .unwrap();
        assert!(!original.join(&kept).exists());
        assert!(alternate.join(&kept).join("dataset.json").exists());
    }

    #[cfg(unix)]
    #[test]
    fn symlinked_dataset_identity_is_not_used_to_authorize_removal() {
        use std::os::unix::fs::symlink;

        let dir = tempfile::tempdir().unwrap();
        let intent = interrupted_delete(dir.path(), CRASH_AFTER_SAVE);
        let kept = dir.path().join("databases").join(intent.kept_name());
        let metadata = kept.join("dataset.json");
        let external = dir.path().join("outside-dataset.json");
        std::fs::rename(&metadata, &external).unwrap();
        symlink(&external, &metadata).unwrap();

        drop(Catalog::open(dir.path(), Default::default()).unwrap());
        assert!(external.exists());
        assert!(std::fs::symlink_metadata(&metadata).unwrap().is_symlink());
        assert!(dir.path().join(INTENT_FILE).exists());
    }

    #[test]
    fn journal_preflight_precedes_other_startup_recovery() {
        let dir = tempfile::tempdir().unwrap();
        let catalog = Catalog::open(dir.path(), Default::default()).unwrap();
        let dataset = catalog.create("active", &Default::default()).unwrap();
        let id = dataset.dataset_id();
        drop(dataset);
        drop(catalog);
        super::super::rename::prepare(dir.path(), "active", "alternate", id).unwrap();
        let databases = dir.path().join("databases");
        std::fs::rename(databases.join("active"), databases.join("alternate")).unwrap();
        let restore = databases.join(".restore-dataset-1");
        std::fs::create_dir(&restore).unwrap();
        let journal = dir.path().join(INTENT_FILE);
        for bytes in [br#"{"format":5,"future":true}"#.as_slice(), b"invalid json"] {
            std::fs::write(&journal, bytes).unwrap();
            assert!(Catalog::open(dir.path(), Default::default()).is_err());
            assert!(dir.path().join("rename.json").exists());
            assert!(databases.join("alternate").exists());
            assert!(!databases.join("active").exists());
            assert!(restore.exists());
        }
        std::fs::remove_file(&journal).unwrap();
        let catalog = Catalog::open(dir.path(), Default::default()).unwrap();
        assert_eq!(catalog.get("active").unwrap().dataset_id(), id);
        assert!(!dir.path().join("rename.json").exists());
        assert!(!restore.exists());
    }

    #[test]
    fn ambiguous_registry_identity_refuses_before_startup_recovery() {
        let dir = tempfile::tempdir().unwrap();
        let intent = interrupted_delete(dir.path(), CRASH_AFTER_SAVE);
        let databases = dir.path().join("databases");
        let kept = databases.join(intent.kept_name());
        let restore = databases.join(".restore-doomed-42");
        std::fs::create_dir(&restore).unwrap();
        let config = dir.path().join(super::super::CONFIG_FILE);
        let replacement = format!(
            r#"{{"name":"doomed","type":"persistent","id":"{}"}}"#,
            Uuid::new_v4()
        );
        let original = format!(
            r#"{{"name":"doomed","type":"persistent","id":"{}"}}"#,
            intent.id
        );
        let duplicate_id = original.replacen(
            r#""id":"#,
            &format!(r#""id":"{}","id":"#, Uuid::new_v4()),
            1,
        );
        for bytes in [
            format!(r#"{{"format":2,"datasets":[{replacement},{original}]}}"#),
            format!(r#"{{"format":2,"datasets":[{duplicate_id}]}}"#),
            String::from(r#"{"format":4,"datasets":[]}"#),
        ] {
            std::fs::write(&config, bytes).unwrap();
            assert!(Catalog::open(dir.path(), Default::default()).is_err());
            assert!(kept.exists());
            assert!(restore.exists());
            assert!(dir.path().join(INTENT_FILE).exists());
        }
    }

    #[test]
    fn ambiguous_journals_refuse_recovery_and_legacy_versions_remain_readable() {
        let dir = tempfile::tempdir().unwrap();
        let intent = interrupted_delete(dir.path(), CRASH_AFTER_SAVE);
        let kept = dir.path().join("databases").join(intent.kept_name());
        let journal = dir.path().join(INTENT_FILE);
        let record = format!(
            r#"{{"name":"doomed","id":"{}","token":"{}"}}"#,
            intent.id, intent.token
        );
        for bytes in [
            format!(r#"{{"format":5,"format":1,"intents":[{record}]}}"#),
            format!(r#"{{"format":1,"intents":[{record},{record}]}}"#),
            format!(
                r#"{{"format":1,"intents":[{}]}}"#,
                record.replace(r#""name":"doomed""#, r#""name":"foreign","name":"doomed""#)
            ),
            format!(
                r#"{{"format":1,"intents":[{}]}}"#,
                record.replace("doomed", "../escape")
            ),
            format!(r#"{{"format":1,"intents":[{record}],"intents":[{record}]}}"#),
            String::from(r#"{"format":0,"intents":[]}"#),
            String::from(r#"{"format":null,"intents":[]}"#),
        ] {
            std::fs::write(&journal, &bytes).unwrap();
            assert!(
                matches!(
                    Catalog::open(dir.path(), Default::default()),
                    Err(Error::Corrupt(_))
                ),
                "{bytes}"
            );
            assert!(kept.exists());
            assert_eq!(std::fs::read_to_string(&journal).unwrap(), bytes);
        }

        for version in [1, 2] {
            let mut record: serde_json::Value = serde_json::from_str(&record).unwrap();
            if version == 2 {
                record["state"] = "deleting".into();
            }
            std::fs::write(
                &journal,
                serde_json::to_vec(&serde_json::json!({
                    "format": version, "intents": [record]
                }))
                .unwrap(),
            )
            .unwrap();
            drop(Catalog::open(dir.path(), Default::default()).unwrap());
            assert!(kept.exists());
            assert_eq!(
                read_intents(dir.path()).unwrap()[0].state,
                IntentState::Legacy
            );
        }
    }

    #[test]
    fn startup_retires_only_crashed_journal_temporary_files() {
        let dir = tempfile::tempdir().unwrap();
        interrupted_delete(dir.path(), CRASH_AFTER_INTENT);
        let temp = dir
            .path()
            .join(INTENT_FILE)
            .with_extension(format!("tmp-{}", Uuid::new_v4()));
        sparkles_core::fsutil::write_synced(&temp, br#"{"format":5,"future":true}"#).unwrap();
        let foreign = dir.path().join("delete.tmp-manual");
        std::fs::write(&foreign, b"operator file").unwrap();
        #[cfg(unix)]
        let link = {
            let link = dir
                .path()
                .join(INTENT_FILE)
                .with_extension(format!("tmp-{}", Uuid::new_v4()));
            std::os::unix::fs::symlink(&foreign, &link).unwrap();
            link
        };
        let catalog = Catalog::open(dir.path(), Default::default()).unwrap();
        assert!(catalog.get("doomed").is_some());
        assert!(!temp.exists());
        assert!(foreign.exists());
        #[cfg(unix)]
        assert!(std::fs::symlink_metadata(&link).unwrap().is_symlink());
    }

    #[cfg(feature = "backup")]
    #[test]
    fn actual_keep_replaced_copy_stays_recognized_after_rename() {
        use crate::store::Store;

        let dir = tempfile::tempdir().unwrap();
        let catalog = Catalog::open(dir.path(), Default::default()).unwrap();
        let original = catalog.create("ds", &Default::default()).unwrap();
        let original_id = original.dataset_id();
        drop(original);
        let databases = dir.path().join("databases");
        let staged = databases.join(".restore-ds-42");
        drop(Store::open(&staged, Default::default()).unwrap());
        drop(super::super::swap::replace_in_place(&catalog, "ds", &staged, "42", true).unwrap());
        drop(catalog.rename("ds", "renamed").unwrap());
        drop(catalog);

        let capture = EventCapture::default();
        let catalog = tracing::subscriber::with_default(capture.clone(), || {
            Catalog::open(dir.path(), Default::default()).unwrap()
        });
        assert!(catalog.get("renamed").is_some());
        let backup = Store::open(&databases.join(".kept-ds-42"), Default::default()).unwrap();
        assert_eq!(backup.dataset_id(), original_id);
        assert!(capture.warnings().is_empty(), "{:?}", capture.warnings());
    }

    #[test]
    fn clearing_one_delete_leaves_an_overlapping_intent_recoverable() {
        use super::super::{DELETE_FAIL, DELETE_HOOK, DELETE_TRASH_RENAME};

        let dir = tempfile::tempdir().unwrap();
        let catalog = Catalog::open(dir.path(), Default::default()).unwrap();
        drop(catalog.create("one", &Default::default()).unwrap());
        drop(catalog.create("two", &Default::default()).unwrap());
        let other = catalog.clone();
        let root = dir.path().to_path_buf();
        DELETE_HOOK.set(Some(Box::new(move || {
            std::thread::scope(|scope| {
                scope
                    .spawn(|| {
                        CRASH_AFTER.set(CRASH_AFTER_SAVE);
                        assert!(other.delete("two").is_err());
                    })
                    .join()
                    .unwrap();
            });
            assert_eq!(read_intents(&root).unwrap().len(), 2);
        })));
        DELETE_FAIL.set(1 << DELETE_TRASH_RENAME);
        assert!(catalog.delete("one").unwrap());
        DELETE_HOOK.set(None);
        DELETE_FAIL.set(0);
        drop(catalog);

        let intents = read_intents(dir.path()).unwrap();
        assert_eq!(intents.len(), 1);
        assert_eq!(intents[0].name, "two");
        drop(Catalog::open(dir.path(), Default::default()).unwrap());
        assert!(!dir.path().join(INTENT_FILE).exists());
        assert!(kept_entries(dir.path()).is_empty());
    }
}
