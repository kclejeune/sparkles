//! The runtime values of model secrets (spec C19 §11.2).
//!
//! A provider names its key with `apiKey.secret`. The declared source of a secret is
//! `--model-secret NAME=env:VAR|file:PATH`, and a server administrator may store a
//! runtime value with `PUT /$/server/secrets/{name}`, which overrides the declared source
//! unless the settings file locks `secrets.NAME`. A runtime value is kept in
//! `<dataDir>/secrets/NAME` with mode 0600, in a directory with mode 0700, and the model
//! configuration reads it as a `file:` source at each request.
//!
//! Each runtime value is sealed (see [`Sealing`]) with the key of `serve --secrets-key
//! SOURCE`, the systemd credential `sparkles-secrets-key`, or a key that the server
//! generates in `<dataDir>/secrets.key` when it first stores a value. Plaintext values
//! found at the start are sealed in place. `--secrets-key none` keeps plaintext files.
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
use parking_lot::RwLock;
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};
use std::io::Write as _;
use std::path::{Path as FsPath, PathBuf};
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

/// The start of a sealed value, as `sparkles_backup::SEALED_SECRET_PREFIX` writes it. A
/// build without the `backup` feature still recognizes sealed files.
const SEALED_MARK: &str = "sparkles-sealed-secret/";

fn is_sealed(bytes: &[u8]) -> bool {
    bytes.starts_with(SEALED_MARK.as_bytes())
}

/// The file in the data directory that holds a key the server generated. Server backups
/// leave it out, so that the sealed values they hold do not open without the key.
pub const SECRETS_KEY_FILE: &str = "secrets.key";

/// The systemd credential that holds the secrets key when `--secrets-key` is absent.
pub const SECRETS_KEY_CREDENTIAL: &str = "sparkles-secrets-key";

/// The sealer of the backup crate, or an empty stand-in in a build without it.
#[cfg(feature = "backup")]
mod sealer {
    use anyhow::Context as _;
    pub use sparkles_backup::SecretSealer as Sealer;
    use std::path::Path;
    use zeroize::Zeroizing;

    pub fn seal(s: &Sealer, name: &str, value: &[u8]) -> std::io::Result<Vec<u8>> {
        s.seal(name, value).map_err(std::io::Error::other)
    }

    pub fn open(s: &Sealer, name: &str, stored: &[u8]) -> Option<Zeroizing<Vec<u8>>> {
        s.open(name, stored).ok()
    }

    pub fn memory(s: &Sealer) -> &'static str {
        s.key_memory().as_str()
    }

    /// The key of `--secrets-key SOURCE`, read once. See [`super::Sealing::start`].
    pub fn configured(source: &str, data_dir: &Path) -> anyhow::Result<Sealer> {
        use sparkles::backup::config::{KeyInput, KeySource, RepositoryEncryption};
        use sparkles::backup::keys::{KeyContext, read_key};
        let bad = || {
            anyhow::anyhow!(
                "--secrets-key takes file:PATH, env:VAR, credential:NAME, command:CMD or none"
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
                key: key.clone(),
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
        let key = rt
            .block_on(read_key(&key, "secrets-key", &context, &ctl))
            .map_err(|e| anyhow::anyhow!("--secrets-key: {e}"))?;
        Ok(Sealer::from_key(key))
    }

    /// The key in `path`, a file the server generated. It must be a regular file that
    /// only its owner can read, owned by the user the server runs as or by root.
    pub fn read_file(path: &Path) -> anyhow::Result<Sealer> {
        use std::io::Read as _;
        let shown = path.display();
        let mut o = std::fs::OpenOptions::new();
        o.read(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            o.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
        }
        let f = o
            .open(path)
            .with_context(|| format!("cannot open the secrets key {shown}"))?;
        let md = f.metadata()?;
        if !md.is_file() || md.len() > 4096 {
            anyhow::bail!("the secrets key {shown} must be a regular file of at most 4096 bytes");
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::{MetadataExt, PermissionsExt};
            if md.permissions().mode() & 0o077 != 0 {
                anyhow::bail!(
                    "the secrets key {shown} must not be readable by its group or by others (chmod 0400)"
                );
            }
            // SAFETY: geteuid has no preconditions and cannot fail.
            let me = unsafe { libc::geteuid() };
            if md.uid() != me && md.uid() != 0 {
                anyhow::bail!(
                    "the secrets key {shown} must belong to the user the server runs as, or to root"
                );
            }
        }
        let mut bytes = Zeroizing::new(Vec::with_capacity(129));
        f.take(4097)
            .read_to_end(&mut bytes)
            .with_context(|| format!("cannot read the secrets key {shown}"))?;
        let key = sparkles::backup::keys::decode_key(&bytes, SECRETS_KEY_LABEL)
            .map_err(|e| anyhow::anyhow!("{e} ({shown})"))?;
        Ok(Sealer::from_key(key))
    }

    const SECRETS_KEY_LABEL: &str = super::SECRETS_KEY_FILE;

    /// Generate a key and write it to `path` with mode 0400, as 64 hexadecimal digits.
    /// The data directory must belong to the user the server runs as. The file appears
    /// whole or not at all, and an existing file is never replaced.
    pub fn generate(path: &Path) -> std::io::Result<Sealer> {
        use std::io::Write as _;
        let dir = path.parent().unwrap_or(Path::new("."));
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            // SAFETY: geteuid has no preconditions and cannot fail.
            let me = unsafe { libc::geteuid() };
            if std::fs::metadata(dir)?.uid() != me {
                return Err(std::io::Error::other(format!(
                    "the data directory {} belongs to another user, so the server does not keep a key in it: change its owner to the user the server runs as, start it with --secrets-key SOURCE, or start it with --secrets-key none to keep the values as plaintext",
                    dir.display()
                )));
            }
        }
        let key = Sealer::generate_key().map_err(std::io::Error::other)?;
        let mut text = Zeroizing::new(String::with_capacity(65));
        for b in key.iter() {
            text.push(char::from_digit(u32::from(b >> 4), 16).unwrap_or('0'));
            text.push(char::from_digit(u32::from(b & 15), 16).unwrap_or('0'));
        }
        text.push('\n');
        let tmp = dir.join(format!(
            ".{}.{}.tmp",
            super::SECRETS_KEY_FILE,
            std::process::id()
        ));
        let _ = std::fs::remove_file(&tmp);
        let mut o = std::fs::OpenOptions::new();
        o.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            o.mode(0o400);
        }
        let written = (|| {
            let mut f = o.open(&tmp)?;
            // the umask may have reduced the mode that the file was made with
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                f.set_permissions(std::fs::Permissions::from_mode(0o400))?;
            }
            f.write_all(text.as_bytes())?;
            f.sync_all()?;
            drop(f);
            // a link fails when the name exists, where a rename would replace it
            std::fs::hard_link(&tmp, path)
        })();
        let _ = std::fs::remove_file(&tmp);
        written?;
        // the name must survive a crash before any value is sealed with the key
        if let Err(e) = super::sync_dir(dir) {
            let _ = std::fs::remove_file(path);
            return Err(std::io::Error::other(format!(
                "cannot make the secrets key {} durable: {e}",
                path.display()
            )));
        }
        Ok(Sealer::from_key(key))
    }
}

