//! The runtime values of model secrets (spec C19 §11.2).
//!
//! A provider names its key with `apiKey.secret`. The declared source of a secret is
//! `--model-secret NAME=env:VAR|file:PATH`, and a server administrator may store a
//! runtime value with `PUT /$/server/secrets/{name}`, which overrides the declared source
//! unless the settings file locks `secrets.NAME`. A runtime value is kept in
//! `<dataDir>/secrets/NAME` with mode 0600, in a directory with mode 0700, and the model
//! configuration reads it as a `file:` source at each request.
//!
//! With `serve --secrets-key SOURCE`, each runtime value is sealed with that key (see
//! [`Sealing`]), and plaintext values found at the start are sealed in place.
//!
//! No route returns a value, and none is logged. The body of a `PUT` is read into a
//! [`SecretValue`], whose `Debug` prints nothing of it, and errors about the body never
//! quote it. `GET /$/server/secrets` reads only the names and times of the files.

use super::http::who;
use crate::auth::Principal;
use crate::http::{AdminBody, ApiResult, blocking, err_body, err_code};
use crate::state::AppState;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, put};
use axum::{Extension, Json, Router};
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};
use std::io::Write as _;
use std::path::Path as FsPath;
use std::sync::Arc;
use std::time::SystemTime;

type St = State<Arc<AppState>>;

/// The directory of runtime secrets, in the data directory.
pub const SECRETS_DIR: &str = "secrets";

/// The longest value a secret may hold, in bytes.
const MAX_VALUE: usize = 16 * 1024;

pub fn routes() -> Router<Arc<AppState>> {
    Router::new().route("/$/server/secrets", get(list)).route(
        "/$/server/secrets/{name}",
        put(put_secret).delete(delete_secret),
    )
}

/// A secret's value, read from a request body. Its `Debug` prints nothing of it, it has
/// no `Display` and no `Serialize`, so it can reach neither a log line nor an answer.
pub struct SecretValue(String);

impl std::fmt::Debug for SecretValue {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("SecretValue([redacted])")
    }
}

impl SecretValue {
    /// The value of a `PUT` body `{"value": "..."}`. The error never quotes the body.
    pub fn from_body(body: &[u8]) -> Result<SecretValue, String> {
        const SHAPE: &str = "the body must be {\"value\": \"...\"} with the key as a string";
        let v: Value = serde_json::from_slice(body).map_err(|_| SHAPE.to_string())?;
        let Value::Object(mut m) = v else {
            return Err(SHAPE.into());
        };
        if m.len() != 1 {
            return Err(SHAPE.into());
        }
        let Some(Value::String(s)) = m.remove("value") else {
            return Err(SHAPE.into());
        };
        let t = s.trim();
        if t.is_empty() {
            return Err("the value is empty".into());
        }
        if t.len() > MAX_VALUE {
            return Err(format!("the value is longer than {MAX_VALUE} bytes"));
        }
        if t.chars().any(char::is_control) {
            return Err("the value holds a control character, such as a line break".into());
        }
        if t.starts_with(SEALED_MARK) {
            return Err("the value looks like a sealed secret file".into());
        }
        Ok(SecretValue(t.to_string()))
    }

    fn bytes(&self) -> &[u8] {
        self.0.as_bytes()
    }
}

/// The start of a sealed value, as `sparkles_backup::crypto::SEALED_SECRET_PREFIX`
/// writes it. A build without `backup-encryption` still recognizes sealed files.
const SEALED_MARK: &str = "sparkles-sealed-secret/";

fn is_sealed(bytes: &[u8]) -> bool {
    bytes.starts_with(SEALED_MARK.as_bytes())
}

/// How runtime values are kept: sealed with the key of `serve --secrets-key`, or as
/// plaintext files with mode 0600 without one.
#[derive(Clone, Debug, Default)]
pub struct Sealing {
    #[cfg(feature = "backup-encryption")]
    sealer: Option<sparkles_backup::crypto::SecretSealer>,
}

