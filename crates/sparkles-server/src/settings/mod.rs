//! Layered dataset settings (spec C19): the settings kinds, the declared settings file
//! of `serve --settings`, and the effective value of a kind for a dataset.
//!
//! A kind's effective value is its built-in defaults, then the `defaults` of the
//! settings file, then the file's entry for the dataset, then the runtime layer kept in
//! the dataset's file, merged as RFC 7396 says. The runtime layer holds only what was
//! changed at runtime, and a file written before layering holds a complete object,
//! which reads as a runtime layer that sets every field. Fields the settings file locks
//! take their value from the declared layers whatever the runtime layer says.
//!
//! Every feature reads its settings through [`effective`], never from the file.

pub mod http;
pub mod merge;
#[cfg(test)]
mod tests;

use crate::models::Models;
use crate::state::{AppState, Dataset};
use anyhow::Context;
use arc_swap::ArcSwap;
use merge::{at, forbidden_member, leaves, merged, parse_path, path_string, starts_with};
use parking_lot::Mutex;
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// How validation treats model providers.
#[derive(Clone, Copy)]
pub enum Providers<'a> {
    /// skip the checks that need the model configuration (`settings check` without
    /// `--model-config`)
    Unchecked,
    /// against a model configuration (`None`: the server has none)
    Checked(Option<&'a Models>),
}

/// A settings type: what serde reads, and the checks of its values.
pub trait Typed: Serialize + DeserializeOwned + Default {
    fn validate(&self, providers: Providers) -> Result<(), String>;
}

/// Where a kind's values live.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Scope {
    /// one object per dataset, in the dataset's directory
    Dataset,
    /// one object for the server, kept by the server rather than a dataset; no kind
    /// registered so far has it
    #[allow(dead_code)]
    Server,
}

/// A settings kind (§3).
pub struct Kind {
    pub name: &'static str,
    pub scope: Scope,
    /// the file in the dataset's directory that keeps the runtime layer
    pub file: &'static str,
    /// the kind's top-level members
    pub members: &'static [&'static str],
    /// whether the file holds members of its own besides the kind's (`ingest.json`
    /// keeps its profiles)
    shared: bool,
    defaults: fn() -> Value,
    /// the object read as the kind's type and written back, when it reads
    normalize: fn(&Value) -> Result<Value, String>,
    check: fn(&Value, Providers) -> Result<(), String>,
}

fn defaults_of<T: Typed>() -> Value {
    serde_json::to_value(T::default()).unwrap_or_default()
}

fn normalize_as<T: Typed>(v: &Value) -> Result<Value, String> {
    let t: T = serde_json::from_value(v.clone()).map_err(|e| e.to_string())?;
    serde_json::to_value(&t).map_err(|e| e.to_string())
}

fn check_as<T: Typed>(v: &Value, providers: Providers) -> Result<(), String> {
    let t: T = serde_json::from_value(v.clone()).map_err(|e| e.to_string())?;
    t.validate(providers)
}

pub static ASSISTANT: Kind = Kind {
    name: "assistant",
    scope: Scope::Dataset,
    file: crate::assistant::ASSISTANT_FILE,
    members: &[
        "enabled",
        "roles",
        "ask",
        "explain",
        "optimize",
        "ingest",
        "send",
        "sendByProvider",
        "rowsForSummary",
        "budget",
        "deadlineSecs",
        "historyDays",
        "routing",
    ],
    shared: false,
    defaults: defaults_of::<crate::assistant::AssistantSettings>,
    normalize: normalize_as::<crate::assistant::AssistantSettings>,
    check: check_as::<crate::assistant::AssistantSettings>,
};

pub static MEMORY: Kind = Kind {
    name: "memory",
    scope: Scope::Dataset,
    file: crate::assist::MEMORY_FILE,
    members: &[
        "agentGraphs",
        "consolidatedGraph",
        "agents",
        "imports",
        "consolidation",
        "retention",
    ],
    shared: false,
    defaults: defaults_of::<crate::assist::MemorySettings>,
    normalize: normalize_as::<crate::assist::MemorySettings>,
    check: check_as::<crate::assist::MemorySettings>,
};