#[cfg(not(feature = "backup"))]
mod sealer {
    use std::path::Path;

    /// A build without the `backup` feature never makes a sealer: every constructor
    /// below fails.
    #[derive(Clone, Debug)]
    #[allow(dead_code)]
    pub struct Sealer(());

    pub fn seal(_: &Sealer, _: &str, _: &[u8]) -> std::io::Result<Vec<u8>> {
        Err(std::io::Error::other(
            "sealed secrets need a build with the backup feature",
        ))
    }

    pub fn open(_: &Sealer, _: &str, _: &[u8]) -> Option<Vec<u8>> {
        None
    }

    pub fn memory(_: &Sealer) -> &'static str {
        "heap"
    }

    pub fn configured(_: &str, _: &Path) -> anyhow::Result<Sealer> {
        anyhow::bail!("--secrets-key needs a build with the backup feature")
    }

    pub fn read_file(_: &Path) -> anyhow::Result<Sealer> {
        anyhow::bail!("sealed secrets need a build with the backup feature")
    }

    pub fn generate(_: &Path) -> std::io::Result<Sealer> {
        Err(std::io::Error::other(
            "sealed secrets need a build with the backup feature",
        ))
    }
}

/// Where the key that seals runtime values comes from.
#[derive(Clone, Debug, Default)]
enum Mode {
    /// No key: values are plaintext files with mode 0600 (`--secrets-key none`, a server
    /// without a data directory, or a build without the `backup` feature).
    #[default]
    Plaintext,
    /// `--secrets-key SOURCE`, or the systemd credential `sparkles-secrets-key`.
    Configured,
    /// `<dataDir>/secrets.key`, generated when the first value is stored.
    Generated(PathBuf),
}

#[derive(Debug, Default)]
struct SealingState {
    mode: Mode,
    /// The sealer, once a key is read or generated.
    sealer: RwLock<Option<sealer::Sealer>>,
}

/// How runtime values are kept: sealed with a key, which by default the server generates
/// in its data directory, or as plaintext files with mode 0600 with `--secrets-key none`.
/// Clones share the key, so a key generated for one request serves every reader.
#[derive(Clone, Debug, Default)]
pub struct Sealing {
    state: Arc<SealingState>,
}

impl Sealing {
    /// The sealing of `serve` with `--secrets-key SOURCE` or without it, read once at
    /// the start, before the server's runtime starts.
    ///
    /// `SOURCE` is `file:PATH`, `env:VAR`, `credential:NAME` (in
    /// `$CREDENTIALS_DIRECTORY`) or `command:CMD`, where `CMD` is a JSON argv array or an
    /// absolute program and its arguments split at spaces, run without a shell. The key
    /// is 32 bytes, as 64 hexadecimal digits, base64 or raw bytes, like a backup
    /// repository key. A key file may not lie in `data_dir`. `none` keeps values as
    /// plaintext.
    ///
    /// Without `SOURCE`, the key is the systemd credential `sparkles-secrets-key` when
    /// there is one, then `<data_dir>/secrets.key`. When neither exists, the server
    /// generates `secrets.key` when it first stores a value.
    pub fn start(source: Option<&str>, data_dir: &FsPath) -> anyhow::Result<Sealing> {
        let configured = |source: &str| -> anyhow::Result<Sealing> {
            Ok(Sealing::with(
                Mode::Configured,
                Some(sealer::configured(source, data_dir)?),
            ))
        };
        match source.map(str::trim) {
            Some("none") => return Ok(Sealing::default()),
            Some(source) => return configured(source),
            None => {}
        }
        if data_dir.as_os_str().is_empty() || !cfg!(feature = "backup") {
            return Ok(Sealing::default());
        }
        if let Some(creds) = std::env::var_os("CREDENTIALS_DIRECTORY")
            && FsPath::new(&creds).join(SECRETS_KEY_CREDENTIAL).is_file()
        {
            return configured(&format!("credential:{SECRETS_KEY_CREDENTIAL}"));
        }
        let path = data_dir.join(SECRETS_KEY_FILE);
        let sealer = match std::fs::symlink_metadata(&path) {
            Ok(_) => Some(sealer::read_file(&path)?),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
            Err(e) => anyhow::bail!("cannot read the secrets key {}: {e}", path.display()),
        };
        Ok(Sealing::with(Mode::Generated(path), sealer))
    }