impl Sealing {
    /// Read the key of `--secrets-key SOURCE` once. `SOURCE` is `file:PATH`,
    /// `env:VAR`, `credential:NAME` (in `$CREDENTIALS_DIRECTORY`) or `command:CMD`,
    /// where `CMD` is a JSON argv array or an absolute program and its arguments split
    /// at spaces, run without a shell. The key is 32 bytes, as 64 hexadecimal digits,
    /// base64 or raw bytes, like a backup repository key. A key file may not lie in
    /// `data_dir`.
    #[cfg(feature = "backup-encryption")]
    pub fn load(source: &str, data_dir: &FsPath) -> anyhow::Result<Sealing> {
        use sparkles::backup::config::{KeyInput, KeySource, RepositoryEncryption};
        use sparkles::backup::keys::{KeyContext, resolve};
        let bad = || {
            anyhow::anyhow!(
                "--secrets-key takes file:PATH, env:VAR, credential:NAME or command:CMD"
            )
        };
        let (kind, rest) = source.split_once(':').ok_or_else(bad)?;
        if rest.is_empty() {
            return Err(bad());
        }
        let key = match kind {
            "file" => KeySource::File {
                path: std::path::absolute(rest)?.to_string_lossy().into_owned(),
            },
            "env" => KeySource::Env { var: rest.into() },
            "credential" => KeySource::Credential { name: rest.into() },
            "command" => KeySource::Command {
                argv: if rest.trim_start().starts_with('[') {
                    serde_json::from_str(rest).map_err(|_| {
                        anyhow::anyhow!(
                            "--secrets-key command:[...] must be a JSON array of strings"
                        )
                    })?
                } else {
                    rest.split_whitespace().map(str::to_string).collect()
                },
                timeout_secs: 10,
            },
            _ => return Err(bad()),
        };
        let settings = RepositoryEncryption {
            keys: vec![KeyInput {
                label: "secrets-key".into(),
                key,
            }],
            single_key_ok: true,
        };
        settings
            .validate()
            .map_err(|e| anyhow::anyhow!("--secrets-key: {e}"))?;
        let context = KeyContext::from_environment(vec![data_dir.to_path_buf()]);
        let ctl = sparkles_backup::Ctl::with_cancel(Default::default());
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()?;
        let mut keys = rt
            .block_on(resolve(&settings, &context, &ctl))
            .map_err(|e| anyhow::anyhow!("--secrets-key: {e}"))?;
        let key = keys
            .keys
            .pop()
            .ok_or_else(|| anyhow::anyhow!("--secrets-key: no key was read"))?;
        Ok(Sealing {
            sealer: Some(sparkles_backup::crypto::SecretSealer::new(&key)),
        })
    }

    /// `--secrets-key` in a build without `backup-encryption`.
    #[cfg(not(feature = "backup-encryption"))]
    pub fn load(_source: &str, _data_dir: &FsPath) -> anyhow::Result<Sealing> {
        anyhow::bail!("--secrets-key needs a build with the backup-encryption feature")
    }

