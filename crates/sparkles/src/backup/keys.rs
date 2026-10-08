//! Bounded operator-only local key resolution. Configuration holds references only.
use super::config::{KeySource, RepositoryEncryption};
use anyhow::{Result, bail};
use base64::{Engine, engine::general_purpose::STANDARD};
use sparkles_backup::{
    Ctl,
    crypto::{EncryptionOptions, LocalKey, LocalKeySource, Passphrase},
};
use std::{
    io::Read,
    path::{Path, PathBuf},
    process::Stdio,
    time::Duration,
};
use tokio::io::AsyncReadExt;
use zeroize::Zeroizing;

const LIMIT: usize = 4096;

/// The only environment variables a key command receives. Everything else, such
/// as other repositories' `env` keys and cloud credentials, is withheld.
pub const COMMAND_ENVIRONMENT: &[&str] = &[
    "PATH",
    "HOME",
    "LANG",
    "XDG_RUNTIME_DIR",
    "CREDENTIALS_DIRECTORY",
];

/// A key file may belong to the user the process runs as, or to root.
#[cfg(unix)]
fn owner_allowed(owner: u32, effective: u32) -> bool {
    owner == effective || owner == 0
}

/// Locations secrets may not occupy. Credential names resolve below one explicit root.
#[derive(Clone, Debug, Default)]
pub struct KeyContext {
    pub forbidden: Vec<PathBuf>,
    pub credential_directory: Option<PathBuf>,
}
impl KeyContext {
    pub fn from_environment(forbidden: Vec<PathBuf>) -> Self {
        Self {
            forbidden,
            credential_directory: std::env::var_os("CREDENTIALS_DIRECTORY").map(PathBuf::from),
        }
    }
}

fn unavailable(label: &str, reason: &str) -> anyhow::Error {
    anyhow::anyhow!("repository key {label:?}: {reason}")
}

fn read_private(
    path: &str,
    context: &KeyContext,
    label: &str,
    required_root: Option<&Path>,
) -> Result<Zeroizing<Vec<u8>>> {
    let canonical =
        std::fs::canonicalize(path).map_err(|_| unavailable(label, "cannot resolve key file"))?;
    let mut opts = std::fs::OpenOptions::new();
    opts.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
    }
    let file = opts
        .open(&canonical)
        .map_err(|_| unavailable(label, "cannot open key file"))?;
    let meta = file
        .metadata()
        .map_err(|_| unavailable(label, "cannot inspect key file"))?;
    if !meta.is_file() || meta.len() > LIMIT as u64 {
        bail!(
            "{}",
            unavailable(label, "key file must be regular and at most 4096 bytes")
        );
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::{MetadataExt, PermissionsExt};
        if meta.permissions().mode() & 0o077 != 0 {
            bail!(
                "{}",
                unavailable(label, "key file must not be group/world accessible")
            );
        }
        // SAFETY: geteuid has no preconditions and cannot fail.
        if !owner_allowed(meta.uid(), unsafe { libc::geteuid() }) {
            bail!(
                "{}",
                unavailable(
                    label,
                    "key file must be owned by the user running sparkles or by root"
                )
            );
        }
    }
    #[cfg(any(target_os = "linux", target_os = "android"))]
    let opened = {
        use std::os::fd::AsRawFd;
        std::fs::canonicalize(format!("/proc/self/fd/{}", file.as_raw_fd()))
            .map_err(|_| unavailable(label, "cannot validate opened key file"))?
    };
    #[cfg(not(any(target_os = "linux", target_os = "android")))]
    let opened = canonical;
    // Offline CLI commands may not know every local catalog up front. Recognize
    // dataset/catalog ancestors in addition to the caller's explicit forbidden roots.
    for root in opened.ancestors().skip(1) {
        if (root.join("CURRENT").is_file() && root.join("dataset.json").is_file())
            || (root.join("config.json").is_file() && root.join("databases").is_dir())
        {
            bail!(
                "{}",
                unavailable(label, "key file lies inside a dataset or server catalog")
            );
        }
    }
    if let Some(root) = required_root {
        let root = std::fs::canonicalize(root)
            .map_err(|_| unavailable(label, "cannot resolve credential directory"))?;
        if !opened.starts_with(root) {
            bail!(
                "{}",
                unavailable(label, "credential file escapes its directory")
            );
        }
    }
    for root in &context.forbidden {
        let root = std::fs::canonicalize(root)
            .or_else(|_| std::path::absolute(root))
            .map_err(|_| unavailable(label, "cannot validate forbidden key location"))?;
        if opened.starts_with(root) {
            bail!(
                "{}",
                unavailable(label, "key file lies inside a forbidden data directory")
            );
        }
    }
    let mut bytes = Zeroizing::new(Vec::with_capacity(LIMIT + 1));
    file.take((LIMIT + 1) as u64)
        .read_to_end(&mut bytes)
        .map_err(|_| unavailable(label, "cannot read key file"))?;
    if bytes.len() > LIMIT {
        bail!("{}", unavailable(label, "key input exceeds 4096 bytes"));
    }
    Ok(bytes)
}

