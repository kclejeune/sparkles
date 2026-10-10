//! Server-wide settings (spec C19 §11): the `models` kind.
//!
//! The effective model configuration is C18's built-in defaults, then the declared
//! configuration of `--model-config`, then the runtime layer in `<dataDir>/models.json`,
//! merged as RFC 7396 says, with the fields that the settings file's `server.locked`
//! names taken from the declared layer. Providers merge member by member, a runtime
//! `null` removes a declared provider, and each role list is one field. The effective
//! object is checked as `--model-config` is, and the configuration built from it is the
//! one [`AppState::models`] serves. A change swaps it, so requests that start after it
//! use it and requests in flight keep theirs.
//!
//! A secret's runtime value ([`super::secrets`]) takes the place of its `--model-secret`
//! source unless `server.locked` names `secrets.NAME`.

use super::http::{Op, Write, bad, if_match, op_of, plan};
use super::merge::path_string;
use super::{Declared, Kind, Providers, Resolved, Scope, read_file, resolve_layers};
use crate::http::{ApiResult, blocking, err};
use crate::models::{ModelArgs, Models, ModelsConfig, PROVIDER_MEMBERS, Role};
use crate::state::AppState;
use axum::http::{HeaderMap, StatusCode};
use parking_lot::Mutex;
use serde_json::{Map, Value, json};
use sparkles::vector::embed::SecretSource;
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// The runtime layer of `models`, in the data directory.
pub const MODELS_FILE: &str = "models.json";

pub static MODELS: Kind = Kind {
    name: "models",
    scope: Scope::Server,
    file: MODELS_FILE,
    members: &["providers", "roles", "routing"],
    removable: &["providers"],
    shared: false,
    defaults: models_defaults,
    normalize: models_normalize,
    check: models_check,
};

/// C18's built-in defaults: no provider and no role list.
fn models_defaults() -> Value {
    json!({ "providers": {}, "roles": {} })
}

/// The effective object is kept as JSON: serde's form of the configuration would add
/// every member a provider leaves out.
fn models_normalize(v: &Value) -> Result<Value, String> {
    ModelsConfig::from_value(v)
        .map(|_| v.clone())
        .map_err(|e| format!("{e:#}"))
}

fn models_check(v: &Value, _: Providers) -> Result<(), String> {
    ModelsConfig::from_value(v)
        .map(|_| ())
        .map_err(|e| format!("{e:#}"))
}

const ROUTING_MEMBERS: &[&str] = &["complexityThreshold", "exampleScore"];

/// The form of a field of `server.locked` after its root (§11.1): a member of `models`,
/// a provider or one of its members, a role list, a routing setting, or a secret's name.
pub fn check_lock(root: &str, p: &[String]) -> Result<(), String> {
    match root {
        "secrets" => {
            if p.len() != 1 {
                return Err("a secret is locked by its name, such as secrets.anthropic".into());
            }
            crate::models::check_secret_name(&p[0])
        }
        _ => {
            let top = p[0].as_str();
            if !MODELS.members.contains(&top) {
                return Err(format!(
                    "models has no member {top:?}; use {}",
                    MODELS.members.join(", ")
                ));
            }
            match top {
                "providers" => {
                    if let Some(m) = p.get(2)
                        && !PROVIDER_MEMBERS.contains(&m.as_str())
                    {
                        return Err(format!("a provider has no member {m:?}"));
                    }
                }
                "roles" => {
                    if let Some(r) = p.get(1)
                        && Role::parse(r).is_none()
                    {
                        return Err(format!("no role named {r:?}"));
                    }
                    if p.len() > 2 {
                        return Err("a role list is one field, such as models.roles.draft".into());
                    }
                }
                _ => {
                    if let Some(m) = p.get(1)
                        && !ROUTING_MEMBERS.contains(&m.as_str())
                    {
                        return Err(format!("routing has no member {m:?}"));
                    }
                }
            }
            Ok(())
        }
    }
}

/// The locks of `server.locked` that name nothing the model configuration defines: a
/// provider it lacks, which the lock keeps from being added at runtime, and a secret
/// that no provider uses (`sparkles settings check --model-config`).
pub fn lock_warnings(d: &Declared, cfg: &ModelsConfig) -> Vec<String> {
    let mut out = Vec::new();
    for p in d.server_locked(&MODELS) {
        if p.len() >= 2 && p[0] == "providers" && !cfg.providers.contains_key(&p[1]) {
            out.push(format!(
                "server.locked: models.{} names provider {:?}, which the model configuration does not define; the lock keeps it from being added at runtime",
                path_string(&p),
                p[1]
            ));
        }
    }
    for name in d.locked_secrets() {
        let used = cfg
            .providers
            .values()
            .any(|p| p.api_key.as_ref().is_some_and(|k| k.secret == name));
        if !used {
            out.push(format!(
                "server.locked: secrets.{name} names a secret that no provider of the model configuration uses"
            ));
        }
    }
    out
}