    /// `"sealed"` with a key, else `"plaintext"`.
    pub fn storage(&self) -> &'static str {
        if self.sealed() { "sealed" } else { "plaintext" }
    }

    pub fn sealed(&self) -> bool {
        #[cfg(feature = "backup-encryption")]
        {
            self.sealer.is_some()
        }
        #[cfg(not(feature = "backup-encryption"))]
        {
            false
        }
    }

    /// Store `value` as the runtime value of `name` in `dir`, sealed with a key.
    pub(crate) fn write(&self, dir: &FsPath, name: &str, value: &[u8]) -> std::io::Result<()> {
        #[cfg(feature = "backup-encryption")]
        if let Some(s) = &self.sealer {
            let sealed = s.seal(name, value).map_err(std::io::Error::other)?;
            return write_atomic(&dir.join(name), &sealed, 0o600);
        }
        write_atomic(&dir.join(name), value, 0o600)
    }

    /// The value in `stored` of the secret `name`, opened when it is sealed. The error
    /// never holds the value.
    fn open(&self, name: &str, stored: Vec<u8>) -> Result<String, String> {
        if !is_sealed(&stored) {
            return String::from_utf8(stored)
                .map_err(|_| format!("the stored value of secret {name:?} is not text"));
        }
        #[cfg(feature = "backup-encryption")]
        if let Some(s) = &self.sealer {
            let plain = s.open(name, &stored).map_err(|_| {
                format!("the stored value of secret {name:?} does not open with the secrets key")
            })?;
            return String::from_utf8(plain.to_vec())
                .map_err(|_| format!("the stored value of secret {name:?} is not text"));
        }
        Err(format!(
            "the stored value of secret {name:?} is sealed, and the server has no --secrets-key"
        ))
    }

    /// The runtime value of `name` in the file `path`, trimmed.
    pub fn read(&self, path: &FsPath, name: &str) -> Result<String, String> {
        let bytes = std::fs::read(path)
            .map_err(|_| format!("the stored value of secret {name:?} cannot be read"))?;
        self.open(name, bytes).map(|v| v.trim().to_string())
    }

    /// Check the runtime values in `dir` at the start. Every sealed value must open,
    /// so a wrong key, a changed file or a file renamed to another secret stops the
    /// start. With a key, each plaintext value is sealed in place. Without one, a sealed
    /// value stops the start.
    pub fn prepare(&self, dir: &FsPath) -> anyhow::Result<()> {
        let mut failed = Vec::new();
        let mut migrated = 0usize;
        for name in stored(dir).into_keys() {
            let bytes = std::fs::read(dir.join(&name))
                .map_err(|e| anyhow::anyhow!("cannot read secret {name}: {e}"))?;
            let was_sealed = is_sealed(&bytes);
            match self.open(&name, bytes) {
                Err(_) => failed.push(name),
                Ok(v) if !was_sealed && self.sealed() => {
                    self.write(dir, &name, v.trim().as_bytes())
                        .map_err(|e| anyhow::anyhow!("cannot seal secret {name}: {e}"))?;
                    migrated += 1;
                }
                Ok(_) => {}
            }
        }
        if !failed.is_empty() {
            let names = failed.join(", ");
            if self.sealed() {
                anyhow::bail!(
                    "the secrets key does not open the stored secrets {names} in {}: it is not the key they were sealed with, or a file was changed or renamed",
                    dir.display()
                );
            }
            anyhow::bail!(
                "the stored secrets {names} in {} are sealed: start the server with --secrets-key",
                dir.display()
            );
        }
        if migrated > 0 {
            tracing::info!("sealed {migrated} plaintext runtime secret(s) with the secrets key");
        }
        Ok(())
    }
}

/// Make `dir` with mode 0700, or set that mode on it.
fn private_dir(dir: &FsPath) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
        std::fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(dir)?;
        std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))
    }
    #[cfg(not(unix))]
    {
        std::fs::create_dir_all(dir)
    }
}

/// Write `bytes` to `path` atomically: a new file in the same directory with `mode`,
/// flushed to disk, then renamed over `path`, and the directory flushed.
pub(crate) fn write_atomic(path: &FsPath, bytes: &[u8], mode: u32) -> std::io::Result<()> {
    let dir = path.parent().unwrap_or(FsPath::new("."));
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let tmp = dir.join(format!(".{name}.{}.tmp", std::process::id()));
    let _ = std::fs::remove_file(&tmp);
    let mut o = std::fs::OpenOptions::new();
    o.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        o.mode(mode);
    }
    #[cfg(not(unix))]
    let _ = mode;
    let written = (|| {
        let mut f = o.open(&tmp)?;
        f.write_all(bytes)?;
        f.sync_all()?;
        drop(f);
        std::fs::rename(&tmp, path)
    })();
    if written.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    written?;
    #[cfg(unix)]
    if let Ok(d) = std::fs::File::open(dir) {
        let _ = d.sync_all();
    }
    Ok(())
}