/// Decode a 32-byte key given as raw bytes, 64 hexadecimal digits or canonical
/// base64. Raw input that is entirely printable ASCII is refused, because it is
/// almost certainly a typed password rather than random key bytes. A random
/// 32-byte key is all printable with a probability of about 10^-13.
fn decode_key(bytes: &[u8], label: &str) -> Result<Zeroizing<[u8; 32]>> {
    let text = bytes
        .iter()
        .all(|b| b.is_ascii_graphic() || b.is_ascii_whitespace());
    let mut key = Zeroizing::new([0; 32]);
    if bytes.len() == 32 && !text {
        key.copy_from_slice(bytes);
        return Ok(key);
    }
    if bytes.len() == 32 {
        bail!(
            "{}",
            unavailable(
                label,
                "a 32-character text key is not accepted as raw bytes; \
                 encode the key as 64 hexadecimal digits or canonical base64"
            )
        );
    }
    let first = bytes
        .iter()
        .position(|b| !b.is_ascii_whitespace())
        .unwrap_or(bytes.len());
    let last = bytes
        .iter()
        .rposition(|b| !b.is_ascii_whitespace())
        .map_or(first, |i| i + 1);
    let bytes = &bytes[first..last];
    if bytes.len() == 64 && bytes.iter().all(u8::is_ascii_hexdigit) {
        fn digit(b: u8) -> u8 {
            if b.is_ascii_digit() {
                b - b'0'
            } else {
                b.to_ascii_lowercase() - b'a' + 10
            }
        }
        for (i, pair) in bytes.as_chunks::<2>().0.iter().enumerate() {
            key[i] = digit(pair[0]) * 16 + digit(pair[1]);
        }
    } else {
        let decoded = Zeroizing::new(
            STANDARD
                .decode(bytes)
                .map_err(|_| unavailable(label, "expected raw32, hex64 or standard base64 key"))?,
        );
        let canonical = Zeroizing::new(STANDARD.encode(&decoded));
        if decoded.len() != 32 || canonical.as_bytes() != bytes {
            bail!(
                "{}",
                unavailable(label, "expected a canonical encoding of a 32-byte key")
            );
        }
        key.copy_from_slice(&decoded);
    }
    Ok(key)
}

