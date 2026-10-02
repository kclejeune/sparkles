//! `validation.json`: the per-dataset configuration of write-time validation, shared by
//! the SHACL and ShEx guards. The store reads only `mode` (whether a guard is required);
//! [`config_language`] tells the server and the CLI which validator installs the guard.
//!
//! Format 1 is SHACL (`sparkles_shacl::guard::ValidationConfig`). Format 2 adds
//! `language` (`"shacl"` or `"shex"`) and is what both guards write; a file without
//! `language` is SHACL, so format 1 files keep working unchanged.

use super::{GuardLanguage, GuardStatus, SeverityCounts, Strategy, ValidationSummary};
use crate::error::{Error, Result};
use crate::id::Id;
use crate::sparql::ctx::{DEFAULT_GRAPH_IRI, UNION_GRAPH_IRI};
use crate::store::Snapshot;
use serde::{Deserialize, Serialize};
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};

/// The configuration file in a database directory.
pub const CONFIG_FILE: &str = "validation.json";
/// A SHACL shapes file copied into the database directory.
pub const SHACL_SHAPES_FILE: &str = "validation-shapes.ttl";
/// A ShEx schema copied into the database directory, in ShExC.
pub const SHEX_SCHEMA_SHEXC_FILE: &str = "validation-schema.shex";
/// A ShEx schema copied into the database directory, in ShExJ.
pub const SHEX_SCHEMA_SHEXJ_FILE: &str = "validation-schema.json";
/// Every file of a database's write-time validation: kept by clones and backups, and
/// removed when validation is turned off.
pub const FILES: [&str; 4] = [
    CONFIG_FILE,
    SHACL_SHAPES_FILE,
    SHEX_SCHEMA_SHEXC_FILE,
    SHEX_SCHEMA_SHEXJ_FILE,
];
/// The validation state of the last validated commit (see [`StatusFile`]). Not one of
/// [`FILES`]: a copy of the database starts with an unknown state.
pub const STATUS_FILE: &str = "validation-status.json";
/// The reasoner's graph of materialized inferences.
pub const INFERRED_GRAPH: &str = "urn:x-sparkles:inferred";

/// The language of `<root>/validation.json`: `None` without a file, SHACL when the file
/// has no `language` (format 1). A file that is not JSON, or names another language, is
/// an error (the store keeps refusing writes until it is fixed).
pub fn config_language(root: &Path) -> Result<Option<GuardLanguage>> {
    let bytes = match std::fs::read(root.join(CONFIG_FILE)) {
        Ok(b) => b,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e.into()),
    };
    let j: serde_json::Value = serde_json::from_slice(&bytes)
        .map_err(|e| Error::Invalid(format!("{CONFIG_FILE}: {e}")))?;
    match j.get("language") {
        None => Ok(Some(GuardLanguage::Shacl)),
        Some(l) => serde_json::from_value(l.clone()).map(Some).map_err(|_| {
            Error::Invalid(format!(
                "{CONFIG_FILE}: unknown language {l} (\"shacl\" or \"shex\")"
            ))
        }),
    }
}

/// Remove the validation files of a database directory, except `keep` (missing files
/// are fine).
pub fn remove_files(root: &Path, keep: &[&str]) -> Result<()> {
    for f in FILES {
        if keep.contains(&f) {
            continue;
        }
        match std::fs::remove_file(root.join(f)) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => return Err(e.into()),
            _ => {}
        }
    }
    Ok(())
}

/// Replace a file durably (write and sync a temporary file, rename it, sync the
/// directory).
pub fn write_atomic(path: &Path, bytes: &[u8]) -> Result<()> {
    crate::store::write_atomic(path, bytes)
}

/// The SHA-256 of `b` in lowercase hex (`sha256` of a copied shapes or schema file).
pub fn sha256_hex(b: &[u8]) -> String {
    use sha2::Digest;
    sha2::Sha256::digest(b)
        .iter()
        .map(|x| format!("{x:02x}"))
        .collect()
}