/// The runtime values in `dir`, by name, with the time each was set. Only the names and
/// times are read, never the values.
pub fn stored(dir: &FsPath) -> BTreeMap<String, SystemTime> {
    let mut out = BTreeMap::new();
    let Ok(rd) = std::fs::read_dir(dir) else {
        return out;
    };
    for e in rd.flatten() {
        let Some(name) = e.file_name().to_str().map(str::to_string) else {
            continue;
        };
        if crate::models::check_secret_name(&name).is_err() {
            continue;
        }
        let Ok(md) = e.metadata() else {
            continue;
        };
        if md.is_file() {
            out.insert(name, md.modified().unwrap_or(SystemTime::UNIX_EPOCH));
        }
    }
    out
}

fn rfc3339(t: SystemTime) -> String {
    chrono::DateTime::<chrono::Utc>::from(t).to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
}

fn name_of(name: &str) -> ApiResult<()> {
    crate::models::check_secret_name(name)
        .map_err(|e| err_code(StatusCode::BAD_REQUEST, "bad-secret-name", e))
}

fn no_dir() -> crate::http::ApiError {
    err_code(
        StatusCode::CONFLICT,
        "no-data-directory",
        "this server has no data directory to keep secrets in",
    )
}

/// `GET /$/server/secrets`: every secret that a source, a stored value, a provider or a
/// lock names.
async fn list(State(st): St) -> ApiResult<Json<Value>> {
    blocking(move || {
        let layers = &st.settings.server;
        let d = st.settings.declared();
        let declared = layers.secret_sources();
        let stored = layers.secrets_dir().map(|p| stored(&p)).unwrap_or_default();
        let mut users: BTreeMap<String, Vec<String>> = BTreeMap::new();
        if let Some(m) = st.models() {
            for (provider, p) in &m.config.providers {
                for name in p.secret_names() {
                    let u = users.entry(name.to_string()).or_default();
                    if !u.contains(provider) {
                        u.push(provider.clone());
                    }
                }
            }
        }
        let channels = crate::notify::secret_users(&st);
        let names: BTreeSet<String> = declared
            .keys()
            .cloned()
            .chain(stored.keys().cloned())
            .chain(users.keys().cloned())
            .chain(channels.keys().cloned())
            .chain(d.locked_secrets().map(str::to_string))
            .collect();
        let secrets: Vec<Value> = names
            .into_iter()
            .map(|name| {
                let locked = d.secret_locked(&name);
                let set = stored.get(&name).copied();
                let source = if set.is_some() && !locked {
                    "runtime"
                } else if declared.contains_key(&name) {
                    "declared"
                } else {
                    "missing"
                };
                json!({
                    "name": name,
                    "source": source,
                    "declared": declared.contains_key(&name),
                    "locked": locked,
                    "setAt": set.map(rfc3339),
                    "overridden": set.is_some() && locked,
                    "providers": users.get(&name).cloned().unwrap_or_default(),
                    "channels": channels.get(&name).cloned().unwrap_or_default(),
                })
            })
            .collect();
        Ok(Json(
            json!({ "storage": layers.sealing.storage(), "secrets": secrets }),
        ))
    })
    .await
}

/// `PUT /$/server/secrets/{name}` with `{"value": "..."}`: store a runtime value, `204`.
async fn put_secret(
    State(st): St,
    Path(name): Path<String>,
    p: Option<Extension<Principal>>,
    AdminBody(body): AdminBody,
) -> ApiResult<Response> {
    super::http::refuse_read_only(&st)?;
    name_of(&name)?;
    let value = SecretValue::from_body(&body)
        .map_err(|e| err_code(StatusCode::BAD_REQUEST, "bad-secret", e))?;
    drop(body);
    let principal = who(p);
    blocking(move || {
        let lock = st.settings.write_lock("", &super::MODELS);
        let _g = lock.lock();
        let d = st.settings.declared();
        if d.secret_locked(&name) {
            let field = format!("secrets.{name}");
            return Err(err_body(
                StatusCode::CONFLICT,
                json!({
                    "error": format!("the settings file locks {field}; its declared source applies"),
                    "code": "locked-by-config",
                    "fields": [field],
                }),
            ));
        }
        let dir = st.settings.server.secrets_dir().ok_or_else(no_dir)?;
        private_dir(&dir).map_err(|e| internal(&name, e))?;
        st.settings
            .server
            .sealing
            .write(&dir, &name, value.bytes())
            .map_err(|e| internal(&name, e))?;
        drop(value);
        if let Err(e) = super::server::apply_with(&st, &d) {
            tracing::error!("model configuration not rebuilt after storing secret {name}: {e}");
        }
        tracing::info!(
            target: "sparkles::audit",
            event = "secret_set",
            secret = name.as_str(),
            principal = principal.as_str()
        );
        Ok(StatusCode::NO_CONTENT.into_response())
    })
    .await
}

