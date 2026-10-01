//! Repository configuration, URL parsing, backend construction, the marker
//! (attach or initialize), the connection test, listing and deletion.
//!
//! Backend notes (checked against `object_store` 0.14.2):
//!
//! * `LocalFileSystem` does **not** fsync by default. `with_fsync(true)` makes
//!   `put_opts`, `copy_opts`, `rename_opts` and multipart completion call `sync_all` on
//!   the written file and fsync the parent directories (Unix only) before returning;
//!   plain deletes are never synced. `fs` repositories must be built with it.
//!   `PutMode::Create` writes a staging file and publishes it with a hard link
//!   (`AlreadyExists` if the key exists), so the target filesystem must support hard
//!   links (some SMB/FUSE mounts do not: the connection test's create step fails there).
//!   `PutMode::Update` is `NotImplemented` on `LocalFileSystem`.
//! * `AmazonS3` defaults to `S3ConditionalPut::ETagMatch`: `PutMode::Create` sends
//!   `If-None-Match: *` and maps `304`/`412` to `Error::AlreadyExists` (R2 answers
//!   `412`). With `S3ConditionalPut::Disabled` (`conditionalWrites: false`),
//!   `PutMode::Create` fails with `Error::NotImplemented`, so single-writer
//!   repositories must use HEAD then `PutMode::Overwrite`.
//! * `RetryConfig::default()` is 10 retries within 3 minutes, the spec's policy.
//! * SSE: `AmazonS3ConfigKey::ServerSideEncryption` = `AES256` for SSE-S3;
//!   `AmazonS3Builder::with_sse_kms_encryption(key_id)` for SSE-KMS.
//!
//! Every repository's store is wrapped in [`RepoStore`]: it counts object requests by
//! operation and outcome ([`Repository::requests`], the `object_requests_total`
//! metric) and retries transient errors of backends without an HTTP retry layer of
//! their own (`fs`, `memory`, and stores passed in [`OpenEnv::store`]).

use crate::cache::ManifestCache;
use crate::error::{Code, Result};
use crate::layout::{self, Marker};
use crate::throttle::Throttle;
use crate::{
    BackupError, BackupSummary, Credentials, Ctl, ListFilter, LockKind, LockOperation, Manifest,
    OpenEnv, RepoConfig, RepoStats, RepoType, Sse, TestReport, TestStep, TestStepKind, lock,
};
use bytes::Bytes;
use futures::{StreamExt, TryStreamExt};
use object_store::path::Path as Key;
use object_store::{
    ObjectMeta, ObjectStore, ObjectStoreExt, PutMode, PutOptions, PutPayload, PutResult,
};
use sparkles::outbound::OutboundPolicy;
use std::collections::HashSet;
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU8, Ordering};
use std::time::Instant;
use uuid::Uuid;

pub use instrumented::{OpCounts, RepoStore, RequestCounts, RequestOp, RequestStats};

/// An opened repository: a backend store, its marker, and this process's settings for
/// it. Cheap to share (`Arc<Repository>`); every method takes `&self`.
pub struct Repository {
    /// the backend, rooted at the repository (the prefix is applied by the store),
    /// wrapped in a [`RepoStore`]
    pub(crate) store: Arc<dyn ObjectStore>,
    pub(crate) config: RepoConfig,
    pub(crate) marker: Marker,
    pub(crate) env: OpenEnv,
    pub(crate) cache: ManifestCache,
    pub(crate) upload: Throttle,
    pub(crate) download: Throttle,
    /// object requests by operation and outcome (shared with the [`RepoStore`])
    pub(crate) requests: Arc<RequestStats>,
    /// conditional-write support as last detected: [`COND_UNKNOWN`], [`COND_YES`] or
    /// [`COND_NO`] (the connection test, or a create answered `NotImplemented`)
    pub(crate) conditional: AtomicU8,
}

pub(crate) const COND_UNKNOWN: u8 = 0;
pub(crate) const COND_YES: u8 = 1;
pub(crate) const COND_NO: u8 = 2;

impl std::fmt::Debug for Repository {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Repository")
            .field("name", &self.config.name)
            .field("location", &self.config.location())
            .field("id", &self.marker.id)
            .field("store", &self.store.to_string())
            .field("lock_wait", &self.env.lock_wait)
            .field("cache", &self.cache)
            .field("upload", &self.upload)
            .field("download", &self.download)
            .finish()
    }
}

fn invalid(field: &str, msg: impl std::fmt::Display) -> BackupError {
    BackupError::new(Code::InvalidConfig, format!("{field}: {msg}")).with("field", field)
}

/// Query parameters that carry secrets: refused in URLs, whatever their value.
const SECRET_PARAMS: [&str; 10] = [
    "access_key_id",
    "secret_access_key",
    "session_token",
    "token",
    "aws_access_key_id",
    "aws_secret_access_key",
    "aws_session_token",
    "password",
    "sas_token",
    "service_account_key",
];

impl RepoConfig {
    /// A configuration from a `--repo` URL: `file:///abs/dir`,
    /// `s3://bucket/prefix?region=…&endpoint=…&path_style=true&allow_http=true`,
    /// `memory://`, and (with their features) `gs://bucket/prefix`, `az://container/prefix`.
    /// Userinfo and credential query parameters are refused (`400 invalid-config`), as
    /// are unknown parameters.
    ///
    /// Every type also takes `readonly`, `conditional_writes`, `max_concurrency`,
    /// `max_upload_bytes_per_sec` and `max_download_bytes_per_sec`; `s3` also `sse` and
    /// `kms_key_id`. The result is not validated: see [`validate`](Self::validate).
    pub fn from_url(name: &str, url: &str) -> Result<RepoConfig> {
        let (scheme, rest) = url
            .split_once("://")
            .ok_or_else(|| invalid("url", format!("{url:?} is not a repository URL")))?;
        let kind = match scheme.to_ascii_lowercase().as_str() {
            "file" => RepoType::Fs,
            "s3" | "s3a" => RepoType::S3,
            "gs" | "gcs" => RepoType::Gcs,
            "az" | "azure" | "abfs" => RepoType::Azure,
            "memory" => RepoType::Memory,
            other => return Err(invalid("url", format!("unknown scheme {other:?}"))),
        };
        let (rest, query) = match rest.split_once('?') {
            Some((r, q)) => (r, Some(q)),
            None => (rest, None),
        };
        let rest = rest.split_once('#').map_or(rest, |(r, _)| r);
        let (authority, path) = match rest.find('/') {
            Some(i) => (&rest[..i], &rest[i..]),
            None => (rest, ""),
        };
        if authority.contains('@') {
            return Err(invalid(
                "url",
                "credentials are not accepted in repository URLs: use the environment or a \
                 credentials file",
            ));
        }
        let decode = |s: &str| -> Result<String> {
            percent_encoding::percent_decode_str(s)
                .decode_utf8()
                .map(|c| c.into_owned())
                .map_err(|_| invalid("url", "not valid UTF-8 after percent-decoding"))
        };
        let mut c = RepoConfig {
            name: name.to_string(),
            kind,
            // the wire default (`RepoConfig::default()` has `false`)
            conditional_writes: true,
            ..Default::default()
        };
        match kind {
            RepoType::Fs => {
                if !(authority.is_empty() || authority.eq_ignore_ascii_case("localhost")) {
                    return Err(invalid("url", "file URLs name no host: file:///abs/dir"));
                }
                c.path = Some(decode(path)?);
            }
            RepoType::Memory => {
                if !authority.is_empty() || !path.trim_matches('/').is_empty() {
                    return Err(invalid("url", "memory:// takes no location"));
                }
            }
            _ => {
                if authority.is_empty() {
                    return Err(invalid("url", "no bucket"));
                }
                c.bucket = Some(decode(authority)?);
                let prefix = decode(path.trim_matches('/'))?;
                c.prefix = (!prefix.is_empty()).then_some(prefix);
            }
        }
        for pair in query.unwrap_or("").split('&').filter(|p| !p.is_empty()) {
            let (k, v) = pair.split_once('=').unwrap_or((pair, ""));
            let k = decode(k)?;
            let v = decode(v)?;
            if SECRET_PARAMS.contains(&k.to_ascii_lowercase().as_str()) {
                return Err(invalid(
                    &k,
                    "credentials are not accepted in repository URLs: use the environment \
                     or a credentials file",
                ));
            }
            let flag = |v: &str| match v {
                "" | "true" | "1" | "yes" => Ok(true),
                "false" | "0" | "no" => Ok(false),
                _ => Err(invalid(&k, format!("{v:?} is not a boolean"))),
            };
            let num = |v: &str| {
                v.parse::<u64>()
                    .map_err(|_| invalid(&k, format!("{v:?} is not a number")))
            };
            let s3 = kind == RepoType::S3;
            match k.as_str() {
                "readonly" => c.readonly = flag(&v)?,
                "conditional_writes" => c.conditional_writes = flag(&v)?,
                "max_concurrency" => c.max_concurrency = Some(num(&v)?.min(u32::MAX as u64) as u32),
                "max_upload_bytes_per_sec" => c.max_upload_bytes_per_sec = Some(num(&v)?),
                "max_download_bytes_per_sec" => c.max_download_bytes_per_sec = Some(num(&v)?),
                "region" if s3 => c.region = Some(v),
                "endpoint" if s3 => c.endpoint = Some(v),
                "path_style" if s3 => c.path_style = flag(&v)?,
                "allow_http" if s3 => c.allow_http = flag(&v)?,
                "sse" if s3 => {
                    c.sse = Some(match v.as_str() {
                        "AES256" => Sse::Aes256,
                        "aws:kms" => Sse::AwsKms,
                        _ => return Err(invalid("sse", format!("{v:?} is not AES256 or aws:kms"))),
                    })
                }
                "kms_key_id" if s3 => c.kms_key_id = Some(v),
                _ => return Err(invalid(&k, "unknown parameter")),
            }
        }
        Ok(c)
    }