pub static INGEST: Kind = Kind {
    name: "ingest",
    scope: Scope::Dataset,
    file: INGEST_FILE,
    members: &["keepText", "confirmTokens", "autoConfidence"],
    shared: true,
    defaults: defaults_of::<IngestFields>,
    normalize: normalize_as::<IngestFields>,
    check: check_as::<IngestFields>,
};

/// Every kind, in the order the answers list them.
pub static KINDS: [&Kind; 3] = [&ASSISTANT, &MEMORY, &INGEST];

pub fn kind(name: &str) -> Option<&'static Kind> {
    KINDS.iter().copied().find(|k| k.name == name)
}

impl Kind {
    /// The built-in defaults, the first layer.
    pub fn default_value(&self) -> Value {
        (self.defaults)()
    }
}

/// The file of ingest profiles and settings, of which the `ingest` kind holds the
/// settings members.
pub const INGEST_FILE: &str = "ingest.json";

fn yes() -> bool {
    true
}

/// The settings members of `ingest.json` (C18 §7.3).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct IngestFields {
    /// keep the text of sources as chunks
    #[serde(default = "yes")]
    pub keep_text: bool,
    /// the estimate in tokens above which `POST /$/ingest` waits for a confirmation
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub confirm_tokens: Option<u64>,
    /// the confidence that `auto` mode needs of every fact
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auto_confidence: Option<f64>,
}

impl Default for IngestFields {
    fn default() -> IngestFields {
        IngestFields {
            keep_text: true,
            confirm_tokens: None,
            auto_confidence: None,
        }
    }
}

impl Typed for IngestFields {
    fn validate(&self, _: Providers) -> Result<(), String> {
        if self
            .auto_confidence
            .is_some_and(|c| !(0.0..=1.0).contains(&c))
        {
            return Err("autoConfidence is between 0 and 1".into());
        }
        Ok(())
    }
}

// ------------------------------------------------------------ the settings file ------

/// The declared values and locks of one entry of the settings file: `defaults`, or a
/// dataset's.
#[derive(Clone, Debug, Default)]
struct Entry {
    /// by kind
    values: BTreeMap<&'static str, Value>,
    /// (kind, field)
    locked: Vec<(&'static str, Vec<String>)>,
}

/// The settings file of `serve --settings` (§5).
#[derive(Clone, Debug, Default)]
pub struct Declared {
    defaults: Entry,
    datasets: BTreeMap<String, Entry>,
    /// `server.locked`: the locked fields of server-wide kinds, such as
    /// `models.providers.claude.endpoint` or `secrets.anthropic`, as (root, field)
    server_locked: Vec<(String, Vec<String>)>,
}

/// The names that `server.locked` may start with: the server-wide kinds and the
/// secrets of the model configuration.
const SERVER_LOCK_ROOTS: &[&str] = &["models", "secrets"];

impl Declared {
    /// Read a settings file: its form, the members it may hold, and every effective
    /// object it declares, for `defaults` alone and for each dataset.
    pub fn parse(text: &str, providers: Providers) -> Result<Declared, String> {
        let v: Value = serde_json::from_str(text).map_err(|e| e.to_string())?;
        if let Some(f) = forbidden_member(&v) {
            return Err(format!(
                "{f} is not allowed: providers are defined in the server's model configuration only"
            ));
        }
        let Value::Object(top) = v else {
            return Err("the settings file must be a JSON object".into());
        };
        let mut d = Declared::default();
        for (k, v) in top {
            match k.as_str() {
                "defaults" => d.defaults = parse_entry(&v).map_err(|e| format!("defaults: {e}"))?,
                "datasets" => {
                    let Value::Object(m) = v else {
                        return Err("datasets: an object of dataset names".into());
                    };
                    for (name, e) in m {
                        if name.is_empty() {
                            return Err("datasets: an empty dataset name".into());
                        }
                        let entry = parse_entry(&e).map_err(|e| format!("datasets.{name}: {e}"))?;
                        d.datasets.insert(name, entry);
                    }
                }
                "server" => {
                    d.server_locked = parse_server(&v).map_err(|e| format!("server: {e}"))?
                }
                other => {
                    return Err(format!(
                        "unknown member {other:?}: a settings file holds defaults, datasets and server"
                    ));
                }
            }
        }
        d.validate(providers)?;
        Ok(d)
    }