/// `DELETE /$/server/secrets/{name}`: remove the runtime value, so the declared source
/// applies again, `204` whether or not there was one.
async fn delete_secret(
    State(st): St,
    Path(name): Path<String>,
    p: Option<Extension<Principal>>,
) -> ApiResult<Response> {
    super::http::refuse_read_only(&st)?;
    name_of(&name)?;
    let principal = who(p);
    blocking(move || {
        let lock = st.settings.write_lock("", &super::MODELS);
        let _g = lock.lock();
        let Some(dir) = st.settings.server.secrets_dir() else {
            return Ok(StatusCode::NO_CONTENT.into_response());
        };
        let removed = match std::fs::remove_file(dir.join(&name)) {
            Ok(()) => true,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => false,
            Err(e) => return Err(internal(&name, e)),
        };
        if removed {
            if let Err(e) = super::server::apply(&st) {
                tracing::error!(
                    "model configuration not rebuilt after removing secret {name}: {e}"
                );
            }
            tracing::info!(
                target: "sparkles::audit",
                event = "secret_removed",
                secret = name.as_str(),
                principal = principal.as_str()
            );
        }
        Ok(StatusCode::NO_CONTENT.into_response())
    })
    .await
}

/// A failure to write or remove a secret's file: the name and the error of the file
/// system, which never holds the value.
fn internal(name: &str, e: std::io::Error) -> crate::http::ApiError {
    crate::http::err(
        StatusCode::INTERNAL_SERVER_ERROR,
        format!("cannot store secret {name}: {e}"),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn values_are_read_without_being_quoted() {
        let v = SecretValue::from_body(br#"{"value": "  sk-abc  "}"#).unwrap();
        assert_eq!(v.bytes(), b"sk-abc");
        assert_eq!(format!("{v:?}"), "SecretValue([redacted])");
        for body in [
            &br#"sk-abc"#[..],
            br#"{"value": "sk-abc", "x": 1}"#,
            br#"{"valu": "sk-abc"}"#,
            br#"{"value": ["sk-abc"]}"#,
            br#"["sk-abc"]"#,
            br#"{"value": "sk-a\nbc"}"#,
            br#"{"value": "   "}"#,
        ] {
            let e = SecretValue::from_body(body).unwrap_err();
            assert!(!e.contains("sk-"), "{e}");
        }
    }

    #[cfg(unix)]
    #[test]
    fn files_are_private() {
        use std::os::unix::fs::PermissionsExt;
        let t = tempfile::tempdir().unwrap();
        let dir = t.path().join("secrets");
        private_dir(&dir).unwrap();
        write_atomic(&dir.join("a"), b"x", 0o600).unwrap();
        write_atomic(&dir.join("a"), b"y", 0o600).unwrap();
        let mode = |p: &FsPath| std::fs::metadata(p).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode(&dir), 0o700);
        assert_eq!(mode(&dir.join("a")), 0o600);
        assert_eq!(std::fs::read(dir.join("a")).unwrap(), b"y");
        std::fs::write(dir.join(".hidden"), "z").unwrap();
        assert_eq!(stored(&dir).keys().collect::<Vec<_>>(), ["a"]);
    }
}
