//! The JSON types of backup repositories: the HTTP API bodies and responses, the
//! manifest and lock objects stored in a repository, and `restore.json`.
//!
//! Field names are camelCase on the wire. Timestamps are RFC 3339 strings in UTC with
//! milliseconds (`2026-09-30T14:05:12.101Z`), ids are UUIDs, and sizes are bytes. Types
//! read from untrusted places (manifests, lock objects) ignore unknown fields, so later
//! formats can add fields; request bodies ignore them too (a client may send back what
//! it read).

use serde::{Deserialize, Serialize};
use serde_json::Value as J;
use uuid::Uuid;

pub use sparkles::store::FileKind;

fn is_false(b: &bool) -> bool {
    !*b
}
fn yes() -> bool {
    true
}

// ---------------------------------------------------------------- repositories ------

/// Storage backend of a repository.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RepoType {
    /// an absolute local or mounted directory
    #[default]
    Fs,
    /// AWS S3 or an S3-compatible service (`endpoint`)
    S3,
    /// Google Cloud Storage (experimental)
    Gcs,
    /// Azure Blob Storage (experimental)
    Azure,
    /// process memory (tests only; not offered by the UI)
    Memory,
}

impl RepoType {
    pub fn as_str(self) -> &'static str {
        match self {
            RepoType::Fs => "fs",
            RepoType::S3 => "s3",
            RepoType::Gcs => "gcs",
            RepoType::Azure => "azure",
            RepoType::Memory => "memory",
        }
    }
}

/// Where S3 credentials come from. Sparkles never stores secrets: only these references.
///
/// Repositories registered through the HTTP API may only use [`Named`](Self::Named)
/// sources, which the operator defines in the server's backup config file: the other
/// forms name environment variables, files or the instance's own credentials, which an
/// API caller must not be able to send to an endpoint of their choice.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(
    tag = "source",
    rename_all = "lowercase",
    rename_all_fields = "camelCase"
)]
pub enum Credentials {
    /// the backend's environment and provider chain (`AWS_*`, web identity, instance
    /// metadata)
    #[default]
    Default,
    /// the named environment variables
    Env {
        access_key_id_var: String,
        secret_access_key_var: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        session_token_var: Option<String>,
    },
    /// a JSON file `{accessKeyId, secretAccessKey, sessionToken?}`, re-read at each open
    File { path: String },
    /// a credential source the server's operator defined under this name
    /// (`[credentials.<name>]` of the backup config file); the server resolves it to
    /// one of the other forms before opening the repository
    Named { name: String },
}

/// S3 server-side encryption.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Sse {
    /// SSE-S3
    #[serde(rename = "AES256")]
    Aes256,
    /// SSE-KMS with `kmsKeyId`
    #[serde(rename = "aws:kms")]
    AwsKms,
}

/// A repository's settings (`RepositoryConfig`: the body of `POST /$/repositories`, a
/// `repositories.json` entry, and the camelCase form of a config-file table).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RepoConfig {
    /// `[a-z0-9][a-z0-9_-]{0,63}`, unique on a server (with policies' names)
    pub name: String,
    #[serde(rename = "type")]
    pub kind: RepoType,
    /// fs: absolute, not inside the server's data directory
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    /// s3, gcs, azure
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bucket: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prefix: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub region: Option<String>,
    /// s3: the service URL of MinIO, R2, Ceph RGW, …
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub endpoint: Option<String>,
    /// s3: path-style addressing
    #[serde(default, skip_serializing_if = "is_false")]
    pub path_style: bool,
    /// s3: allow an `http://` endpoint
    #[serde(default, skip_serializing_if = "is_false")]
    pub allow_http: bool,
    #[serde(default)]
    pub credentials: Credentials,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sse: Option<Sse>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kms_key_id: Option<String>,
    /// conditional creates (`If-None-Match: *`); `false`: single writer, HEAD-then-PUT
    #[serde(default = "yes")]
    pub conditional_writes: bool,
    /// never write, not even lock objects
    #[serde(default)]
    pub readonly: bool,
    /// parallel object requests (default 8 for s3, 4 otherwise)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_concurrency: Option<u32>,
    /// token-bucket bandwidth limits shared by the repository's tasks (`None`: unlimited)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_upload_bytes_per_sec: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_download_bytes_per_sec: Option<u64>,
}

