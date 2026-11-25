//! Crash-safe recovery of interrupted catalog dataset deletes and startup reporting of
//! retained `.kept-*` entries under `databases/`.
//!
//! Before a managed persistent dataset is removed from `config.json` and moved aside to
//! `databases/.kept-<name>-<token>`, [`record`] persists a versioned intent in
//! `<dir>/delete.json`. On startup, [`recover`] uses `config.json` as the commit point:
//! * If the dataset is absent from `config.json` (or `config.json` now registers a
//!   different dataset UUID at `databases/<name>`), a matching non-symlink
//!   `databases/.kept-<name>-<token>` directory is removed and its disk bytes are
//!   reclaimed.
//! * If a crash occurred after `config.json` dropped the dataset and before the directory
//!   was renamed aside, a non-symlink `databases/<name>` whose `dataset.json` matches the
//!   recorded UUID is removed.
//! * If the dataset is still registered with the same identity (or its root is missing
//!   after a failed rollback), the retained bytes are left untouched and the stale intent
//!   is cleared.
//! * Any remaining `.kept-*` entry that is not an operator-requested `keepReplaced` copy
//!   of a live registered dataset, or any intent-backed entry whose removal fails, is left
//!   in place without failing startup and reported with a structured warning naming the
//!   path and retained bytes. Symlinks are never followed or removed.

use super::{DatasetKind, KEPT_PREFIX, check_name, read_registry, sync_dir, write_file_atomic};
use crate::{Error, Result};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use std::ffi::OsString;
use std::path::Path;
use uuid::Uuid;

/// The versioned catalog delete-intent file in the catalog root directory.
pub(super) const INTENT_FILE: &str = "delete.json";

/// Crash right after recording `delete.json`, before `config.json` removes the dataset.
pub(super) const CRASH_AFTER_INTENT: u8 = 1;
/// Crash right after `config.json` removes the dataset, before moving `databases/<name>` aside.
pub(super) const CRASH_AFTER_SAVE: u8 = 2;
/// Crash right after renaming `databases/<name>` to `databases/.kept-<name>-<token>` and syncing.
#[cfg(test)]
pub(super) const CRASH_AFTER_KEPT_RENAME: u8 = 3;