    /// Check a configuration before use (`400 invalid-config` naming the field): the
    /// name grammar; the fields the type needs and no others' (`path` for `fs`,
    /// `bucket` for `s3`); an absolute `fs` path outside every `forbid_under`; an
    /// `endpoint` URL without userinfo or query, `http://` only with `allow_http`;
    /// `kmsKeyId` only with `sse: "aws:kms"`; limits > 0.
    pub fn validate(&self, forbid_under: &[PathBuf]) -> Result<()> {
        if !layout::valid_repo_name(&self.name) {
            return Err(BackupError::new(
                Code::InvalidName,
                format!(
                    "invalid repository name {:?}: [a-z0-9][a-z0-9_-]{{0,63}}",
                    self.name
                ),
            ));
        }
        let t = self.kind.as_str();
        let none = |field: &str, set: bool| {
            if set {
                Err(invalid(field, format!("not used by {t} repositories")))
            } else {
                Ok(())
            }
        };
        let cloud = matches!(self.kind, RepoType::S3 | RepoType::Gcs | RepoType::Azure);
        let s3 = self.kind == RepoType::S3;
        none("path", self.kind != RepoType::Fs && self.path.is_some())?;
        none("bucket", !cloud && self.bucket.is_some())?;
        none("prefix", !cloud && self.prefix.is_some())?;
        none("region", !s3 && self.region.is_some())?;
        none("endpoint", !s3 && self.endpoint.is_some())?;
        none("pathStyle", !s3 && self.path_style)?;
        none("allowHttp", !s3 && self.allow_http)?;
        none("sse", !s3 && self.sse.is_some())?;
        none("kmsKeyId", !s3 && self.kms_key_id.is_some())?;
        none(
            "credentials",
            !s3 && self.credentials != Credentials::Default,
        )?;
        match self.kind {
            RepoType::Fs => {
                let p = self
                    .path
                    .as_deref()
                    .filter(|p| !p.is_empty())
                    .ok_or_else(|| invalid("path", "required for fs repositories"))?;
                let p = Path::new(p);
                if !p.is_absolute() {
                    return Err(invalid("path", "must be absolute"));
                }
                if p.components().any(|c| c == Component::ParentDir) {
                    return Err(invalid("path", "must not contain `..`"));
                }
                for dir in forbid_under {
                    if is_within(p, dir) {
                        return Err(invalid(
                            "path",
                            format!("must not be inside {}", dir.display()),
                        ));
                    }
                }
            }
            RepoType::S3 | RepoType::Gcs | RepoType::Azure => {
                let b = self.bucket.as_deref().unwrap_or("");
                if b.is_empty() || b.contains('/') {
                    return Err(invalid("bucket", "required, without `/`"));
                }
                // (the bucket and the region become part of the service's host name)
                if b.len() > 255 || !b.bytes().all(host_part) {
                    return Err(invalid("bucket", "use letters, digits, `.`, `_` and `-`"));
                }
                let prefix = self.prefix.as_deref().unwrap_or("").trim_matches('/');
                if !prefix.is_empty()
                    && prefix
                        .split('/')
                        .any(|s| s.is_empty() || s == "." || s == "..")
                {
                    return Err(invalid("prefix", "empty, `.` or `..` segments"));
                }
            }
            RepoType::Memory => {}
        }
        if let Some(r) = &self.region
            && (r.is_empty() || r.len() > 64 || !r.bytes().all(|c| c != b'.' && host_part(c)))
        {
            return Err(invalid("region", "use letters, digits, `_` and `-`"));
        }
        if let Some(e) = &self.endpoint {
            let (scheme, rest) = e
                .split_once("://")
                .ok_or_else(|| invalid("endpoint", "not a URL"))?;
            match scheme {
                "https" => {}
                "http" if self.allow_http => {}
                "http" => return Err(invalid("endpoint", "http:// needs allowHttp: true")),
                _ => return Err(invalid("endpoint", "must be http:// or https://")),
            }
            let authority = rest.split('/').next().unwrap_or("");
            if authority.is_empty() {
                return Err(invalid("endpoint", "no host"));
            }
            if authority.contains('@') {
                return Err(invalid("endpoint", "credentials are not accepted in URLs"));
            }
            if rest.contains(['?', '#']) {
                return Err(invalid("endpoint", "no query or fragment"));
            }
        }
        if self.kms_key_id.is_some() && self.sse != Some(Sse::AwsKms) {
            return Err(invalid("kmsKeyId", "only with sse: \"aws:kms\""));
        }
        match &self.credentials {
            Credentials::Default => {}
            Credentials::Env {
                access_key_id_var,
                secret_access_key_var,
                session_token_var,
            } => {
                for (f, v) in [
                    ("credentials.accessKeyIdVar", Some(access_key_id_var)),
                    (
                        "credentials.secretAccessKeyVar",
                        Some(secret_access_key_var),
                    ),
                    ("credentials.sessionTokenVar", session_token_var.as_ref()),
                ] {
                    if let Some(v) = v
                        && (v.is_empty() || v.contains(['=', '\0']))
                    {
                        return Err(invalid(f, "not an environment variable name"));
                    }
                }
            }
            Credentials::File { path } => {
                if !Path::new(path).is_absolute() {
                    return Err(invalid("credentials.path", "must be absolute"));
                }
            }
            Credentials::Named { name } => {
                if !layout::valid_repo_name(name) {
                    return Err(invalid(
                        "credentials.name",
                        "not a credential source name ([a-z0-9][a-z0-9_-]{0,63})",
                    ));
                }
            }
        }
        if self.max_concurrency == Some(0) {
            return Err(invalid("maxConcurrency", "must be at least 1"));
        }
        if self.max_upload_bytes_per_sec == Some(0) {
            return Err(invalid(
                "maxUploadBytesPerSec",
                "must be positive (or null)",
            ));
        }
        if self.max_download_bytes_per_sec == Some(0) {
            return Err(invalid(
                "maxDownloadBytesPerSec",
                "must be positive (or null)",
            ));
        }
        Ok(())
    }
}