// ---------------------------------------------------------------- the layers ------

/// The declared and runtime layers of `models` and the declared secret sources
/// (`Settings::server`).
#[derive(Default)]
pub struct ServerLayers {
    /// the configuration of `--model-config` in its bare form (`None` without one)
    declared: Mutex<Option<Value>>,
    /// the sources of `--model-secret`
    sources: Mutex<BTreeMap<String, SecretSource>>,
    /// the runtime layer, as kept in `<dataDir>/models.json`
    runtime: Mutex<Value>,
    /// the data directory (`None`: the runtime layer lives in the process, and no
    /// secret can be stored)
    dir: Option<PathBuf>,
}

impl ServerLayers {
    /// Keep the runtime layer and the secrets in the data directory `dir` (none for an
    /// empty path, as in embedded use).
    pub fn set_dir(&mut self, dir: &Path) {
        self.dir = (!dir.as_os_str().is_empty()).then(|| dir.to_path_buf());
    }

    fn declared(&self) -> Option<Value> {
        self.declared.lock().clone()
    }

    /// The sources of `--model-secret`.
    pub fn secret_sources(&self) -> BTreeMap<String, SecretSource> {
        self.sources.lock().clone()
    }

    /// The runtime layer (an empty object before one is read).
    fn runtime(&self) -> Value {
        match &*self.runtime.lock() {
            Value::Null => Value::Object(Map::new()),
            v => v.clone(),
        }
    }

    /// `<dataDir>/secrets`, where runtime values are kept.
    pub fn secrets_dir(&self) -> Option<PathBuf> {
        self.dir
            .as_ref()
            .map(|d| d.join(super::secrets::SECRETS_DIR))
    }

    fn runtime_path(&self) -> Option<PathBuf> {
        self.dir.as_ref().map(|d| d.join(MODELS_FILE))
    }

    /// Read `--model-config` and `--model-secret`. The configuration must be valid by
    /// itself, as before the layers existed.
    fn load_declared(&self, args: &ModelArgs) -> anyhow::Result<()> {
        let sources = crate::models::parse_secrets(&args.model_secret)?;
        let declared = match &args.model_config {
            Some(p) => Some(ModelsConfig::load_value(p)?.1),
            None => None,
        };
        *self.declared.lock() = declared;
        *self.sources.lock() = sources;
        Ok(())
    }

    /// Read the runtime layer from its file (an empty layer without one).
    fn load_runtime(&self) -> anyhow::Result<()> {
        use anyhow::Context;
        let Some(path) = self.runtime_path() else {
            return Ok(());
        };
        let v = match std::fs::read(&path) {
            Ok(b) => serde_json::from_slice::<Value>(&b)
                .with_context(|| format!("{} is not JSON", path.display()))?,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Value::Object(Map::new()),
            Err(e) => return Err(e).with_context(|| format!("reading {}", path.display())),
        };
        if !v.is_object() {
            anyhow::bail!("{} is not a JSON object", path.display());
        }
        *self.runtime.lock() = v;
        Ok(())
    }

    /// Replace the runtime layer, in its file first. An empty layer removes the file.
    fn store_runtime(&self, v: &Value) -> std::io::Result<()> {
        if let Some(path) = self.runtime_path() {
            if v.as_object().is_none_or(Map::is_empty) {
                match std::fs::remove_file(&path) {
                    Err(e) if e.kind() != std::io::ErrorKind::NotFound => return Err(e),
                    _ => {}
                }
            } else {
                let mut bytes = serde_json::to_vec_pretty(v).unwrap_or_default();
                bytes.push(b'\n');
                super::secrets::write_atomic(&path, &bytes, 0o644)?;
            }
        }
        *self.runtime.lock() = v.clone();
        Ok(())
    }

    /// Whether anything configures models: `--model-config` or a runtime layer.
    fn configured(&self, runtime: &Value) -> bool {
        self.declared.lock().is_some() || runtime.as_object().is_some_and(|m| !m.is_empty())
    }
}

/// The layers of `models` with the locks of `d`.
fn resolve_models(st: &AppState, d: &Declared, runtime: Value) -> Resolved {
    let declared = st.settings.server.declared();
    let layers: Vec<&Value> = declared.iter().collect();
    resolve_layers(
        &MODELS,
        &layers,
        d.server_locked(&MODELS),
        runtime,
        Providers::Unchecked,
    )
}

/// The `models` kind as it stands, for `GET /$/server/settings/models`.
pub fn resolved(st: &AppState) -> Resolved {
    resolve_models(st, &st.settings.declared(), st.settings.server.runtime())
}

