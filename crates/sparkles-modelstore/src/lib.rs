//! A verified store of model files (spec F12 §4).
//!
//! A model is a set of files: weights, a tokenizer, configuration. Sparkles reads them from
//! one of two places.
//!
//! * A **local directory** the operator provides, such as a Nix store path built by a
//!   fixed-output derivation. [`LocalSnapshot::open`] checks that the files exist and,
//!   for files with a declared digest, that the digest matches.
//! * A **download** planned from a source (the Hugging Face Hub through [`HubSource`], or
//!   any list of URLs with digests) and kept under
//!   `<root>/<repo>/<revision>/`. [`ModelStore::fetch`] downloads each file to a `.part`
//!   file, resumes a partial file with an HTTP range request, checks its size and digest,
//!   and moves it into a staging directory. When every file is in, it writes a manifest
//!   and renames the staging directory into place, so a snapshot directory either exists
//!   complete and verified or does not exist.
//!
//! The store does not speak HTTP itself. The caller passes an [`HttpClient`], which in
//! the server is the outbound-policy-checked client, so the allowed destinations,
//! timeouts and redirect checks apply to model downloads as to any other request.
//!
//! Nothing here knows about embeddings: the OCR models of PDF ingestion use the same
//! store.

mod digest;
mod hub;

use std::collections::HashMap;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

pub use digest::{Digest, git_blob_reader, sha256_reader};
pub use hub::{DEFAULT_ENDPOINT, HubSource, is_pinned};

/// The name of the manifest a completed snapshot directory holds.
pub const MANIFEST: &str = "sparkles-manifest.json";

/// Errors of the store.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error(
        "revision {0:?} is not a full commit id; pin the model to a 40-character commit, or allow unpinned revisions explicitly"
    )]
    Unpinned(String),
    #[error("invalid repository id {0:?}; expected owner/name")]
    BadRepo(String),
    #[error("invalid file path {0:?}")]
    BadPath(String),
    #[error("{0}")]
    Http(String),
    #[error("GET {url}: HTTP status {status}")]
    Status { url: String, status: u16 },
    #[error("{path}: expected {expected}, found {found}")]
    DigestMismatch {
        path: String,
        expected: Digest,
        found: Digest,
    },
    #[error("{path}: expected {expected} bytes, received {found}")]
    SizeMismatch {
        path: String,
        expected: u64,
        found: u64,
    },
    #[error("{0}: missing")]
    Missing(String),
    #[error("{0}")]
    Invalid(String),
    #[error("{0}")]
    Io(#[from] io::Error),
}

pub type Result<T> = std::result::Result<T, Error>;

/// A response whose status and headers are in. The body is read as a stream.
pub struct HttpResponse {
    pub status: u16,
    pub body: Box<dyn Read + Send>,
}

/// A blocking HTTP GET. The server implements it on its outbound policy. An
/// implementation follows redirects (checking each hop against its policy) and must
/// not forward an `Authorization` header to another host.
pub trait HttpClient: Send + Sync {
    /// GET `url` with `headers`. Any status is a response; an error is a refused
    /// destination or a network failure, described in the string.
    fn get(&self, url: &str, headers: &[(&str, &str)])
    -> std::result::Result<HttpResponse, String>;
}

/// A snapshot's identity: a repository (`owner/name`) and a revision. For the Hub the
/// revision is a full commit id; other sources may use any stable version string.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct SnapshotId {
    pub repo: String,
    pub revision: String,
}

/// One file to download.
#[derive(Clone, Debug)]
pub struct RemoteFile {
    /// The path inside the snapshot, with `/` separators.
    pub path: String,
    pub url: String,
    pub size: u64,
    pub digest: Digest,
}

/// The files of a snapshot and where to get them.
#[derive(Clone, Debug)]
pub struct FetchPlan {
    pub id: SnapshotId,
    pub files: Vec<RemoteFile>,
    /// Headers sent with every file request (a Hub token).
    pub headers: Vec<(String, String)>,
}

