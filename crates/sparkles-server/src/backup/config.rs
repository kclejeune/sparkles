//! The backup config file (`serve --backup-config`, and the file `sparkles repo add`
//! edits): TOML with snake_case keys, `deny_unknown_fields`, converted to the camelCase
//! API types. No secrets: credentials are references to environment variables or files.
//!
//! ```toml
//! version = 1
//!
//! [repositories.local]
//! type = "fs"
//! path = "/srv/backups/sparkles"
//!
//! [repositories.s3-main]
//! type = "s3"
//! bucket = "kg-backups"
//! prefix = "prod/sparkles"
//! credentials = { source = "file", path = "/run/secrets/sparkles-s3.json" }
//!
//! [policies.nightly]
//! repository = "s3-main"
//! schedule = "30 2 * * *"
//! timezone = "Europe/Berlin"
//! retention = { expire_after = "30d", min_count = 7, max_count = 60 }
//!
//! # what repositories registered through the HTTP API may use
//! [api]
//! fs_roots = ["/srv/backups"]
//!
//! [credentials.minio]
//! source = "env"
//! access_key_id_var = "MINIO_ACCESS_KEY"
//! secret_access_key_var = "MINIO_SECRET_KEY"
//! ```
//!
//! Repositories registered through the API are held to the operator's choices here:
//! their credentials can only be one of the `[credentials.<name>]` sources (by name:
//! `{"source": "named", "name": "minio"}`), never environment variables, files or the
//! default provider chain of their own choosing; `fs` ones must lie under one of
//! `[api] fs_roots` when it is set.

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use sparkles_backup::{CatchUp, Credentials, PolicyConfig, RepoConfig, RepoType, Retention, Sse};
use std::collections::BTreeMap;
use std::path::Path;

/// The file header `sparkles repo add` and `repo remove` write (they rewrite the file
/// through a serde round trip).
pub const HEADER: &str = "# Sparkles backup repositories and policies. Written by `sparkles repo add` and\n# `sparkles repo remove`, which do not keep comments. No secrets here: credentials\n# come from the environment or from files.\n";

/// The whole file.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ConfigFile {
    /// 1
    pub version: u32,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub repositories: BTreeMap<String, RepoToml>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub policies: BTreeMap<String, PolicyToml>,
    /// `[credentials.<name>]`: credential sources repositories name
    /// (`credentials = { source = "named", name = … }`), the only ones API
    /// registrations may use
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub credentials: BTreeMap<String, CredentialsToml>,
    /// `[api]`
    #[serde(default, skip_serializing_if = "ApiToml::is_empty")]
    pub api: ApiToml,
}

/// `[api]`: limits of repositories registered through the HTTP API.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ApiToml {
    /// absolute directories `fs` repositories must lie under (empty: anywhere outside
    /// the server's own directories)
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub fs_roots: Vec<String>,
}

impl ApiToml {
    fn is_empty(&self) -> bool {
        self.fs_roots.is_empty()
    }
}

fn is_false(b: &bool) -> bool {
    !*b
}
fn yes() -> bool {
    true
}
fn is_true(b: &bool) -> bool {
    *b
}

/// `[repositories.<name>]`
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RepoToml {
    #[serde(rename = "type")]
    pub kind: RepoType,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bucket: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prefix: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub region: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub endpoint: Option<String>,
    #[serde(default, skip_serializing_if = "is_false")]
    pub path_style: bool,
    #[serde(default, skip_serializing_if = "is_false")]
    pub allow_http: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub credentials: Option<CredentialsToml>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sse: Option<Sse>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kms_key_id: Option<String>,
    #[serde(default = "yes", skip_serializing_if = "is_true")]
    pub conditional_writes: bool,
    #[serde(default, skip_serializing_if = "is_false")]
    pub readonly: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_concurrency: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_upload_bytes_per_sec: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_download_bytes_per_sec: Option<u64>,
}

/// `credentials = { source = … }`
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "source", rename_all = "lowercase", deny_unknown_fields)]
pub enum CredentialsToml {
    Default,
    Env {
        access_key_id_var: String,
        secret_access_key_var: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        session_token_var: Option<String>,
    },
    File {
        path: String,
    },
    /// a `[credentials.<name>]` source
    Named {
        name: String,
    },
}