    fn validate(&self, providers: Providers) -> Result<(), String> {
        let names = std::iter::once(None).chain(self.datasets.keys().map(Some));
        for name in names {
            for k in KINDS {
                let r = resolve(
                    k,
                    self,
                    name.map_or("", String::as_str),
                    Value::Object(Map::new()),
                    providers,
                );
                if let Err(e) = r.status {
                    return Err(match name {
                        Some(n) => format!("datasets.{n}.{}: {e}", k.name),
                        None => format!("defaults.{}: {e}", k.name),
                    });
                }
            }
        }
        Ok(())
    }

    /// The names of the file's dataset entries.
    pub fn dataset_names(&self) -> impl Iterator<Item = &String> {
        self.datasets.keys()
    }

    /// The declared layers of `kind` for `dataset`, `defaults` then the dataset's entry.
    fn layers(&self, kind: &Kind, dataset: &str) -> (Option<&Value>, Option<&Value>) {
        (
            self.defaults.values.get(kind.name),
            self.datasets
                .get(dataset)
                .and_then(|e| e.values.get(kind.name)),
        )
    }

    /// The locked fields of the server-wide `kind` (`server.locked`).
    #[allow(dead_code)]
    pub fn server_locked(&self, kind: &Kind) -> Vec<Vec<String>> {
        self.server_locked
            .iter()
            .filter(|(k, _)| k == kind.name)
            .map(|(_, p)| p.clone())
            .collect()
    }

    /// The fields of `kind` locked for `dataset`.
    fn locked(&self, kind: &Kind, dataset: &str) -> Vec<Vec<String>> {
        let mut out: Vec<Vec<String>> = Vec::new();
        let ds = self.datasets.get(dataset);
        for (k, p) in self
            .defaults
            .locked
            .iter()
            .chain(ds.into_iter().flat_map(|e| e.locked.iter()))
        {
            if *k == kind.name && !out.contains(p) {
                out.push(p.clone());
            }
        }
        out
    }
}

fn parse_entry(v: &Value) -> Result<Entry, String> {
    let Value::Object(m) = v else {
        return Err("an object of settings kinds and locked".into());
    };
    let mut e = Entry::default();
    for (k, v) in m {
        if k == "locked" {
            let Value::Array(list) = v else {
                return Err("locked: a list of fields such as \"assistant.send\"".into());
            };
            for f in list {
                let Some(s) = f.as_str() else {
                    return Err("locked: a list of fields such as \"assistant.send\"".into());
                };
                e.locked.push(parse_lock(s)?);
            }
            continue;
        }
        let Some(kind) = kind(k) else {
            return Err(format!(
                "unknown settings kind {k:?}: use {}",
                KINDS.map(|k| k.name).join(", ")
            ));
        };
        if !v.is_object() {
            return Err(format!("{k}: an object"));
        }
        e.values.insert(kind.name, v.clone());
    }
    Ok(e)
}

/// `server`: only `locked`, whose fields start with a server-wide kind or `secrets`.
fn parse_server(v: &Value) -> Result<Vec<(String, Vec<String>)>, String> {
    let Value::Object(m) = v else {
        return Err("an object with locked".into());
    };
    let mut out = Vec::new();
    for (k, v) in m {
        if k != "locked" {
            return Err(format!("unknown member {k:?}: server holds locked"));
        }
        let list = v
            .as_array()
            .ok_or("locked: a list of fields such as \"models.routing\"")?;
        for f in list {
            let s = f.as_str().unwrap_or_default();
            let mut p = parse_path(s)
                .filter(|p| p.len() >= 2)
                .ok_or_else(|| format!("locked: {f} is not a field such as \"models.routing\""))?;
            if !SERVER_LOCK_ROOTS.contains(&p[0].as_str()) {
                return Err(format!(
                    "locked: {s:?} names no server-wide kind; use {}",
                    SERVER_LOCK_ROOTS.join(", ")
                ));
            }
            let root = p.remove(0);
            out.push((root, p));
        }
    }
    Ok(out)
}

/// A locked field, such as `assistant.send` (§4.1).
fn parse_lock(s: &str) -> Result<(&'static str, Vec<String>), String> {
    let bad = || format!("locked: {s:?} is not a field such as \"assistant.send\"");
    let mut p = parse_path(s).ok_or_else(bad)?;
    if p.len() < 2 {
        return Err(bad());
    }
    let k = kind(&p[0]).ok_or_else(|| {
        format!(
            "locked: {s:?} names no settings kind; use {}",
            KINDS.map(|k| k.name).join(", ")
        )
    })?;
    if !k.members.contains(&p[1].as_str()) {
        return Err(format!("locked: {} has no member {:?}", k.name, p[1]));
    }
    p.remove(0);
    Ok((k.name, p))
}

// ---------------------------------------------------------------- resolution ------

/// The layers and the effective value of one kind for one dataset (§4).
impl std::fmt::Debug for Kind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.name)
    }
}

