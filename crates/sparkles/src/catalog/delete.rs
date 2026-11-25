//! Recover managed persistent deletes under catalog and dataset process exclusion.
//! The source is staged and synchronized before removing its registry row. The
//! durable registry decides whether startup restores the source or reclaims it.

use super::delete_fs::{Kind, RecoveryDir};
use super::{
    DatasetKind, KEPT_PREFIX, Refusal, Registry, RegistryEntry, TRASH_PREFIX, check_name,
    read_registry, sync_dir, write_file_atomic, write_registry,
};
use crate::{Error, Result};
use serde::{Deserialize, Serialize};
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
    Legacy,
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

    fn trash_name(&self) -> String {
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
    let catalog = RecoveryDir::open(dir)?;
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
        let intent: DeleteIntent = match format {
            1 => parse_object::<LegacyIntent>(bytes)?.into(),
            2 => parse_object::<LegacyV2Intent>(bytes)?.into(),
            _ => parse_object(bytes)?,
        };
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
            Ok(()) => sync_dir(dir),
            Err(error) if error.kind() == ErrorKind::NotFound => Ok(()),
            Err(error) => Err(error.into()),
        }
    } else {
        #[derive(Serialize)]
        struct Records<'a> {
            intents: &'a [DeleteIntent],
        }
        let bytes = sparkles_core::formats::settings_to_vec(INTENT_FILE, &Records { intents })?;
        write_file_atomic(&path, &bytes)
    }
}

pub(super) fn check_prepare(dir: &Path) -> Result<()> {
    read_intents(dir).map(|_| ())
}