/// A byte of a bucket name or a region: letters, digits, `.`, `_` and `-`.
fn host_part(c: u8) -> bool {
    c.is_ascii_alphanumeric() || matches!(c, b'.' | b'_' | b'-')
}

/// Whether `p` is `dir` or inside it: compared lexically, and again with both
/// canonicalized (symbolic links) when they exist.
pub fn is_within(p: &Path, dir: &Path) -> bool {
    fn norm(p: &Path) -> PathBuf {
        let mut out = PathBuf::new();
        for c in p.components() {
            match c {
                Component::CurDir => {}
                Component::ParentDir => {
                    out.pop();
                }
                c => out.push(c),
            }
        }
        out
    }
    if norm(p).starts_with(norm(dir)) {
        return true;
    }
    // the repository directory may not exist yet: canonicalize its nearest ancestor
    let mut existing = p.to_path_buf();
    let mut tail = Vec::new();
    while !existing.exists() {
        match (
            existing.file_name().map(|f| f.to_os_string()),
            existing.parent(),
        ) {
            (Some(f), Some(parent)) => {
                tail.push(f);
                existing = parent.to_path_buf();
            }
            _ => return false,
        }
    }
    let (Ok(mut cp), Ok(cd)) = (existing.canonicalize(), dir.canonicalize()) else {
        return false;
    };
    for f in tail.into_iter().rev() {
        cp.push(f);
    }
    cp.starts_with(cd)
}

/// Build the backend store of a configuration: `LocalFileSystem` (with fsync, and the
/// directory created) for `fs`; `AmazonS3` with `RetryConfig::default()`, the
/// credentials source, SSE, and `S3ConditionalPut::Disabled` when `conditionalWrites`
/// is false, for `s3`; `InMemory` for `memory`. Wrapped in `LimitStore`
/// (`maxConcurrency`) and a `PrefixStore` for a prefix. Credentials files are read here
/// (so rotation works); their contents never appear in errors.
pub fn build_store(cfg: &RepoConfig) -> Result<Arc<dyn ObjectStore>> {
    build_store_with(cfg, None)
}

/// [`build_store`], with the network destinations of `s3`, `gcs` and `azure`
/// repositories under `outbound` (see [`OpenEnv::outbound`]): the endpoint (or the
/// service's host) is checked now, and every connection resolves its host through the
/// policy, so it reaches only addresses the policy allows (a later DNS answer cannot
/// point it elsewhere). A refused destination is `400 invalid-config`.
pub fn build_store_with(
    cfg: &RepoConfig,
    outbound: Option<&OutboundPolicy>,
) -> Result<Arc<dyn ObjectStore>> {
    if let Some(p) = outbound
        && matches!(cfg.kind, RepoType::S3 | RepoType::Gcs | RepoType::Azure)
    {
        check_destination(cfg, p)?;
    }
    let base: Arc<dyn ObjectStore> = match cfg.kind {
        RepoType::Memory => Arc::new(object_store::memory::InMemory::new()),
        RepoType::Fs => fs_store(cfg)?,
        RepoType::S3 => s3_store(cfg, outbound)?,
        RepoType::Gcs | RepoType::Azure => cloud_store(cfg, outbound)?,
    };
    Ok(limited(base, cfg.concurrency()))
}

/// Check the endpoint of a repository against an outbound policy: its scheme and
/// address, and the addresses its host name resolves to now (connections are checked
/// again as they are made, see [`PolicyResolver`]). Without an endpoint the service's
/// own host is resolved when connecting.
fn check_destination(cfg: &RepoConfig, p: &OutboundPolicy) -> Result<()> {
    let Some(e) = &cfg.endpoint else {
        return Ok(());
    };
    let refused = |f: sparkles::outbound::Failure| {
        invalid(
            "endpoint",
            format!("the server's outbound policy refuses it: {f}"),
        )
    };
    let url = p.check_url(e).map_err(refused)?;
    // an address was checked by `check_url`; a name is resolved
    if let Some(host) = url.host_str()
        && host
            .trim_matches(['[', ']'])
            .parse::<std::net::IpAddr>()
            .is_err()
    {
        p.check_host(host).map_err(refused)?;
    }
    Ok(())
}

/// Resolves the host names of a repository's connections through an outbound policy
/// (every address must be allowed; the connection goes to exactly those addresses).
/// Every host name and every address: excluded from the placeholder proxy of
/// repositories under an outbound policy.
#[cfg(any(feature = "s3", feature = "gcs", feature = "azure"))]
const NO_PROXY_EXCLUDES: &str = "*,0.0.0.0/0,::/0";

#[cfg(any(feature = "s3", feature = "gcs", feature = "azure"))]
#[derive(Debug)]
struct PolicyResolver(std::panic::AssertUnwindSafe<OutboundPolicy>);

#[cfg(any(feature = "s3", feature = "gcs", feature = "azure"))]
impl PolicyResolver {
    /// Client options whose connections resolve through `p`, and never through a proxy
    /// of the environment (`HTTPS_PROXY`, `HTTP_PROXY`, `ALL_PROXY`): a proxy would
    /// resolve and connect to the endpoint itself, past this resolver. A proxy that
    /// excludes every host and address takes the place of the environment's (the HTTP
    /// client reads the environment only when no proxy is configured) and is never
    /// used.
    fn options(p: &OutboundPolicy) -> object_store::ClientOptions {
        object_store::ClientOptions::new()
            .with_dns_resolver(Arc::new(PolicyResolver(std::panic::AssertUnwindSafe(
                p.clone(),
            ))))
            .with_proxy_url("http://127.0.0.1:9")
            .with_proxy_excludes(NO_PROXY_EXCLUDES)
    }
}

#[cfg(any(feature = "s3", feature = "gcs", feature = "azure"))]
impl object_store::client::DnsResolver for PolicyResolver {
    fn resolve(&self, host: &str) -> object_store::client::DnsFuture {
        let (p, host) = (self.0.0.clone(), host.to_string());
        Box::pin(async move {
            let checked = tokio::task::spawn_blocking(move || p.check_host(&host)).await?;
            checked.map_err(|f| f.to_string().into())
        })
    }
}

fn limited(store: Arc<dyn ObjectStore>, n: usize) -> Arc<dyn ObjectStore> {
    Arc::new(object_store::limit::LimitStore::new(store, n.max(1)))
}