    fn with(mode: Mode, sealer: Option<sealer::Sealer>) -> Sealing {
        Sealing {
            state: Arc::new(SealingState {
                mode,
                sealer: RwLock::new(sealer),
            }),
        }
    }

    /// `"sealed"` when values are sealed, else `"plaintext"`.
    pub fn storage(&self) -> &'static str {
        if self.sealed() { "sealed" } else { "plaintext" }
    }

    /// Whether stored values are sealed, or will be once a key is generated.
    pub fn sealed(&self) -> bool {
        !matches!(self.state.mode, Mode::Plaintext)
    }

    /// `"configured"` for `--secrets-key` or the systemd credential, `"generated"` for
    /// `<dataDir>/secrets.key`, and `None` for plaintext values.
    pub fn key_source(&self) -> Option<&'static str> {
        match self.state.mode {
            Mode::Plaintext => None,
            Mode::Configured => Some("configured"),
            Mode::Generated(_) => Some("generated"),
        }
    }

    /// `"locked"` or `"heap"`, where the key is kept, once there is a key.
    pub fn key_memory(&self) -> Option<&'static str> {
        self.state.sealer.read().as_ref().map(sealer::memory)
    }

    /// The sealer for a write: the key that was read, or a key generated now in the data
    /// directory. `None` for plaintext values.
    fn sealer_for_write(&self) -> std::io::Result<Option<sealer::Sealer>> {
        let path = match &self.state.mode {
            Mode::Plaintext => return Ok(None),
            Mode::Configured => return Ok(self.state.sealer.read().clone()),
            Mode::Generated(path) => path,
        };
        if let Some(s) = self.state.sealer.read().clone() {
            return Ok(Some(s));
        }
        let mut slot = self.state.sealer.write();
        if let Some(s) = slot.clone() {
            return Ok(Some(s));
        }
        let s = sealer::generate(path)?;
        tracing::info!(
            "generated the secrets key {}; it seals the runtime secrets of this data directory",
            path.display()
        );
        *slot = Some(s.clone());
        Ok(Some(s))
    }

    /// Store `value` as the runtime value of `name` in `dir`, sealed unless the server
    /// keeps plaintext values.
    pub(crate) fn write(&self, dir: &FsPath, name: &str, value: &[u8]) -> std::io::Result<()> {
        match self.sealer_for_write()? {
            Some(s) => write_atomic(&dir.join(name), &sealer::seal(&s, name, value)?, 0o600),
            None => write_atomic(&dir.join(name), value, 0o600),
        }
    }

    /// The value in `stored` of the secret `name`, opened when it is sealed. The error
    /// never holds the value.
    fn open(&self, name: &str, stored: Vec<u8>) -> Result<Plain, String> {
        if !is_sealed(&stored) {
            #[cfg(feature = "backup")]
            let stored = zeroize::Zeroizing::new(stored);
            return std::str::from_utf8(&stored)
                .map(|v| plain(v.to_owned()))
                .map_err(|_| format!("the stored value of secret {name:?} is not text"));
        }
        let Some(s) = self.state.sealer.read().clone() else {
            return Err(format!(
                "the stored value of secret {name:?} is sealed, and the server has no secrets key"
            ));
        };
        let opened = sealer::open(&s, name, &stored).ok_or_else(|| {
            format!("the stored value of secret {name:?} does not open with the secrets key")
        })?;
        std::str::from_utf8(&opened)
            .map(|v| plain(v.to_owned()))
            .map_err(|_| format!("the stored value of secret {name:?} is not text"))
    }

    /// The runtime value of `name` in the file `path`, trimmed.
    pub fn read(&self, path: &FsPath, name: &str) -> Result<String, String> {
        let bytes = std::fs::read(path)
            .map_err(|_| format!("the stored value of secret {name:?} cannot be read"))?;
        self.open(name, bytes).map(|v| v.trim().to_string())
    }

    /// Every runtime value in `dir`, opened. A value that does not open stops it, with
    /// the names of all such values.
    fn open_all(&self, dir: &FsPath) -> anyhow::Result<Vec<Opened>> {
        self.open_all_with(dir, None)
    }

    /// Like [`Sealing::open_all`], and a sealed value that does not open with this key
    /// may open with `next`, the key of a `rekey` that stopped partway. The server never
    /// passes `next`, so it never accepts two keys.
    fn open_all_with(&self, dir: &FsPath, next: Option<&Sealing>) -> anyhow::Result<Vec<Opened>> {
        let mut out = Vec::new();
        let mut failed = Vec::new();
        for name in stored(dir).into_keys() {
            let bytes = std::fs::read(dir.join(&name))
                .map_err(|e| anyhow::anyhow!("cannot read secret {name}: {e}"))?;
            let was_sealed = is_sealed(&bytes);
            let kept = (was_sealed && next.is_some()).then(|| bytes.clone());
            let (held, value) = match self.open(&name, bytes) {
                Ok(v) => (
                    if was_sealed {
                        Held::Current
                    } else {
                        Held::Plain
                    },
                    v,
                ),
                Err(_) => match next.zip(kept).map(|(n, b)| n.open(&name, b)) {
                    Some(Ok(v)) => (Held::Next, v),
                    _ => {
                        failed.push(name);
                        continue;
                    }
                },
            };
            out.push(Opened { name, held, value });
        }
        if failed.is_empty() {
            return Ok(out);
        }
        let names = failed.join(", ");
        let shown = dir.display();
        let has_key = self.state.sealer.read().is_some();
        match &self.state.mode {
            Mode::Configured => anyhow::bail!(
                "the secrets key does not open the stored secrets {names} in {shown}: it is not the key they were sealed with, or a file was changed or renamed. If a sparkles secrets rekey was interrupted, run the same command again to finish it."
            ),
            Mode::Generated(path) if has_key => anyhow::bail!(
                "the secrets key {} does not open the stored secrets {names} in {shown}: it is not the key they were sealed with, or a file was changed or renamed. Start the server with --secrets-key and the key they were sealed with. If a sparkles secrets rekey was interrupted, run the same command again to finish it.",
                path.display()
            ),
            Mode::Generated(path) => anyhow::bail!(
                "the stored secrets {names} in {shown} are sealed, and there is no {}: start the server with --secrets-key and the key they were sealed with, or put the key file back",
                path.display()
            ),
            Mode::Plaintext if !cfg!(feature = "backup") => anyhow::bail!(
                "the stored secrets {names} in {shown} are sealed, and this build has no backup feature to open them: use a build with it, and run sparkles secrets unseal there to go back to plaintext"
            ),
            Mode::Plaintext => anyhow::bail!(
                "the stored secrets {names} in {shown} are sealed: start the server with --secrets-key and the key they were sealed with"
            ),
        }
    }

    /// Check the runtime values in `dir` at the start. Every sealed value must open,
    /// so a wrong key, a changed file or a file renamed to another secret stops the
    /// start. Unless the server keeps plaintext values, each plaintext value is sealed in
    /// place, with a generated key when there is no other.
    pub fn prepare(&self, dir: &FsPath) -> anyhow::Result<()> {
        remove_stale_temps(dir, is_secret_temp);
        if let Some(data) = dir.parent() {
            remove_stale_temps(data, is_key_temp);
        }
        let mut migrated = 0usize;
        for o in self.open_all(dir)? {
            if o.held == Held::Plain && self.sealed() {
                let name = &o.name;
                self.write(dir, name, o.value.trim().as_bytes())
                    .map_err(|e| anyhow::anyhow!("cannot seal secret {name}: {e}"))?;
                migrated += 1;
            }
        }
        if migrated > 0 {
            tracing::info!("sealed {migrated} plaintext runtime secret(s) with the secrets key");
        }
        Ok(())
    }
}