/// The secret sources in force: the declared ones, with each runtime value that `d`
/// does not lock in place of its declared source, and the names of those values.
fn secret_sources(
    st: &AppState,
    d: &Declared,
) -> (BTreeMap<String, SecretSource>, BTreeSet<String>) {
    let layers = &st.settings.server;
    let mut sources = layers.secret_sources();
    let mut runtime = BTreeSet::new();
    if let Some(dir) = layers.secrets_dir() {
        for name in super::secrets::stored(&dir).into_keys() {
            if !d.secret_locked(&name) {
                sources.insert(name.clone(), SecretSource::File(dir.join(&name)));
                runtime.insert(name);
            }
        }
    }
    (sources, runtime)
}

/// Build the model configuration of `runtime` with the locks of `d` and serve it. An
/// effective object that is not valid leaves the configuration in force alone.
fn build(st: &AppState, d: &Declared, runtime: Value) -> Result<(), String> {
    if !st.settings.server.configured(&runtime) {
        st.set_models(None);
        return Ok(());
    }
    let r = resolve_models(st, d, runtime);
    r.status.clone()?;
    let cfg = ModelsConfig::from_value(&r.effective).map_err(|e| format!("{e:#}"))?;
    let (sources, runtime_secrets) = secret_sources(st, d);
    let mut m = Models::new(cfg, sources, st.outbound.clone());
    m.set_runtime_secrets(runtime_secrets);
    if let Some(old) = st.models() {
        m.inherit(&old);
    }
    st.set_models(Some(Arc::new(m)));
    Ok(())
}

/// Serve the effective model configuration with the locks of `d`.
pub fn apply_with(st: &AppState, d: &Declared) -> Result<(), String> {
    build(st, d, st.settings.server.runtime())
}

/// Serve the effective model configuration.
pub fn apply(st: &AppState) -> Result<(), String> {
    apply_with(st, &st.settings.declared())
}

/// Serve the effective configuration, or when it is not valid, the declared one alone:
/// a runtime layer that a change of `--model-config` or of the locks made invalid
/// neither stops the server nor keeps it from starting, and its status reports why.
fn apply_or_declared(st: &AppState, d: &Declared) -> Result<(), String> {
    match apply_with(st, d) {
        Ok(()) => Ok(()),
        Err(e) => {
            tracing::error!(
                "the runtime model settings do not apply, the declared configuration is used until they are changed: {e}"
            );
            if st.models().is_none() {
                build(st, d, Value::Object(Map::new()))?;
            }
            Err(e)
        }
    }
}

/// Read the settings file, the model configuration and the runtime layer at the start
/// of `serve`, and serve the effective configuration. A settings file or a model
/// configuration that does not load fails the start, and so does a settings file whose
/// dataset entries name providers that the effective configuration lacks.
pub fn start(st: &mut AppState, settings: Option<&Path>, args: &ModelArgs) -> anyhow::Result<()> {
    // read without the provider checks, which need the models that its locks shape
    let declared = settings
        .map(|p| read_file(p, Providers::Unchecked))
        .transpose()?;
    let mut s = super::Settings::default();
    // the stores' prefix filters hold the handle, so it is kept
    s.declared = st.settings.declared.clone();
    if let (Some(path), Some(d)) = (settings, declared) {
        s.file = Some(path.to_path_buf());
        s.declared.store(Arc::new(d));
        s.status.lock().read_at = Some(super::now());
    }
    s.server.set_dir(&st.data_dir.clone());
    s.server.load_declared(args)?;
    s.server.load_runtime()?;
    s.set_models_file(args.model_config.clone());
    st.settings = s;
    let d = st.settings.declared();
    if let Err(e) = apply_or_declared(st, &d) {
        st.settings.models_reloaded(Err(e));
    }
    if settings.is_some() {
        d.validate(Providers::Checked(st.models().as_deref()))
            .map_err(|e| {
                anyhow::anyhow!(
                    "settings file {}: {e}",
                    settings.unwrap_or(Path::new("")).display()
                )
            })?;
    }
    Ok(())
}