#[derive(Clone, Debug)]
pub struct Resolved {
    pub kind: &'static Kind,
    /// the declared layers merged: `defaults` then the dataset's entry
    pub declared: Value,
    /// the runtime layer as it is in the file
    pub runtime: Value,
    /// the built-in defaults and the declared layers
    pub base: Value,
    /// the effective object: the read form of the kind's type when it reads
    pub effective: Value,
    /// the locked fields
    pub locked: Vec<Vec<String>>,
    /// the locked fields whose runtime value is ignored
    pub overridden: Vec<Vec<String>>,
    /// the source of each field of `effective`
    pub sources: BTreeMap<Vec<String>, &'static str>,
    /// whether the effective object is valid, and why not
    pub status: Result<(), String>,
    /// the tag of the runtime layer
    pub etag: String,
}

/// The effective value of a dataset-wide `kind` with the runtime layer `runtime`, the
/// declared layers of `declared` for `dataset`, and the locks applied.
pub fn resolve(
    kind: &'static Kind,
    declared: &Declared,
    dataset: &str,
    runtime: Value,
    providers: Providers,
) -> Resolved {
    let (dd, de) = declared.layers(kind, dataset);
    let layers: Vec<&Value> = [dd, de].into_iter().flatten().collect();
    resolve_layers(
        kind,
        &layers,
        declared.locked(kind, dataset),
        runtime,
        providers,
    )
}

/// The effective value of `kind` from its built-in defaults, the declared `layers` in
/// order, and `runtime`, with the `locked` fields taken from the declared layers. Both
/// scopes resolve here: a dataset-wide kind with the settings file's `defaults` and
/// dataset entry, and a server-wide kind with its own declared source and the locks of
/// `server.locked`.
pub fn resolve_layers(
    kind: &'static Kind,
    layers: &[&Value],
    locked: Vec<Vec<String>>,
    runtime: Value,
    providers: Providers,
) -> Resolved {
    let mut decl = Value::Object(Map::new());
    let mut base = (kind.defaults)();
    for l in layers {
        merge::merge(&mut decl, l);
        merge::merge(&mut base, l);
    }
    let unlocked = merged(&base, &runtime);
    let mut eff = unlocked.clone();
    let mut overridden = Vec::new();
    for l in &locked {
        let fixed = at(&base, l).cloned();
        if at(&unlocked, l) != fixed.as_ref() {
            overridden.push(l.clone());
        }
        match fixed {
            Some(v) => merge::set_at(&mut eff, l, v),
            None => {
                merge::remove_at(&mut eff, l);
            }
        }
    }
    let status = (kind.check)(&eff, providers);
    if let Ok(n) = (kind.normalize)(&eff) {
        eff = n;
    }
    let in_declared = |p: &[String]| layers.iter().any(|l| covers(l, p));
    let sources = leaves(&eff)
        .into_iter()
        .map(|p| {
            let s = if locked.iter().any(|l| starts_with(&p, l)) {
                "locked"
            } else if covers(&runtime, &p) {
                "runtime"
            } else if in_declared(&p) {
                "declared"
            } else {
                "default"
            };
            (p, s)
        })
        .collect();
    let etag = etag_of(&runtime);
    Resolved {
        kind,
        declared: decl,
        runtime,
        base,
        effective: eff,
        locked,
        overridden,
        sources,
        status,
        etag,
    }
}