impl RepoConfig {
    /// `maxConcurrency`, or its default for the type.
    pub fn concurrency(&self) -> usize {
        match self.max_concurrency {
            Some(n) if n > 0 => n as usize,
            _ if matches!(self.kind, RepoType::S3 | RepoType::Gcs | RepoType::Azure) => 8,
            _ => 4,
        }
    }

    /// Whether `other` names the same location (type, path, bucket, prefix, endpoint),
    /// which `PUT /$/repositories/{repo}` may not change (`409 location-immutable`).
    pub fn same_location(&self, other: &RepoConfig) -> bool {
        self.kind == other.kind
            && self.path == other.path
            && self.bucket == other.bucket
            && self.prefix == other.prefix
            && self.endpoint == other.endpoint
    }

    /// A short human description of the location (`/srv/backups`, `s3://bucket/prefix`).
    pub fn location(&self) -> String {
        match self.kind {
            RepoType::Fs => self.path.clone().unwrap_or_default(),
            RepoType::Memory => "memory://".into(),
            k => {
                let scheme = match k {
                    RepoType::Gcs => "gs",
                    RepoType::Azure => "az",
                    _ => "s3",
                };
                let mut s = format!("{scheme}://{}", self.bucket.as_deref().unwrap_or(""));
                if let Some(p) = self.prefix.as_deref().filter(|p| !p.is_empty()) {
                    s.push('/');
                    s.push_str(p.trim_matches('/'));
                }
                s
            }
        }
    }
}

/// Where a repository or policy is defined.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ConfigSource {
    /// registered through the HTTP API (or UI); editable there
    #[default]
    Api,
    /// the `--backup-config` file; read-only through the API
    Config,
}

/// Reachability of a repository, from its last connection test or operation.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RepoStatus {
    pub reachable: bool,
    /// when this was determined
    pub checked: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub conditional_writes: Option<bool>,
    /// no conditional writes (configured or detected): one writer only
    pub single_writer: bool,
}

/// Totals of a repository, from the last listing or GC.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RepoStats {
    pub backups: u64,
    pub datasets: u64,
    pub stored_bytes: u64,
    pub logical_bytes: u64,
    /// logical / stored (1.0 for an empty repository)
    pub dedup_ratio: f64,
    pub as_of: String,
}

/// A registered repository (`Repository` of the API: the settings plus server state).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Repository {
    #[serde(flatten)]
    pub config: RepoConfig,
    pub source: ConfigSource,
    /// the repository id from its marker; `None` until reachable once
    pub id: Option<Uuid>,
    pub status: RepoStatus,
    pub stats: Option<RepoStats>,
    /// the last GC (`gc/last.json`)
    #[serde(default)]
    pub last_gc: Option<LastGc>,
    /// policies that back up to this repository (a delete is refused while any does)
    #[serde(default)]
    pub policies: Vec<String>,
    /// the connection test of a registration (`POST /$/repositories`)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub test: Option<TestReport>,
}

/// What a principal without `server-admin` sees of a repository (to pick a target).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RepositoryBrief {
    pub name: String,
    #[serde(rename = "type")]
    pub kind: RepoType,
    pub readonly: bool,
    pub reachable: bool,
}

/// One entry of `GET /$/repositories`: full for `server-admin`, brief otherwise.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum RepositoryEntry {
    Full(Box<Repository>),
    Brief(RepositoryBrief),
}

/// `GET /$/repositories`
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct RepositoryList {
    pub repositories: Vec<RepositoryEntry>,
}

