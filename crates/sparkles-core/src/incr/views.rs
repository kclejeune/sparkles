//! View definitions (spec F13 §2.1): `<root>/views/{name}/view.json`, with the state the
//! runtime reports for each view.

use super::ops::PlanNode;
use crate::error::{Error, Result};
pub use crate::store::definitions::Paused;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// The directory of a dataset's views.
pub const DIR: &str = "views";
/// A view's definition file.
pub const FILE: &str = "view.json";
/// The format of `view.json` this release writes for a view that runs.
pub const FORMAT: u32 = 1;
/// The format of a paused `view.json`: format 1 with a `paused` member, written for a
/// restored or cloned view until it is resumed. Releases that read only format 1 refuse
/// it, so they never run a view that waits for its checks.
pub const PAUSED_FORMAT: u32 = 2;
/// The default budget of a view's state: 256 MiB.
pub const DEFAULT_MEMORY_BYTES: u64 = 256 << 20;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ViewMode {
    #[default]
    Table,
    Graph,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Provenance {
    #[default]
    None,
    Count,
    Why,
    Lineage,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Maintenance {
    #[default]
    Async,
    Commit,
}

fn main_branch() -> Vec<String> {
    vec!["main".into()]
}

fn default_memory() -> u64 {
    DEFAULT_MEMORY_BYTES
}

/// A view's definition, as `PUT /$/views/{ds}/{name}` takes it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ViewDef {
    /// a SELECT or CONSTRUCT query (or `storedQuery`)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub query: Option<String>,
    /// a stored query (C16) by name, in place of `query`
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stored_query: Option<String>,
    /// where the result lives: `table` (Phase 1) or `graph` (Phase 2)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mode: Option<ViewMode>,
    /// the named graph of a `graph` view
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub graph: Option<String>,
    /// whose view of the dataset the query reads (the definer, when absent)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub principal: Option<String>,
    #[serde(default)]
    pub provenance: Provenance,
    #[serde(default)]
    pub maintenance: Maintenance,
    #[serde(default = "main_branch")]
    pub branches: Vec<String>,
    #[serde(default = "default_memory")]
    pub memory_bytes: u64,
    #[serde(default)]
    pub keep_commits: u64,
}

impl Default for ViewDef {
    fn default() -> ViewDef {
        ViewDef {
            query: None,
            stored_query: None,
            mode: None,
            graph: None,
            principal: None,
            provenance: Provenance::None,
            maintenance: Maintenance::Async,
            branches: main_branch(),
            memory_bytes: DEFAULT_MEMORY_BYTES,
            keep_commits: 0,
        }
    }
}

impl ViewDef {
    /// A `table` view of `query`.
    pub fn table(query: impl Into<String>) -> ViewDef {
        ViewDef {
            query: Some(query.into()),
            ..Default::default()
        }
    }

    /// Refuse what Phase 1 does not do, with the reason (`400 unsupported-option`).
    pub fn check(&self) -> Result<()> {
        let no = |what: &str| -> Result<()> {
            Err(super::error(
                "unsupported-option",
                format!("{what} is not supported yet (spec F13 Phase 2 or 3)"),
            ))
        };
        match (&self.query, &self.stored_query) {
            (None, None) => {
                return Err(super::error(
                    "invalid-view",
                    "a view needs a query or a storedQuery",
                ));
            }
            (Some(_), Some(_)) => {
                return Err(super::error(
                    "invalid-view",
                    "a view has a query or a storedQuery, not both",
                ));
            }
            _ => {}
        }
        if self.mode == Some(ViewMode::Graph) || self.graph.is_some() {
            return no("a graph view");
        }
        if self.provenance != Provenance::None {
            return no("provenance");
        }
        if self.maintenance != Maintenance::Async {
            return no("maintenance at commit");
        }
        if self.branches != main_branch() {
            return no("a view on branches other than main");
        }
        if self.keep_commits != 0 {
            return no("keepCommits");
        }
        if self.memory_bytes == 0 {
            return Err(super::error("invalid-view", "memoryBytes must be positive"));
        }
        Ok(())
    }
}

/// What a view is doing.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(tag = "state", rename_all = "kebab-case")]
pub enum ViewState {
    /// evaluated from scratch
    Building,
    /// at the head commit
    Current,
    /// maintained, this many commits behind the head
    Behind {
        commits: u64,
    },
    /// not maintained until a rebuild (`over-budget`, `pass-timeout`)
    Suspended {
        reason: String,
    },
    Failed {
        error: String,
    },
}

/// A view as `GET /$/views/{ds}` lists it.
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ViewInfo {
    pub name: String,
    pub definition: ViewDef,
    #[serde(flatten)]
    pub state: ViewState,
    /// the commit the rows reflect
    #[serde(skip_serializing_if = "Option::is_none")]
    pub commit: Option<u64>,
    /// the result's columns
    pub columns: Vec<String>,
    /// the number of rows, multiplicities counted
    pub rows: u64,
    /// the approximate memory of the view's state
    pub state_bytes: u64,
    /// passes run, and passes the footprint skipped
    pub passes: u64,
    pub skipped: u64,
    /// time spent in passes and in `recompute` nodes, in milliseconds
    pub pass_ms: f64,
    pub recompute_ms: f64,
    /// `recompute` when every relevant commit evaluates the whole query again, as on a
    /// dataset with RDFS on read or materialized inferences, `incremental` otherwise
    /// (absent while the view has no circuit)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub evaluation: Option<super::ops::Mode>,
    /// why the whole query is evaluated again
    #[serde(skip_serializing_if = "Option::is_none")]
    pub evaluation_reason: Option<String>,
    /// the plan with each operator's mode
    #[serde(skip_serializing_if = "Option::is_none")]
    pub plan: Option<PlanNode>,
}

/// The directory of view `name` under the dataset root.
pub fn dir(root: &Path, name: &str) -> PathBuf {
    root.join(DIR).join(name)
}

/// Whether `name` can name a view: letters, digits, `_` and `-`, starting with a letter
/// or digit, at most 64 characters (the rule of stored queries).
pub fn valid_name(name: &str) -> bool {
    crate::stored::valid_name(name)
}

/// Read the definitions under `root`, by name, each with its pause when it was
/// restored or cloned and not resumed. A newer format refuses the whole set with
/// [`Error::Unsupported`], so it is never overwritten.
pub fn read_all(root: &Path) -> Result<Vec<(String, ViewDef, Option<Paused>)>> {
    let base = root.join(DIR);
    let entries = match std::fs::read_dir(&base) {
        Ok(e) => e,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(e.into()),
    };
    let mut out = Vec::new();
    for e in entries {
        let e = e?;
        let Ok(name) = e.file_name().into_string() else {
            continue;
        };
        if !valid_name(&name) {
            continue;
        }
        let path = e.path().join(FILE);
        let bytes = match std::fs::read(&path) {
            Ok(b) => b,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
            Err(e) => return Err(e.into()),
        };
        let (def, paused) = parse(&format!("views/{name}/{FILE}"), &bytes)?;
        out.push((name, def, paused));
    }
    out.sort_by(|a, b| a.0.cmp(&b.0));
    Ok(out)
}

/// Parse the `view.json` at `path` (for messages): its version, its schema and, in
/// format 2, its pause. The definition is not checked against what this release
/// supports ([`ViewDef::check`]).
pub fn parse(path: &str, bytes: &[u8]) -> Result<(ViewDef, Option<Paused>)> {
    let mut v: serde_json::Value =
        serde_json::from_slice(bytes).map_err(|e| Error::Corrupt(format!("{path}: {e}")))?;
    let format = crate::formats::check_settings(FILE, &v).map_err(|e| match e {
        Error::Unsupported(m) => Error::Unsupported(format!("{path}: {m}")),
        Error::Corrupt(m) => Error::Corrupt(format!("{path}: {m}")),
        e => e,
    })?;
    let Some(o) = v.as_object_mut() else {
        return Err(Error::Corrupt(format!("{path}: not a JSON object")));
    };
    o.remove("format");
    let paused = match (format, o.remove("paused")) {
        (PAUSED_FORMAT, Some(p)) => Some(
            serde_json::from_value::<Paused>(p)
                .map_err(|e| Error::Corrupt(format!("{path}: paused: {e}")))?,
        ),
        (PAUSED_FORMAT, None) => {
            return Err(Error::Corrupt(format!("{path}: format 2 without `paused`")));
        }
        (_, Some(_)) => {
            return Err(Error::Corrupt(format!("{path}: `paused` needs format 2")));
        }
        (_, None) => None,
    };
    let def: ViewDef =
        serde_json::from_value(v).map_err(|e| Error::Corrupt(format!("{path}: {e}")))?;
    Ok((def, paused))
}

/// The bytes of `def` as `view.json`, paused when `paused` is given.
pub fn to_bytes(def: &ViewDef, paused: Option<&Paused>) -> Result<Vec<u8>> {
    let mut v = serde_json::to_value(def).map_err(|e| Error::Invalid(format!("{FILE}: {e}")))?;
    let format = match (paused, v.as_object_mut()) {
        (Some(p), Some(o)) => {
            o.insert(
                "paused".into(),
                serde_json::to_value(p).map_err(|e| Error::Invalid(e.to_string()))?,
            );
            PAUSED_FORMAT
        }
        _ => FORMAT,
    };
    crate::formats::settings_to_vec_at(FILE, &v, format)
}

/// Write a definition atomically.
pub fn write(root: &Path, name: &str, def: &ViewDef) -> Result<()> {
    let d = dir(root, name);
    std::fs::create_dir_all(&d)?;
    let bytes = to_bytes(def, None)?;
    crate::store::write_atomic(&d.join(FILE), &bytes)
}

/// Remove a view's definition and state.
pub fn remove(root: &Path, name: &str) -> Result<()> {
    match std::fs::remove_dir_all(dir(root, name)) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e.into()),
    }
}