/// How an opened value was kept.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Held {
    /// A plaintext file.
    Plain,
    /// Sealed, and opened with the current key.
    Current,
    /// Sealed, and opened only with the target key of a `rekey` that stopped partway.
    Next,
}

/// An opened value is zeroed when dropped, in a build that has `zeroize`.
#[cfg(feature = "backup")]
type Plain = zeroize::Zeroizing<String>;
#[cfg(not(feature = "backup"))]
type Plain = String;

fn plain(s: String) -> Plain {
    #[cfg(feature = "backup")]
    return zeroize::Zeroizing::new(s);
    #[cfg(not(feature = "backup"))]
    return s;
}

struct Opened {
    name: String,
    held: Held,
    value: Plain,
}

/// What an offline change to the stored values did.
#[derive(Debug)]
pub struct Offline {
    /// The values written.
    pub values: usize,
    /// The values that an earlier, interrupted `rekey` had sealed with the new key.
    pub already: usize,
    /// The generated key file that `rekey` removed, since nothing uses it any more.
    pub removed_key: Option<PathBuf>,
}

/// The stored values of a stopped server's data directory, opened, with its catalog
/// lock held until this is dropped.
struct OfflineValues {
    _lock: std::fs::File,
    current: Sealing,
    dir: PathBuf,
    values: Vec<Opened>,
}

/// Take the catalog lock of the stopped server's data directory `data`.
fn lock_data(data: &FsPath) -> anyhow::Result<std::fs::File> {
    if !data.is_dir() {
        anyhow::bail!("{} is not a data directory", data.display());
    }
    sparkles::catalog::lock_data_dir(data)
        .map_err(|e| anyhow::anyhow!("{e}: stop the server that uses {} first", data.display()))
}

/// Open every stored value of `data` with the key that `serve` would use (`source` as
/// for `--secrets-key`), holding `lock`. A value that opens only with `next` is
/// `Held::Next`.
fn offline_values(
    lock: std::fs::File,
    data: &FsPath,
    source: Option<&str>,
    next: Option<&Sealing>,
) -> anyhow::Result<OfflineValues> {
    let current = Sealing::start(source, data)?;
    let dir = data.join(SECRETS_DIR);
    let values = current.open_all_with(&dir, next)?;
    Ok(OfflineValues {
        _lock: lock,
        current,
        dir,
        values,
    })
}

/// `sparkles secrets unseal --data DIR`: write every stored value of a stopped server
/// back as a plaintext file with mode 0600, for a release that does not seal them. The
/// key file stays, so a later start of a sealing release seals them again with it.
pub fn unseal(data: &FsPath, source: Option<&str>) -> anyhow::Result<Offline> {
    let lock = lock_data(data)?;
    remove_stale_temps(data, is_key_temp);
    remove_stale_temps(&data.join(SECRETS_DIR), is_secret_temp);
    let opened = offline_values(lock, data, source, None)?;
    let dir = &opened.dir;
    let mut n = 0;
    for o in &opened.values {
        if o.held == Held::Current {
            write_atomic(&dir.join(&o.name), o.value.trim().as_bytes(), 0o600)
                .map_err(|e| anyhow::anyhow!("cannot write secret {}: {e}", o.name))?;
            n += 1;
        }
    }
    Ok(Offline {
        values: n,
        already: 0,
        removed_key: None,
    })
}