/// A step of the connection test.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum TestStepKind {
    /// create `probe/<uuid>` with a conditional create
    Create,
    /// create it again, expecting "already exists" (conditional-write support)
    CreateAgain,
    Read,
    List,
    Delete,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TestStep {
    pub step: TestStepKind,
    pub ok: bool,
    pub millis: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// `POST /$/repositories/{repo}/test`
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TestReport {
    pub ok: bool,
    pub conditional_writes: bool,
    pub steps: Vec<TestStep>,
}

// --------------------------------------------------------------------- backups ------

/// The dataset of a backup summary.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DatasetRef {
    pub name: String,
    pub id: Uuid,
}

/// The commit of a backup summary.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CommitRef {
    pub seq: u64,
    pub timestamp: String,
    pub quads: u64,
    /// `commit:<seq>`
    #[serde(rename = "ref")]
    pub reference: String,
}

/// Depth of a verification.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum VerifyLevel {
    /// manifests validate and every blob is present with its size (LIST/HEAD only)
    #[default]
    Exists,
    /// and every blob is downloaded, decoded and hashed
    Data,
    /// and a full restore into a temporary directory, `check` in full mode, head and
    /// quad count compared
    Restore,
}

/// Outcome of a verification (a backup's own result is `ok` or `error`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum VerifyStatus {
    Ok,
    /// only orphans (repository verify)
    Warning,
    Error,
}

/// The last verification of a backup on this server (`<data>/backup/verify.json`).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Verified {
    pub level: VerifyLevel,
    pub status: VerifyStatus,
    pub at: String,
}

/// A backup in listings (`BackupSummary`).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BackupSummary {
    pub name: String,
    /// the repository's name on this server
    pub repository: String,
    pub dataset: DatasetRef,
    pub commit: CommitRef,
    pub created: String,
    pub completed: String,
    pub millis: u64,
    /// sum of the file sizes
    pub logical_bytes: u64,
    /// stored bytes of the blobs this backup uploaded first
    pub added_bytes: u64,
    pub policy: Option<String>,
    pub run: Option<String>,
    pub note: Option<String>,
    /// only under `/$/backups/{ds}`: the backup's dataset id is the live dataset's
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub same_lineage: Option<bool>,
    /// the last verification on this server, if any
    #[serde(default)]
    pub verified: Option<Verified>,
}

/// A stored blob of a file.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct BlobRef {
    /// lowercase hex SHA-256 of the plaintext
    pub id: String,
    /// plaintext bytes
    pub size: u64,
}

/// A file of a backup.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileEntry {
    /// relative to the database root (`gen-0001/spo.dat`)
    pub path: String,
    pub kind: FileKind,
    pub size: u64,
    /// SHA-256 of the whole restored file
    pub sha256: String,
    /// the file is the concatenation of these blobs, in order
    pub blobs: Vec<BlobRef>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ServerInfo {
    pub version: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TextDerived {
    pub rebuild_on_restore: bool,
}

/// Derived state left out of a backup and rebuilt after a restore.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Derived {
    /// the full-text index (`text.json` is backed up; the index is rebuilt at open)
    #[serde(default)]
    pub text: Option<TextDerived>,
}

/// A backup with its manifest details (`GET /$/backups/{ds}/{repo}/{backup}`).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Backup {
    #[serde(flatten)]
    pub summary: BackupSummary,
    pub format: u32,
    pub generation: String,
    pub index_format: u32,
    /// the backup whose blobs this one reused (the newest of the same dataset id)
    pub parent: Option<String>,
    pub server: ServerInfo,
    pub files: Vec<FileEntry>,
    /// what the backup uploaded and reused
    pub stats: ManifestStats,
    pub derived: Derived,
}

/// The dataset of a manifest.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ManifestDataset {
    pub name: String,
    pub id: Uuid,
    /// `persistent`
    #[serde(rename = "type")]
    pub kind: String,
}

/// The full commit object of the captured commit (as `sparkles::commit::CommitInfo`
/// serializes).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ManifestCommit {
    pub seq: u64,
    pub parent: Option<u64>,
    #[serde(rename = "ref")]
    pub reference: String,
    pub timestamp: String,
    pub kind: String,
    pub inserted: u64,
    pub deleted: u64,
    pub quads: u64,
    pub generation: String,
    pub bulk: bool,
    pub exact: bool,
    #[serde(default, skip_serializing_if = "is_false")]
    pub reconstructed: bool,
}