#[cfg(feature = "fs")]
fn fs_store(cfg: &RepoConfig) -> Result<Arc<dyn ObjectStore>> {
    let path = cfg
        .path
        .as_deref()
        .ok_or_else(|| invalid("path", "required for fs repositories"))?;
    if !cfg.readonly {
        std::fs::create_dir_all(path).map_err(|e| {
            BackupError::new(
                Code::RepositoryUnavailable,
                format!("repository unavailable: cannot create {path}: {e}"),
            )
        })?;
    }
    let fs = object_store::local::LocalFileSystem::new_with_prefix(path)
        .map_err(|e| {
            BackupError::new(
                Code::RepositoryUnavailable,
                format!("repository unavailable: {path}: {e}"),
            )
        })?
        .with_fsync(true);
    Ok(Arc::new(fs))
}

#[cfg(not(feature = "fs"))]
fn fs_store(_: &RepoConfig) -> Result<Arc<dyn ObjectStore>> {
    Err(invalid(
        "type",
        "fs repositories are not built into this binary",
    ))
}

#[cfg(feature = "s3")]
fn s3_store(cfg: &RepoConfig, outbound: Option<&OutboundPolicy>) -> Result<Arc<dyn ObjectStore>> {
    use object_store::aws::{AmazonS3Builder, AmazonS3ConfigKey, S3ConditionalPut};
    let mut b = match &cfg.credentials {
        Credentials::Default => AmazonS3Builder::from_env(),
        Credentials::Env {
            access_key_id_var,
            secret_access_key_var,
            session_token_var,
        } => {
            let var = |field: &str, v: &str| {
                std::env::var(v)
                    .map_err(|_| invalid(field, format!("environment variable {v} is not set")))
            };
            let mut b = AmazonS3Builder::new()
                .with_access_key_id(var("credentials.accessKeyIdVar", access_key_id_var)?)
                .with_secret_access_key(var(
                    "credentials.secretAccessKeyVar",
                    secret_access_key_var,
                )?);
            if let Some(t) = session_token_var {
                b = b.with_token(var("credentials.sessionTokenVar", t)?);
            }
            b
        }
        Credentials::File { path } => {
            #[derive(serde::Deserialize)]
            #[serde(rename_all = "camelCase")]
            struct Keys {
                access_key_id: String,
                secret_access_key: String,
                #[serde(default)]
                session_token: Option<String>,
            }
            // never echo the file's content in an error
            let bytes = std::fs::read(path)
                .map_err(|e| invalid("credentials.path", format!("{path}: {}", e.kind())))?;
            let k: Keys = serde_json::from_slice(&bytes).map_err(|_| {
                invalid(
                    "credentials.path",
                    format!("{path}: not JSON {{accessKeyId, secretAccessKey, sessionToken?}}"),
                )
            })?;
            let mut b = AmazonS3Builder::new()
                .with_access_key_id(k.access_key_id)
                .with_secret_access_key(k.secret_access_key);
            if let Some(t) = k.session_token {
                b = b.with_token(t);
            }
            b
        }
        Credentials::Named { name } => {
            return Err(invalid(
                "credentials.name",
                format!("the credential source {name:?} is defined by the server, not here"),
            ));
        }
    };
    if !matches!(cfg.credentials, Credentials::Default) && cfg.region.is_none() {
        // `from_env` reads the region itself; explicit credentials start from `new`
        if let Some(r) = ["AWS_REGION", "AWS_DEFAULT_REGION"]
            .iter()
            .find_map(|v| std::env::var(v).ok())
        {
            b = b.with_region(r);
        }
    }
    if let Some(p) = outbound {
        // (fresh client options: no proxy from the environment's AWS_* settings either)
        b = b.with_client_options(PolicyResolver::options(p));
    }
    b = b
        .with_bucket_name(cfg.bucket.clone().unwrap_or_default())
        .with_retry(object_store::RetryConfig::default())
        .with_allow_http(cfg.allow_http);
    if let Some(r) = &cfg.region {
        b = b.with_region(r);
    }
    match &cfg.endpoint {
        // with an endpoint the bucket is always a path segment (MinIO, R2, Ceph RGW)
        Some(e) => b = b.with_endpoint(e).with_virtual_hosted_style_request(false),
        None => b = b.with_virtual_hosted_style_request(!cfg.path_style),
    }
    if !cfg.conditional_writes {
        b = b.with_conditional_put(S3ConditionalPut::Disabled);
    }
    match (cfg.sse, &cfg.kms_key_id) {
        (Some(Sse::AwsKms), Some(key)) => b = b.with_sse_kms_encryption(key),
        (Some(sse), _) => {
            let key: AmazonS3ConfigKey = "aws_server_side_encryption"
                .parse()
                .map_err(|e| BackupError::internal(format!("{e}")))?;
            let v = match sse {
                Sse::Aes256 => "AES256",
                Sse::AwsKms => "aws:kms",
            };
            b = b.with_config(key, v);
        }
        (None, _) => {}
    }
    let s3 = b
        .build()
        .map_err(|e| invalid("s3", crate::error::redact_urls(&e.to_string())))?;
    Ok(prefixed(Arc::new(s3), cfg.prefix.as_deref()))
}

#[cfg(not(feature = "s3"))]
fn s3_store(_: &RepoConfig, _: Option<&OutboundPolicy>) -> Result<Arc<dyn ObjectStore>> {
    Err(invalid(
        "type",
        "s3 repositories are not built into this binary",
    ))
}

#[cfg_attr(not(any(feature = "gcs", feature = "azure")), allow(unused_variables))]
fn cloud_store(
    cfg: &RepoConfig,
    outbound: Option<&OutboundPolicy>,
) -> Result<Arc<dyn ObjectStore>> {
    #[cfg(feature = "gcs")]
    if cfg.kind == RepoType::Gcs {
        let mut b = object_store::gcp::GoogleCloudStorageBuilder::from_env();
        if let Some(p) = outbound {
            b = b.with_client_options(PolicyResolver::options(p));
        }
        let gcs = b
            .with_bucket_name(cfg.bucket.clone().unwrap_or_default())
            .with_retry(object_store::RetryConfig::default())
            .build()
            .map_err(|e| invalid("gcs", crate::error::redact_urls(&e.to_string())))?;
        return Ok(prefixed(Arc::new(gcs), cfg.prefix.as_deref()));
    }
    #[cfg(feature = "azure")]
    if cfg.kind == RepoType::Azure {
        let mut b = object_store::azure::MicrosoftAzureBuilder::from_env();
        if let Some(p) = outbound {
            b = b.with_client_options(PolicyResolver::options(p));
        }
        let az = b
            .with_container_name(cfg.bucket.clone().unwrap_or_default())
            .with_retry(object_store::RetryConfig::default())
            .build()
            .map_err(|e| invalid("azure", crate::error::redact_urls(&e.to_string())))?;
        return Ok(prefixed(Arc::new(az), cfg.prefix.as_deref()));
    }
    Err(invalid(
        "type",
        format!(
            "{} repositories are not built into this binary",
            cfg.kind.as_str()
        ),
    ))
}

#[cfg_attr(
    not(any(feature = "s3", feature = "gcs", feature = "azure")),
    allow(dead_code)
)]
fn prefixed(store: Arc<dyn ObjectStore>, prefix: Option<&str>) -> Arc<dyn ObjectStore> {
    match prefix
        .map(|p| p.trim_matches('/'))
        .filter(|p| !p.is_empty())
    {
        Some(p) => Arc::new(object_store::prefix::PrefixStore::new(store, p)),
        None => store,
    }
}