/// `sparkles secrets rekey --data DIR --new-key SOURCE`: seal every stored value of a
/// stopped server again under the key of `new_key`. Every value is opened before any is
/// written. A value that an earlier, interrupted run sealed with the new key is kept,
/// so running the same command again finishes it. When the old key was the generated
/// `secrets.key`, the file is removed once every value opens with the new key, so that
/// a start without `--secrets-key` does not pick it up.
pub fn rekey(data: &FsPath, source: Option<&str>, new_key: &str) -> anyhow::Result<Offline> {
    if new_key.trim() == "none" {
        anyhow::bail!("--new-key none would keep no key: use sparkles secrets unseal");
    }
    // the lock comes first, so that a running server is refused before any key is read
    let lock = lock_data(data)?;
    remove_stale_temps(&data.join(SECRETS_DIR), is_secret_temp);
    let new = Sealing::start(Some(new_key), data)?;
    let dir = data.join(SECRETS_DIR);
    let has_dir = match std::fs::symlink_metadata(&dir) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => false,
        Err(e) => anyhow::bail!("cannot inspect {}: {e}", dir.display()),
        Ok(_) => {
            // Follow an existing symlink, but never treat a dangling one as absent.
            let md = std::fs::metadata(&dir)
                .map_err(|e| anyhow::anyhow!("cannot inspect {}: {e}", dir.display()))?;
            if !md.is_dir() {
                anyhow::bail!("{} is not a secrets directory", dir.display());
            }
            true
        }
    };
    let opened = offline_values(lock, data, source, Some(&new))?;
    let dir = &opened.dir;
    let mut written = 0usize;
    let mut already = 0usize;
    for o in &opened.values {
        if o.held == Held::Next {
            already += 1;
            continue;
        }
        new.write(dir, &o.name, o.value.trim().as_bytes())
            .map_err(|e| {
                anyhow::anyhow!(
                    "cannot seal secret {}: {e}. The secrets before it are sealed with the new key and the others with the old one: run the same sparkles secrets rekey command again to finish",
                    o.name
                )
            })?;
        written += 1;
    }
    // every file must open with the new key before the old key goes
    let mut unsealed = Vec::new();
    for name in stored(dir).into_keys() {
        let ok = std::fs::read(dir.join(&name))
            .ok()
            .is_some_and(|b| is_sealed(&b) && new.open(&name, b).is_ok());
        if !ok {
            unsealed.push(name);
        }
    }
    if !unsealed.is_empty() {
        anyhow::bail!(
            "the stored secrets {} in {} do not open with the new key after the rekey, so the old key stays: run the same sparkles secrets rekey command again",
            unsealed.join(", "),
            dir.display()
        );
    }
    if has_dir {
        sync_dir(dir).map_err(|e| anyhow::anyhow!("cannot flush {}: {e}", dir.display()))?;
    }
    let mut removed_key = None;
    if let Mode::Generated(path) = &opened.current.state.mode {
        match std::fs::remove_file(path) {
            Ok(()) => removed_key = Some(path.clone()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => anyhow::bail!("cannot remove {}: {e}", path.display()),
        }
        remove_stale_temps(data, is_key_temp);
        sync_dir(data).map_err(|e| anyhow::anyhow!("cannot flush {}: {e}", data.display()))?;
    }
    Ok(Offline {
        values: written,
        already,
        removed_key,
    })
}

#[cfg(test)]
thread_local! {
    /// Makes [`sync_dir`] fail on this thread, to test the paths that must report it.
    pub(crate) static FAIL_DIR_SYNC: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    static DIR_SYNC_PROBE: std::cell::RefCell<Option<(PathBuf, bool, usize)>> = const { std::cell::RefCell::new(None) };
}

/// Flush the directory `dir`, so that the names made or removed in it survive a crash.
pub(crate) fn sync_dir(dir: &FsPath) -> std::io::Result<()> {
    #[cfg(test)]
    if FAIL_DIR_SYNC.with(|f| f.get()) {
        return Err(std::io::Error::other("injected directory sync failure"));
    }
    #[cfg(test)]
    DIR_SYNC_PROBE.with(|slot| {
        if let Some((path, fail, calls)) = slot.borrow_mut().as_mut()
            && path == dir
        {
            *calls += 1;
            if *fail {
                return Err(std::io::Error::other(
                    "injected targeted directory sync failure",
                ));
            }
        }
        Ok(())
    })?;
    #[cfg(unix)]
    std::fs::File::open(dir)?.sync_all()?;
    #[cfg(not(unix))]
    let _ = dir;
    Ok(())
}

/// Remove the files in `dir` whose names `matches`, which a write that crashed left
/// behind. A directory is never removed. Errors are ignored, and only the count is
/// logged. Only a start or an offline command calls this, under the catalog lock, since
/// a request handler's write may own a temporary file.
fn remove_stale_temps(dir: &FsPath, matches: impl Fn(&str) -> bool) {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return;
    };
    let mut removed = 0usize;
    for e in rd.flatten() {
        let Some(name) = e.file_name().to_str().map(str::to_string) else {
            continue;
        };
        if !matches(&name) {
            continue;
        }
        let Ok(kind) = e.file_type() else {
            continue;
        };
        if !kind.is_dir() && std::fs::remove_file(e.path()).is_ok() {
            removed += 1;
        }
    }
    if removed > 0 {
        tracing::info!(
            "removed {removed} stale temporary file(s) of an interrupted write in {}",
            dir.display()
        );
    }
}

fn all_digits(s: &str) -> bool {
    !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit())
}

/// `.secrets.key.PID.tmp`, the file that `generate` writes before it links the key.
fn is_key_temp(name: &str) -> bool {
    name.strip_prefix(".secrets.key.")
        .and_then(|n| n.strip_suffix(".tmp"))
        .is_some_and(all_digits)
}