impl From<&sparkles::commit::CommitInfo> for ManifestCommit {
    fn from(c: &sparkles::commit::CommitInfo) -> ManifestCommit {
        ManifestCommit {
            seq: c.seq,
            parent: c.parent(),
            reference: c.reference(),
            timestamp: c.timestamp(),
            kind: c.kind.name().to_string(),
            inserted: c.inserted,
            deleted: c.deleted,
            quads: c.quads,
            generation: c.generation_name(),
            bulk: c.bulk,
            exact: c.exact,
            reconstructed: c.reconstructed,
        }
    }
}

/// Upload statistics of a backup.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ManifestStats {
    pub logical_bytes: u64,
    pub added_bytes: u64,
    pub files: u64,
    pub blobs: u64,
    pub new_blobs: u64,
    pub reused_blobs: u64,
}

/// `backups/<name>.json`: the immutable description of a backup (format 1). Created
/// last, with a conditional create; a backup exists iff its manifest does. Validated
/// before use (see `manifest::validate`).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Manifest {
    /// 1
    pub format: u32,
    /// `sparkles-backup`
    pub kind: String,
    pub name: String,
    /// unique id of this backup
    pub id: Uuid,
    pub repository_id: Uuid,
    pub dataset: ManifestDataset,
    pub commit: ManifestCommit,
    pub generation: String,
    pub index_format: u32,
    pub created: String,
    pub completed: String,
    pub millis: u64,
    pub server: ServerInfo,
    pub parent: Option<String>,
    pub policy: Option<String>,
    pub run: Option<String>,
    pub note: Option<String>,
    pub files: Vec<FileEntry>,
    pub stats: ManifestStats,
    pub derived: Derived,
    /// reserved for client-side encryption; this format requires `null`
    #[serde(default)]
    pub encryption: Option<J>,
}

impl Manifest {
    /// The listing form, for the repository registered as `repository`.
    pub fn summary(&self, repository: &str) -> BackupSummary {
        BackupSummary {
            name: self.name.clone(),
            repository: repository.to_string(),
            dataset: DatasetRef {
                name: self.dataset.name.clone(),
                id: self.dataset.id,
            },
            commit: CommitRef {
                seq: self.commit.seq,
                timestamp: self.commit.timestamp.clone(),
                quads: self.commit.quads,
                reference: self.commit.reference.clone(),
            },
            created: self.created.clone(),
            completed: self.completed.clone(),
            millis: self.millis,
            logical_bytes: self.stats.logical_bytes,
            added_bytes: self.stats.added_bytes,
            policy: self.policy.clone(),
            run: self.run.clone(),
            note: self.note.clone(),
            same_lineage: None,
            verified: None,
        }
    }

    /// The detail form (`Backup`).
    pub fn view(&self, repository: &str) -> Backup {
        Backup {
            summary: self.summary(repository),
            format: self.format,
            generation: self.generation.clone(),
            index_format: self.index_format,
            parent: self.parent.clone(),
            server: self.server.clone(),
            files: self.files.clone(),
            stats: self.stats.clone(),
            derived: self.derived.clone(),
        }
    }
}

/// `GET /$/repositories/{repo}/backups`: newest first; `next` is the `before` cursor of
/// the following page.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct BackupPage {
    pub backups: Vec<BackupSummary>,
    pub next: Option<String>,
}

/// `GET /$/backups/{ds}`
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DatasetBackups {
    pub dataset: String,
    /// the live dataset's id (`None` when no such dataset is registered)
    pub dataset_id: Option<Uuid>,
    pub backups: Vec<BackupSummary>,
}

/// `POST /$/backups/{ds}`
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CreateBackupRequest {
    pub repository: String,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub note: Option<String>,
}

/// Which dataset id a restore gives the restored dataset.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Identity {
    /// keep the id unless a dataset registered here has it, else `new`
    #[default]
    Auto,
    /// mint a fresh id (`forkedFrom` names the source commit)
    New,
    /// keep the id (`409 duplicate-dataset-id` if it is in use)
    Keep,
}