/// Whether a layer sets the field at `path`, or a field above it to something other
/// than an object.
fn covers(layer: &Value, path: &[String]) -> bool {
    let mut v = layer;
    for k in path {
        match v {
            Value::Object(m) => match m.get(k) {
                Some(x) => v = x,
                None => return false,
            },
            _ => return true,
        }
    }
    true
}

/// A strong tag of a runtime layer.
pub fn etag_of(runtime: &Value) -> String {
    use sha2::Digest;
    let bytes = serde_json::to_vec(runtime).unwrap_or_default();
    let d = sha2::Sha256::digest(&bytes);
    let hex: String = d.iter().take(10).map(|b| format!("{b:02x}")).collect();
    format!("\"{hex}\"")
}

impl Resolved {
    /// The answer of `GET /$/settings/{ds}/{kind}`.
    pub fn json(&self, dataset: &str) -> Value {
        let paths = |l: &[Vec<String>]| l.iter().map(|p| path_string(p)).collect::<Vec<_>>();
        let sources: Map<String, Value> = self
            .sources
            .iter()
            .map(|(p, s)| (path_string(p), Value::from(*s)))
            .collect();
        json!({
            "dataset": dataset,
            "kind": self.kind.name,
            "effective": self.effective,
            "declared": self.declared,
            "runtime": self.runtime,
            "sources": sources,
            "locked": paths(&self.locked),
            "overridden": paths(&self.overridden),
            "status": match &self.status {
                Ok(()) => json!({ "valid": true }),
                Err(e) => json!({ "valid": false, "error": e }),
            },
            "etag": self.etag,
        })
    }

    /// The effective object as the kind's type: the defaults when it does not read,
    /// which is logged. An object that reads but does not validate, such as one whose
    /// role names a provider the server no longer has, is used as it is, and the
    /// features treat it as they treat a missing provider.
    pub fn typed<T: Typed>(&self, dataset: &str) -> T {
        serde_json::from_value(self.effective.clone()).unwrap_or_else(|e| {
            tracing::warn!(dataset, kind = self.kind.name, "settings: {e}");
            T::default()
        })
    }
}

// ----------------------------------------------------------------- the state ------

/// A dataset (`None` for a server-wide kind) and a kind.
type LockKey = (Option<String>, &'static str);

#[derive(Default)]
struct FileStatus {
    read_at: Option<String>,
    error: Option<String>,
    error_at: Option<String>,
}

/// The settings file and what the server knows of its reads (`AppState::settings`).
#[derive(Default)]
pub struct Settings {
    /// `serve --settings`
    file: Option<PathBuf>,
    declared: ArcSwap<Declared>,
    status: Mutex<FileStatus>,
    /// `--model-config` and its reads (§7)
    models_file: Option<PathBuf>,
    models_status: Mutex<FileStatus>,
    /// the write locks, by dataset (`None` for a server-wide kind) and kind
    locks: Mutex<HashMap<LockKey, Arc<Mutex<()>>>>,
}

fn now() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
}

/// Read and check a settings file.
pub fn read_file(path: &Path, providers: Providers) -> anyhow::Result<Declared> {
    let text =
        std::fs::read_to_string(path).with_context(|| format!("cannot read {}", path.display()))?;
    Declared::parse(&text, providers)
        .map_err(|e| anyhow::anyhow!("settings file {}: {e}", path.display()))
}

impl Settings {
    /// The settings of `serve --settings FILE`, checked against `models`; a file that
    /// does not read or check fails the start.
    pub fn load(path: &Path, models: Option<&Models>) -> anyhow::Result<Settings> {
        let d = read_file(path, Providers::Checked(models))?;
        let s = Settings {
            file: Some(path.to_path_buf()),
            declared: ArcSwap::from_pointee(d),
            ..Default::default()
        };
        s.status.lock().read_at = Some(now());
        Ok(s)
    }

    /// Note the model configuration that SIGHUP reads again.
    pub fn set_models_file(&mut self, path: Option<PathBuf>) {
        if path.is_some() {
            self.models_status.lock().read_at = Some(now());
        }
        self.models_file = path;
    }

    pub fn declared(&self) -> Arc<Declared> {
        self.declared.load_full()
    }