/// Whether an object-store error means "the object is not there".
pub(crate) fn is_not_found(e: &object_store::Error) -> bool {
    matches!(e, object_store::Error::NotFound { .. })
}

/// Whether an object-store error answers a conditional create of an existing key.
pub(crate) fn is_already_exists(e: &object_store::Error) -> bool {
    matches!(
        e,
        object_store::Error::AlreadyExists { .. } | object_store::Error::Precondition { .. }
    )
}

impl Repository {
    /// Attach to the repository at `cfg`'s location, or initialize it:
    /// * a marker: parse and check it (`422 incompatible-repository` for a newer
    ///   format or encryption);
    /// * nothing at all and `env.init`: create the marker with `PutMode::Create` (a
    ///   concurrent initializer's marker wins and is read back);
    /// * objects but no marker: `409 not-a-repository` (never write into a prefix
    ///   Sparkles did not create).
    ///
    /// Uses `env.store` instead of [`build_store`] when set. Read-only
    /// configurations never write (an empty location is then `409 not-a-repository`).
    /// Does not run the connection test.
    pub async fn open(cfg: &RepoConfig, env: &OpenEnv) -> Result<Repository> {
        cfg.validate(&env.forbid_under)?;
        let (inner, attempts) = match &env.store {
            Some(s) => (
                limited(s.clone(), cfg.concurrency()),
                instrumented::LOCAL_ATTEMPTS,
            ),
            None => {
                let s = build_store_with(cfg, env.outbound.as_ref())?;
                let attempts = match cfg.kind {
                    // the HTTP client retries (RetryConfig)
                    RepoType::S3 | RepoType::Gcs | RepoType::Azure => 1,
                    RepoType::Fs | RepoType::Memory => instrumented::LOCAL_ATTEMPTS,
                };
                (s, attempts)
            }
        };
        let requests = env.requests.clone().unwrap_or_default();
        let store: Arc<dyn ObjectStore> =
            Arc::new(RepoStore::new(inner, requests.clone(), attempts));
        let marker = attach_or_init(&store, cfg, env).await?;
        let cache_dir = env
            .cache_dir
            .as_ref()
            .map(|d| d.join(marker.id.to_string()));
        Ok(Repository {
            store,
            config: cfg.clone(),
            marker,
            env: env.clone(),
            cache: ManifestCache::new(cache_dir),
            upload: Throttle::new(cfg.max_upload_bytes_per_sec),
            download: Throttle::new(cfg.max_download_bytes_per_sec),
            requests,
            conditional: AtomicU8::new(if cfg.conditional_writes {
                COND_UNKNOWN
            } else {
                COND_NO
            }),
        })
    }

    /// The repository id (from the marker).
    pub fn id(&self) -> Uuid {
        self.marker.id
    }

    pub fn config(&self) -> &RepoConfig {
        &self.config
    }

    pub fn marker(&self) -> &Marker {
        &self.marker
    }

    /// The backend store (rooted at the repository).
    pub fn store(&self) -> &Arc<dyn ObjectStore> {
        &self.store
    }

    pub fn readonly(&self) -> bool {
        self.config.readonly
    }

    /// Object requests made through this repository so far, by operation and outcome
    /// ("not found" and "already exists" answers count as `ok`).
    pub fn requests(&self) -> RequestCounts {
        self.requests.snapshot()
    }

    /// Conditional-write support as detected by the last connection test (or a create
    /// the backend refused): `None` before any test.
    pub fn conditional_writes(&self) -> Option<bool> {
        match self.conditional.load(Ordering::Relaxed) {
            COND_YES => Some(true),
            COND_NO => Some(false),
            _ => None,
        }
    }

    /// No conditional creates, configured (`conditionalWrites: false`) or detected:
    /// names are then unique only with a single writer (HEAD, then an overwrite).
    pub fn single_writer(&self) -> bool {
        !self.config.conditional_writes || self.conditional.load(Ordering::Relaxed) == COND_NO
    }

    /// Create `key` unless it exists: `Ok(Some(put result))` if this call created it,
    /// `Ok(None)` if it already existed. A conditional create (`PutMode::Create`); for
    /// single-writer repositories (and a backend that answers `NotImplemented`, which is
    /// then remembered) a `HEAD` followed by an overwrite.
    pub(crate) async fn create_object(
        &self,
        key: &Key,
        payload: PutPayload,
    ) -> Result<Option<PutResult>> {
        if !self.single_writer() {
            match self
                .store
                .put_opts(key, payload.clone(), PutOptions::from(PutMode::Create))
                .await
            {
                Ok(r) => return Ok(Some(r)),
                Err(e) if is_already_exists(&e) => return Ok(None),
                Err(object_store::Error::NotImplemented { .. }) => self.no_conditional_writes(),
                Err(e) => return Err(e.into()),
            }
        }
        match self.store.head(key).await {
            Ok(_) => return Ok(None),
            Err(e) if is_not_found(&e) => {}
            Err(e) => return Err(e.into()),
        }
        Ok(Some(self.store.put(key, payload).await?))
    }

    /// Remember that the backend refused a conditional create (`NotImplemented`).
    pub(crate) fn no_conditional_writes(&self) {
        if self.conditional.swap(COND_NO, Ordering::Relaxed) != COND_NO {
            tracing::warn!(
                target: "sparkles::backup",
                repository = %self.config.name,
                "the backend does not support conditional creates: single writer"
            );
        }
    }