/// The time now, for `updated`.
pub fn now_rfc3339() -> String {
    crate::commit::rfc3339_ms(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_millis() as i64),
    )
}

/// The data graph: `"default"`, `"union"`, or a list of graph IRIs.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum DataGraphSel {
    Named(String),
    Graphs(Vec<String>),
}

impl Default for DataGraphSel {
    fn default() -> Self {
        DataGraphSel::Named("default".into())
    }
}

/// The graphs a validation reads, in the form of the validators' options.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct DataGraphs {
    /// `None`: the store's default graph
    pub data_graph: Option<String>,
    pub extra_graphs: Vec<String>,
    pub exclude_graphs: Vec<String>,
}

impl DataGraphSel {
    /// Check the value (not the graphs' existence).
    pub fn check(&self) -> Result<()> {
        match self {
            DataGraphSel::Named(n) if n != "default" && n != "union" => Err(Error::Invalid(
                "dataGraph must be \"default\", \"union\" or a list of graph IRIs".into(),
            )),
            DataGraphSel::Graphs(gs) => {
                for g in gs {
                    oxrdf::NamedNode::new(g.as_str())
                        .map_err(|e| Error::Invalid(format!("dataGraph <{g}>: {e}")))?;
                }
                Ok(())
            }
            _ => Ok(()),
        }
    }

    /// The graphs to validate over `snap`, with the inferred graph merged in or left
    /// out, and `exclude` never read; `None` when the selection is a list of graphs none
    /// of which exists (nothing to validate).
    pub fn graphs(
        &self,
        snap: &Snapshot,
        include_inferences: bool,
        exclude: &[String],
    ) -> Option<DataGraphs> {
        let mut o = DataGraphs {
            exclude_graphs: exclude.to_vec(),
            ..Default::default()
        };
        if include_inferences {
            o.extra_graphs.push(INFERRED_GRAPH.into());
        } else {
            o.exclude_graphs.push(INFERRED_GRAPH.into());
        }
        match self {
            DataGraphSel::Named(n) if n == "union" => o.data_graph = Some(UNION_GRAPH_IRI.into()),
            DataGraphSel::Named(_) => {}
            DataGraphSel::Graphs(gs) => {
                let exists = |g: &str| g == DEFAULT_GRAPH_IRI || snap.lookup_iri(g).is_some();
                let mut present = gs.iter().filter(|g| exists(g));
                o.data_graph = Some(present.next()?.clone());
                o.extra_graphs.extend(present.cloned());
            }
        }
        Some(o)
    }

    /// Whether a change to graph id `g` can change the data graph (the inferred graph
    /// counts when `include_inferences`; graphs in `exclude` never do).
    pub fn touches(
        &self,
        snap: &Snapshot,
        g: u64,
        include_inferences: bool,
        exclude: &[String],
    ) -> bool {
        let id = |iri: &str| {
            if iri == DEFAULT_GRAPH_IRI {
                Some(Id::DEFAULT_GRAPH.0)
            } else {
                snap.lookup_iri(iri).map(|i| i.0)
            }
        };
        if exclude.iter().any(|x| id(x) == Some(g)) {
            return false;
        }
        if id(INFERRED_GRAPH) == Some(g) {
            return include_inferences;
        }
        match self {
            DataGraphSel::Named(n) if n == "union" => true,
            DataGraphSel::Named(_) => snap.union_default_graph || g == Id::DEFAULT_GRAPH.0,
            DataGraphSel::Graphs(gs) => gs.iter().any(|x| id(x) == Some(g)),
        }
    }
}

/// The validation state of the last committed head.
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Baseline {
    pub commit: u64,
    /// no blocking results (`None`: unknown, e.g. after a bypassed write)
    pub conforms: Option<bool>,
    pub blocking: u64,
    pub total: u64,
    /// the results by severity
    pub by_severity: SeverityCounts,
    pub millis: u64,
}

