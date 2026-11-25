//! Recover managed persistent deletes under catalog and dataset process exclusion.
//! A forward intent uses the registry as its commit point. Rollback first revokes
//! that authorization durably so startup can never reclaim a rollback copy.

use super::delete_fs::{Kind, RecoveryDir};
use super::{
    DatasetKind, KEPT_PREFIX, Registry, RegistryEntry, TRASH_PREFIX, check_name, read_registry,
    sync_dir, write_file_atomic, write_registry,
};
use crate::reasoning::ReasoningRecord;
use crate::{Error, Result};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
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
pub(super) const CRASH_AFTER_FAILED_ROLLBACK_MOVE: u8 = 5;
#[cfg(test)]
pub(super) const CRASH_AFTER_FAILED_ROLLBACK_SYNC: u8 = 6;
#[cfg(test)]
pub(super) const CRASH_AFTER_ROLLBACK_MOVE_SYNC: u8 = 7;
#[cfg(test)]
pub(super) const CRASH_AFTER_SAVE_ROLLBACK_MARK: u8 = 8;

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
    Deleting,
    RollingBack,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct DeleteIntent {
    pub(super) name: String,
    pub(super) id: Uuid,
    pub(super) token: Uuid,
    pub(super) state: IntentState,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) reasoning: Option<ReasoningRecord>,
}