#[cfg(test)]
thread_local! {
    static CRASH_AFTER: std::cell::Cell<u8> = const { std::cell::Cell::new(0) };
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

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct DeleteIntent {
    pub(super) name: String,
    pub(super) id: Uuid,
    pub(super) token: Uuid,
}

impl DeleteIntent {
    pub(super) fn kept_name(&self) -> String {
        format!("{KEPT_PREFIX}{}-{}", self.name, self.token)
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct IntentFile {
    intents: Vec<DeleteIntent>,
}

/// Read and validate `<dir>/delete.json`. Missing returns an empty list; an unknown
/// newer `format` returns [`Error::Unsupported`]; missing `format`, malformed JSON,
/// unknown fields or invalid dataset names return [`Error::Corrupt`].
fn read_intents(dir: &Path) -> Result<Vec<DeleteIntent>> {
    let bytes = match std::fs::read(dir.join(INTENT_FILE)) {
        Ok(bytes) => bytes,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(e.into()),
    };
    let mut value: serde_json::Value = serde_json::from_slice(&bytes)
        .map_err(|e| Error::Corrupt(format!("{INTENT_FILE}: {e}")))?;
    let object = value
        .as_object_mut()
        .ok_or_else(|| Error::Corrupt(format!("{INTENT_FILE}: expected a JSON object")))?;
    if !object.contains_key("format") || object["format"].is_null() {
        return Err(Error::Corrupt(format!(
            "{INTENT_FILE}: missing format version"
        )));
    }
    sparkles_core::formats::check_settings(INTENT_FILE, &value)?;
    if let Some(object) = value.as_object_mut() {
        object.remove("format");
    }
    let parsed: IntentFile =
        serde_json::from_value(value).map_err(|e| Error::Corrupt(format!("{INTENT_FILE}: {e}")))?;
    for intent in &parsed.intents {
        check_name(&intent.name).map_err(|e| Error::Corrupt(format!("{INTENT_FILE}: {e}")))?;
    }
    Ok(parsed.intents)
}

/// Persist `intents` atomically to `<dir>/delete.json`, or remove the file when empty.
fn write_intents(dir: &Path, intents: &[DeleteIntent]) -> Result<()> {
    let path = dir.join(INTENT_FILE);
    if intents.is_empty() {
        match std::fs::remove_file(&path) {
            Ok(()) => sync_dir(dir),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(e.into()),
        }
    } else {
        let file = IntentFile {
            intents: intents.to_vec(),
        };
        let bytes = sparkles_core::formats::settings_to_vec(INTENT_FILE, &file)?;
        write_file_atomic(&path, &bytes)
    }
}

/// Verify that an existing `delete.json`, if any, is readable and of a supported version
/// before a delete closes its dataset.
pub(super) fn check_prepare(dir: &Path) -> Result<()> {
    let _ = read_intents(dir)?;
    Ok(())
}

/// Record `intent` durably in `<dir>/delete.json` before saving `config.json` or moving
/// the dataset directory aside.
pub(super) fn record(dir: &Path, intent: &DeleteIntent) -> Result<()> {
    check_name(&intent.name)?;
    let mut intents = read_intents(dir)?;
    if !intents
        .iter()
        .any(|existing| existing.token == intent.token)
    {
        intents.push(intent.clone());
    }
    write_intents(dir, &intents)
}

/// Remove the intent identified by `token` from `<dir>/delete.json`, deleting the file
/// when no intents remain.
pub(super) fn clear(dir: &Path, token: Uuid) -> Result<()> {
    let mut intents = read_intents(dir)?;
    let before = intents.len();
    intents.retain(|intent| intent.token != token);
    if intents.len() != before || intents.is_empty() {
        write_intents(dir, &intents)?;
    }
    Ok(())
}

/// The dataset name of a `keepReplaced` directory `.kept-<ds>-<digits>`.
fn keep_replaced_dataset(file_name: &str) -> Option<&str> {
    let rest = file_name.strip_prefix(KEPT_PREFIX)?;
    let (ds, task) = rest.rsplit_once('-')?;
    (!ds.is_empty() && !task.is_empty() && task.bytes().all(|b| b.is_ascii_digit())).then_some(ds)
}

enum DatasetIdCheck {
    Match,
    Mismatch(Option<Uuid>),
    Missing,
    Refused(&'static str),
}

/// Read `<dir>/dataset.json` without following symlinks and compare its `id` to `expected`.
fn check_dataset_id_no_follow(dir: &Path, expected: Uuid) -> DatasetIdCheck {
    let file = dir.join("dataset.json");
    let meta = match std::fs::symlink_metadata(&file) {
        Ok(meta) => meta,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return DatasetIdCheck::Missing,
        Err(_) => return DatasetIdCheck::Refused("unreadable dataset.json metadata"),
    };
    if meta.file_type().is_symlink() || !meta.file_type().is_file() {
        return DatasetIdCheck::Refused("dataset.json is not a regular file");
    }
    let Ok(bytes) = std::fs::read(&file) else {
        return DatasetIdCheck::Refused("unreadable dataset.json");
    };
    let Ok(value) = serde_json::from_slice::<serde_json::Value>(&bytes) else {
        return DatasetIdCheck::Refused("malformed dataset.json");
    };
    let found = value
        .get("id")
        .and_then(|v| serde_json::from_value::<Uuid>(v.clone()).ok());
    match found {
        Some(id) if id == expected => DatasetIdCheck::Match,
        other => DatasetIdCheck::Mismatch(other),
    }
}

/// Read `<dir>/dataset.json` without following symlinks, returning its UUID if valid.
fn read_dataset_id_no_follow(dir: &Path) -> Option<Uuid> {
    let root_meta = std::fs::symlink_metadata(dir).ok()?;
    if root_meta.file_type().is_symlink() || !root_meta.file_type().is_dir() {
        return None;
    }
    let file = dir.join("dataset.json");
    let file_meta = std::fs::symlink_metadata(&file).ok()?;
    if file_meta.file_type().is_symlink() || !file_meta.file_type().is_file() {
        return None;
    }
    let bytes = std::fs::read(&file).ok()?;
    let value: serde_json::Value = serde_json::from_slice(&bytes).ok()?;
    serde_json::from_value(value.get("id")?.clone()).ok()
}

/// Sum the regular-file bytes under `path` without ever following symlinks. Unreadable
/// entries are skipped so permission errors never abort startup reporting.
fn retained_bytes_no_follow(path: &Path) -> u64 {
    let Ok(root_meta) = std::fs::symlink_metadata(path) else {
        return 0;
    };
    if root_meta.file_type().is_symlink() {
        return 0;
    }
    if root_meta.file_type().is_file() {
        return root_meta.len();
    }
    if !root_meta.file_type().is_dir() {
        return 0;
    }
    let mut total = 0u64;
    let mut stack = vec![path.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let child = entry.path();
            let Ok(meta) = std::fs::symlink_metadata(&child) else {
                continue;
            };
            if meta.file_type().is_symlink() {
                continue;
            }
            if meta.file_type().is_dir() {
                stack.push(child);
            } else if meta.file_type().is_file() {
                total = total.saturating_add(meta.len());
            }
        }
    }
    total
}

/// Finish interrupted deletes recorded in `<dir>/delete.json` and warn about any
/// remaining unrecognized or unremovable `.kept-*` entries under `<dir>/databases/`.
pub(super) fn recover(dir: &Path) -> Result<()> {
    let intents = read_intents(dir)?;
    let databases = dir.join("databases");
    let reg = read_registry(dir)?;
    let registered_persistent: BTreeSet<String> = reg
        .datasets
        .into_iter()
        .filter(|e| e.kind == DatasetKind::Persistent)
        .map(|e| e.name)
        .collect();

    let mut remaining = Vec::new();
    let mut warned: BTreeSet<OsString> = BTreeSet::new();
    let mut databases_changed = false;

    for intent in intents {
        let kept_name = intent.kept_name();
        let kept_path = databases.join(&kept_name);
        let live_root = databases.join(&intent.name);
        let is_registered = registered_persistent.contains(&intent.name);

        // If `intent.name` is registered as persistent, only allow cleaning up `kept_path`
        // when `databases/<name>` already holds a distinct live dataset (created after the
        // delete committed). Never touch `live_root`, and never touch `kept_path` when the
        // registered dataset's root is missing or has the same UUID.
        let can_clean_kept = if is_registered {
            read_dataset_id_no_follow(&live_root).is_some_and(|live_id| live_id != intent.id)
        } else {
            true
        };

        match std::fs::symlink_metadata(&kept_path) {
            Ok(meta) => {
                if !can_clean_kept {
                    // Registered dataset still owns this identity (e.g. failed rollback or
                    // pre-commit crash): drop the stale intent and leave kept bytes alone.
                    continue;
                }
                if meta.file_type().is_symlink() || !meta.file_type().is_dir() {
                    let kind = if meta.file_type().is_symlink() {
                        "symlink"
                    } else {
                        "non-directory"
                    };
                    let retained_bytes = retained_bytes_no_follow(&kept_path);
                    tracing::warn!(
                        target: "sparkles::catalog",
                        path = %kept_path.display(),
                        retained_bytes,
                        kind,
                        "skipping interrupted delete cleanup because kept entry is not a directory"
                    );
                    warned.insert(OsString::from(&kept_name));
                    continue;
                }
                match check_dataset_id_no_follow(&kept_path, intent.id) {
                    DatasetIdCheck::Match | DatasetIdCheck::Missing => {
                        let reclaimed_bytes = retained_bytes_no_follow(&kept_path);
                        match std::fs::remove_dir_all(&kept_path) {
                            Ok(()) => {
                                databases_changed = true;
                                tracing::info!(
                                    target: "sparkles::catalog",
                                    path = %kept_path.display(),
                                    dataset = %intent.name,
                                    reclaimed_bytes,
                                    "removed interrupted delete directory on startup"
                                );
                            }
                            Err(error) => {
                                let retained_bytes = retained_bytes_no_follow(&kept_path);
                                tracing::warn!(
                                    target: "sparkles::catalog",
                                    path = %kept_path.display(),
                                    dataset = %intent.name,
                                    retained_bytes,
                                    %error,
                                    "could not remove interrupted delete directory on startup; retaining bytes until next retry or manual cleanup"
                                );
                                warned.insert(OsString::from(&kept_name));
                                remaining.push(intent);
                            }
                        }
                    }
                    DatasetIdCheck::Mismatch(found) => {
                        let retained_bytes = retained_bytes_no_follow(&kept_path);
                        tracing::warn!(
                            target: "sparkles::catalog",
                            path = %kept_path.display(),
                            dataset = %intent.name,
                            expected_id = %intent.id,
                            found_id = ?found,
                            retained_bytes,
                            "skipping interrupted delete cleanup because kept directory identity does not match delete intent"
                        );
                        warned.insert(OsString::from(&kept_name));
                    }
                    DatasetIdCheck::Refused(reason) => {
                        let retained_bytes = retained_bytes_no_follow(&kept_path);
                        tracing::warn!(
                            target: "sparkles::catalog",
                            path = %kept_path.display(),
                            dataset = %intent.name,
                            retained_bytes,
                            reason,
                            "skipping interrupted delete cleanup because kept directory metadata could not be verified"
                        );
                        warned.insert(OsString::from(&kept_name));
                    }
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                // If the crash happened after config.json removed the dataset and before
                // moving `databases/<name>` aside, remove `databases/<name>` only when it
                // is unregistered, not a symlink, and its dataset.json matches `intent.id`.
                if !is_registered
                    && let Ok(meta) = std::fs::symlink_metadata(&live_root)
                    && !meta.file_type().is_symlink()
                    && meta.file_type().is_dir()
                    && matches!(
                        check_dataset_id_no_follow(&live_root, intent.id),
                        DatasetIdCheck::Match
                    )
                {
                    let reclaimed_bytes = retained_bytes_no_follow(&live_root);
                    match std::fs::remove_dir_all(&live_root) {
                        Ok(()) => {
                            databases_changed = true;
                            tracing::info!(
                                target: "sparkles::catalog",
                                path = %live_root.display(),
                                dataset = %intent.name,
                                reclaimed_bytes,
                                "removed unregistered dataset root from interrupted delete on startup"
                            );
                        }
                        Err(error) => {
                            let retained_bytes = retained_bytes_no_follow(&live_root);
                            tracing::warn!(
                                target: "sparkles::catalog",
                                path = %live_root.display(),
                                dataset = %intent.name,
                                retained_bytes,
                                %error,
                                "could not remove unregistered dataset root from interrupted delete on startup"
                            );
                            remaining.push(intent);
                        }
                    }
                }
            }
            Err(error) => {
                tracing::warn!(
                    target: "sparkles::catalog",
                    path = %kept_path.display(),
                    dataset = %intent.name,
                    %error,
                    "could not inspect interrupted delete directory on startup"
                );
                warned.insert(OsString::from(&kept_name));
                remaining.push(intent);
            }
        }
    }

    if databases_changed {
        sync_dir(&databases)?;
    }
    if dir.join(INTENT_FILE).exists() || !remaining.is_empty() {
        write_intents(dir, &remaining)?;
    }

    // Report any remaining `.kept-*` entries under `databases/` that are not live
    // `keepReplaced` backups and were not already warned about above.
    let entries = match std::fs::read_dir(&databases) {
        Ok(entries) => entries,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(e.into()),
    };
    for entry in entries {
        let entry = entry?;
        let file_name = entry.file_name();
        let name = file_name.to_string_lossy();
        if !name.starts_with(KEPT_PREFIX) || warned.contains(&file_name) {
            continue;
        }
        let path = entry.path();
        let meta = match std::fs::symlink_metadata(&path) {
            Ok(meta) => meta,
            Err(error) => {
                tracing::warn!(
                    target: "sparkles::catalog",
                    path = %path.display(),
                    retained_bytes = 0u64,
                    %error,
                    "retained kept entry could not be inspected on startup"
                );
                continue;
            }
        };
        if meta.file_type().is_symlink() {
            tracing::warn!(
                target: "sparkles::catalog",
                path = %path.display(),
                retained_bytes = 0u64,
                kind = "symlink",
                "unregistered kept symlink in databases directory was left untouched"
            );
            continue;
        }
        if meta.file_type().is_dir()
            && let Some(ds) = keep_replaced_dataset(&name)
            && registered_persistent.contains(ds)
            && std::fs::symlink_metadata(databases.join(ds))
                .is_ok_and(|m| !m.file_type().is_symlink() && m.file_type().is_dir())
        {
            continue;
        }
        let retained_bytes = retained_bytes_no_follow(&path);
        tracing::warn!(
            target: "sparkles::catalog",
            path = %path.display(),
            entry = %name,
            retained_bytes,
            "retained kept entry in databases directory has no recorded delete intent and was left untouched"
        );
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

        // Verify crash state: intent exists with format 1, kept dir holds bytes, config dropped dataset.
        let intent_raw: serde_json::Value =
            serde_json::from_slice(&std::fs::read(dir.path().join(INTENT_FILE)).unwrap()).unwrap();
        assert_eq!(intent_raw["format"], 1);
        let kept = kept_entries(dir.path());
        assert_eq!(kept.len(), 1);
        assert!(
            kept[0]
                .file_name()
                .unwrap()
                .to_string_lossy()
                .starts_with(".kept-my-doomed-")
        );
        assert!(retained_bytes_no_follow(&kept[0]) > 0);
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
            0u8,
            1 << super::super::DELETE_TRASH_RENAME,
            1 << super::super::DELETE_TRASH_SYNC,
        ] {
            let dir = tempfile::tempdir().unwrap();
            let cat = Catalog::open(dir.path(), Default::default()).unwrap();
            let ds = cat.create("temp", &Default::default()).unwrap();
            ds.update("INSERT DATA { <urn:t> <urn:p> 1 }").unwrap();
            drop(ds);

            super::super::DELETE_FAIL.set(fail_mask);
            let ok = cat.delete("temp");
            super::super::DELETE_FAIL.set(0);
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

    #[test]
    fn keep_replaced_copy_of_live_dataset_is_preserved_without_warning() {
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
        assert!(capture.warnings().is_empty(), "{:?}", capture.warnings());
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
            br#"{"format": 2, "intents": []}"#,
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
            br#"{"format": 2, "intents": []}"#,
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
}