/// Own the provider through cleanup even when its caller future is dropped. Tokio's
/// orphan queue is signal driven, so merely dropping a killed Child can leave it
/// unreaped until another child exits. The sole Child owner can try_wait on a
/// cleanup thread even after the originating runtime has stopped.
struct Process {
    child: Option<tokio::process::Child>,
    #[cfg(unix)]
    group: i32,
    #[cfg(test)]
    fail_reaper_spawn: bool,
}
impl std::ops::Deref for Process {
    type Target = tokio::process::Child;
    fn deref(&self) -> &Self::Target {
        self.child.as_ref().expect("owned provider")
    }
}
impl std::ops::DerefMut for Process {
    fn deref_mut(&mut self) -> &mut Self::Target {
        self.child.as_mut().expect("owned provider")
    }
}
impl Process {
    fn reap(mut child: tokio::process::Child) {
        loop {
            match child.try_wait() {
                Ok(None) => std::thread::sleep(Duration::from_millis(5)),
                Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
                Ok(Some(_)) | Err(_) => break,
            }
        }
    }
    fn kill_group(&mut self) {
        #[cfg(unix)]
        if self.group > 0 {
            unsafe {
                libc::kill(-self.group, libc::SIGKILL);
            }
            // The process may be reaped by the next await. Never signal this PID
            // again after relinquishing that identity fence.
            self.group = 0;
        }
    }
}
impl Drop for Process {
    fn drop(&mut self) {
        self.kill_group();
        if let Some(mut child) = self.child.take() {
            if child.id().is_none() {
                return;
            }
            let _ = child.start_kill();
            // try_wait updates Tokio's ownership state when it reaps; unlike raw
            // waitpid it cannot leave a live Child targeting a reused PID. Retain
            // sole ownership if resource pressure prevents spawning the reaper.
            let carrier = std::sync::Arc::new(std::sync::Mutex::new(Some(child)));
            let worker = carrier.clone();
            #[cfg(test)]
            let fail_spawn = self.fail_reaper_spawn;
            #[cfg(not(test))]
            let fail_spawn = false;
            let spawned = if fail_spawn {
                Err(std::io::Error::other("injected reaper spawn failure"))
            } else {
                std::thread::Builder::new()
                    .name("sparkles-key-reap".into())
                    .spawn(move || {
                        if let Some(child) = worker.lock().unwrap_or_else(|e| e.into_inner()).take()
                        {
                            Self::reap(child);
                        }
                    })
            };
            if spawned.is_err()
                && let Some(child) = carrier.lock().unwrap_or_else(|e| e.into_inner()).take()
            {
                Self::reap(child);
            }
        }
    }
}