impl From<CredentialsToml> for Credentials {
    fn from(c: CredentialsToml) -> Credentials {
        match c {
            CredentialsToml::Default => Credentials::Default,
            CredentialsToml::Env {
                access_key_id_var,
                secret_access_key_var,
                session_token_var,
            } => Credentials::Env {
                access_key_id_var,
                secret_access_key_var,
                session_token_var,
            },
            CredentialsToml::File { path } => Credentials::File { path },
            CredentialsToml::Named { name } => Credentials::Named { name },
        }
    }
}

impl From<&Credentials> for CredentialsToml {
    fn from(c: &Credentials) -> CredentialsToml {
        match c.clone() {
            Credentials::Default => CredentialsToml::Default,
            Credentials::Env {
                access_key_id_var,
                secret_access_key_var,
                session_token_var,
            } => CredentialsToml::Env {
                access_key_id_var,
                secret_access_key_var,
                session_token_var,
            },
            Credentials::File { path } => CredentialsToml::File { path },
            Credentials::Named { name } => CredentialsToml::Named { name },
        }
    }
}

impl RepoToml {
    /// The API form of the table `[repositories.<name>]`.
    pub fn to_config(&self, name: &str) -> RepoConfig {
        RepoConfig {
            name: name.to_string(),
            kind: self.kind,
            path: self.path.clone(),
            bucket: self.bucket.clone(),
            prefix: self.prefix.clone(),
            region: self.region.clone(),
            endpoint: self.endpoint.clone(),
            path_style: self.path_style,
            allow_http: self.allow_http,
            credentials: self.credentials.clone().map(Into::into).unwrap_or_default(),
            sse: self.sse,
            kms_key_id: self.kms_key_id.clone(),
            conditional_writes: self.conditional_writes,
            readonly: self.readonly,
            max_concurrency: self.max_concurrency,
            max_upload_bytes_per_sec: self.max_upload_bytes_per_sec,
            max_download_bytes_per_sec: self.max_download_bytes_per_sec,
        }
    }

    /// The table of an API configuration (its name is the table key).
    pub fn from_config(c: &RepoConfig) -> RepoToml {
        RepoToml {
            kind: c.kind,
            path: c.path.clone(),
            bucket: c.bucket.clone(),
            prefix: c.prefix.clone(),
            region: c.region.clone(),
            endpoint: c.endpoint.clone(),
            path_style: c.path_style,
            allow_http: c.allow_http,
            credentials: (c.credentials != Credentials::Default).then(|| (&c.credentials).into()),
            sse: c.sse,
            kms_key_id: c.kms_key_id.clone(),
            conditional_writes: c.conditional_writes,
            readonly: c.readonly,
            max_concurrency: c.max_concurrency,
            max_upload_bytes_per_sec: c.max_upload_bytes_per_sec,
            max_download_bytes_per_sec: c.max_download_bytes_per_sec,
        }
    }
}

fn all_datasets() -> Vec<String> {
    vec!["*".into()]
}
fn utc() -> String {
    "UTC".into()
}
fn default_template() -> String {
    "{policy}-{dataset}-{time}".into()
}
fn one() -> u32 {
    1
}

/// `retention = { … }`
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RetentionToml {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expire_after: Option<String>,
    #[serde(default = "one")]
    pub min_count: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_count: Option<u32>,
}

impl Default for RetentionToml {
    fn default() -> RetentionToml {
        RetentionToml {
            expire_after: None,
            min_count: 1,
            max_count: None,
        }
    }
}

/// `[policies.<name>]`
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PolicyToml {
    pub repository: String,
    #[serde(default = "all_datasets")]
    pub datasets: Vec<String>,
    pub schedule: String,
    #[serde(default = "utc")]
    pub timezone: String,
    #[serde(default = "default_template")]
    pub name_template: String,
    #[serde(default)]
    pub retention: RetentionToml,
    #[serde(default, skip_serializing_if = "is_false")]
    pub skip_unchanged: bool,
    #[serde(default, skip_serializing_if = "is_false")]
    pub gc_after_retention: bool,
    #[serde(default)]
    pub catch_up: CatchUp,
    #[serde(default = "yes")]
    pub enabled: bool,
}

impl PolicyToml {
    pub fn to_config(&self, name: &str) -> PolicyConfig {
        PolicyConfig {
            name: name.to_string(),
            repository: self.repository.clone(),
            datasets: self.datasets.clone(),
            schedule: self.schedule.clone(),
            timezone: self.timezone.clone(),
            name_template: self.name_template.clone(),
            retention: Retention {
                expire_after: self.retention.expire_after.clone(),
                min_count: self.retention.min_count,
                max_count: self.retention.max_count,
            },
            skip_unchanged: self.skip_unchanged,
            gc_after_retention: self.gc_after_retention,
            catch_up: self.catch_up,
            enabled: self.enabled,
        }
    }