/// What SIGHUP does: read `--model-config` and `--model-secret` again, the runtime
/// layer of `models` and the settings file, then serve the configuration they give. A
/// model configuration or a settings file that does not load is logged and the previous
/// one kept, and so is a settings file whose dataset entries name providers the new
/// configuration lacks. Requests in flight keep the configuration they started with.
pub fn reload(st: &AppState, args: &ModelArgs) {
    let settings = &st.settings;
    let lock = settings.write_lock("", &MODELS);
    let _g = lock.lock();
    let mut outcome: Result<(), String> = Ok(());
    if let Err(e) = settings.server.load_declared(args) {
        let msg = format!("{e:#}");
        tracing::error!("model configuration not reloaded, the previous one stays: {msg}");
        outcome = Err(msg);
    }
    if let Err(e) = settings.server.load_runtime() {
        let msg = format!("{e:#}");
        tracing::error!("runtime model settings not reloaded, the previous ones stay: {msg}");
        outcome = outcome.and(Err(msg));
    }
    let previous = settings.declared();
    let candidate = settings
        .file
        .as_ref()
        .map(|p| read_file(p, Providers::Unchecked));
    let locks = match &candidate {
        Some(Ok(d)) => d.clone(),
        _ => (*previous).clone(),
    };
    if let Err(e) = apply_or_declared(st, &locks) {
        outcome = outcome.and(Err(e));
    }
    if args.model_config.is_some() || outcome.is_err() {
        if outcome.is_ok() {
            tracing::info!("model configuration reloaded");
        }
        settings.models_reloaded(outcome);
    }
    // the settings file, checked against the models it shapes
    let models = st.models();
    let checked = match candidate {
        None => return,
        Some(Err(e)) => Err(format!("{e:#}")),
        Some(Ok(d)) => match d.validate(Providers::Checked(models.as_deref())) {
            Ok(()) => Ok(d),
            Err(e) => Err(format!(
                "settings file {}: {e}",
                settings.file.as_deref().unwrap_or(Path::new("")).display()
            )),
        },
    };
    match checked {
        Ok(d) => {
            settings.declared.store(Arc::new(d));
            let mut s = settings.status.lock();
            s.read_at = Some(super::now());
            s.error = None;
            s.error_at = None;
            tracing::info!("settings reloaded");
        }
        Err(msg) => {
            tracing::error!("settings not reloaded, the previous ones stay: {msg}");
            {
                let mut s = settings.status.lock();
                s.error = Some(msg);
                s.error_at = Some(super::now());
            }
            // the models follow the locks of the file that stays
            let _ = apply_or_declared(st, &previous);
        }
    }
}

// ------------------------------------------------------------------- writes ------

/// The fields where `a` and `b` differ: the paths of the members that one side lacks or
/// that differ, without descending into a member that only one side has.
fn changed(a: &Value, b: &Value) -> Vec<String> {
    fn walk(a: Option<&Value>, b: Option<&Value>, path: &mut Vec<String>, out: &mut Vec<String>) {
        match (a, b) {
            (Some(Value::Object(x)), Some(Value::Object(y))) => {
                let keys: BTreeSet<&String> = x.keys().chain(y.keys()).collect();
                for k in keys {
                    path.push(k.clone());
                    walk(x.get(k), y.get(k), path, out);
                    path.pop();
                }
            }
            (x, y) if x != y => out.push(path_string(path)),
            _ => {}
        }
    }
    let mut out = Vec::new();
    walk(Some(a), Some(b), &mut Vec::new(), &mut out);
    out
}

/// Apply `w` to the runtime layer of a server-wide kind (§11.3): check `If-Match`, the
/// locks and the effective object after the change, store the layer, serve the new
/// configuration and record the change under `sparkles::audit`.
pub async fn write(
    st: Arc<AppState>,
    kind: &'static Kind,
    w: Write,
    headers: &HeaderMap,
    principal: String,
) -> ApiResult<Resolved> {
    super::http::refuse_read_only(&st)?;
    let if_match = if_match(headers);
    let op: Op = op_of(w, kind)?;
    let operation = match &op {
        Op::Put(_) => "put",
        Op::Patch(_) => "patch",
        Op::Delete(_) => "delete",
    };
    blocking(move || {
        let lock = st.settings.write_lock("", kind);
        let _g = lock.lock();
        let d = st.settings.declared();
        let cur = resolve_models(&st, &d, st.settings.server.runtime());
        let new = plan(&cur, op, if_match.as_deref(), "the server")?;
        let r = resolve_models(&st, &d, new);
        if let Err(e) = &r.status {
            return Err(bad(format!("{}: {e}", kind.name)));
        }
        if r.runtime == cur.runtime {
            return Ok(r);
        }
        st.settings.server.store_runtime(&r.runtime).map_err(|e| {
            err(
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("cannot store {MODELS_FILE}: {e}"),
            )
        })?;
        if let Err(e) = apply_with(&st, &d) {
            tracing::error!("model configuration not rebuilt: {e}");
        }
        let fields = changed(&cur.effective, &r.effective);
        tracing::info!(
            target: "sparkles::audit",
            event = "server_settings_changed",
            kind = kind.name,
            operation,
            fields = fields.join(", ").as_str(),
            principal = principal.as_str()
        );
        super::log_invalid(&st);
        Ok(r)
    })
    .await
}