    /// Read the settings file again. A file that does not read or check is logged and
    /// the previous one kept.
    pub fn reload(&self, models: Option<&Models>) -> Result<(), String> {
        let Some(path) = &self.file else {
            return Ok(());
        };
        match read_file(path, Providers::Checked(models)) {
            Ok(d) => {
                self.declared.store(Arc::new(d));
                let mut s = self.status.lock();
                s.read_at = Some(now());
                s.error = None;
                s.error_at = None;
                tracing::info!("settings reloaded from {}", path.display());
                Ok(())
            }
            Err(e) => {
                let msg = format!("{e:#}");
                tracing::error!("settings not reloaded, the previous ones stay: {msg}");
                let mut s = self.status.lock();
                s.error = Some(msg.clone());
                s.error_at = Some(now());
                Err(msg)
            }
        }
    }

    /// Note the outcome of reading the model configuration again.
    pub fn models_reloaded(&self, outcome: Result<(), String>) {
        let mut s = self.models_status.lock();
        match outcome {
            Ok(()) => {
                s.read_at = Some(now());
                s.error = None;
                s.error_at = None;
            }
            Err(e) => {
                s.error = Some(e);
                s.error_at = Some(now());
            }
        }
    }

    /// The lock that serializes writes to `kind` of `dataset`, or of the server for a
    /// server-wide kind.
    pub fn write_lock(&self, dataset: &str, kind: &'static Kind) -> Arc<Mutex<()>> {
        let scope = (kind.scope == Scope::Dataset).then(|| dataset.to_string());
        self.locks
            .lock()
            .entry((scope, kind.name))
            .or_default()
            .clone()
    }

    /// `GET /$/settings`.
    pub fn status_json(&self, st: &AppState) -> Value {
        let d = self.declared();
        let names = st.datasets();
        let unmatched: Vec<String> = d
            .dataset_names()
            .filter(|n| !names.contains_key(*n))
            .cloned()
            .collect();
        let file = |path: &Option<PathBuf>, s: &FileStatus| {
            json!({
                "path": path.as_ref().map(|p| p.display().to_string()),
                "readAt": s.read_at,
                "error": s.error,
                "errorAt": s.error_at,
            })
        };
        let mut out = file(&self.file, &self.status.lock());
        out["declared"] = d.dataset_names().cloned().collect::<Vec<_>>().into();
        out["unmatched"] = unmatched.into();
        out["kinds"] = KINDS.map(|k| k.name).to_vec().into();
        out["models"] = file(&self.models_file, &self.models_status.lock());
        out
    }
}

// ------------------------------------------------------ the dataset's runtime layer ------

/// Settings belong to the dataset, not to a branch.
fn main_of(ds: &Dataset) -> Option<Arc<Dataset>> {
    ds.main()
}

/// The runtime layer of `kind` in the dataset's file (an empty object without one).
pub fn runtime(st: &AppState, ds: &Dataset, kind: &Kind) -> anyhow::Result<Value> {
    let main = main_of(ds);
    let ds = main.as_deref().unwrap_or(ds);
    let v = crate::assist::read_file(st, ds, kind.file)?;
    Ok(match v {
        None => Value::Object(Map::new()),
        Some(Value::Object(mut m)) => {
            if kind.shared {
                m.retain(|k, _| kind.members.contains(&k.as_str()));
            }
            Value::Object(m)
        }
        Some(_) => anyhow::bail!("{} of {} is not a JSON object", kind.file, ds.name),
    })
}

/// Replace the runtime layer of `kind` in the dataset's file, keeping the file's other
/// members. A file left empty is removed.
pub fn store_runtime(
    st: &AppState,
    ds: &Dataset,
    kind: &Kind,
    layer: &Value,
) -> anyhow::Result<()> {
    let main = main_of(ds);
    let ds = main.as_deref().unwrap_or(ds);
    let mut m = match layer {
        Value::Object(m) => m.clone(),
        _ => Map::new(),
    };
    if kind.shared
        && let Some(Value::Object(old)) = crate::assist::read_file(st, ds, kind.file)?
    {
        for (k, v) in old {
            if !kind.members.contains(&k.as_str()) {
                m.insert(k, v);
            }
        }
    }
    if m.is_empty() {
        crate::assist::remove_file(st, ds, kind.file)
    } else {
        crate::assist::write_file(st, ds, kind.file, &Value::Object(m))
    }
}