    #[cfg_attr(not(test), allow(dead_code))]
    pub fn from_config(p: &PolicyConfig) -> PolicyToml {
        PolicyToml {
            repository: p.repository.clone(),
            datasets: p.datasets.clone(),
            schedule: p.schedule.clone(),
            timezone: p.timezone.clone(),
            name_template: p.name_template.clone(),
            retention: RetentionToml {
                expire_after: p.retention.expire_after.clone(),
                min_count: p.retention.min_count,
                max_count: p.retention.max_count,
            },
            skip_unchanged: p.skip_unchanged,
            gc_after_retention: p.gc_after_retention,
            catch_up: p.catch_up,
            enabled: p.enabled,
        }
    }
}

impl ConfigFile {
    /// Parse the file's text: TOML errors name the line and column; `version` must be
    /// 1 and every name must follow the repository/policy grammar. Semantic checks of
    /// the entries (`RepoConfig::validate`, schedules) are the registry's.
    pub fn parse(text: &str) -> Result<ConfigFile> {
        let f: ConfigFile = toml::from_str(text)?;
        if f.version != 1 {
            bail!("unsupported version {} (expected 1)", f.version);
        }
        for name in f
            .repositories
            .keys()
            .chain(f.policies.keys())
            .chain(f.credentials.keys())
        {
            if !sparkles_backup::layout::valid_repo_name(name) {
                bail!("invalid name {name:?}: use a-z, 0-9, '_' and '-' (max 64)");
            }
        }
        for (name, c) in &f.credentials {
            if matches!(c, CredentialsToml::Named { .. }) {
                bail!("credentials {name:?}: a credential source cannot name another");
            }
        }
        for (name, r) in &f.repositories {
            if let Some(CredentialsToml::Named { name: n }) = &r.credentials
                && !f.credentials.contains_key(n)
            {
                bail!("repository {name:?}: no [credentials.{n}] in this file");
            }
        }
        for root in &f.api.fs_roots {
            if !Path::new(root).is_absolute() {
                bail!("[api] fs_roots: {root:?} is not an absolute path");
            }
        }
        Ok(f)
    }

    /// The file's text: [`HEADER`] and the TOML.
    pub fn to_text(&self) -> Result<String> {
        Ok(format!("{HEADER}\n{}", toml::to_string_pretty(self)?))
    }

    /// The repositories in API form.
    pub fn repository_configs(&self) -> Vec<RepoConfig> {
        self.repositories
            .iter()
            .map(|(n, r)| r.to_config(n))
            .collect()
    }

    /// The policies in API form.
    pub fn policy_configs(&self) -> Vec<PolicyConfig> {
        self.policies.iter().map(|(n, p)| p.to_config(n)).collect()
    }

    /// The credential sources in API form.
    pub fn credential_sources(&self) -> BTreeMap<String, Credentials> {
        self.credentials
            .iter()
            .map(|(n, c)| (n.clone(), c.clone().into()))
            .collect()
    }
}