impl DeleteIntent {
    fn kept_name(&self) -> String {
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
            state: IntentState::Deleting,
            reasoning: None,
        }
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
        let intent: DeleteIntent = if format == 1 {
            parse_object::<LegacyIntent>(bytes)?.into()
        } else {
            parse_object(bytes)?
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

pub(super) fn record(dir: &Path, intent: &DeleteIntent) -> Result<()> {
    check_name(&intent.name)?;
    let mut intents = read_intents(dir)?;
    if intents.iter().any(|entry| entry.token == intent.token) {
        return Err(corrupt(format!("duplicate delete token {}", intent.token)));
    }
    // A new explicit delete of the same dataset supersedes an unfinished rollback.
    intents.retain(|entry| {
        !(entry.state == IntentState::RollingBack
            && entry.name == intent.name
            && entry.id == intent.id)
    });
    intents.push(intent.clone());
    write_intents(dir, &intents)
}

pub(super) fn mark_rolling_back(dir: &Path, token: Uuid) -> Result<()> {
    let mut intents = read_intents(dir)?;
    let intent = intents
        .iter_mut()
        .find(|entry| entry.token == token)
        .ok_or_else(|| corrupt(format!("missing delete token {token}")))?;
    intent.state = IntentState::RollingBack;
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

fn finish_delete(
    databases: &RecoveryDir,
    intent: &DeleteIntent,
    registered: bool,
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
    let (mut staged, candidate) = match (kept_dir, trash_dir) {
        (Some(_), Some(_)) => return Err(Error::Corrupt("both staged delete paths exist".into())),
        (Some(candidate), None) => (kept_name, Some(candidate)),
        (None, trash_dir) => (trash_name, trash_dir),
    };
    *warning_name = staged.into();
    if candidate.is_some() && registered {
        // If the registered root is missing or holds the same UUID, the kept
        // path may be its only copy. An uncertain trash path needs its intent
        // to prevent the generic trash scan from claiming it on a later start.
        match child_if_present(databases, name)? {
            Some(live) if dataset_id(&live)?.is_some_and(|id| id != intent.id) => {}
            _ if staged == trash_name => {
                return Err(Error::Corrupt(
                    "trash may belong to a registered dataset".into(),
                ));
            }
            _ => return Ok(()),
        }
    }
    let _lock;
    let candidate = match candidate {
        Some(candidate) => {
            _lock = lock_candidate(&candidate, intent.id, true)?;
            // A restart may see a rename whose parent sync never completed.
            databases.sync()?;
            candidate
        }
        None if registered => return Ok(()),
        None => {
            *warning_name = name.into();
            let Some(live) = child_if_present(databases, name)? else {
                return Ok(());
            };
            _lock = lock_candidate(&live, intent.id, false)?;
            staged = OsStr::new(&kept);
            let kept = databases.rename_child(name, live, staged)?;
            *warning_name = staged.into();
            databases.sync()?;
            kept
        }
    };
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

fn finish_rollback(
    dir: &Path,
    databases: &RecoveryDir,
    intent: &DeleteIntent,
    registry: &mut Registry,
    warning_name: &mut OsString,
) -> Result<()> {
    if registry
        .datasets
        .iter()
        .any(|entry| entry.name == intent.name)
    {
        return Ok(());
    }

    let kept = intent.kept_name();
    let (name, original) = match child_if_present(databases, OsStr::new(&kept))? {
        Some(kept_dir) => (kept.as_str(), Some(kept_dir)),
        None => {
            *warning_name = intent.name.as_str().into();
            (
                intent.name.as_str(),
                child_if_present(databases, OsStr::new(&intent.name))?,
            )
        }
    };
    let original =
        original.ok_or_else(|| Error::Corrupt("rollback directory is missing".into()))?;
    require_identity(&original, intent.id, false)?;
    if name == kept
        && let Some(live) = child_if_present(databases, OsStr::new(&intent.name))?
    {
        require_identity(&live, intent.id, false)?;
    }
    databases.sync()?;
    registry.datasets.push(RegistryEntry {
        name: intent.name.clone(),
        kind: DatasetKind::Persistent,
        reasoning: intent.reasoning.clone(),
    });
    registry
        .datasets
        .sort_by(|left, right| left.name.cmp(&right.name));
    if let Err(error) = write_registry(dir, registry) {
        registry.datasets.retain(|entry| entry.name != intent.name);
        return Err(error);
    }
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
    intents: Vec<DeleteIntent>,
) -> Result<()> {
    let catalog = RecoveryDir::open(dir)?;
    clean_temps(&catalog)?;
    let mut registry = read_registry(dir)?;
    // A prior process may have made a registry rename visible without syncing it.
    // Make the registry decision durable before it can authorize removing bytes.
    if !intents.is_empty() {
        catalog.sync()?;
    }
    let mut retained = Vec::new();
    let mut warned = BTreeSet::new();
    let tracked_trash: BTreeSet<_> = intents.iter().map(DeleteIntent::trash_name).collect();
    let count = intents.len();
    for intent in intents {
        let mut warning_name = OsString::from(intent.kept_name());
        let result = match intent.state {
            IntentState::Deleting => finish_delete(
                databases,
                &intent,
                registry.datasets.iter().any(|entry| {
                    entry.name == intent.name && entry.kind == DatasetKind::Persistent
                }),
                &mut warning_name,
            ),
            IntentState::RollingBack => {
                finish_rollback(dir, databases, &intent, &mut registry, &mut warning_name)
            }
        };
        if let Err(error) = result {
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
    Ok(())
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

        CRASH_AFTER.set(CRASH_AFTER_KEPT_RENAME);
        let err = cat.delete("my-doomed");
        CRASH_AFTER.set(0);
        assert!(err.is_err());
        drop(cat);

        // Verify that the journal and staged bytes survived removal from the registry.
        let intent_raw: serde_json::Value =
            serde_json::from_slice(&std::fs::read(dir.path().join(INTENT_FILE)).unwrap()).unwrap();
        assert_eq!(intent_raw["format"], 2);
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
    fn interrupted_delete_after_save_before_move_removes_unregistered_root() {
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

        assert!(dir.path().join("databases/doomed").exists());
        assert!(dir.path().join(INTENT_FILE).exists());

        let reopened = Catalog::open(dir.path(), Default::default()).unwrap();
        assert!(!dir.path().join("databases/doomed").exists());
        assert!(!dir.path().join(INTENT_FILE).exists());
        assert_eq!(reopened.get("active").unwrap().dataset_id(), active_id);
        drop(reopened.create("doomed", &Default::default()).unwrap());
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

        CRASH_AFTER.set(CRASH_AFTER_KEPT_RENAME);
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
                state: IntentState::Deleting,
                reasoning: None,
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
        CRASH_AFTER.set(CRASH_AFTER_KEPT_RENAME);
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

        CRASH_AFTER.set(CRASH_AFTER_KEPT_RENAME);
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
            br#"{"format": 3, "future": true}"#,
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
            br#"{"format": 3, "future": true}"#,
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
        CRASH_AFTER.set(CRASH_AFTER_KEPT_RENAME);
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
    fn interrupted_rollbacks_keep_the_original_dataset_registered() {
        use super::super::{
            DELETE_BEFORE_MOVE, DELETE_FAIL, DELETE_ROLLBACK_MOVE, DELETE_ROLLBACK_REGISTRY_SAVE,
            DELETE_ROLLBACK_SYNC, DELETE_SAVE_SYNC, DELETE_STAGE_SYNC,
        };
        use crate::store::Store;

        for (failure, stage, kept) in [
            (
                1 << DELETE_BEFORE_MOVE,
                CRASH_AFTER_FAILED_FORWARD_MOVE,
                false,
            ),
            (
                (1 << DELETE_STAGE_SYNC) | (1 << DELETE_ROLLBACK_MOVE),
                CRASH_AFTER_FAILED_ROLLBACK_MOVE,
                true,
            ),
            (
                (1 << DELETE_STAGE_SYNC) | (1 << DELETE_ROLLBACK_SYNC),
                CRASH_AFTER_FAILED_ROLLBACK_SYNC,
                false,
            ),
            (
                1 << DELETE_STAGE_SYNC,
                CRASH_AFTER_ROLLBACK_MOVE_SYNC,
                false,
            ),
            (1 << DELETE_SAVE_SYNC, CRASH_AFTER_SAVE_ROLLBACK_MARK, false),
            (
                (1 << DELETE_STAGE_SYNC) | (1 << DELETE_ROLLBACK_REGISTRY_SAVE),
                0,
                false,
            ),
        ] {
            let dir = tempfile::tempdir().unwrap();
            DELETE_FAIL.set(failure);
            let intent = interrupted_delete(dir.path(), stage);
            DELETE_FAIL.set(0);
            assert_eq!(intent.state, IntentState::RollingBack);
            assert!(read_registry(dir.path()).unwrap().datasets.is_empty());

            let capture = EventCapture::default();
            let catalog = tracing::subscriber::with_default(capture.clone(), || {
                Catalog::open(dir.path(), Default::default()).unwrap()
            });
            assert!(!dir.path().join(INTENT_FILE).exists());
            if kept {
                assert!(catalog.get("doomed").is_none());
                assert!(catalog.refusal("doomed").is_some());
                assert!(catalog.create("doomed", &Default::default()).is_err());
                let path = dir.path().join("databases").join(intent.kept_name());
                let store = Store::open(&path, Default::default()).unwrap();
                assert_eq!(store.dataset_id(), intent.id);
                assert_eq!(store.snapshot().len(), 1);
                assert!(capture.warnings().iter().any(|event| {
                    event
                        .fields
                        .get("path")
                        .is_some_and(|p| p.contains(&intent.kept_name()))
                        && event.fields.contains_key("retained_bytes")
                }));
            } else {
                let dataset = catalog.get("doomed").unwrap();
                assert_eq!(dataset.dataset_id(), intent.id);
                assert_eq!(dataset.len(), 1);
            }
        }
    }

    #[test]
    fn stale_forward_intent_cannot_reclaim_registered_datasets_only_copy() {
        let dir = tempfile::tempdir().unwrap();
        let intent = interrupted_delete(dir.path(), CRASH_AFTER_KEPT_RENAME);
        write_registry(
            dir.path(),
            &Registry {
                datasets: vec![RegistryEntry {
                    name: intent.name.clone(),
                    kind: DatasetKind::Persistent,
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
        assert!(!dir.path().join(INTENT_FILE).exists());
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
            let name = if stage == CRASH_AFTER_SAVE {
                intent.name.clone()
            } else {
                intent.kept_name()
            };
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
            drop(Catalog::open(dir.path(), Default::default()).unwrap());
            assert!(!root.exists());
            assert!(!dir.path().join(INTENT_FILE).exists());
        }
    }

    #[test]
    fn restart_stages_the_original_root_before_partial_removal() {
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
        let intent = interrupted_delete(dir.path(), CRASH_AFTER_KEPT_RENAME);
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

        for stage in [CRASH_AFTER_SAVE, CRASH_AFTER_KEPT_RENAME] {
            let dir = tempfile::tempdir().unwrap();
            let intent = interrupted_delete(dir.path(), stage);
            let name = if stage == CRASH_AFTER_SAVE {
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
            drop(Catalog::open(dir.path(), Default::default()).unwrap());
            assert!(!root.exists());
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
        let intent = interrupted_delete(dir.path(), CRASH_AFTER_KEPT_RENAME);
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
        let intent = interrupted_delete(dir.path(), CRASH_AFTER_SAVE);
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
        interrupted_delete(dir.path(), CRASH_AFTER_SAVE);
        let intents = read_intents(dir.path()).unwrap();
        let databases = open_databases(dir.path()).unwrap();
        let original = dir.path().join("original");
        let alternate = dir.path().join("alternate");
        std::fs::rename(databases.path(), &original).unwrap();
        std::fs::create_dir_all(alternate.join("doomed")).unwrap();
        std::fs::copy(
            original.join("doomed/dataset.json"),
            alternate.join("doomed/dataset.json"),
        )
        .unwrap();
        symlink(&alternate, databases.path()).unwrap();

        recover(dir.path(), &databases, intents).unwrap();
        assert!(!original.join("doomed").exists());
        assert!(alternate.join("doomed/dataset.json").exists());
    }

    #[cfg(unix)]
    #[test]
    fn symlinked_dataset_identity_is_not_used_to_authorize_removal() {
        use std::os::unix::fs::symlink;

        let dir = tempfile::tempdir().unwrap();
        let intent = interrupted_delete(dir.path(), CRASH_AFTER_KEPT_RENAME);
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
        for bytes in [br#"{"format":3,"future":true}"#.as_slice(), b"invalid json"] {
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
    fn ambiguous_journals_refuse_recovery_and_format_one_still_reads() {
        let dir = tempfile::tempdir().unwrap();
        let intent = interrupted_delete(dir.path(), CRASH_AFTER_KEPT_RENAME);
        let kept = dir.path().join("databases").join(intent.kept_name());
        let journal = dir.path().join(INTENT_FILE);
        let record = format!(
            r#"{{"name":"doomed","id":"{}","token":"{}"}}"#,
            intent.id, intent.token
        );
        for bytes in [
            format!(r#"{{"format":3,"format":1,"intents":[{record}]}}"#),
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

        std::fs::write(&journal, format!(r#"{{"format":1,"intents":[{record}]}}"#)).unwrap();
        drop(Catalog::open(dir.path(), Default::default()).unwrap());
        assert!(!kept.exists());
        assert!(!journal.exists());
    }

    #[test]
    fn startup_retires_only_crashed_journal_temporary_files() {
        let dir = tempfile::tempdir().unwrap();
        interrupted_delete(dir.path(), CRASH_AFTER_INTENT);
        let temp = dir
            .path()
            .join(INTENT_FILE)
            .with_extension(format!("tmp-{}", Uuid::new_v4()));
        sparkles_core::fsutil::write_synced(&temp, br#"{"format":3,"future":true}"#).unwrap();
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
                        CRASH_AFTER.set(CRASH_AFTER_KEPT_RENAME);
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