/// The integrity check run on a restored directory before it is opened.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CheckLevel {
    #[default]
    Quick,
    Full,
    None,
}

/// `POST /$/backups/{ds}/{repo}/{backup}/restore`
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RestoreRequest {
    /// default: `{ds}`
    #[serde(default)]
    pub target: Option<String>,
    /// replace the registered dataset `target` in place
    #[serde(default)]
    pub replace: bool,
    #[serde(default)]
    pub identity: Identity,
    #[serde(default)]
    pub check: CheckLevel,
    /// keep `databases/.replaced-<t>-<task>` after an in-place restore
    #[serde(default)]
    pub keep_replaced: bool,
}

/// `POST …/verify` (a backup or a repository)
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct VerifyRequest {
    #[serde(default)]
    pub level: VerifyLevel,
}

/// `restore.json` of a restored database directory (informational).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RestoreRecord {
    /// 1
    pub restore_format: u32,
    pub repository: RestoreRepository,
    pub backup: String,
    pub source: RestoreSource,
    /// `kept` or `new`
    pub identity: String,
    pub time: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RestoreRepository {
    pub name: String,
    pub id: Uuid,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RestoreSource {
    pub dataset_id: Uuid,
    pub seq: u64,
    pub name: String,
}

// ---------------------------------------------------------------------- verify ------

/// One backup's result in a [`VerifyReport`].
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct BackupVerify {
    pub name: String,
    pub status: VerifyStatus,
    /// ids of missing blobs (or blobs of the wrong size)
    pub missing: Vec<String>,
    /// ids of blobs whose content does not hash to their id
    pub corrupt: Vec<String>,
    /// the `sparkles::check` report (level `restore`)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub check: Option<J>,
}

/// Unreferenced blobs (GC candidates, not errors).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Orphans {
    pub blobs: u64,
    pub bytes: u64,
}

/// Object requests a verification made.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct VerifyRequests {
    pub list: u64,
    pub head: u64,
    pub get: u64,
}

/// The result of a verification (`detail` of a `backup-verify` task).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct VerifyReport {
    pub level: VerifyLevel,
    pub status: VerifyStatus,
    pub backups: Vec<BackupVerify>,
    /// repository verify only
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub orphans: Option<Orphans>,
    pub requests: VerifyRequests,
    pub millis: u64,
}

// -------------------------------------------------------------------------- gc ------

/// `POST /$/repositories/{repo}/gc`
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GcRequest {
    #[serde(default)]
    pub dry_run: bool,
    /// default 24
    #[serde(default)]
    pub grace_hours: Option<f64>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct GcRequests {
    pub list: u64,
    pub get: u64,
    pub delete: u64,
}

/// The result of a GC (`detail` of a `backup-gc` task, `gc/last.json`).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GcReport {
    pub dry_run: bool,
    pub manifests: u64,
    pub referenced_blobs: u64,
    pub listed_blobs: u64,
    pub candidates: u64,
    pub deleted: u64,
    pub deleted_bytes: u64,
    /// unreferenced blobs younger than the grace period
    pub kept_young: u64,
    pub stored_bytes_after: u64,
    pub requests: GcRequests,
    pub millis: u64,
    pub lock_wait_millis: u64,
}

/// `Repository.lastGc`: the report of `gc/last.json` and when that GC finished.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct LastGc {
    #[serde(flatten)]
    pub report: GcReport,
    pub finished: String,
}

// ----------------------------------------------------------------------- locks ------

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum LockKind {
    Shared,
    Exclusive,
}

/// The operation a lock was taken for.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum LockOperation {
    Create,
    Restore,
    Verify,
    Delete,
    Gc,
}

/// Who holds a lock.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct LockHolder {
    pub host: String,
    pub pid: u32,
    /// a hash of the server's data directory (empty for the CLI)
    pub server: String,
    pub version: String,
}

/// `locks/<uuid>.json` as stored.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct LockObject {
    pub kind: LockKind,
    pub holder: LockHolder,
    pub operation: LockOperation,
    pub created: String,
}