async fn command(
    argv: &[String],
    timeout: u64,
    ctl: &Ctl,
    label: &str,
) -> Result<Zeroizing<Vec<u8>>> {
    let mut cmd = tokio::process::Command::new(&argv[0]);
    cmd.env_clear();
    for name in COMMAND_ENVIRONMENT {
        if let Some(value) = std::env::var_os(name) {
            cmd.env(name, value);
        }
    }
    cmd.args(&argv[1..])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true);
    #[cfg(unix)]
    {
        cmd.process_group(0);
    }
    let child = cmd
        .spawn()
        .map_err(|_| unavailable(label, "cannot start key command"))?;
    let mut child = Process {
        #[cfg(unix)]
        group: child.id().unwrap_or(0) as i32,
        child: Some(child),
        #[cfg(test)]
        fail_reaper_spawn: false,
    };
    let mut stdout = child.stdout.take().expect("piped stdout");
    let deadline = tokio::time::Instant::now() + Duration::from_secs(timeout);
    let mut bytes = Zeroizing::new(Vec::with_capacity(LIMIT + 1));
    let result = async {
        let mut block = Zeroizing::new([0;512]);
        loop {
            ctl.check()?;
            let got = tokio::select! {
                _ = tokio::time::sleep_until(deadline) => return Err(unavailable(label,"key command timed out")),
                _ = tokio::time::sleep(Duration::from_millis(25)) => continue,
                got = stdout.read(block.as_mut()) => got.map_err(|_| unavailable(label,"cannot read key command output"))?,
            };
            if got==0 { break; }
            if bytes.len()+got>LIMIT { return Err(unavailable(label,"key command output exceeds 4096 bytes")); }
            bytes.extend_from_slice(&block[..got]);
        }
        loop {
            ctl.check()?;
            #[cfg(any(target_os="linux",target_os="android"))]
            {
                // Observe exit without reaping: the owned PID/group cannot be reused
                // before descendants are killed. Tokio performs the actual reap below.
                let mut info=unsafe { std::mem::zeroed::<libc::siginfo_t>() };
                let rc=unsafe { libc::waitid(libc::P_PID,child.group as libc::id_t,&mut info,libc::WEXITED|libc::WNOWAIT|libc::WNOHANG) };
                if rc<0 {
                    if std::io::Error::last_os_error().kind()==std::io::ErrorKind::Interrupted {continue;}
                    child.group=0; // An externally reaped PID must never be signaled after reuse.
                    return Err(unavailable(label,"cannot inspect key command exit"));
                }
                if unsafe { info.si_pid() }>0 {
                    child.kill_group();
                    let status=child.wait().await.map_err(|_| unavailable(label,"cannot reap key command"))?;
                    child.child = None;
                    if !status.success() {return Err(unavailable(label,"key command exited unsuccessfully"));}
                    return Ok(());
                }
                tokio::select! {
                    _ = tokio::time::sleep_until(deadline) => return Err(unavailable(label,"key command timed out")),
                    _ = tokio::time::sleep(Duration::from_millis(25)) => {},
                }
            }
            #[cfg(not(any(target_os="linux",target_os="android")))]
            tokio::select! {
                _ = tokio::time::sleep_until(deadline) => return Err(unavailable(label,"key command timed out")),
                _ = tokio::time::sleep(Duration::from_millis(25)) => {},
                status=child.wait() => {
                    let status=status.map_err(|_| unavailable(label,"cannot reap key command"))?;
                    #[cfg(unix)] { child.group=0; }
                    child.child = None;
                    if !status.success() { return Err(unavailable(label,"key command exited unsuccessfully")); }
                    return Ok(());
                }
            }
        }
    }.await;
    if result.is_err() && child.child.is_some() {
        child.kill_group();
        let _ = child.start_kill();
        let _ = child.wait().await;
        child.child = None;
    }
    result?;
    Ok(bytes)
}

/// Resolve online inputs once for a repository handle. Offline recovery inputs can be
/// supplied separately to key-add and need not be kept in this configuration.
pub async fn resolve(
    settings: &RepositoryEncryption,
    context: &KeyContext,
    ctl: &Ctl,
) -> Result<EncryptionOptions> {
    settings.validate()?;
    if !cfg!(any(target_os = "linux", target_os = "android")) {
        bail!("protected repository key memory is unavailable on this platform");
    }
    let mut out = EncryptionOptions {
        single_key_ok: settings.single_key_ok,
        ..Default::default()
    };
    for input in &settings.keys {
        ctl.check()?;
        let (mut bytes, source) = match &input.key {
            KeySource::File { path } | KeySource::PassphraseFile { path } => {
                let path = path.clone();
                let context = context.clone();
                let label = input.label.clone();
                (
                    tokio::task::spawn_blocking(move || {
                        read_private(&path, &context, &label, None)
                    })
                    .await
                    .map_err(|_| unavailable(&input.label, "key file worker failed"))??,
                    LocalKeySource::File,
                )
            }
            KeySource::Env { var } => {
                let value = std::env::var_os(var).ok_or_else(|| {
                    unavailable(&input.label, "key environment variable is missing")
                })?;
                #[cfg(unix)]
                let bytes = {
                    use std::os::unix::ffi::OsStringExt;
                    Zeroizing::new(value.into_vec())
                };
                #[cfg(not(unix))]
                let bytes = Zeroizing::new(
                    value
                        .into_string()
                        .map_err(|_| {
                            unavailable(&input.label, "key environment value is not text")
                        })?
                        .into_bytes(),
                );
                (bytes, LocalKeySource::Env)
            }
            KeySource::Credential { name } => {
                let directory = context
                    .credential_directory
                    .as_ref()
                    .filter(|p| p.is_absolute())
                    .ok_or_else(|| {
                        unavailable(&input.label, "credential directory is unavailable")
                    })?;
                let path = directory
                    .join(name)
                    .to_str()
                    .ok_or_else(|| unavailable(&input.label, "credential path is not text"))?
                    .to_owned();
                let directory = directory.clone();
                let context = context.clone();
                let label = input.label.clone();
                (
                    tokio::task::spawn_blocking(move || {
                        read_private(&path, &context, &label, Some(&directory))
                    })
                    .await
                    .map_err(|_| unavailable(&input.label, "credential worker failed"))??,
                    LocalKeySource::Credential,
                )
            }
            KeySource::Command { argv, timeout_secs } => (
                command(argv, *timeout_secs, ctl, &input.label).await?,
                LocalKeySource::Command,
            ),
        };
        ctl.check()?;
        if bytes.is_empty() || bytes.len() > LIMIT {
            bail!(
                "{}",
                unavailable(&input.label, "key input must contain 1..4096 bytes")
            );
        }
        if matches!(input.key, KeySource::PassphraseFile { .. }) {
            out.passphrases
                .push(Passphrase::new(&input.label, std::mem::take(&mut *bytes))?);
        } else {
            out.keys.push(LocalKey::new(
                &input.label,
                source,
                decode_key(&bytes, &input.label)?,
            )?);
        }
    }
    Ok(out)
}