    /// The connection test: create `probe/<uuid>` (`PutMode::Create`), create it again
    /// expecting "already exists" (`conditionalWrites`), read it back, list `probe/`,
    /// delete it. Each step's latency and error; later steps are skipped after a failed
    /// create. Read-only repositories only list (the report says so in the steps). Never
    /// fails as a whole: problems are in the report.
    pub async fn test(&self) -> Result<TestReport> {
        fn step(step: TestStepKind, t: Instant, r: std::result::Result<(), String>) -> TestStep {
            TestStep {
                step,
                ok: r.is_ok(),
                millis: t.elapsed().as_millis() as u64,
                error: r.err(),
            }
        }
        let msg = |e: object_store::Error| BackupError::from(e).message().to_string();
        let mut steps = Vec::new();
        if self.config.readonly {
            let t = Instant::now();
            let r = self
                .store
                .list(Some(&Key::from(layout::BACKUPS)))
                .try_next()
                .await
                .map(|_| ())
                .map_err(msg);
            steps.push(step(TestStepKind::List, t, r));
            return Ok(TestReport {
                ok: steps.iter().all(|s| s.ok),
                conditional_writes: self.config.conditional_writes,
                steps,
            });
        }
        let key = layout::probe_key(Uuid::new_v4());
        let body = Bytes::from(format!(
            "sparkles connection test {}\n",
            crate::now_rfc3339()
        ));
        let conditional_mode = self.config.conditional_writes;
        // create
        let t = Instant::now();
        let mut conditional = conditional_mode;
        let created = if conditional_mode {
            match self
                .store
                .put_opts(&key, body.clone().into(), PutOptions::from(PutMode::Create))
                .await
            {
                Err(object_store::Error::NotImplemented { .. }) => {
                    conditional = false;
                    self.store.put(&key, body.clone().into()).await
                }
                r => r,
            }
        } else {
            self.store.put(&key, body.clone().into()).await
        };
        steps.push(step(
            TestStepKind::Create,
            t,
            created.map(|_| ()).map_err(msg),
        ));
        if !steps[0].ok {
            for s in [
                TestStepKind::CreateAgain,
                TestStepKind::Read,
                TestStepKind::List,
                TestStepKind::Delete,
            ] {
                steps.push(TestStep {
                    step: s,
                    ok: false,
                    millis: 0,
                    error: Some("skipped".into()),
                });
            }
            return Ok(TestReport {
                ok: false,
                conditional_writes: false,
                steps,
            });
        }
        // create again: must be refused
        let t = Instant::now();
        let again = if conditional {
            match self
                .store
                .put_opts(&key, body.clone().into(), PutOptions::from(PutMode::Create))
                .await
            {
                Err(e) if is_already_exists(&e) => Ok(()),
                Ok(_) => {
                    conditional = false;
                    Err(
                        "the second conditional create succeeded: the service ignores \
                         If-None-Match"
                            .to_string(),
                    )
                }
                Err(e) => {
                    conditional = false;
                    Err(msg(e))
                }
            }
        } else {
            // not asked for: single writer by configuration
            Ok(())
        };
        steps.push(step(TestStepKind::CreateAgain, t, again));
        // read
        let t = Instant::now();
        let read = match self.store.get(&key).await {
            Ok(g) => match g.bytes().await {
                Ok(b) if b == body => Ok(()),
                Ok(_) => Err("read back different content".to_string()),
                Err(e) => Err(msg(e)),
            },
            Err(e) => Err(msg(e)),
        };
        steps.push(step(TestStepKind::Read, t, read));
        // list
        let t = Instant::now();
        let listed = self
            .store
            .list(Some(&Key::from(layout::PROBE)))
            .try_collect::<Vec<_>>()
            .await
            .map_err(msg)
            .and_then(|l| {
                if l.iter().any(|m| m.location == key) {
                    Ok(())
                } else {
                    Err("the probe object is not listed".to_string())
                }
            });
        steps.push(step(TestStepKind::List, t, listed));
        // delete
        let t = Instant::now();
        let deleted = self.store.delete(&key).await.map_err(msg);
        steps.push(step(TestStepKind::Delete, t, deleted));
        self.conditional.store(
            if conditional { COND_YES } else { COND_NO },
            Ordering::Relaxed,
        );
        Ok(TestReport {
            ok: steps.iter().all(|s| s.ok),
            conditional_writes: conditional,
            steps,
        })
    }

    /// Every object under `backups/` with its manifest (from the cache, else a `GET`,
    /// `maxConcurrency` in parallel). Keys that are not `backups/<valid name>.json` are
    /// left out; a manifest that does not parse, or whose `name` is not its key's, is an
    /// `Err` in its entry. Also returns the number of `GET`s made.
    pub(crate) async fn scan_manifests(&self) -> Result<(Vec<ManifestEntry>, u64)> {
        let metas: Vec<ObjectMeta> = self
            .store
            .list(Some(&Key::from(layout::BACKUPS)))
            .try_collect()
            .await?;
        let mut hits = Vec::new();
        let mut misses = Vec::new();
        for meta in metas {
            let Some(name) = layout::backup_name_of(&meta.location).map(str::to_string) else {
                continue;
            };
            let version = crate::cache::version_of(&meta);
            match self.cache.get(&name, &version, meta.size) {
                Some(m) => hits.push(ManifestEntry {
                    manifest: named(&name, m).map(Arc::new),
                    name,
                }),
                None => misses.push((name, meta)),
            }
        }
        let gets = misses.len() as u64;
        let fetched: Vec<ManifestEntry> = futures::stream::iter(misses)
            .map(|(name, meta)| async move {
                let manifest = self.fetch_manifest(&name, Some(&meta)).await.map(Arc::new);
                ManifestEntry { name, manifest }
            })
            .buffer_unordered(self.config.concurrency())
            .collect()
            .await;
        // a backend failure (not a bad manifest) fails the listing; a manifest deleted
        // since the listing is left out
        for e in fetched {
            match &e.manifest {
                Err(err) if err.code() == Code::RepositoryUnavailable => return Err(err.clone()),
                Err(err) if err.code() == Code::NoSuchBackup => {}
                _ => hits.push(e),
            }
        }
        Ok((hits, gets))
    }

    /// The manifest at `meta` (a listed object): from the cache, else a `GET` (then
    /// cached). Its `name` must be its key's.
    async fn fetch_manifest(&self, name: &str, meta: Option<&ObjectMeta>) -> Result<Manifest> {
        let (m, _, _) = crate::manifest::fetch(self, name, meta).await?;
        named(name, m)
    }

    /// The backups matching `f`, newest `completed` first: one `LIST backups/`, then the
    /// manifests missing from the cache (`GET`, `maxConcurrency` in parallel). A
    /// manifest that does not parse is skipped with a WARN.
    pub async fn list(&self, f: &ListFilter) -> Result<Vec<BackupSummary>> {
        let before = match &f.before {
            Some(b) => Some(parse_time(b).ok_or_else(|| {
                BackupError::new(
                    Code::InvalidRequest,
                    format!("before: {b:?} is not an RFC 3339 time"),
                )
            })?),
            None => None,
        };
        let mut ms = self.manifests().await?;
        ms.retain(|m| {
            f.dataset.as_ref().is_none_or(|d| &m.dataset.name == d)
                && f.dataset_id.is_none_or(|id| m.dataset.id == id)
                && f.policy
                    .as_ref()
                    .is_none_or(|p| m.policy.as_ref() == Some(p))
                && before.is_none_or(|b| parse_time(&m.completed).is_some_and(|c| c < b))
        });
        sort_newest_first(&mut ms);
        if let Some(n) = f.limit {
            ms.truncate(n);
        }
        Ok(ms.iter().map(|m| m.summary(&self.config.name)).collect())
    }

    /// Every manifest that parses (the others are skipped with a WARN), unsorted.
    pub(crate) async fn manifests(&self) -> Result<Vec<Arc<Manifest>>> {
        let (entries, _) = self.scan_manifests().await?;
        Ok(entries
            .into_iter()
            .filter_map(|e| match e.manifest {
                Ok(m) => Some(m),
                Err(err) => {
                    tracing::warn!(
                        target: "sparkles::backup",
                        repository = %self.config.name,
                        backup = %e.name,
                        "skipping an unreadable manifest: {err}"
                    );
                    None
                }
            })
            .collect())
    }

    /// The manifest of backup `name` (`404 no-such-backup`), parsed (not validated for
    /// restore: see `manifest::validate`).
    pub async fn manifest(&self, name: &str) -> Result<Manifest> {
        self.fetch_manifest(name, None).await
    }

    /// Totals from a listing of `backups/` and `blobs/` (`stats` of the API).
    /// `storedBytes` counts blob objects (manifests are small and not counted).
    pub async fn stats(&self) -> Result<RepoStats> {
        let ms = self.manifests().await?;
        let datasets: HashSet<Uuid> = ms.iter().map(|m| m.dataset.id).collect();
        let logical_bytes: u64 = ms.iter().map(|m| m.stats.logical_bytes).sum();
        let stored_bytes: u64 = self
            .store
            .list(Some(&Key::from(layout::BLOBS)))
            .try_fold(0u64, |n, m| async move {
                Ok(if layout::blob_id_of(&m.location).is_some() {
                    n + m.size
                } else {
                    n
                })
            })
            .await?;
        Ok(RepoStats {
            backups: ms.len() as u64,
            datasets: datasets.len() as u64,
            stored_bytes,
            logical_bytes,
            dedup_ratio: if stored_bytes == 0 {
                1.0
            } else {
                logical_bytes as f64 / stored_bytes as f64
            },
            as_of: crate::now_rfc3339(),
        })
    }