/// `.NAME.PID.tmp`, the file that `write_atomic` writes before it renames it to NAME.
fn is_secret_temp(name: &str) -> bool {
    name.strip_prefix('.')
        .and_then(|n| n.strip_suffix(".tmp"))
        .and_then(|n| n.rsplit_once('.'))
        .is_some_and(|(n, pid)| all_digits(pid) && crate::models::check_secret_name(n).is_ok())
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
/// flushed to disk, then renamed over `path`, and the directory flushed. The error of
/// the directory flush is returned. The new file is then in place, but it may not
/// survive a crash.
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
    sync_dir(dir)
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
        let sealing = &layers.sealing;
        Ok(Json(json!({
            "storage": sealing.storage(),
            "keySource": sealing.key_source(),
            "keyMemory": sealing.key_memory(),
            "secrets": secrets,
        })))
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

    #[cfg(all(feature = "backup", unix))]
    struct SyncProbe(Option<(PathBuf, bool, usize)>);

    #[cfg(all(feature = "backup", unix))]
    impl SyncProbe {
        fn new(path: &FsPath, fail: bool) -> Self {
            Self(DIR_SYNC_PROBE.with(|slot| slot.replace(Some((path.to_path_buf(), fail, 0)))))
        }

        fn calls(&self) -> usize {
            DIR_SYNC_PROBE.with(|slot| slot.borrow().as_ref().unwrap().2)
        }

        fn allow(&self) {
            DIR_SYNC_PROBE.with(|slot| slot.borrow_mut().as_mut().unwrap().1 = false);
        }
    }

    #[cfg(all(feature = "backup", unix))]
    impl Drop for SyncProbe {
        fn drop(&mut self) {
            DIR_SYNC_PROBE.with(|slot| slot.replace(self.0.take()));
        }
    }

    #[cfg(all(feature = "backup", unix))]
    fn offline_fixture() -> (tempfile::TempDir, PathBuf, String) {
        use std::os::unix::fs::PermissionsExt;
        let t = tempfile::tempdir().unwrap();
        let data = t.path().join("data");
        private_dir(&data.join(SECRETS_DIR)).unwrap();
        let key = t.path().join("new.key");
        std::fs::write(&key, "07".repeat(32)).unwrap();
        std::fs::set_permissions(&key, std::fs::Permissions::from_mode(0o600)).unwrap();
        let source = format!("file:{}", key.display());
        (t, data, source)
    }

    #[cfg(all(feature = "backup", unix))]
    #[test]
    fn rekey_retry_repeats_parent_sync_after_key_removal() {
        let (_t, data, source) = offline_fixture();
        let dir = data.join(SECRETS_DIR);
        Sealing::start(None, &data)
            .unwrap()
            .write(&dir, "a", b"value-a")
            .unwrap();
        let probe = SyncProbe::new(&data, true);
        let e = rekey(&data, None, &source).unwrap_err().to_string();
        assert!(e.contains("cannot flush"), "{e}");
        assert_eq!(probe.calls(), 1);
        assert!(!data.join(SECRETS_KEY_FILE).exists());
        let migrated = std::fs::read(dir.join("a")).unwrap();
        let e = rekey(&data, None, &source).unwrap_err().to_string();
        assert!(e.contains("cannot flush"), "{e}");
        assert_eq!(probe.calls(), 2);
        assert_eq!(std::fs::read(dir.join("a")).unwrap(), migrated);
        probe.allow();
        let done = rekey(&data, None, &source).unwrap();
        assert_eq!((done.values, done.already, done.removed_key), (0, 1, None));
        assert_eq!(probe.calls(), 3);
        assert_eq!(std::fs::read(dir.join("a")).unwrap(), migrated);
        assert_eq!(
            Sealing::start(Some(&source), &data)
                .unwrap()
                .read(&dir.join("a"), "a")
                .unwrap(),
            "value-a"
        );
        assert!(!data.join(SECRETS_KEY_FILE).exists());
    }

    #[cfg(all(feature = "backup", unix))]
    #[test]
    fn rekey_retry_cleans_stale_keys_after_key_removal() {
        let (_t, data, source) = offline_fixture();
        let dir = data.join(SECRETS_DIR);
        Sealing::start(None, &data)
            .unwrap()
            .write(&dir, "a", b"value-a")
            .unwrap();
        let key = data.join(SECRETS_KEY_FILE);
        let old = std::fs::read(&key).unwrap();
        Sealing::start(Some(&source), &data)
            .unwrap()
            .write(&dir, "a", b"value-a")
            .unwrap();
        std::fs::remove_file(&key).unwrap();
        let stale = data.join(".secrets.key.4242.tmp");
        std::fs::write(&stale, old).unwrap();
        let keep = data.join(".notes.tmp");
        std::fs::write(&keep, "keep").unwrap();
        let keep_dir = data.join(".secrets.key.4243.tmp");
        std::fs::create_dir(&keep_dir).unwrap();
        let probe = SyncProbe::new(&data, false);
        let done = rekey(&data, None, &source).unwrap();
        assert_eq!((done.values, done.already, done.removed_key), (0, 1, None));
        assert!(!stale.exists(), "retry left a stale copy of the old key");
        assert_eq!(probe.calls(), 1);
        assert_eq!(std::fs::read_to_string(keep).unwrap(), "keep");
        assert!(keep_dir.is_dir());
    }

    #[cfg(all(feature = "backup", unix))]
    #[test]
    fn unseal_cleans_stale_keys_and_preserves_canonical_key() {
        let (_t, data, _source) = offline_fixture();
        let dir = data.join(SECRETS_DIR);
        Sealing::start(None, &data)
            .unwrap()
            .write(&dir, "a", b"value-a")
            .unwrap();
        let key = data.join(SECRETS_KEY_FILE);
        let old = std::fs::read(&key).unwrap();
        let stale = data.join(".secrets.key.4242.tmp");
        std::fs::write(&stale, &old).unwrap();
        let keep = data.join(".notes.tmp");
        std::fs::write(&keep, "keep").unwrap();
        let keep_dir = data.join(".secrets.key.4243.tmp");
        std::fs::create_dir(&keep_dir).unwrap();
        let done = unseal(&data, None).unwrap();
        assert_eq!(done.values, 1);
        assert!(!stale.exists(), "unseal left a stale key temporary");
        assert_eq!(std::fs::read(key).unwrap(), old);
        assert_eq!(std::fs::read_to_string(dir.join("a")).unwrap(), "value-a");
        assert_eq!(std::fs::read_to_string(keep).unwrap(), "keep");
        assert!(keep_dir.is_dir());
    }

    #[cfg(all(feature = "backup", unix))]
    #[test]
    fn rekey_allows_an_absent_secrets_directory() {
        for generated in [false, true] {
            let (_t, data, source) = offline_fixture();
            std::fs::remove_dir(data.join(SECRETS_DIR)).unwrap();
            let key = data.join(SECRETS_KEY_FILE);
            if generated {
                sealer::generate(&key).unwrap();
            }
            let probe = SyncProbe::new(&data, false);
            let done = rekey(&data, None, &source).unwrap();
            assert_eq!((done.values, done.already), (0, 0));
            assert_eq!(done.removed_key, generated.then_some(key.clone()));
            assert!(!key.exists());
            assert!(!data.join(SECRETS_DIR).exists());
            assert_eq!(probe.calls(), 1);
        }
    }

    #[cfg(all(feature = "backup", unix))]
    #[test]
    fn rekey_empty_directories_and_configured_keys_keep_their_semantics() {
        for present in [false, true] {
            let (t, data, source) = offline_fixture();
            if !present {
                std::fs::remove_dir(data.join(SECRETS_DIR)).unwrap();
            }
            let configured_key = t.path().join("new.key");
            let key_bytes = std::fs::read(&configured_key).unwrap();
            let probe = SyncProbe::new(&data, false);
            let done = rekey(&data, Some(&source), &source).unwrap();
            assert_eq!((done.values, done.already, done.removed_key), (0, 0, None));
            assert_eq!(std::fs::read(configured_key).unwrap(), key_bytes);
            assert!(!data.join(SECRETS_KEY_FILE).exists());
            assert_eq!(data.join(SECRETS_DIR).is_dir(), present);
            assert_eq!(probe.calls(), 0);
        }
    }

    #[cfg(all(feature = "backup", unix))]
    #[test]
    fn rekey_refuses_non_directory_and_dangling_secrets_paths() {
        use std::os::unix::fs::symlink;
        for kind in ["file", "dangling", "loop"] {
            let (_t, data, source) = offline_fixture();
            let dir = data.join(SECRETS_DIR);
            let key = data.join(SECRETS_KEY_FILE);
            sealer::generate(&key).unwrap();
            let old = std::fs::read(&key).unwrap();
            std::fs::remove_dir(&dir).unwrap();
            match kind {
                "dangling" => symlink(data.join("missing"), &dir).unwrap(),
                "loop" => symlink(&dir, &dir).unwrap(),
                _ => std::fs::write(&dir, "not a directory").unwrap(),
            }
            assert!(rekey(&data, None, &source).is_err());
            assert_eq!(std::fs::read(key).unwrap(), old);
        }
    }

    #[cfg(all(feature = "backup", unix))]
    #[test]
    fn rekey_empty_directory_sync_error_keeps_the_generated_key() {
        let (_t, data, source) = offline_fixture();
        let key = data.join(SECRETS_KEY_FILE);
        sealer::generate(&key).unwrap();
        let old = std::fs::read(&key).unwrap();
        let probe = SyncProbe::new(&data.join(SECRETS_DIR), true);
        let e = rekey(&data, None, &source).unwrap_err().to_string();
        assert!(e.contains("cannot flush"), "{e}");
        assert_eq!(probe.calls(), 1);
        assert_eq!(std::fs::read(key).unwrap(), old);
    }

    #[cfg(all(feature = "backup", unix))]
    #[test]
    fn migration_and_retry_preserve_key_classification() {
        let (_t, data, source) = offline_fixture();
        let dir = data.join(SECRETS_DIR);
        let current = Sealing::start(None, &data).unwrap();
        current.write(&dir, "a", b"current").unwrap();
        let next = Sealing::start(Some(&source), &data).unwrap();
        next.write(&dir, "b", b"next").unwrap();
        std::fs::write(dir.join("c"), "plaintext").unwrap();
        let values = current.open_all_with(&dir, Some(&next)).unwrap();
        let got: Vec<_> = values
            .iter()
            .map(|v| (v.name.as_str(), v.held, v.value.as_str()))
            .collect();
        assert_eq!(
            got,
            [
                ("a", Held::Current, "current"),
                ("b", Held::Next, "next"),
                ("c", Held::Plain, "plaintext")
            ]
        );
        // A normal server does not accept the retry key as a fallback.
        assert!(current.open_all(&dir).is_err());
        std::fs::remove_file(dir.join("b")).unwrap();
        let values = current.open_all_with(&dir, Some(&current)).unwrap();
        assert_eq!(
            values[0].held,
            Held::Current,
            "equal keys prefer the current key"
        );
        assert_eq!(values[1].held, Held::Plain);
    }

    #[cfg(all(feature = "backup", unix))]
    #[test]
    fn malformed_utf8_is_refused_before_rekey_changes_any_value() {
        for sealed in [false, true] {
            let (_t, data, source) = offline_fixture();
            let dir = data.join(SECRETS_DIR);
            let current = Sealing::start(None, &data).unwrap();
            current.write(&dir, "a", b"valid").unwrap();
            let invalid = b"invalid-text-\xff";
            if sealed {
                current.write(&dir, "b", invalid).unwrap();
            } else {
                std::fs::write(dir.join("b"), invalid).unwrap();
            }
            let before = std::fs::read(dir.join("a")).unwrap();
            let key = data.join(SECRETS_KEY_FILE);
            let old = std::fs::read(&key).unwrap();
            let e = current
                .open("b", std::fs::read(dir.join("b")).unwrap())
                .unwrap_err();
            assert!(e.contains("not text"), "{e}");
            assert!(rekey(&data, None, &source).is_err());
            assert_eq!(std::fs::read(dir.join("a")).unwrap(), before);
            assert_eq!(std::fs::read(key).unwrap(), old);
        }
    }

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

    /// A key that is generated is durable before any value is sealed with it. When the
    /// directory sync fails, nothing is written, and no key stays.
    #[cfg(all(feature = "backup", unix))]
    #[test]
    fn generated_key_must_be_durable_before_sealing() {
        use std::os::unix::fs::PermissionsExt;
        let t = tempfile::tempdir().unwrap();
        let data = t.path().join("data");
        std::fs::create_dir_all(&data).unwrap();
        let dir = data.join(SECRETS_DIR);
        private_dir(&dir).unwrap();
        let s = Sealing::start(None, &data).unwrap();
        FAIL_DIR_SYNC.with(|f| f.set(true));
        let e = s.write(&dir, "gw", b"v");
        assert!(e.is_err(), "write succeeded without a durable key");
        assert!(!data.join(SECRETS_KEY_FILE).exists());
        assert!(!dir.join("gw").exists());
        assert!(s.key_memory().is_none());
        let old = dir.join("old");
        std::fs::write(&old, "plain").unwrap();
        std::fs::set_permissions(&old, std::fs::Permissions::from_mode(0o600)).unwrap();
        assert!(s.prepare(&dir).is_err());
        assert_eq!(std::fs::read_to_string(&old).unwrap(), "plain");
        FAIL_DIR_SYNC.with(|f| f.set(false));
        s.prepare(&dir).unwrap();
        assert!(
            std::fs::read_to_string(&old)
                .unwrap()
                .starts_with("sparkles-sealed-secret/1 ")
        );
        let mode = std::fs::metadata(data.join(SECRETS_KEY_FILE))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o400);
    }

    /// The generated key is readable by its owner under any umask. The umask is
    /// per process, so the test runs [`generated_key_mode_child`] in a child process
    /// that sets it, and tests that run in parallel keep theirs.
    #[cfg(all(feature = "backup", target_os = "linux"))]
    #[test]
    fn generated_key_mode_ignores_umask() {
        use std::os::unix::fs::PermissionsExt;
        use std::os::unix::process::CommandExt;
        let t = tempfile::tempdir().unwrap();
        let data = t.path().join("data");
        std::fs::create_dir_all(&data).unwrap();
        let mut cmd = std::process::Command::new(std::env::current_exe().unwrap());
        cmd.args([
            "--exact",
            "settings::secrets::tests::generated_key_mode_child",
            "--ignored",
        ])
        .env("SPARKLES_TEST_UMASK_DATA", &data);
        // SAFETY: umask is async-signal-safe and the closure does nothing else.
        unsafe {
            cmd.pre_exec(|| {
                libc::umask(0o477);
                Ok(())
            });
        }
        let out = cmd.output().unwrap();
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stdout)
        );
        let key = data.join(SECRETS_KEY_FILE);
        let mode = std::fs::metadata(&key).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o400);
        Sealing::start(None, &data).unwrap();
    }

    /// The child of [`generated_key_mode_ignores_umask`]. It does nothing on its own.
    #[cfg(all(feature = "backup", target_os = "linux"))]
    #[test]
    #[ignore = "runs in a child process of generated_key_mode_ignores_umask"]
    fn generated_key_mode_child() {
        let Some(data) = std::env::var_os("SPARKLES_TEST_UMASK_DATA") else {
            return;
        };
        let data = PathBuf::from(data);
        let dir = data.join(SECRETS_DIR);
        private_dir(&dir).unwrap();
        let s = Sealing::start(None, &data).unwrap();
        s.write(&dir, "gw", b"v").unwrap();
    }

    /// Temporary files that a crashed write left in the data directory or in `secrets/`
    /// can hold a key or a plaintext value, so a start removes them.
    #[cfg(all(feature = "backup", unix))]
    #[test]
    fn stale_temporary_files_are_removed() {
        let t = tempfile::tempdir().unwrap();
        let data = t.path().join("data");
        let dir = data.join(SECRETS_DIR);
        private_dir(&dir).unwrap();
        let key_tmp = data.join(".secrets.key.4242.tmp");
        std::fs::write(&key_tmp, "ab".repeat(32)).unwrap();
        let secret_tmp = dir.join(".gw.4242.tmp");
        std::fs::write(&secret_tmp, "plain").unwrap();
        std::fs::create_dir(dir.join(".x.1.tmp")).unwrap();
        std::fs::write(dir.join(".notes.tmp"), "keep").unwrap();
        Sealing::start(None, &data).unwrap().prepare(&dir).unwrap();
        assert!(!key_tmp.exists());
        assert!(!secret_tmp.exists());
        assert!(dir.join(".x.1.tmp").is_dir());
        assert!(dir.join(".notes.tmp").exists());
    }
}
