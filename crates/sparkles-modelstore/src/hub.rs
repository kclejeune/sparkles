//! The Hugging Face Hub as a source of model files.
//!
//! `GET {endpoint}/api/models/{repo}/revision/{revision}?blobs=true` lists a revision's
//! files with their sizes, their Git blob ids and, for files in Git LFS, the SHA-256 of
//! their content. `GET {endpoint}/{repo}/resolve/{revision}/{path}` serves a file, usually
//! through a redirect to a CDN.

use std::io::Read;

use percent_encoding::{AsciiSet, NON_ALPHANUMERIC, utf8_percent_encode};
use serde::Deserialize;

use crate::{Digest, Error, FetchPlan, HttpClient, RemoteFile, Result, SnapshotId};

/// The public Hub.
pub const DEFAULT_ENDPOINT: &str = "https://huggingface.co";

/// A Hub endpoint and, for gated or private repositories, a token.
#[derive(Clone, Debug)]
pub struct HubSource {
    pub endpoint: String,
    pub token: Option<String>,
}

impl Default for HubSource {
    fn default() -> Self {
        HubSource {
            endpoint: DEFAULT_ENDPOINT.to_string(),
            token: None,
        }
    }
}

#[derive(Deserialize)]
struct Listing {
    sha: Option<String>,
    #[serde(default)]
    siblings: Vec<Sibling>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Sibling {
    rfilename: String,
    size: Option<u64>,
    blob_id: Option<String>,
    lfs: Option<Lfs>,
}

#[derive(Deserialize)]
struct Lfs {
    sha256: String,
    size: u64,
}

const PATH_SEGMENT: &AsciiSet = &NON_ALPHANUMERIC
    .remove(b'-')
    .remove(b'_')
    .remove(b'.')
    .remove(b'~');

/// Whether `rev` is a full 40-character commit id.
pub fn is_pinned(rev: &str) -> bool {
    rev.len() == 40 && rev.bytes().all(|b| b.is_ascii_hexdigit())
}

impl HubSource {
    fn headers(&self) -> Vec<(&'static str, String)> {
        let mut h = vec![("Accept", "application/json".to_string())];
        if let Some(t) = &self.token {
            h.push(("Authorization", format!("Bearer {t}")));
        }
        h
    }

    fn base(&self) -> &str {
        self.endpoint.trim_end_matches('/')
    }

    /// The URL of one file of a revision.
    pub fn file_url(&self, repo: &str, revision: &str, path: &str) -> String {
        let path: Vec<String> = path
            .split('/')
            .map(|s| utf8_percent_encode(s, PATH_SEGMENT).to_string())
            .collect();
        format!(
            "{}/{}/resolve/{}/{}",
            self.base(),
            repo,
            utf8_percent_encode(revision, PATH_SEGMENT),
            path.join("/")
        )
    }

    /// List `repo` at `revision` and plan the download of the files `select` accepts.
    ///
    /// The revision must be a full commit id unless `allow_unpinned` is set, in which
    /// case a branch or tag is resolved to the commit the Hub reports, and the plan names
    /// that commit. Every planned file has a digest: SHA-256 for LFS files, the Git blob
    /// id for the others. A listing without either for a selected file is refused.
    pub fn plan(
        &self,
        client: &dyn HttpClient,
        repo: &str,
        revision: &str,
        allow_unpinned: bool,
        select: &dyn Fn(&str) -> bool,
    ) -> Result<FetchPlan> {
        crate::check_repo(repo)?;
        if !is_pinned(revision) && !allow_unpinned {
            return Err(Error::Unpinned(revision.to_string()));
        }
        let url = format!(
            "{}/api/models/{}/revision/{}?blobs=true",
            self.base(),
            repo,
            utf8_percent_encode(revision, PATH_SEGMENT)
        );
        let headers = self.headers();
        let hdrs: Vec<(&str, &str)> = headers.iter().map(|(k, v)| (*k, v.as_str())).collect();
        let resp = client
            .get(&url, &hdrs)
            .map_err(|m| Error::Http(format!("GET {url}: {m}")))?;
        if resp.status != 200 {
            return Err(Error::Status {
                url,
                status: resp.status,
            });
        }
        let mut body = Vec::new();
        resp.body
            .take(64 << 20)
            .read_to_end(&mut body)
            .map_err(|e| Error::Http(format!("GET {url}: {e}")))?;
        let listing: Listing = serde_json::from_slice(&body)
            .map_err(|e| Error::Invalid(format!("the Hub's listing of {repo}: {e}")))?;
        let sha = listing
            .sha
            .ok_or_else(|| Error::Invalid(format!("the Hub's listing of {repo} has no commit")))?;
        if !is_pinned(&sha) {
            return Err(Error::Invalid(format!(
                "the Hub's listing of {repo} names commit {sha:?}"
            )));
        }
        if is_pinned(revision) && !sha.eq_ignore_ascii_case(revision) {
            return Err(Error::Invalid(format!(
                "asked for {revision} of {repo}, the Hub listed {sha}"
            )));
        }
        let sha = sha.to_ascii_lowercase();
        let mut files = Vec::new();
        for s in listing.siblings {
            if !select(&s.rfilename) {
                continue;
            }
            crate::check_rel_path(&s.rfilename)?;
            let (digest, size) = match (&s.lfs, &s.blob_id) {
                (Some(l), _) => (
                    Digest::sha256_hex(&l.sha256).ok_or_else(|| {
                        Error::Invalid(format!("bad LFS hash for {}", s.rfilename))
                    })?,
                    l.size,
                ),
                (None, Some(b)) => (
                    Digest::git_blob_hex(b).ok_or_else(|| {
                        Error::Invalid(format!("bad blob id for {}", s.rfilename))
                    })?,
                    s.size.ok_or_else(|| {
                        Error::Invalid(format!("no size for {} in the listing", s.rfilename))
                    })?,
                ),
                (None, None) => {
                    return Err(Error::Invalid(format!(
                        "the Hub's listing gives no hash for {}",
                        s.rfilename
                    )));
                }
            };
            files.push(RemoteFile {
                url: self.file_url(repo, &sha, &s.rfilename),
                path: s.rfilename,
                size,
                digest,
            });
        }
        files.sort_by(|a, b| a.path.cmp(&b.path));
        Ok(FetchPlan {
            id: SnapshotId {
                repo: repo.to_string(),
                revision: sha,
            },
            files,
            headers: self
                .token
                .as_ref()
                .map(|t| vec![("Authorization".to_string(), format!("Bearer {t}"))])
                .unwrap_or_default(),
        })
    }
}