/// Read and parse the config file at `path` (errors name the file). The file may not
/// be readable by group or others without a WARN (it names secret files and
/// variables, though it holds no secrets).
pub fn load(path: &Path) -> Result<ConfigFile> {
    let text = std::fs::read_to_string(path)
        .with_context(|| format!("reading backup config {}", path.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if let Ok(m) = std::fs::metadata(path)
            && m.permissions().mode() & 0o077 != 0
        {
            tracing::warn!(
                "backup config {} is readable by group or others",
                path.display()
            );
        }
    }
    ConfigFile::parse(&text).with_context(|| format!("backup config {}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    const EXAMPLE: &str = r#"
version = 1

[repositories.local]
type = "fs"
path = "/srv/backups/sparkles"

[repositories.s3-main]
type = "s3"
bucket = "kg-backups"
prefix = "prod/sparkles"
region = "eu-central-1"
credentials = { source = "file", path = "/run/secrets/sparkles-s3.json" }
sse = "aws:kms"
kms_key_id = "arn:aws:kms:eu-central-1:111122223333:key/k"
max_concurrency = 8
max_upload_bytes_per_sec = 104857600

[repositories.dr-source]
type = "s3"
bucket = "kg-backups"
prefix = "prod/sparkles"
readonly = true
credentials = { source = "env", access_key_id_var = "K", secret_access_key_var = "S" }

[repositories.minio]
type = "s3"
bucket = "lab"
endpoint = "http://127.0.0.1:9000"
allow_http = true
credentials = { source = "named", name = "lab" }

[credentials.lab]
source = "env"
access_key_id_var = "LAB_KEY"
secret_access_key_var = "LAB_SECRET"

[api]
fs_roots = ["/srv/backups"]

[policies.nightly]
repository = "s3-main"
datasets = ["*"]
schedule = "30 2 * * *"
timezone = "Europe/Berlin"
name_template = "{policy}-{dataset}-{date:%Y%m%d}"
retention = { expire_after = "30d", min_count = 7, max_count = 60 }
gc_after_retention = true
"#;

    #[test]
    fn the_example_parses_and_converts() {
        let f = ConfigFile::parse(EXAMPLE).unwrap();
        let repos = f.repository_configs();
        let s3 = repos.iter().find(|r| r.name == "s3-main").unwrap();
        assert_eq!(s3.kind, RepoType::S3);
        assert_eq!(s3.sse, Some(Sse::AwsKms));
        assert_eq!(
            s3.credentials,
            Credentials::File {
                path: "/run/secrets/sparkles-s3.json".into()
            }
        );
        assert!(s3.conditional_writes && !s3.readonly);
        let dr = repos.iter().find(|r| r.name == "dr-source").unwrap();
        assert!(dr.readonly);
        assert!(matches!(dr.credentials, Credentials::Env { .. }));
        let local = repos.iter().find(|r| r.name == "local").unwrap();
        assert_eq!(local.credentials, Credentials::Default);
        let minio = repos.iter().find(|r| r.name == "minio").unwrap();
        assert_eq!(minio.credentials, Credentials::Named { name: "lab".into() });
        assert!(matches!(
            f.credential_sources()["lab"],
            Credentials::Env { .. }
        ));
        assert_eq!(f.api.fs_roots, ["/srv/backups"]);
        let p = &f.policy_configs()[0];
        assert_eq!(p.name, "nightly");
        assert_eq!(p.retention.min_count, 7);
        assert_eq!(p.retention.expire_after.as_deref(), Some("30d"));
        assert!(p.gc_after_retention && p.enabled);
        assert_eq!(p.catch_up, CatchUp::One);
    }

    #[test]
    fn round_trips_through_text() {
        let f = ConfigFile::parse(EXAMPLE).unwrap();
        let text = f.to_text().unwrap();
        assert!(text.starts_with("# Sparkles backup"));
        assert_eq!(ConfigFile::parse(&text).unwrap(), f);
        // and through the API form
        let mut again = ConfigFile {
            version: 1,
            // (no API form)
            credentials: f.credentials.clone(),
            api: f.api.clone(),
            ..Default::default()
        };
        for r in f.repository_configs() {
            again
                .repositories
                .insert(r.name.clone(), RepoToml::from_config(&r));
        }
        for p in f.policy_configs() {
            again
                .policies
                .insert(p.name.clone(), PolicyToml::from_config(&p));
        }
        assert_eq!(again, f);
    }

    #[test]
    fn mistakes_are_refused() {
        let e = ConfigFile::parse("version = 1\n[repositories.a]\ntype = \"fs\"\npth = \"/x\"\n")
            .unwrap_err();
        let msg = format!("{e:#}");
        assert!(msg.contains("pth") && msg.contains("line 4"), "{msg}");
        assert!(ConfigFile::parse("version = 2\n").is_err());
        assert!(ConfigFile::parse("version = 1\n[repositories.Bad]\ntype = \"fs\"\n").is_err());
        assert!(ConfigFile::parse("version = 1\n[other]\n").is_err());
        assert!(
            ConfigFile::parse(
                "version = 1\n[repositories.a]\ntype = \"s3\"\ncredentials = { source = \"env\", key = \"x\" }\n"
            )
            .is_err()
        );
        // a named source must exist, and cannot name another
        let e = ConfigFile::parse(
            "version = 1\n[repositories.a]\ntype = \"s3\"\nbucket = \"b\"\ncredentials = { source = \"named\", name = \"x\" }\n",
        )
        .unwrap_err();
        assert!(format!("{e:#}").contains("[credentials.x]"), "{e:#}");
        assert!(
            ConfigFile::parse(
                "version = 1\n[credentials.a]\nsource = \"named\"\nname = \"b\"\n[credentials.b]\nsource = \"default\"\n"
            )
            .is_err()
        );
        assert!(ConfigFile::parse("version = 1\n[api]\nfs_roots = [\"rel\"]\n").is_err());
    }
}