/// `validation-status.json`: the validation state of a commit, written after the commit
/// so a restart need not validate the head in full. It is trusted only for the head it
/// names and the configuration whose `validation.json` hashes to `configHash`, so a
/// crash before it is written leaves the state unknown, never wrong.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StatusFile {
    pub format: u32,
    pub commit: u64,
    pub config_hash: String,
    pub blocking: u64,
    pub total: u64,
    pub violations: u64,
    pub warnings: u64,
    pub infos: u64,
    pub millis: u64,
}

impl StatusFile {
    /// The state of `b` under the configuration hashing to `config_hash`.
    pub fn of(b: &Baseline, config_hash: &str) -> StatusFile {
        StatusFile {
            format: 1,
            commit: b.commit,
            config_hash: config_hash.to_string(),
            blocking: b.blocking,
            total: b.total,
            violations: b.by_severity.violation,
            warnings: b.by_severity.warning,
            infos: b.by_severity.info,
            millis: b.millis,
        }
    }

    /// Write it to `root` (replacing the file by a rename; not synced, because a stale
    /// or missing file only makes the state unknown).
    pub fn write(&self, root: &Path) -> Result<()> {
        let tmp = root.join(format!("{STATUS_FILE}.tmp"));
        std::fs::write(
            &tmp,
            serde_json::to_vec(self).map_err(std::io::Error::other)?,
        )?;
        std::fs::rename(&tmp, root.join(STATUS_FILE))?;
        Ok(())
    }

    /// The baseline it records, when it is the state of commit `head` under the
    /// configuration hashing to `config_hash`.
    pub fn read(root: &Path, head: u64, config_hash: &str) -> Option<Baseline> {
        let bytes = std::fs::read(root.join(STATUS_FILE)).ok()?;
        let f: StatusFile = serde_json::from_slice(&bytes).ok()?;
        (f.format == 1 && f.commit == head && f.config_hash == config_hash).then_some(Baseline {
            commit: f.commit,
            conforms: Some(f.blocking == 0),
            blocking: f.blocking,
            total: f.total,
            by_severity: SeverityCounts {
                violation: f.violations,
                warning: f.warnings,
                info: f.infos,
            },
            millis: f.millis,
        })
    }

    /// Remove the file of `root` (a missing file is fine).
    pub fn remove(root: &Path) -> Result<()> {
        match std::fs::remove_file(root.join(STATUS_FILE)) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e.into()),
            _ => Ok(()),
        }
    }
}

/// One validated write, as the status shows it.
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CheckRecord {
    /// when the validation finished (RFC 3339)
    pub time: String,
    /// the commit kind (`update`, `load`, …)
    pub kind: &'static str,
    pub status: GuardStatus,
    pub strategy: Strategy,
    pub blocking: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub introduced: Option<u64>,
    pub total: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub focus_nodes: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fallback: Option<String>,
    pub millis: u64,
    /// the first (highest ranked) result
    #[serde(skip_serializing_if = "Option::is_none")]
    pub first: Option<serde_json::Value>,
}

/// The last validated write and the last rejected ones, for the status.
#[derive(Debug, Default)]
pub struct CheckHistory {
    last: parking_lot::Mutex<Option<CheckRecord>>,
    rejected: parking_lot::Mutex<std::collections::VecDeque<CheckRecord>>,
}

/// Rejected writes the status keeps.
const REJECTED_KEPT: usize = 10;

impl CheckHistory {
    /// Record a validation (skipped and bypassed writes are not recorded).
    pub fn record(&self, kind: crate::commit::CommitKind, s: &ValidationSummary) {
        if matches!(s.status, GuardStatus::Skipped | GuardStatus::Bypassed) {
            return;
        }
        let r = CheckRecord {
            time: now_rfc3339(),
            kind: kind.name(),
            status: s.status,
            strategy: s.strategy,
            blocking: s.blocking,
            introduced: s.introduced,
            total: s.total,
            focus_nodes: s.focus_nodes,
            fallback: s.fallback.clone(),
            millis: s.millis,
            first: s.results.first().cloned(),
        };
        if s.status == GuardStatus::Rejected {
            let mut q = self.rejected.lock();
            if q.len() == REJECTED_KEPT {
                q.pop_back();
            }
            q.push_front(r.clone());
        }
        *self.last.lock() = Some(r);
    }