/// The layers and effective value of `kind` for a dataset. A runtime file that cannot
/// be read is logged and left out.
pub fn resolved(st: &AppState, ds: &Dataset, kind: &'static Kind) -> Resolved {
    let rt = runtime(st, ds, kind).unwrap_or_else(|e| {
        tracing::warn!(dataset = %ds.name, "{e:#}");
        Value::Object(Map::new())
    });
    let models = st.models();
    resolve(
        kind,
        &st.settings.declared(),
        &ds.name,
        rt,
        Providers::Checked(models.as_deref()),
    )
}

/// The effective settings of `kind` for a dataset, as its type.
pub fn effective<T: Typed>(st: &AppState, ds: &Dataset, kind: &'static Kind) -> T {
    resolved(st, ds, kind).typed(&ds.name)
}

/// The settings of a database directory without a server (`sparkles ask --loc`): the
/// built-in defaults and the runtime layer.
pub fn effective_at<T: Typed>(dir: &Path, kind: &'static Kind) -> anyhow::Result<Option<T>> {
    let rt = match std::fs::read(dir.join(kind.file)) {
        Ok(b) => serde_json::from_slice::<Value>(&b)
            .with_context(|| format!("{} of {}", kind.file, dir.display()))?,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e).with_context(|| format!("reading {}", kind.file)),
    };
    let r = resolve(kind, &Declared::default(), "", rt, Providers::Unchecked);
    Ok(Some(serde_json::from_value(r.effective).with_context(
        || format!("{} of {}", kind.file, dir.display()),
    )?))
}

/// After a reload, log the effective objects that are no longer valid (§4.3).
pub fn log_invalid(st: &AppState) {
    for ds in st.datasets().values() {
        for k in KINDS {
            if let Err(e) = resolved(st, ds, k).status {
                tracing::warn!(dataset = %ds.name, kind = k.name, "settings are not valid: {e}");
            }
        }
    }
}

/// Read the model configuration and the settings file again on SIGHUP, in that order,
/// since the settings are checked against the providers. Either one that does not load
/// is logged and the previous one kept; requests in flight keep the configuration they
/// started with.
#[cfg(unix)]
pub fn spawn_reload_on_sighup(st: Arc<AppState>, models: crate::models::ModelArgs) {
    use tokio::signal::unix::{SignalKind, signal};
    let Ok(mut hup) = signal(SignalKind::hangup()) else {
        return;
    };
    tokio::spawn(async move {
        while hup.recv().await.is_some() {
            let st = st.clone();
            let models = models.clone();
            let _ = tokio::task::spawn_blocking(move || reload(&st, &models)).await;
        }
    });
}

/// What SIGHUP does (see [`spawn_reload_on_sighup`]).
pub fn reload(st: &AppState, models: &crate::models::ModelArgs) {
    if models.model_config.is_some() {
        match models.load(st.outbound.clone()) {
            Ok(m) => {
                st.set_models(m);
                st.settings.models_reloaded(Ok(()));
                tracing::info!("model configuration reloaded");
            }
            Err(e) => {
                let msg = format!("{e:#}");
                tracing::error!("model configuration not reloaded, the previous one stays: {msg}");
                st.settings.models_reloaded(Err(msg));
            }
        }
    }
    let m = st.models();
    let _ = st.settings.reload(m.as_deref());
    log_invalid(st);
}

/// `sparkles settings check FILE [--model-config FILE]` (§8).
pub fn check_file(path: &Path, model_config: Option<&Path>) -> anyhow::Result<()> {
    let models = model_config
        .map(|p| -> anyhow::Result<Models> {
            let cfg = crate::models::ModelsConfig::load(p)?;
            Ok(Models::new(cfg, BTreeMap::new(), Default::default()))
        })
        .transpose()?;
    let providers = match &models {
        Some(m) => Providers::Checked(Some(m)),
        None => Providers::Unchecked,
    };
    let d = read_file(path, providers)?;
    println!(
        "{}: valid ({} dataset entr{})",
        path.display(),
        d.datasets.len(),
        if d.datasets.len() == 1 { "y" } else { "ies" }
    );
    Ok(())
}
