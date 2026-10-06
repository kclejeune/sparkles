//! Typed bodies of the Sparkles API and the small value types of the options. Each typed
//! body keeps the members it does not know in `extra`, so a newer server does not break
//! an older client.

use oxrdf::NamedNode;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use std::fmt;

/// One commit of a dataset (`Commit` of the API reference).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Commit {
    pub seq: u64,
    pub parent: Option<u64>,
    /// `commit:42`
    #[serde(rename = "ref")]
    pub reference: String,
    /// RFC 3339; absent from a dry run's preview.
    #[serde(default)]
    pub timestamp: Option<String>,
    /// `update`, `gsp-put`, `upload`, …
    pub kind: String,
    /// Absent when the caller's grants cover only part of the dataset.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub inserted: Option<u64>,
    /// Absent when the caller's grants cover only part of the dataset.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub deleted: Option<u64>,
    /// The dataset's size after the commit; absent for a graph-restricted caller.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub quads: Option<u64>,
    pub generation: String,
    pub bulk: bool,
    /// Absent with the dataset-wide counts for a graph-restricted caller.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exact: Option<bool>,
    #[serde(default)]
    pub message: Option<String>,
    #[serde(default)]
    pub digest: Option<String>,
    #[serde(default)]
    pub unvalidated: Option<bool>,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

/// A page of `GET /$/commits/{ds}`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CommitList {
    pub dataset: String,
    pub dataset_id: String,
    pub head: u64,
    pub first_retained: u64,
    pub complete: bool,
    pub commits: Vec<Commit>,
    /// The URL of the next page.
    pub next: Option<String>,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

/// A background task of `/$/tasks`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Task {
    pub id: String,
    /// `compact`, `backup`, `clone`, …
    pub kind: String,
    /// Empty for a server-wide task.
    pub dataset: String,
    /// `queued`, `running`, `done`, `failed` or `cancelled`.
    pub state: String,
    pub started_at: String,
    #[serde(default)]
    pub finished_at: Option<String>,
    pub cancellable: bool,
    #[serde(default)]
    pub success: Option<bool>,
    #[serde(default)]
    pub message: Option<String>,
    #[serde(default)]
    pub progress: Option<f64>,
    #[serde(default)]
    pub detail: Option<Value>,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

impl Task {
    /// Whether the task has ended, successfully or not.
    pub fn is_finished(&self) -> bool {
        matches!(self.state.as_str(), "done" | "failed" | "cancelled")
    }
}

/// A dataset's description (`DatasetInfo`).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DatasetInfo {
    #[serde(default)]
    pub name: Option<String>,
    /// `persistent` or `mem`.
    #[serde(rename = "type", default)]
    pub kind: Option<String>,
    /// Approximate, base plus delta.
    #[serde(default)]
    pub quads: Option<u64>,
    /// The dataset id (a UUID).
    #[serde(default)]
    pub id: Option<String>,
    /// The head commit.
    #[serde(default)]
    pub head: Option<u64>,
    /// The head commit's time.
    #[serde(default)]
    pub modified: Option<String>,
    /// The caller's level with authentication on: `read`, `write` or `admin`.
    #[serde(default)]
    pub access: Option<String>,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

/// `GET /$/server`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ServerInfo {
    /// Left out for anonymous callers when authentication is on.
    #[serde(default)]
    pub version: Option<String>,
    pub started_at: String,
    pub uptime_seconds: f64,
    pub read_only: bool,
    pub datasets: Vec<DatasetInfo>,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

/// `GET /$/whoami`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Whoami {
    pub auth_enabled: bool,
    pub principal: Principal,
    /// Server permissions: `metrics`, `federate`, `server-admin`.
    pub server: Vec<String>,
    /// The caller's level on each dataset it may use.
    pub datasets: Map<String, Value>,
    #[serde(default)]
    pub token_id: Option<String>,
    #[serde(default)]
    pub expires: Option<String>,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

/// Who a request acts for.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Principal {
    /// `local`, `anonymous`, `user`, `token`, `oidc` or `proxy`.
    pub kind: String,
    #[serde(default)]
    pub name: Option<String>,
    /// Tokens: their owner, such as `oidc:alice@example.org`.
    #[serde(default)]
    pub owner: Option<String>,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

/// `GET /$/commits/{ds}/{ref}`: one commit and its dataset.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct CommitResponse {
    pub commit: Commit,
    pub dataset: String,
    pub dataset_id: String,
}

/// `GET /$/datasets`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub(crate) struct DatasetList {
    pub datasets: Vec<DatasetInfo>,
}

/// The receipt members of a write's body (`Receipt` of the API reference).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ReceiptBody {
    pub dataset: String,
    pub dataset_id: String,
    pub committed: bool,
    pub commit: Commit,
}

/// What a write did. `commit_seq` and `dataset_id` come from the `Sparkles-Commit` and
/// `Sparkles-Dataset-Id` headers; `committed` and `commit` from the receipt body, which a
/// [`Dataset`](crate::Dataset) always asks for. A plain endpoint's write has the status
/// and the body only.
#[derive(Clone, Debug)]
pub struct Receipt {
    pub status: u16,
    /// The commit the write produced, or the unchanged head for a write with no net
    /// effect.
    pub commit_seq: Option<u64>,
    pub dataset_id: Option<String>,
    /// Whether a commit was made: false for a write with no net effect and for a dry run.
    pub committed: Option<bool>,
    pub commit: Option<Commit>,
    /// Whether this was a dry run (`Sparkles-Dry-Run: true`).
    pub dry_run: bool,
    /// The whole JSON body: an update's statistics, a Graph Store write's counts, a dry
    /// run's report; `Null` when the body was empty or not JSON.
    pub body: Value,
}

/// A state of a dataset to read (`at=`): the head, a commit, an instant or a named
/// snapshot.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum At {
    Head,
    Commit(u64),
    /// An RFC 3339 instant; the last commit at or before it.
    Time(String),
    Snapshot(String),
}

impl fmt::Display for At {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            At::Head => f.write_str("head"),
            At::Commit(n) => write!(f, "commit:{n}"),
            At::Time(t) => write!(f, "time:{t}"),
            At::Snapshot(s) => write!(f, "snapshot:{s}"),
        }
    }
}

impl From<u64> for At {
    fn from(n: u64) -> At {
        At::Commit(n)
    }
}

/// A Graph Store target.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Graph {
    /// `?default`
    Default,
    /// `?graph=<iri>`
    Named(NamedNode),
    /// The union of the named graphs (`?graph=union`), for reads on Sparkles and Fuseki.
    Union,
}

impl From<NamedNode> for Graph {
    fn from(n: NamedNode) -> Graph {
        Graph::Named(n)
    }
}

impl From<oxrdf::NamedNodeRef<'_>> for Graph {
    fn from(n: oxrdf::NamedNodeRef<'_>) -> Graph {
        Graph::Named(n.into_owned())
    }
}

impl Graph {
    /// The Graph Store query parameter.
    pub(crate) fn param(&self) -> (&'static str, String) {
        match self {
            Graph::Default => ("default", String::new()),
            Graph::Named(n) => ("graph", n.as_str().to_string()),
            Graph::Union => ("graph", "union".to_string()),
        }
    }
}

/// The storage of a new dataset.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DatasetType {
    Persistent,
    Memory,
}

impl DatasetType {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            DatasetType::Persistent => "persistent",
            DatasetType::Memory => "mem",
        }
    }
}