    /// The last validated write.
    pub fn last(&self) -> Option<CheckRecord> {
        self.last.lock().clone()
    }

    /// The last rejected writes, newest first.
    pub fn rejected(&self) -> Vec<CheckRecord> {
        self.rejected.lock().iter().cloned().collect()
    }
}

/// A guard's decisions since it was installed.
#[derive(Clone, Debug, Default, Serialize)]
pub struct Counters {
    pub passed: u64,
    pub warned: u64,
    pub rejected: u64,
    pub skipped: u64,
    pub bypassed: u64,
}

/// A guard's decision counters, updated as it decides.
#[derive(Debug, Default)]
pub struct DecisionCounts([AtomicU64; 5]);

impl DecisionCounts {
    /// Count one decision.
    pub fn count(&self, s: GuardStatus) {
        let i = match s {
            GuardStatus::Passed => 0,
            GuardStatus::Warned => 1,
            GuardStatus::Rejected => 2,
            GuardStatus::Skipped => 3,
            GuardStatus::Bypassed => 4,
        };
        self.0[i].fetch_add(1, Ordering::Relaxed);
    }

    /// The counts so far.
    pub fn get(&self) -> Counters {
        let c = |i: usize| self.0[i].load(Ordering::Relaxed);
        Counters {
            passed: c(0),
            warned: c(1),
            rejected: c(2),
            skipped: c(3),
            bypassed: c(4),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decision_counts() {
        let c = DecisionCounts::default();
        c.count(GuardStatus::Rejected);
        c.count(GuardStatus::Rejected);
        c.count(GuardStatus::Bypassed);
        let got = c.get();
        assert_eq!((got.passed, got.rejected, got.bypassed), (0, 2, 1));
    }

    #[test]
    fn language_of_a_config() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(config_language(dir.path()).unwrap(), None);
        let write = |s: &str| std::fs::write(dir.path().join(CONFIG_FILE), s).unwrap();
        write(r#"{"format":1,"mode":"reject","shapes":{"graphs":["urn:s"]}}"#);
        assert_eq!(
            config_language(dir.path()).unwrap(),
            Some(GuardLanguage::Shacl)
        );
        write(r#"{"format":2,"language":"shex","mode":"warn"}"#);
        assert_eq!(
            config_language(dir.path()).unwrap(),
            Some(GuardLanguage::Shex)
        );
        write(r#"{"format":2,"language":"owl","mode":"warn"}"#);
        assert!(config_language(dir.path()).is_err());
        write("not json");
        assert!(config_language(dir.path()).is_err());
    }

    #[test]
    fn data_graph_values() {
        assert!(DataGraphSel::default().check().is_ok());
        assert!(DataGraphSel::Named("union".into()).check().is_ok());
        assert!(DataGraphSel::Named("other".into()).check().is_err());
        assert!(
            DataGraphSel::Graphs(vec!["http://ex.org/g".into()])
                .check()
                .is_ok()
        );
        assert!(
            DataGraphSel::Graphs(vec!["not an iri".into()])
                .check()
                .is_err()
        );
    }

    #[test]
    fn removes_only_validation_files() {
        let dir = tempfile::tempdir().unwrap();
        for f in FILES.iter().chain(&["CURRENT"]) {
            std::fs::write(dir.path().join(f), "x").unwrap();
        }
        remove_files(dir.path(), &[CONFIG_FILE]).unwrap();
        assert!(dir.path().join(CONFIG_FILE).exists());
        assert!(dir.path().join("CURRENT").exists());
        assert!(!dir.path().join(SHACL_SHAPES_FILE).exists());
        assert!(!dir.path().join(SHEX_SCHEMA_SHEXJ_FILE).exists());
        remove_files(dir.path(), &[]).unwrap();
        assert!(!dir.path().join(CONFIG_FILE).exists());
    }
}
