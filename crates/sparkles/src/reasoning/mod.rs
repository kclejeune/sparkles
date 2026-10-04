//! The reasoning state of a dataset: the record of its last materialization
//! (`reasoning.json`) and RDFS on read ([`rdfs`]).
//!
//! The record says which profile and inputs produced the inferences, at which commit,
//! and how the run went. The server's freshness check and the incremental runs read it,
//! and its serde form is that of `reasoning.json`, which older versions also embedded in
//! the server's registry (`config.json`).

pub mod rdfs;

use crate::error::Result;
use serde::{Deserialize, Serialize};
use std::path::Path;

/// The file of a persistent dataset that holds its [`ReasoningRecord`].
pub const RECORD_FILE: &str = "reasoning.json";

/// The named graph that holds the materialized inferences. While a dataset has a
/// [`ReasoningRecord`], its queries read this graph as part of the default graph (see
/// [`Dataset::query_options`](crate::Dataset::query_options)).
pub const INFERRED_GRAPH: &str = "urn:x-sparkles:inferred";

/// The recorded reasoning status (`reasoning.json`, also embedded in the server's
/// registry). Fields after `at` are absent from files written by older versions.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReasoningRecord {
    #[serde(default)]
    pub reasoning_format: u32,
    pub profile: String,
    pub inferred: u64,
    pub at: String,
    /// commit (`seq`) at which the inferences were materialized
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub commit: Option<u64>,
    /// what `commit` counts: `"commit"` (the commit sequence)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub position_source: Option<String>,
    /// the dataset `commit` belongs to
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dataset_id: Option<String>,
    /// rule text of profile `rules`, for re-runs
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rules: Option<String>,
    /// built-in vocabularies added to the profile (`geosparql`)
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub vocabularies: Vec<String>,
    /// `geo:hasDefaultGeometry` materialized for features with one geometry
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub geo_default_geometry: bool,
    #[serde(default)]
    pub warnings: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub millis: Option<u64>,
    /// copied from a clone source whose inferences were already stale
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub inherited_stale: bool,
    /// this dataset's automatic re-runs; `None` follows the server's `--auto-reason`
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auto: Option<AutoSetting>,
    /// how the last run materialized
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run: Option<RunInfo>,
    /// the input graphs as the request configured them (`dataGraphs`, `ontologyGraphs`,
    /// `imports`, `locationMapping`); absent: the default graph and its imports
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub inputs: Option<serde_json::Value>,
    /// the graphs the run read (`default` or IRIs); absent: the default graph alone
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input_graphs: Option<Vec<String>>,
    /// the graphs whose changes make the inferences stale: those read, and those the
    /// imports that did not resolve name; absent: the default graph alone
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub watched_graphs: Option<Vec<String>>,
    /// the imports the run found, resolved or not
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub imports: Vec<serde_json::Value>,
    /// the imports that runs fetched into the dataset, by IRI
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub fetched_imports: Vec<String>,
}

/// How a materialization ran: in full or incrementally, and what it changed.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RunInfo {
    /// `full` or `incremental`
    pub method: String,
    /// why a run that could have updated the previous materialization ran in full
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fallback: Option<String>,
    /// triples added to and removed from the inferred graph
    pub inferred_added: u64,
    pub inferred_removed: u64,
    /// incremental runs: what changed since the previous run
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub changes: Option<RunChanges>,
}

/// What an incremental run found changed and did.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RunChanges {
    /// default graph triples added and removed since the previous run
    pub explicit_added: u64,
    pub explicit_removed: u64,
    /// derived triples whose other proofs were searched for
    pub checked: u64,
    /// derived triples that no longer follow, and new ones (generalized ones included)
    pub removed: u64,
    pub derived: u64,
    /// `memory` (the closure cache) or `store` (read from the dataset)
    pub source: String,
}

/// A dataset's own automatic re-run setting (`PUT /$/reason/{ds}/auto`).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AutoSetting {
    pub enabled: bool,
    /// seconds without a commit before a run; the server's, else 5
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub debounce_seconds: Option<f64>,
    /// seconds after which a run starts even while writes continue; 12 × the debounce
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_delay_seconds: Option<f64>,
}

/// The largest closure a dataset keeps in memory for incremental reasoning, in triples,
/// by default (`serve --reason-cache-triples`).
pub const DEFAULT_CLOSURE_CACHE_TRIPLES: usize = 10_000_000;

/// The record kept in a database directory, if it has one that reads. The record lives
/// in the directory, so it survives restarts however the database is opened.
pub fn read_record(root: &Path) -> Option<ReasoningRecord> {
    serde_json::from_slice(&std::fs::read(root.join(RECORD_FILE)).ok()?).ok()
}

/// Write (or with `None` remove) the record of a database directory durably: a
/// temporary file is synced and renamed over the old one, and the directory is synced,
/// so a crash leaves the old record or the new one, never a torn file.
pub fn write_record(root: &Path, record: Option<&ReasoningRecord>) -> Result<()> {
    let path = root.join(RECORD_FILE);
    match record {
        Some(r) => {
            let mut r = r.clone();
            r.reasoning_format = 2;
            let bytes = serde_json::to_vec_pretty(&r)
                .map_err(|e| crate::Error::invalid(format!("reasoning record: {e}")))?;
            crate::guard::config::write_atomic(&path, &bytes)
        }
        None => match std::fs::remove_file(&path) {
            Ok(()) => sync_dir(root),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(e.into()),
        },
    }
}

/// Flush a directory's entries to stable storage (a no-op where directories cannot be
/// opened for syncing).
pub(crate) fn sync_dir(dir: &Path) -> Result<()> {
    #[cfg(unix)]
    std::fs::File::open(dir)?.sync_all()?;
    #[cfg(not(unix))]
    let _ = dir;
    Ok(())
}