impl FetchPlan {
    /// Apply digests the operator declared. A declared SHA-256 replaces a Git blob id.
    /// A declared digest that contradicts the source's SHA-256, or names a file the plan
    /// lacks, is an error.
    pub fn apply_declared(&mut self, declared: &HashMap<String, Digest>) -> Result<()> {
        for (path, d) in declared {
            let f = self
                .files
                .iter_mut()
                .find(|f| &f.path == path)
                .ok_or_else(|| Error::Missing(path.clone()))?;
            match (&f.digest, d) {
                (Digest::Sha256(_), Digest::Sha256(_)) if &f.digest != d => {
                    return Err(Error::DigestMismatch {
                        path: path.clone(),
                        expected: d.clone(),
                        found: f.digest.clone(),
                    });
                }
                (Digest::Sha256(_), Digest::GitBlobSha1(_)) => {}
                _ => f.digest = d.clone(),
            }
        }
        Ok(())
    }

    /// The total size of the files.
    pub fn bytes(&self) -> u64 {
        self.files.iter().map(|f| f.size).sum()
    }
}

/// A file of a completed snapshot, as its manifest records it.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ManifestFile {
    pub path: String,
    pub size: u64,
    /// Hex SHA-256 of the file, whatever digest the source gave.
    pub sha256: String,
}

/// The manifest of a completed download.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Manifest {
    #[serde(flatten)]
    pub id: SnapshotId,
    pub files: Vec<ManifestFile>,
}

/// A directory of model files that is ready to read.
#[derive(Clone, Debug)]
pub struct Snapshot {
    pub dir: PathBuf,
    /// The download's identity, or `None` for an operator's directory.
    pub id: Option<SnapshotId>,
}

impl Snapshot {
    pub fn path(&self, rel: &str) -> PathBuf {
        self.dir.join(rel)
    }
}

/// A file an operator's directory must hold, with an optional digest to check.
#[derive(Clone, Debug)]
pub struct ExpectedFile {
    pub path: String,
    pub digest: Option<Digest>,
}

/// An operator-provided directory (the Nix route).
pub struct LocalSnapshot;

impl LocalSnapshot {
    /// Open `dir`, checking that each expected file exists and, when it has a digest,
    /// that the digest matches. Nothing is written.
    pub fn open(dir: &Path, expected: &[ExpectedFile]) -> Result<Snapshot> {
        if !dir.is_dir() {
            return Err(Error::Missing(dir.display().to_string()));
        }
        for e in expected {
            check_rel_path(&e.path)?;
            let p = dir.join(&e.path);
            if !p.is_file() {
                return Err(Error::Missing(p.display().to_string()));
            }
            if let Some(d) = &e.digest {
                let found = d.of_file(&p)?;
                if &found != d {
                    return Err(Error::DigestMismatch {
                        path: e.path.clone(),
                        expected: d.clone(),
                        found,
                    });
                }
            }
        }
        Ok(Snapshot {
            dir: dir.to_path_buf(),
            id: None,
        })
    }
}

/// Progress of a download, for status reports.
#[derive(Clone, Debug, Default)]
pub struct Progress {
    pub files_done: usize,
    pub files_total: usize,
    pub bytes_done: u64,
    pub bytes_total: u64,
}

/// Downloaded snapshots under a root directory, `<dataDir>/models` in the server.
#[derive(Clone, Debug)]
pub struct ModelStore {
    root: PathBuf,
}