#[cfg(all(test, any(target_os = "linux", target_os = "android")))]
mod tests {
    use super::super::config::KeyInput;
    use super::*;
    fn settings(key: KeySource) -> RepositoryEncryption {
        RepositoryEncryption {
            keys: vec![KeyInput {
                label: "test".into(),
                key,
            }],
            single_key_ok: true,
        }
    }
    fn private(path: &Path, bytes: &[u8]) {
        std::fs::write(path, bytes).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600)).unwrap();
        }
    }
    #[test]
    fn key_encodings_are_strict_and_errors_do_not_echo_inputs() {
        assert_eq!(*decode_key(&[7; 32], "test").unwrap(), [7; 32]);
        assert_eq!(
            *decode_key(
                b" 0707070707070707070707070707070707070707070707070707070707070707\n",
                "test"
            )
            .unwrap(),
            [7; 32]
        );
        assert_eq!(
            *decode_key(STANDARD.encode([7; 32]).as_bytes(), "test").unwrap(),
            [7; 32]
        );
        // A typed 32-character password must not silently become the key.
        let mut typed = *b"correct horse battery staple 12\n";
        for password in [b"correct-horse-battery-staple-123".as_slice(), &typed] {
            let error = decode_key(password, "test").unwrap_err().to_string();
            assert!(error.contains("hexadecimal"), "{error}");
            assert!(!error.contains("horse"));
        }
        // One non-text byte makes it raw key material again.
        typed[0] = 0x80;
        assert_eq!(decode_key(&typed, "test").unwrap().as_slice(), &typed);
        for input in [
            b"secret-needle-invalid".as_slice(),
            b"AA==",
            b"AQ",
            b"\xff\xff",
        ] {
            let error = decode_key(input, "test").unwrap_err().to_string();
            assert!(!error.contains("secret-needle"));
        }
    }
    #[test]
    fn key_files_must_belong_to_the_effective_user_or_root() {
        assert!(owner_allowed(1000, 1000));
        assert!(owner_allowed(0, 1000));
        assert!(owner_allowed(0, 0));
        assert!(!owner_allowed(1001, 1000));
        assert!(!owner_allowed(1000, 0));
    }
    #[tokio::test]
    async fn key_commands_receive_only_the_allowed_environment() {
        let output = command(&["/usr/bin/env".into()], 5, &Ctl::default(), "test")
            .await
            .unwrap();
        let text = String::from_utf8(output.to_vec()).unwrap();
        for line in text.lines() {
            let name = line.split_once('=').map_or(line, |(name, _)| name);
            assert!(COMMAND_ENVIRONMENT.contains(&name), "leaked {name}");
        }
        // Cargo gives every test process variables outside the allowlist.
        assert!(std::env::vars_os().any(|(name, _)| {
            !COMMAND_ENVIRONMENT
                .iter()
                .any(|allowed| name.to_str() == Some(allowed))
        }));
    }
    #[tokio::test]
    async fn private_files_credentials_bounds_and_forbidden_roots() {
        let tmp = tempfile::tempdir().unwrap();
        let file = tmp.path().join("key");
        private(&file, &[8; 32]);
        let source = settings(KeySource::File {
            path: file.to_str().unwrap().into(),
        });
        assert_eq!(
            resolve(&source, &KeyContext::default(), &Ctl::default())
                .await
                .unwrap()
                .keys
                .len(),
            1
        );
        let forbidden = KeyContext {
            forbidden: vec![tmp.path().into()],
            ..Default::default()
        };
        assert!(resolve(&source, &forbidden, &Ctl::default()).await.is_err());
        let credential = settings(KeySource::Credential { name: "key".into() });
        let context = KeyContext {
            credential_directory: Some(tmp.path().into()),
            ..Default::default()
        };
        assert!(
            resolve(&credential, &context, &Ctl::default())
                .await
                .is_ok()
        );
        assert!(
            resolve(&credential, &KeyContext::default(), &Ctl::default())
                .await
                .is_err()
        );
        assert!(
            resolve(
                &settings(KeySource::Credential {
                    name: "../key".into()
                }),
                &context,
                &Ctl::default()
            )
            .await
            .is_err()
        );
        private(&file, &vec![8; LIMIT + 1]);
        assert!(
            resolve(&source, &KeyContext::default(), &Ctl::default())
                .await
                .is_err()
        );
        private(&file, &[8; 32]);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o640)).unwrap();
            assert!(
                resolve(&source, &KeyContext::default(), &Ctl::default())
                    .await
                    .is_err()
            );
            std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o600)).unwrap();
            let outside = tempfile::tempdir().unwrap();
            let secret = outside.path().join("outside");
            private(&secret, &[9; 32]);
            std::fs::remove_file(&file).unwrap();
            std::os::unix::fs::symlink(secret, &file).unwrap();
            assert!(
                resolve(&credential, &context, &Ctl::default())
                    .await
                    .is_err()
            );
        }
        let directory = settings(KeySource::File {
            path: tmp.path().to_str().unwrap().into(),
        });
        assert!(
            resolve(&directory, &KeyContext::default(), &Ctl::default())
                .await
                .is_err()
        );
    }
    #[tokio::test]
    async fn passphrase_file_preserves_a_final_newline_and_is_not_a_local_key() {
        let tmp = tempfile::tempdir().unwrap();
        let file = tmp.path().join("phrase");
        private(&file, b"exact passphrase\n");
        let inputs = resolve(
            &settings(KeySource::PassphraseFile {
                path: file.to_str().unwrap().into(),
            }),
            &KeyContext::default(),
            &Ctl::default(),
        )
        .await
        .unwrap();
        assert!(inputs.keys.is_empty());
        let store = std::sync::Arc::new(sparkles_backup::object_store::memory::InMemory::new());
        let cfg = sparkles_backup::RepoConfig::from_url("test", "memory://").unwrap();
        let env = sparkles_backup::OpenEnv {
            store: Some(store),
            ..Default::default()
        };
        sparkles_backup::Repository::open_encrypted(&cfg, &env, &inputs)
            .await
            .unwrap();
        let changed = EncryptionOptions {
            passphrases: vec![Passphrase::new("test", b"exact passphrase".to_vec()).unwrap()],
            single_key_ok: true,
            ..Default::default()
        };
        assert!(
            sparkles_backup::Repository::open_encrypted(&cfg, &env, &changed)
                .await
                .is_err()
        );
        assert!(
            sparkles_backup::Repository::open_encrypted(&cfg, &env, &inputs)
                .await
                .is_ok()
        );
    }
    #[tokio::test]
    async fn command_argv_is_literal_and_errors_are_bounded_and_redacted() {
        let tmp = tempfile::tempdir().unwrap();
        let script = tmp.path().join("provider");
        std::fs::write(&script, "#!/bin/sh\nprintf '%s' \"$1\"\n").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o700)).unwrap();
        }
        let literal = "$(touch forbidden);*${HOME}";
        let bytes = command(
            &[script.to_str().unwrap().into(), literal.into()],
            2,
            &Ctl::default(),
            "test",
        )
        .await
        .unwrap();
        assert_eq!(bytes.as_slice(), literal.as_bytes());
        std::fs::write(&script, "#!/bin/sh\nprintf secret-needle >&2\nexit 7\n").unwrap();
        assert!(
            !command(
                &[script.to_str().unwrap().into()],
                2,
                &Ctl::default(),
                "test"
            )
            .await
            .unwrap_err()
            .to_string()
            .contains("secret-needle")
        );
        std::fs::write(&script, "#!/bin/sh\nyes secret-needle\n").unwrap();
        assert!(
            command(
                &[script.to_str().unwrap().into()],
                2,
                &Ctl::default(),
                "test"
            )
            .await
            .unwrap_err()
            .to_string()
            .contains("exceeds")
        );
        std::fs::write(&script, "#!/bin/sh\nsleep 30\n").unwrap();
        assert!(
            command(
                &[script.to_str().unwrap().into()],
                1,
                &Ctl::default(),
                "test"
            )
            .await
            .unwrap_err()
            .to_string()
            .contains("timed out")
        );
    }
    #[tokio::test]
    async fn successful_command_kills_detached_descendants_before_reaping() {
        let tmp = tempfile::tempdir().unwrap();
        let script = tmp.path().join("provider");
        let pidfile = tmp.path().join("pids");
        std::fs::write(&script,"#!/bin/sh\nsleep 30 >/dev/null 2>&1 &\nprintf '%s %s' \"$$\" \"$!\" > \"$1\"\nprintf 'aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa'\n").unwrap();
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o700)).unwrap();
        let output = command(
            &[
                script.to_str().unwrap().into(),
                pidfile.to_str().unwrap().into(),
            ],
            5,
            &Ctl::default(),
            "test",
        )
        .await
        .unwrap();
        assert_eq!(output.as_slice(), &[b'a'; 32]);
        let pids = std::fs::read_to_string(pidfile).unwrap();
        tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                let stopped = pids.split_whitespace().all(|pid| {
                    std::fs::read_to_string(format!("/proc/{pid}/stat")).map_or(true, |stat| {
                        stat.rsplit_once(')').unwrap().1.split_whitespace().next() == Some("Z")
                    })
                });
                if stopped {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
    }
    #[tokio::test]
    async fn aborted_command_future_keeps_an_explicit_reaper_alive() {
        let tmp = tempfile::tempdir().unwrap();
        let pidfile = tmp.path().join("pid");
        let argv = vec![
            "/bin/sh".into(),
            "-c".into(),
            "printf '%s' \"$$\" > \"$1\"; sleep 30".into(),
            "provider".into(),
            pidfile.to_str().unwrap().into(),
        ];
        let task = tokio::spawn(async move { command(&argv, 30, &Ctl::default(), "test").await });
        let pid: libc::pid_t = tokio::time::timeout(Duration::from_secs(3), async {
            loop {
                if let Ok(text) = std::fs::read_to_string(&pidfile)
                    && let Ok(pid) = text.parse()
                {
                    break pid;
                }
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .unwrap();
        task.abort();
        assert!(task.await.unwrap_err().is_cancelled());
        tokio::time::timeout(Duration::from_secs(3), async {
            while unsafe { libc::kill(pid, 0) } == 0 {
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .expect("provider must be reaped, not only killed");
    }

    #[test]
    fn stopped_runtime_does_not_abandon_provider_reaping() {
        let tmp = tempfile::tempdir().unwrap();
        let pidfile = tmp.path().join("pid");
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let argv = vec![
            "/bin/sh".into(),
            "-c".into(),
            "printf '%s' \"$$\" > \"$1\"; sleep 30".into(),
            "provider".into(),
            pidfile.to_str().unwrap().into(),
        ];
        let pid: libc::pid_t = runtime.block_on(async {
            tokio::spawn(async move { command(&argv, 30, &Ctl::default(), "test").await });
            tokio::time::timeout(Duration::from_secs(3), async {
                loop {
                    if let Ok(text) = std::fs::read_to_string(&pidfile)
                        && let Ok(pid) = text.parse()
                    {
                        break pid;
                    }
                    tokio::time::sleep(Duration::from_millis(5)).await;
                }
            })
            .await
            .unwrap()
        });
        drop(runtime);
        let deadline = std::time::Instant::now() + Duration::from_secs(3);
        while unsafe { libc::kill(pid, 0) } == 0 {
            assert!(
                std::time::Instant::now() < deadline,
                "provider survived runtime shutdown as a zombie"
            );
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    #[tokio::test]
    async fn failed_reaper_thread_spawn_retains_child_for_synchronous_reap() {
        let child = tokio::process::Command::new("/bin/sh")
            .args(["-c", "sleep 30"])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .process_group(0)
            .kill_on_drop(true)
            .spawn()
            .unwrap();
        let pid = child.id().unwrap() as libc::pid_t;
        let process = Process {
            child: Some(child),
            group: pid,
            fail_reaper_spawn: true,
        };
        drop(process);
        assert_eq!(
            unsafe { libc::kill(pid, 0) },
            -1,
            "fallback must reap the owned provider"
        );
    }

    #[tokio::test]
    async fn canceled_command_kills_its_child_group_and_reaps_the_provider() {
        let tmp = tempfile::tempdir().unwrap();
        let script = tmp.path().join("provider");
        let pidfile = tmp.path().join("pids");
        std::fs::write(
            &script,
            "#!/bin/sh\nsleep 30 &\nprintf '%s %s' \"$$\" \"$!\" > \"$1\"\nwait\n",
        )
        .unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o700)).unwrap();
        }
        let ctl = Ctl::default();
        let cancel = ctl.cancel.clone();
        let argv = vec![
            script.to_str().unwrap().into(),
            pidfile.to_str().unwrap().into(),
        ];
        let task = tokio::spawn(async move { command(&argv, 5, &ctl, "test").await });
        tokio::time::timeout(Duration::from_secs(3), async {
            while !pidfile.exists() {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        let pids = std::fs::read_to_string(pidfile).unwrap();
        cancel.store(true, std::sync::atomic::Ordering::Relaxed);
        assert!(
            tokio::time::timeout(Duration::from_secs(2), task)
                .await
                .unwrap()
                .unwrap()
                .is_err()
        );
        let mut pids = pids.split_whitespace();
        let provider: libc::pid_t = pids.next().unwrap().parse().unwrap();
        assert_eq!(
            unsafe { libc::kill(provider, 0) },
            -1,
            "provider must be reaped"
        );
        let descendants: Vec<_> = pids.collect();
        tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                // SIGKILL is asynchronous for descendants, which belong to their
                // adopter after the owned provider is reaped. Require them stopped.
                if descendants.iter().all(|pid| {
                    std::fs::read_to_string(format!("/proc/{pid}/stat")).map_or(true, |stat| {
                        stat.rsplit_once(')').unwrap().1.split_whitespace().next() == Some("Z")
                    })
                }) {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .expect("provider descendants were not killed");
    }
}