/// Older registries have no identity. Persist it before staging so startup
/// can distinguish this row from any later registration of the same alias.
pub(super) fn bind_registry_identity(dir: &Path, name: &str, id: Uuid) -> Result<()> {
    let mut registry = read_registry(dir)?;
    let entry = registry
        .datasets
        .iter_mut()
        .find(|entry| entry.name == name && entry.kind == DatasetKind::Persistent)
        .ok_or_else(|| Error::Corrupt(format!("dataset /{name} has no persistent registry row")))?;
    if entry.id != Some(id) {
        entry.id = Some(id);
        write_registry(dir, &registry)?;
    }
    Ok(())
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
pub(super) fn open_databases(dir: &Path) -> Result<RecoveryDir> {
    let catalog = RecoveryDir::open(dir)?;
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

fn dataset_id(dir: &RecoveryDir) -> Result<Option<Uuid>> {
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

fn child_if_present(parent: &RecoveryDir, name: &OsStr) -> io::Result<Option<RecoveryDir>> {
    match parent.child(name) {
        Ok(dir) => Ok(Some(dir)),
        Err(error) if error.kind() == ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error),
    }
}

fn lock_dataset(dir: &RecoveryDir) -> Result<sparkles_core::store::ProcessLock> {
    let lock = dir.open_file(OsStr::new("sparkles.lock"), true)?;
    match lock.try_lock() {
        Ok(()) => {
            let lock = sparkles_core::store::ProcessLock::from_locked_file(lock);
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

fn warn_retained(
    parent: &RecoveryDir,
    name: &OsStr,
    dataset: Option<&str>,
    reason: impl std::fmt::Display,
) {
    let bytes = retained_bytes(parent, name);
    tracing::warn!(
        target: "sparkles::catalog",
        path = %parent.path().join(name).display(),
        dataset,
        retained_bytes = bytes.count,
        retained_bytes_complete = !bytes.incomplete,
        %reason,
        "startup delete recovery retained this entry"
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
fn lock_candidate(
    candidate: &RecoveryDir,
    id: Uuid,
    staged: bool,
) -> Result<sparkles_core::store::ProcessLock> {
    require_identity(candidate, id, staged)?;
    let lock = lock_dataset(candidate)?;
    require_identity(candidate, id, staged)?;
    Ok(lock)
}

/// Move a verified closed store under its process lease without replacing a
/// destination. The caller establishes the fsync boundary for the move.
pub(super) fn move_verified(
    parent: &Path,
    from: &str,
    to: &str,
    id: Uuid,
) -> Result<sparkles_core::store::ProcessLock> {
    let parent = RecoveryDir::open(parent)?;
    let from = OsStr::new(from);
    let candidate = parent.child(from)?;
    let lock = lock_candidate(&candidate, id, false)?;
    parent.rename_child(from, candidate, OsStr::new(to))?;
    Ok(lock)
}

/// Keep the recovery evidence from either operation until an operator resolves
/// two staged candidates for a missing registered root.
#[cfg(feature = "backup")]
pub(super) fn restore_conflicts(
    databases: &RecoveryDir,
    intents: &[DeleteIntent],
) -> Result<BTreeSet<String>> {
    let uncertain: BTreeSet<_> = intents
        .iter()
        .filter(|intent| {
            let missing = |name: &str| {
                databases
                    .entry(OsStr::new(name))
                    .is_err_and(|error| error.kind() == ErrorKind::NotFound)
            };
            let live = databases.entry(OsStr::new(&intent.name));
            !live.is_ok_and(|entry| entry.kind == Kind::Directory)
                && (!missing(&intent.kept_name()) || !missing(&intent.trash_name()))
        })
        .map(|intent| intent.name.as_str())
        .collect();
    let mut conflicts = BTreeSet::new();
    if !uncertain.is_empty() {
        for name in databases.names()? {
            let name = name?;
            if let Some(dataset) = name.to_str().and_then(super::restore::replaced_dataset)
                && uncertain.contains(dataset)
            {
                conflicts.insert(dataset.into());
            }
        }
    }
    Ok(conflicts)
}

fn finish_delete(
    databases: &RecoveryDir,
    intent: &DeleteIntent,
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

    if let Some(registered) = registered {
        let live = child_if_present(databases, name)?;
        // A replacement recorded after this delete is authoritative, even
        // if its root is now missing. A live root must also agree under lock.
        if let Some(id) = registered.id.filter(|id| *id != intent.id) {
            if let Some(live) = live {
                let _live_lock = lock_dataset(&live)?;
                require_identity(&live, id, false)?;
            }
        } else {
            match (registered.id, live, candidate) {
                (_, Some(live), None) => {
                    let _lock = lock_candidate(&live, intent.id, false)?;
                    return Ok(());
                }
                (Some(_), None, Some(kept)) if staged == kept_name => {
                    let _lock = lock_candidate(&kept, intent.id, false)?;
                    databases.rename_child(kept_name, kept, name)?;
                    *warning_name = name.into();
                    databases.sync()?;
                    return Ok(());
                }
                _ => {
                    return Err(Error::Corrupt(
                        "could not verify one original root for the registered delete".into(),
                    ));
                }
            }
        }
    }

    let Some(candidate) = candidate else {
        if registered.is_none() && child_if_present(databases, name)?.is_some() {
            return Err(Error::Corrupt(
                "an unstaged directory remains after registry removal".into(),
            ));
        }
        return Ok(());
    };
    let _lock = lock_candidate(&candidate, intent.id, true)?;
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
    databases.remove_child(staged, &candidate)?;
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
    dir: &Path,
    databases: &RecoveryDir,
    registry: &Registry,
    intents: Vec<DeleteIntent>,
    restore_conflicts: &BTreeSet<String>,
) -> Result<BTreeMap<String, Refusal>> {
    let catalog = RecoveryDir::open(dir)?;
    clean_temps(&catalog)?;
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
    for intent in intents {
        let mut warning_name = OsString::from(intent.kept_name());
        let registered = registry
            .datasets
            .iter()
            .find(|entry| entry.name == intent.name && entry.kind == DatasetKind::Persistent);
        let result = if restore_conflicts.contains(&intent.name) {
            Err(Error::Corrupt(
                "both a staged restore and a staged delete may own the missing root".into(),
            ))
        } else {
            finish_delete(databases, &intent, registered, &mut warning_name)
        };
        if let Err(error) = result {
            if intent.state == IntentState::Prepared
                && registered.is_some_and(|entry| entry.id == Some(intent.id))
            {
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
    if retained.len() != count {
        // Retiring even an already absent cleanup path requires its parent sync.
        databases.sync()?;
        write_intents(dir, &retained)?;
    }

    for name in databases.names()? {
        let name = name?;
        let text = name.to_string_lossy();
        if text.starts_with(TRASH_PREFIX) && !tracked_trash.contains(text.as_ref()) {
            let result = (|| -> Result<()> {
                let Some(candidate) = child_if_present(databases, &name)? else {
                    return Ok(());
                };
                let _lock = lock_dataset(&candidate)?;
                databases.remove_child(&name, &candidate)?;
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
        assert_eq!(intent_raw["format"], 3);
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
                &open_databases(dir.path()).unwrap(),
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
            br#"{"format": 4, "future": true}"#,
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
            br#"{"format": 4, "future": true}"#,
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
            assert!(
                result
                    .unwrap_err()
                    .to_string()
                    .contains("indeterminate commit")
            );
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

    #[cfg(feature = "backup")]
    #[test]
    fn staged_restore_and_delete_conflict_keeps_both_copies() {
        let dir = tempfile::tempdir().unwrap();
        let intent = interrupted_delete(dir.path(), CRASH_AFTER_KEPT_RENAME);
        let databases = dir.path().join("databases");
        let replaced = databases.join(".replaced-doomed-42");
        drop(crate::store::Store::open(&replaced, Default::default()).unwrap());
        let kept = databases.join(intent.kept_name());
        let catalog = Catalog::open(dir.path(), Default::default()).unwrap();
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
        let databases = open_databases(dir.path()).unwrap();
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
            dir.path(),
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
        for bytes in [br#"{"format":4,"future":true}"#.as_slice(), b"invalid json"] {
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
        #[cfg(feature = "backup")]
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
            String::from(r#"{"format":3,"datasets":[]}"#),
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
            format!(r#"{{"format":4,"format":1,"intents":[{record}]}}"#),
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
        sparkles_core::fsutil::write_synced(&temp, br#"{"format":4,"future":true}"#).unwrap();
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