    /// Delete backup `name`'s manifest under a shared lock; its blobs stay until GC.
    /// `Ok(false)` if it did not exist; `409 repository-read-only` on a read-only
    /// repository.
    pub async fn delete(&self, name: &str) -> Result<bool> {
        if self.config.readonly {
            return Err(read_only(&self.config.name));
        }
        if !layout::valid_backup_name(name) {
            return Ok(false);
        }
        let guard = lock::acquire(
            self,
            LockKind::Shared,
            LockOperation::Delete,
            &Ctl::default(),
        )
        .await?;
        let key = layout::manifest_key(name);
        let r = async {
            match self.store.head(&key).await {
                Ok(_) => {}
                Err(e) if is_not_found(&e) => return Ok(false),
                Err(e) => return Err(BackupError::from(e)),
            }
            match self.store.delete(&key).await {
                Ok(()) => {}
                Err(e) if is_not_found(&e) => return Ok(false),
                Err(e) => return Err(e.into()),
            }
            self.cache.remove(name);
            Ok(true)
        }
        .await;
        if let Err(e) = guard.release().await {
            tracing::warn!(target: "sparkles::backup", "releasing a lock: {e}");
        }
        r
    }
}

/// `409 repository-read-only`
pub(crate) fn read_only(repo: &str) -> BackupError {
    BackupError::new(
        Code::RepositoryReadOnly,
        format!("repository {repo} is read-only"),
    )
}

/// `m`, if it is stored under its own name (a manifest copied or renamed to another
/// key is `422 invalid-backup`).
fn named(name: &str, m: Manifest) -> Result<Manifest> {
    if m.name != name {
        return Err(BackupError::invalid_backup(
            "name",
            format!("{:?} is stored as {name}", m.name),
        ));
    }
    Ok(m)
}

/// A listed manifest object and its parsed content.
#[derive(Debug)]
pub(crate) struct ManifestEntry {
    pub name: String,
    pub manifest: Result<Arc<Manifest>>,
}

/// Newest `completed` first; ties (the same millisecond) by `created`, then the
/// commit, then the name, all descending, for a stable order.
pub(crate) fn sort_newest_first(ms: &mut [Arc<Manifest>]) {
    ms.sort_by_cached_key(|m| {
        std::cmp::Reverse((
            parse_time(&m.completed),
            parse_time(&m.created),
            m.commit.seq,
            m.name.clone(),
        ))
    });
}

pub(crate) fn parse_time(s: &str) -> Option<chrono::DateTime<chrono::Utc>> {
    chrono::DateTime::parse_from_rfc3339(s)
        .ok()
        .map(|t| t.with_timezone(&chrono::Utc))
}

/// Read the marker, or initialize an empty location (see [`Repository::open`]).
async fn attach_or_init(
    store: &Arc<dyn ObjectStore>,
    cfg: &RepoConfig,
    env: &OpenEnv,
) -> Result<Marker> {
    let key = layout::marker_key();
    let read = |r: object_store::Result<object_store::GetResult>| async move {
        match r {
            Ok(g) => Ok(Some(Marker::parse(&g.bytes().await?)?)),
            Err(e) if is_not_found(&e) => Ok(None),
            Err(e) => Err(BackupError::from(e)),
        }
    };
    if let Some(m) = read(store.get(&key).await).await? {
        return Ok(m);
    }
    let not_repo = |why: &str| {
        BackupError::new(
            Code::NotARepository,
            format!(
                "{} is not a Sparkles backup repository ({why})",
                cfg.location()
            ),
        )
    };
    if store.list(None).try_next().await?.is_some() {
        return Err(not_repo("it holds other objects and no sparkles-repo.json"));
    }
    if cfg.readonly || !env.init {
        return Err(not_repo("it is empty"));
    }
    let marker = Marker::new(Uuid::new_v4(), crate::now_rfc3339());
    let body = PutPayload::from(marker.to_bytes());
    let created = if cfg.conditional_writes {
        match store
            .put_opts(&key, body.clone(), PutOptions::from(PutMode::Create))
            .await
        {
            Ok(_) => true,
            Err(e) if is_already_exists(&e) => false,
            Err(object_store::Error::NotImplemented { .. }) => {
                store.put(&key, body).await?;
                true
            }
            Err(e) => return Err(e.into()),
        }
    } else {
        store.put(&key, body).await?;
        true
    };
    if created {
        tracing::info!(
            target: "sparkles::backup",
            repository = %cfg.name,
            id = %marker.id,
            "initialized a backup repository at {}",
            cfg.location()
        );
        return Ok(marker);
    }
    // a concurrent initializer won: use its marker
    read(store.get(&key).await)
        .await?
        .ok_or_else(|| BackupError::internal("the repository marker vanished"))
}

mod instrumented {
    //! [`RepoStore`]: request counting and local retries around a backend store.

    use async_trait::async_trait;
    use futures::stream::BoxStream;
    use futures::{Stream, StreamExt};
    use object_store::path::Path;
    use object_store::{
        CopyOptions, GetOptions, GetResult, ListResult, MultipartUpload, ObjectMeta, ObjectStore,
        PutMultipartOptions, PutOptions, PutPayload, PutResult, RenameOptions, Result,
    };
    use serde::{Deserialize, Serialize};
    use std::pin::Pin;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::task::{Context, Poll};
    use std::time::Duration;

    /// Attempts per request for backends without an HTTP retry layer.
    pub(crate) const LOCAL_ATTEMPTS: u32 = 3;
    /// First backoff between attempts (doubled each time).
    const BACKOFF: Duration = Duration::from_millis(100);

    /// The kind of an object request (the `op` label of `object_requests_total`).
    #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
    pub enum RequestOp {
        Put,
        Get,
        Head,
        List,
        Delete,
    }

    impl RequestOp {
        pub const ALL: [RequestOp; 5] = [
            RequestOp::Put,
            RequestOp::Get,
            RequestOp::Head,
            RequestOp::List,
            RequestOp::Delete,
        ];