/// A lock as listed (`GET /$/repositories/{repo}/locks`).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LockInfo {
    /// the `<uuid>` of `locks/<uuid>.json`
    pub id: String,
    pub kind: LockKind,
    pub operation: LockOperation,
    pub holder: LockHolder,
    pub created: String,
    /// the storage server's time of the last write (refresh)
    pub last_modified: String,
    /// older than the stale age: ignored by acquirers, removed by GC
    pub stale: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct LockList {
    pub locks: Vec<LockInfo>,
}

// -------------------------------------------------------------------- policies ------

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

/// Which backups of a policy retention keeps.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Retention {
    /// a duration (`30d`, `12h`); `None`: no age limit
    #[serde(default)]
    pub expire_after: Option<String>,
    /// kept unconditionally (the newest ones), default 1
    #[serde(default = "one")]
    pub min_count: u32,
    #[serde(default)]
    pub max_count: Option<u32>,
}

impl Default for Retention {
    fn default() -> Retention {
        Retention {
            expire_after: None,
            min_count: 1,
            max_count: None,
        }
    }
}

/// What happens to scheduled instants missed while the server was down.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CatchUp {
    /// one run covers them all
    #[default]
    One,
    /// they are recorded as skipped
    None,
}

/// A policy's settings: the body of `POST`/`PUT /$/backup-policies[/{p}]` (a `Policy`
/// without `source` and `state`).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PolicyConfig {
    /// `[a-z0-9][a-z0-9_-]{0,63}`
    pub name: String,
    pub repository: String,
    /// names or `*` globs; default `["*"]`
    #[serde(default = "all_datasets")]
    pub datasets: Vec<String>,
    /// cron (5 fields, or 6 with seconds) or `every <duration>` (at least 1 minute)
    pub schedule: String,
    /// IANA name, default `UTC`
    #[serde(default = "utc")]
    pub timezone: String,
    #[serde(default = "default_template")]
    pub name_template: String,
    #[serde(default)]
    pub retention: Retention,
    /// skip a dataset whose head equals its last policy backup's commit (same id)
    #[serde(default)]
    pub skip_unchanged: bool,
    /// GC the repository after retention (at most once per 24 h)
    #[serde(default)]
    pub gc_after_retention: bool,
    #[serde(default)]
    pub catch_up: CatchUp,
    #[serde(default = "yes")]
    pub enabled: bool,
}

/// Scheduler state of a policy.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PolicyState {
    pub next_run: Option<String>,
    pub last_scheduled_for: Option<String>,
    pub last_run: Option<Box<PolicyRun>>,
    pub last_success: Option<String>,
    pub consecutive_failures: u32,
    pub running_task: Option<String>,
}