impl ModelStore {
    pub fn new(root: impl Into<PathBuf>) -> ModelStore {
        ModelStore { root: root.into() }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Where the snapshot `id` lives when complete.
    pub fn dir(&self, id: &SnapshotId) -> Result<PathBuf> {
        check_repo(&id.repo)?;
        check_segment(&id.revision)?;
        Ok(self.root.join(&id.repo).join(&id.revision))
    }

    fn staging(&self, id: &SnapshotId) -> Result<PathBuf> {
        check_repo(&id.repo)?;
        Ok(self
            .root
            .join(&id.repo)
            .join(format!(".{}.partial", id.revision)))
    }

    /// The completed snapshot `id`, if it was downloaded.
    pub fn get(&self, id: &SnapshotId) -> Result<Option<Snapshot>> {
        let dir = self.dir(id)?;
        Ok(dir.join(MANIFEST).is_file().then(|| Snapshot {
            dir,
            id: Some(id.clone()),
        }))
    }

    /// The completed snapshots under the root, sorted by repository and revision.
    /// Directories without a manifest (staging directories, foreign files) are skipped.
    pub fn list(&self) -> Result<Vec<Manifest>> {
        let mut out = Vec::new();
        let dirs = |p: &Path| -> Result<Vec<(String, PathBuf)>> {
            let mut v = Vec::new();
            match fs::read_dir(p) {
                Ok(rd) => {
                    for e in rd {
                        let e = e?;
                        let name = e.file_name().to_string_lossy().into_owned();
                        if e.file_type()?.is_dir() && check_segment(&name).is_ok() {
                            v.push((name, e.path()));
                        }
                    }
                }
                Err(e) if e.kind() == io::ErrorKind::NotFound => {}
                Err(e) => return Err(e.into()),
            }
            v.sort();
            Ok(v)
        };
        for (owner, op) in dirs(&self.root)? {
            for (name, np) in dirs(&op)? {
                for (rev, rp) in dirs(&np)? {
                    if !rp.join(MANIFEST).is_file() {
                        continue;
                    }
                    let mut m = read_manifest(&rp)?;
                    // the directory names the snapshot, whatever the manifest says
                    m.id = SnapshotId {
                        repo: format!("{owner}/{name}"),
                        revision: rev,
                    };
                    out.push(m);
                }
            }
        }
        Ok(out)
    }

    /// Delete a completed snapshot. Returns whether it existed.
    pub fn remove(&self, id: &SnapshotId) -> Result<bool> {
        let dir = self.dir(id)?;
        if !dir.exists() {
            return Ok(false);
        }
        fs::remove_dir_all(&dir)?;
        let parent = self.root.join(&id.repo);
        // drop empty owner and repository directories
        let _ = fs::remove_dir(&parent);
        if let Some(owner) = parent.parent() {
            let _ = fs::remove_dir(owner);
        }
        Ok(true)
    }

    /// Read the files of a completed snapshot again and compare them with its manifest.
    pub fn verify(&self, id: &SnapshotId) -> Result<()> {
        let dir = self.dir(id)?;
        let m = read_manifest(&dir)?;
        for f in &m.files {
            check_rel_path(&f.path)?;
            let d = Digest::sha256_hex(&f.sha256)
                .ok_or_else(|| Error::Invalid(format!("bad digest {}", f.sha256)))?;
            let found = d.of_file(&dir.join(&f.path))?;
            if found != d {
                return Err(Error::DigestMismatch {
                    path: f.path.clone(),
                    expected: d,
                    found,
                });
            }
        }
        Ok(())
    }

    /// Download the files of `plan` and return the completed snapshot. A snapshot that
    /// is already complete is returned without any request. A download interrupted
    /// earlier resumes: verified files are kept and partial files continue with a range
    /// request. One process at a time downloads a given snapshot; others wait on its lock.
    pub fn fetch(
        &self,
        client: &dyn HttpClient,
        plan: &FetchPlan,
        progress: &mut dyn FnMut(&Progress),
    ) -> Result<Snapshot> {
        if let Some(s) = self.get(&plan.id)? {
            return Ok(s);
        }
        for f in &plan.files {
            check_rel_path(&f.path)?;
        }
        let parent = self.root.join(&plan.id.repo);
        fs::create_dir_all(&parent)?;
        let lock = File::create(parent.join(format!(".{}.lock", plan.id.revision)))?;
        lock.lock()?;
        // another process may have finished while this one waited
        if let Some(s) = self.get(&plan.id)? {
            return Ok(s);
        }
        let staging = self.staging(&plan.id)?;
        fs::create_dir_all(&staging)?;
        let mut p = Progress {
            files_total: plan.files.len(),
            bytes_total: plan.bytes(),
            ..Progress::default()
        };
        progress(&p);
        let headers: Vec<(&str, &str)> = plan
            .headers
            .iter()
            .map(|(k, v)| (k.as_str(), v.as_str()))
            .collect();
        let mut files = Vec::with_capacity(plan.files.len());
        for f in &plan.files {
            let sha256 = fetch_file(client, &headers, &staging, f)?;
            files.push(ManifestFile {
                path: f.path.clone(),
                size: f.size,
                sha256: digest::hex(&sha256),
            });
            p.files_done += 1;
            p.bytes_done += f.size;
            progress(&p);
        }
        let manifest = Manifest {
            id: plan.id.clone(),
            files,
        };
        let mpath = staging.join(MANIFEST);
        let mut mf = File::create(&mpath)?;
        mf.write_all(&serde_json::to_vec_pretty(&manifest).expect("manifest"))?;
        mf.sync_all()?;
        let dir = self.dir(&plan.id)?;
        if dir.exists() {
            // an incomplete directory left by hand: replace it
            fs::remove_dir_all(&dir)?;
        }
        fs::rename(&staging, &dir)?;
        if let Ok(d) = File::open(&parent) {
            let _ = d.sync_all();
        }
        drop(lock);
        let _ = fs::remove_file(parent.join(format!(".{}.lock", plan.id.revision)));
        Ok(Snapshot {
            dir,
            id: Some(plan.id.clone()),
        })
    }
}

fn fetch_file(
    client: &dyn HttpClient,
    headers: &[(&str, &str)],
    staging: &Path,
    f: &RemoteFile,
) -> Result<[u8; 32]> {
    let dest = staging.join(&f.path);
    if let Some(d) = dest.parent() {
        fs::create_dir_all(d)?;
    }
    if dest.is_file() {
        if f.digest.of_file(&dest)? == f.digest {
            return sha256_of(&dest, &f.digest);
        }
        fs::remove_file(&dest)?;
    }
    let part = PathBuf::from(format!("{}.part", dest.display()));
    let mut have = fs::metadata(&part).map(|m| m.len()).unwrap_or(0);
    if have > f.size {
        fs::remove_file(&part)?;
        have = 0;
    }
    if have < f.size || f.size == 0 {
        let range = format!("bytes={have}-");
        let mut h: Vec<(&str, &str)> = headers.to_vec();
        if have > 0 {
            h.push(("Range", &range));
        }
        let resp = client
            .get(&f.url, &h)
            .map_err(|m| Error::Http(format!("GET {}: {m}", f.url)))?;
        let mut out = match resp.status {
            206 if have > 0 => OpenOptions::new().append(true).open(&part)?,
            200 => {
                have = 0;
                File::create(&part)?
            }
            416 if have == f.size => OpenOptions::new().append(true).open(&part)?,
            s => {
                return Err(Error::Status {
                    url: f.url.clone(),
                    status: s,
                });
            }
        };
        let want = f.size - have;
        let mut body = resp.body.take(want + 1);
        let n = io::copy(&mut body, &mut out)?;
        out.sync_all()?;
        if n > want {
            drop(out);
            fs::remove_file(&part)?;
            return Err(Error::SizeMismatch {
                path: f.path.clone(),
                expected: f.size,
                found: have + n,
            });
        }
        if n < want {
            // keep the part for a later resume
            return Err(Error::SizeMismatch {
                path: f.path.clone(),
                expected: f.size,
                found: have + n,
            });
        }
    }
    let found = f.digest.of_file(&part)?;
    if found != f.digest {
        fs::remove_file(&part)?;
        return Err(Error::DigestMismatch {
            path: f.path.clone(),
            expected: f.digest.clone(),
            found,
        });
    }
    let sha = sha256_of(&part, &f.digest)?;
    fs::rename(&part, &dest)?;
    Ok(sha)
}

/// The SHA-256 of a verified file: the digest itself, or a second read for Git blob ids.
fn sha256_of(path: &Path, verified: &Digest) -> Result<[u8; 32]> {
    match verified {
        Digest::Sha256(b) => Ok(*b),
        Digest::GitBlobSha1(_) => Ok(sha256_reader(&mut File::open(path)?)?),
    }
}

/// Read the manifest of a completed snapshot directory.
pub fn read_manifest(dir: &Path) -> Result<Manifest> {
    serde_json::from_slice(&fs::read(dir.join(MANIFEST))?)
        .map_err(|e| Error::Invalid(format!("{}: {e}", dir.join(MANIFEST).display())))
}

/// The default selection of a Hub repository's files: what a sentence-transformers or
/// Transformers model needs to run from safetensors. That is the JSON files at the root
/// (configuration, tokenizer, modules), the safetensors weights (one file or shards),
/// a SentencePiece `tokenizer.model`, and the `config.json` of each numbered module
/// directory such as `1_Pooling`. ONNX, OpenVINO, PyTorch pickles, READMEs and other
/// formats are skipped. A caller that needs other files lists them explicitly.
pub fn sentence_transformers_files(path: &str) -> bool {
    let (dir, file) = match path.rsplit_once('/') {
        Some((d, f)) => (Some(d), f),
        None => (None, path),
    };
    match dir {
        None => {
            file.ends_with(".json") && !file.starts_with("onnx")
                || file == "model.safetensors"
                || (file.starts_with("model-") && file.ends_with(".safetensors"))
                || file == "tokenizer.model"
        }
        Some(d) => {
            !d.contains('/')
                && d.split_once('_')
                    .is_some_and(|(n, _)| n.parse::<u32>().is_ok())
                && file == "config.json"
        }
    }
}

fn check_segment(s: &str) -> Result<()> {
    let ok = !s.is_empty()
        && s.len() <= 96
        && !s.starts_with('.')
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.'));
    if ok {
        Ok(())
    } else {
        Err(Error::BadPath(s.to_string()))
    }
}

/// Check an `owner/name` repository id.
pub fn check_repo(repo: &str) -> Result<()> {
    let mut parts = repo.split('/');
    match (parts.next(), parts.next(), parts.next()) {
        (Some(a), Some(b), None) if check_segment(a).is_ok() && check_segment(b).is_ok() => Ok(()),
        _ => Err(Error::BadRepo(repo.to_string())),
    }
}

/// Check a relative file path inside a snapshot: segments of letters, digits, `-`, `_`
/// and `.`, none empty or starting with a dot.
pub fn check_rel_path(path: &str) -> Result<()> {
    if path.is_empty() || path.split('/').any(|s| check_segment(s).is_err()) {
        return Err(Error::BadPath(path.to_string()));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paths() {
        assert!(check_repo("sentence-transformers/all-MiniLM-L6-v2").is_ok());
        assert!(check_repo("a").is_err());
        assert!(check_repo("a/b/c").is_err());
        assert!(check_repo("../b").is_err());
        assert!(check_rel_path("1_Pooling/config.json").is_ok());
        assert!(check_rel_path("../x").is_err());
        assert!(check_rel_path("/x").is_err());
        assert!(check_rel_path("a//b").is_err());
        assert!(is_pinned("c9745ed1d9f207416be6d2e6f8de32d1f16199bf"));
        assert!(!is_pinned("main"));
    }
}