        pub fn as_str(self) -> &'static str {
            match self {
                RequestOp::Put => "put",
                RequestOp::Get => "get",
                RequestOp::Head => "head",
                RequestOp::List => "list",
                RequestOp::Delete => "delete",
            }
        }
    }

    /// Request counters of one repository.
    #[derive(Debug, Default)]
    pub struct RequestStats {
        counts: [[AtomicU64; 2]; 5],
    }

    #[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
    pub struct OpCounts {
        pub ok: u64,
        pub error: u64,
    }

    /// A snapshot of [`RequestStats`].
    #[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
    pub struct RequestCounts {
        pub put: OpCounts,
        pub get: OpCounts,
        pub head: OpCounts,
        pub list: OpCounts,
        pub delete: OpCounts,
    }

    impl RequestCounts {
        pub fn get(&self, op: RequestOp) -> OpCounts {
            match op {
                RequestOp::Put => self.put,
                RequestOp::Get => self.get,
                RequestOp::Head => self.head,
                RequestOp::List => self.list,
                RequestOp::Delete => self.delete,
            }
        }

        /// Failed requests of every kind.
        pub fn errors(&self) -> u64 {
            RequestOp::ALL.iter().map(|&o| self.get(o).error).sum()
        }
    }

    impl RequestStats {
        pub fn record(&self, op: RequestOp, ok: bool) {
            self.counts[op as usize][usize::from(!ok)].fetch_add(1, Ordering::Relaxed);
        }

        pub fn snapshot(&self) -> RequestCounts {
            let c = |op: RequestOp| OpCounts {
                ok: self.counts[op as usize][0].load(Ordering::Relaxed),
                error: self.counts[op as usize][1].load(Ordering::Relaxed),
            };
            RequestCounts {
                put: c(RequestOp::Put),
                get: c(RequestOp::Get),
                head: c(RequestOp::Head),
                list: c(RequestOp::List),
                delete: c(RequestOp::Delete),
            }
        }
    }

    /// Expected answers ("not found", "already exists", "not modified") are successful
    /// requests.
    fn ok<T>(r: &Result<T>) -> bool {
        use object_store::Error as E;
        match r {
            Ok(_) => true,
            Err(
                E::NotFound { .. }
                | E::AlreadyExists { .. }
                | E::Precondition { .. }
                | E::NotModified { .. },
            ) => true,
            Err(_) => false,
        }
    }

    /// Errors worth another attempt: backend and I/O failures without a more specific
    /// kind.
    fn transient(e: &object_store::Error) -> bool {
        matches!(
            e,
            object_store::Error::Generic { .. } | object_store::Error::JoinError { .. }
        )
    }

    /// A backend store that counts requests and retries transient failures of
    /// individual requests (`attempts` in all, with exponential backoff; 1 for backends
    /// whose HTTP client already retries). Streams (listings, downloaded bodies) are not
    /// retried.
    pub struct RepoStore {
        inner: Arc<dyn ObjectStore>,
        stats: Arc<RequestStats>,
        attempts: u32,
        /// when a retried request last got a WARN (seconds since the epoch)
        warned: AtomicU64,
    }

    impl RepoStore {
        pub fn new(inner: Arc<dyn ObjectStore>, stats: Arc<RequestStats>, attempts: u32) -> Self {
            RepoStore {
                inner,
                stats,
                attempts: attempts.max(1),
                warned: AtomicU64::new(0),
            }
        }

        async fn retry<T, F, Fut>(&self, op: RequestOp, mut f: F) -> Result<T>
        where
            F: FnMut() -> Fut,
            Fut: std::future::Future<Output = Result<T>>,
        {
            let mut attempt = 1;
            loop {
                let r = f().await;
                self.stats.record(op, ok(&r));
                match r {
                    Err(e) if transient(&e) && attempt < self.attempts => {
                        tracing::debug!(
                            target: "sparkles::backup",
                            "retrying a {} request after: {}",
                            op.as_str(),
                            crate::error::redact_urls(&e.to_string())
                        );
                        tokio::time::sleep(BACKOFF * 2u32.pow(attempt - 1)).await;
                        attempt += 1;
                    }
                    r => {
                        if attempt > 1 && r.is_ok() {
                            self.warn_retried(op, attempt);
                        }
                        return r;
                    }
                }
            }
        }

        /// A WARN for a request that succeeded after retries, at most once a minute.
        fn warn_retried(&self, op: RequestOp, attempts: u32) {
            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs();
            let last = self.warned.load(Ordering::Relaxed);
            if now >= last + 60
                && self
                    .warned
                    .compare_exchange(last, now, Ordering::Relaxed, Ordering::Relaxed)
                    .is_ok()
            {
                tracing::warn!(
                    target: "sparkles::backup",
                    store = %self.inner,
                    "a {} request succeeded after {attempts} attempts",
                    op.as_str()
                );
            }
        }
    }

    impl std::fmt::Display for RepoStore {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            self.inner.fmt(f)
        }
    }

    impl std::fmt::Debug for RepoStore {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.debug_struct("RepoStore")
                .field("inner", &self.inner.to_string())
                .field("attempts", &self.attempts)
                .finish()
        }
    }

    /// A listing that counts one `list` request when it ends (or is dropped), failed if
    /// it yielded an error.
    struct CountedList {
        inner: BoxStream<'static, Result<ObjectMeta>>,
        stats: Arc<RequestStats>,
        failed: bool,
        done: bool,
    }

    impl Stream for CountedList {
        type Item = Result<ObjectMeta>;
        fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
            let r = self.inner.poll_next_unpin(cx);
            match &r {
                Poll::Ready(Some(Err(_))) => self.failed = true,
                Poll::Ready(None) if !self.done => {
                    self.done = true;
                    self.stats.record(RequestOp::List, !self.failed);
                }
                _ => {}
            }
            r
        }
    }

    impl Drop for CountedList {
        fn drop(&mut self) {
            if !self.done {
                self.stats.record(RequestOp::List, !self.failed);
            }
        }
    }

    #[async_trait]
    impl ObjectStore for RepoStore {
        async fn put_opts(
            &self,
            location: &Path,
            payload: PutPayload,
            opts: PutOptions,
        ) -> Result<PutResult> {
            self.retry(RequestOp::Put, || {
                self.inner.put_opts(location, payload.clone(), opts.clone())
            })
            .await
        }

        async fn put_multipart_opts(
            &self,
            location: &Path,
            opts: PutMultipartOptions,
        ) -> Result<Box<dyn MultipartUpload>> {
            let r = self.inner.put_multipart_opts(location, opts).await;
            self.stats.record(RequestOp::Put, ok(&r));
            r
        }

        async fn get_opts(&self, location: &Path, options: GetOptions) -> Result<GetResult> {
            let op = if options.head {
                RequestOp::Head
            } else {
                RequestOp::Get
            };
            self.retry(op, || self.inner.get_opts(location, options.clone()))
                .await
        }

        async fn get_ranges(
            &self,
            location: &Path,
            ranges: &[std::ops::Range<u64>],
        ) -> Result<Vec<bytes::Bytes>> {
            self.retry(RequestOp::Get, || self.inner.get_ranges(location, ranges))
                .await
        }

        fn delete_stream(
            &self,
            locations: BoxStream<'static, Result<Path>>,
        ) -> BoxStream<'static, Result<Path>> {
            let stats = self.stats.clone();
            self.inner
                .delete_stream(locations)
                .map(move |r| {
                    stats.record(RequestOp::Delete, ok(&r));
                    r
                })
                .boxed()
        }

        fn list(&self, prefix: Option<&Path>) -> BoxStream<'static, Result<ObjectMeta>> {
            CountedList {
                inner: self.inner.list(prefix),
                stats: self.stats.clone(),
                failed: false,
                done: false,
            }
            .boxed()
        }

        fn list_with_offset(
            &self,
            prefix: Option<&Path>,
            offset: &Path,
        ) -> BoxStream<'static, Result<ObjectMeta>> {
            CountedList {
                inner: self.inner.list_with_offset(prefix, offset),
                stats: self.stats.clone(),
                failed: false,
                done: false,
            }
            .boxed()
        }

        async fn list_with_delimiter(&self, prefix: Option<&Path>) -> Result<ListResult> {
            self.retry(RequestOp::List, || self.inner.list_with_delimiter(prefix))
                .await
        }

        async fn copy_opts(&self, from: &Path, to: &Path, options: CopyOptions) -> Result<()> {
            self.retry(RequestOp::Put, || {
                self.inner.copy_opts(from, to, options.clone())
            })
            .await
        }

        async fn rename_opts(&self, from: &Path, to: &Path, options: RenameOptions) -> Result<()> {
            let r = self.inner.rename_opts(from, to, options).await;
            self.stats.record(RequestOp::Put, ok(&r));
            r
        }
    }
}