/// A policy (`GET /$/backup-policies[/{p}]`).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Policy {
    #[serde(flatten)]
    pub config: PolicyConfig,
    pub source: ConfigSource,
    pub state: PolicyState,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct PolicyList {
    pub policies: Vec<Policy>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum RunTrigger {
    Schedule,
    CatchUp,
    Manual,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RunResult {
    Ok,
    /// some datasets failed
    Partial,
    Failed,
    /// overlap with a running run, or the policy was disabled mid-run
    Skipped,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum DatasetRunResult {
    Ok,
    Failed,
    Skipped,
}

/// One dataset of a policy run.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PolicyRunDataset {
    pub dataset: String,
    pub backup: Option<String>,
    pub result: DatasetRunResult,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub added_bytes: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub millis: Option<u64>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RunRetention {
    pub deleted: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RunGc {
    pub task: String,
}

/// One run of a policy (`GET /$/backup-policies/{p}/runs`, `detail` of a
/// `backup-policy` task).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PolicyRun {
    pub id: String,
    pub policy: String,
    pub trigger: RunTrigger,
    pub scheduled_for: Option<String>,
    pub started: String,
    pub finished: Option<String>,
    pub result: RunResult,
    pub datasets: Vec<PolicyRunDataset>,
    pub retention: Option<RunRetention>,
    pub gc: Option<RunGc>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct PolicyRunList {
    pub runs: Vec<PolicyRun>,
}

/// `POST /$/backup-policies/preview`
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PreviewRequest {
    pub schedule: String,
    #[serde(default = "utc")]
    pub timezone: String,
    /// default 5
    #[serde(default)]
    pub count: Option<usize>,
    /// render a sample name with this template
    #[serde(default)]
    pub name_template: Option<String>,
    /// the dataset of the sample (default `ds`)
    #[serde(default)]
    pub dataset: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PreviewResponse {
    /// the next instants, RFC 3339 UTC
    pub next: Vec<String>,
    /// e.g. "at 02:30 every day (Europe/Berlin)"
    pub description: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sample: Option<String>,
}

/// `POST /$/backup-policies/{p}/retention[?dryRun=true]`
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RetentionResponse {
    pub dry_run: bool,
    pub delete: Vec<BackupSummary>,
    pub keep: Vec<BackupSummary>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub errors: Option<Vec<String>>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn repository_config_wire_form() {
        let c: RepoConfig = serde_json::from_value(json!({
            "name": "s3-main", "type": "s3", "bucket": "kg", "prefix": "prod",
            "pathStyle": true, "allowHttp": true,
            "credentials": {"source": "env", "accessKeyIdVar": "K", "secretAccessKeyVar": "S"},
            "sse": "aws:kms", "kmsKeyId": "arn:x", "maxUploadBytesPerSec": 1048576
        }))
        .unwrap();
        assert_eq!(c.kind, RepoType::S3);
        assert!(c.conditional_writes && !c.readonly);
        assert_eq!(c.concurrency(), 8);
        assert_eq!(c.location(), "s3://kg/prod");
        assert_eq!(
            c.credentials,
            Credentials::Env {
                access_key_id_var: "K".into(),
                secret_access_key_var: "S".into(),
                session_token_var: None
            }
        );
        let v = serde_json::to_value(&c).unwrap();
        assert_eq!(v["credentials"]["accessKeyIdVar"], "K");
        assert_eq!(v["sse"], "aws:kms");
        assert_eq!(v["conditionalWrites"], true);
        let fs: RepoConfig =
            serde_json::from_value(json!({"name": "local", "type": "fs", "path": "/r"})).unwrap();
        assert_eq!(fs.credentials, Credentials::Default);
        assert_eq!(fs.concurrency(), 4);
        assert!(!fs.same_location(&c));
    }

    #[test]
    fn repository_flattens_its_config() {
        let r = Repository {
            config: RepoConfig {
                name: "local".into(),
                path: Some("/r".into()),
                ..Default::default()
            },
            source: ConfigSource::Api,
            id: None,
            status: RepoStatus::default(),
            stats: None,
            last_gc: None,
            policies: vec![],
            test: None,
        };
        let v = serde_json::to_value(&r).unwrap();
        assert_eq!(v["name"], "local");
        assert_eq!(v["type"], "fs");
        assert_eq!(v["source"], "api");
        assert_eq!(v["status"]["singleWriter"], false);
        assert!(v["stats"].is_null() && v["lastGc"].is_null() && v["id"].is_null());
        let back: Repository = serde_json::from_value(v).unwrap();
        assert_eq!(back, r);
    }

    fn manifest() -> Manifest {
        serde_json::from_value(json!({
            "format": 1, "kind": "sparkles-backup", "name": "b2",
            "id": "0b6e5c1a-0000-4000-8000-000000000001",
            "repositoryId": "7d0e5c1a-0000-4000-8000-000000000002",
            "dataset": {"name": "ds", "id": "3f1c9a2e-0000-4000-8000-000000000003", "type": "persistent"},
            "commit": {"seq": 4, "parent": 3, "ref": "commit:4", "timestamp": "2026-09-30T14:05:11.990Z",
                       "kind": "update", "inserted": 1, "deleted": 0, "quads": 3,
                       "generation": "gen-0001", "bulk": false, "exact": true},
            "generation": "gen-0001", "indexFormat": 2,
            "created": "2026-09-30T14:05:11.995Z", "completed": "2026-09-30T14:05:12.101Z",
            "millis": 106, "server": {"version": "0.1.0"}, "parent": "b1",
            "policy": null, "run": null, "note": null,
            "files": [{"path": "CURRENT", "kind": "meta", "size": 8, "sha256": "aa",
                       "blobs": [{"id": "bb", "size": 8}]}],
            "stats": {"logicalBytes": 300, "addedBytes": 73, "files": 1, "blobs": 1,
                      "newBlobs": 1, "reusedBlobs": 0},
            "derived": {"text": {"rebuildOnRestore": true}},
            "encryption": null,
            "someFutureField": 1
        }))
        .unwrap()
    }

    #[test]
    fn manifest_summary_and_view() {
        let m = manifest();
        let s = m.summary("local");
        let v = serde_json::to_value(&s).unwrap();
        assert_eq!(v["commit"]["ref"], "commit:4");
        assert_eq!(v["logicalBytes"], 300);
        assert!(v["verified"].is_null());
        assert!(v.get("sameLineage").is_none());
        let b = serde_json::to_value(m.view("local")).unwrap();
        assert_eq!(b["name"], "b2");
        assert_eq!(b["indexFormat"], 2);
        assert_eq!(b["files"][0]["kind"], "meta");
        assert_eq!(b["derived"]["text"]["rebuildOnRestore"], true);
        // a manifest survives a round trip
        let again: Manifest = serde_json::from_value(serde_json::to_value(&m).unwrap()).unwrap();
        assert_eq!(again, m);
    }

    #[test]
    fn manifest_commit_matches_commit_info() {
        let c = sparkles::commit::CommitInfo {
            seq: 4,
            timestamp_ms: 1_790_000_000_000,
            kind: sparkles::commit::CommitKind::Update,
            inserted: 1,
            deleted: 0,
            quads: 3,
            generation: 1,
            bulk: false,
            exact: true,
            reconstructed: false,
        };
        let m = ManifestCommit::from(&c);
        assert_eq!(
            serde_json::to_value(&m).unwrap(),
            serde_json::to_value(c).unwrap()
        );
    }

    #[test]
    fn policy_defaults() {
        let p: PolicyConfig = serde_json::from_value(json!({
            "name": "nightly", "repository": "local", "schedule": "30 2 * * *"
        }))
        .unwrap();
        assert_eq!(p.datasets, ["*"]);
        assert_eq!(p.timezone, "UTC");
        assert_eq!(p.name_template, "{policy}-{dataset}-{time}");
        assert_eq!(p.retention, Retention::default());
        assert_eq!(p.catch_up, CatchUp::One);
        assert!(p.enabled);
        let pol = Policy {
            config: p,
            source: ConfigSource::Config,
            state: PolicyState::default(),
        };
        let v = serde_json::to_value(&pol).unwrap();
        assert_eq!(v["catchUp"], "one");
        assert_eq!(v["source"], "config");
        assert!(v["state"]["nextRun"].is_null());
        assert_eq!(v["state"]["consecutiveFailures"], 0);
    }

    #[test]
    fn request_defaults() {
        let r: RestoreRequest = serde_json::from_value(json!({})).unwrap();
        assert_eq!(r.identity, Identity::Auto);
        assert_eq!(r.check, CheckLevel::Quick);
        assert!(!r.replace && !r.keep_replaced && r.target.is_none());
        let v: VerifyRequest = serde_json::from_value(json!({})).unwrap();
        assert_eq!(v.level, VerifyLevel::Exists);
        let g: GcRequest = serde_json::from_value(json!({"dryRun": true})).unwrap();
        assert!(g.dry_run && g.grace_hours.is_none());
        let t = TestStep {
            step: TestStepKind::CreateAgain,
            ok: true,
            millis: 3,
            error: None,
        };
        assert_eq!(serde_json::to_value(t).unwrap()["step"], "create-again");
        let run = RunTrigger::CatchUp;
        assert_eq!(serde_json::to_value(run).unwrap(), "catch-up");
    }
}
