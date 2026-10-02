//! Dataset administration from Python: commits, named snapshots, retention and clones,
//! the full-text and vector indexes, and write-time validation. Configurations and
//! statuses that the engine reads and writes as JSON cross over as Python dicts in the
//! same camelCase shape (`text.json`, the vector index configuration, `validation.json`).

use crate::errors::EngineResult;
use pyo3::exceptions::PyValueError;
use pyo3::prelude::*;
use pyo3::sync::PyOnceLock;
use pyo3::types::{PyDict, PyList};
use serde::Serialize;
use serde::de::DeserializeOwned;
use sparkles::commit::{CommitInfo, CommitRange};
use sparkles::history::{At, NamedSnapshot, Retention};
use std::path::PathBuf;
#[cfg(any(feature = "shacl", feature = "shex"))]
use std::sync::Arc;

static JSON: PyOnceLock<(Py<PyAny>, Py<PyAny>)> = PyOnceLock::new();

/// `json.dumps` and `json.loads`.
fn json(py: Python<'_>) -> PyResult<(&Bound<'_, PyAny>, &Bound<'_, PyAny>)> {
    let (dumps, loads) = JSON.get_or_try_init(py, || -> PyResult<_> {
        let m = py.import("json")?;
        Ok((m.getattr("dumps")?.unbind(), m.getattr("loads")?.unbind()))
    })?;
    Ok((dumps.bind(py), loads.bind(py)))
}

/// A Python value (dicts, lists, str, numbers) as an engine configuration.
pub fn from_py<T: DeserializeOwned>(ob: &Bound<'_, PyAny>, what: &str) -> PyResult<T> {
    let (dumps, _) = json(ob.py())?;
    let text: String = dumps.call1((ob,))?.extract()?;
    serde_json::from_str(&text).map_err(|e| PyValueError::new_err(format!("invalid {what}: {e}")))
}

/// An engine value as Python dicts and lists.
pub fn to_py<'py>(py: Python<'py>, v: &impl Serialize) -> PyResult<Bound<'py, PyAny>> {
    let text = serde_json::to_string(v)
        .map_err(|e| pyo3::exceptions::PyRuntimeError::new_err(e.to_string()))?;
    let (_, loads) = json(py)?;
    loads.call1((text,))
}

// ------------------------------------------------------------------- history ----

/// One commit of a dataset: the state after a write that changed data.
#[pyclass(frozen, module = "sparkles", name = "Commit", skip_from_py_object)]
pub struct PyCommit {
    /// the commit number (0 is the empty dataset)
    #[pyo3(get)]
    seq: u64,
    /// milliseconds since the Unix epoch
    #[pyo3(get)]
    timestamp_ms: i64,
    /// what made it: `load`, `update`, `transaction`, …
    #[pyo3(get)]
    kind: &'static str,
    /// quads added relative to the previous commit
    #[pyo3(get)]
    inserted: u64,
    /// quads removed relative to the previous commit
    #[pyo3(get)]
    deleted: u64,
    /// quads in the dataset after the commit
    #[pyo3(get)]
    quads: u64,
}

impl From<CommitInfo> for PyCommit {
    fn from(c: CommitInfo) -> Self {
        PyCommit {
            seq: c.seq,
            timestamp_ms: c.timestamp_ms,
            kind: c.kind.name(),
            inserted: c.inserted,
            deleted: c.deleted,
            quads: c.quads,
        }
    }
}

#[pymethods]
impl PyCommit {
    /// The time of the commit in RFC 3339, in UTC.
    #[getter]
    fn timestamp(&self) -> String {
        sparkles::commit::rfc3339_ms(self.timestamp_ms)
    }

    fn __repr__(&self) -> String {
        format!(
            "<Commit {} {} +{} -{} quads={} at {}>",
            self.seq,
            self.kind,
            self.inserted,
            self.deleted,
            self.quads,
            self.timestamp()
        )
    }
}

/// A named snapshot: a commit kept readable under a name (`at="snapshot:<name>"`).
#[pyclass(frozen, module = "sparkles", name = "Snapshot", skip_from_py_object)]
pub struct PySnapshot {
    #[pyo3(get)]
    name: String,
    /// the pinned commit
    #[pyo3(get)]
    seq: u64,
    /// milliseconds since the Unix epoch
    #[pyo3(get)]
    created_ms: i64,
    #[pyo3(get)]
    note: Option<String>,
    /// when the snapshot lapses (milliseconds since the Unix epoch)
    #[pyo3(get)]
    expires_ms: Option<i64>,
}

impl From<NamedSnapshot> for PySnapshot {
    fn from(s: NamedSnapshot) -> Self {
        PySnapshot {
            name: s.name,
            seq: s.seq,
            created_ms: s.created_ms,
            note: s.note,
            expires_ms: s.expires_ms,
        }
    }
}

#[pymethods]
impl PySnapshot {
    fn __repr__(&self) -> String {
        format!("<Snapshot {:?} commit={}>", self.name, self.seq)
    }
}

pub fn head_commit(ds: &sparkles::Dataset) -> PyCommit {
    ds.head_commit().into()
}

pub fn commits(
    py: Python<'_>,
    ds: &sparkles::Dataset,
    limit: usize,
    before: Option<u64>,
    after: Option<u64>,
) -> PyResult<Vec<PyCommit>> {
    let range = match (before, after) {
        (Some(_), Some(_)) => {
            return Err(PyValueError::new_err("give before or after, not both"));
        }
        (Some(b), None) => CommitRange::Before(b),
        (None, Some(a)) => CommitRange::After(a),
        (None, None) => CommitRange::Latest,
    };
    let page = py.detach(|| ds.commits(range, limit));
    Ok(page.commits.into_iter().map(PyCommit::from).collect())
}

pub fn create_snapshot(
    py: Python<'_>,
    ds: &sparkles::Dataset,
    name: &str,
    at: At,
    note: Option<String>,
    expires_ms: Option<i64>,
) -> PyResult<PySnapshot> {
    let (snap, _) = py
        .detach(|| ds.store().create_snapshot_with(name, &at, note, expires_ms))
        .py(py)?;
    Ok(snap.into())
}

pub fn snapshots(ds: &sparkles::Dataset) -> Vec<PySnapshot> {
    ds.store()
        .snapshots()
        .into_iter()
        .map(PySnapshot::from)
        .collect()
}

pub fn delete_snapshot(py: Python<'_>, ds: &sparkles::Dataset, name: &str) -> PyResult<bool> {
    py.detach(|| ds.store().delete_snapshot(name)).py(py)
}

/// What the dataset's history holds, as a dict.
pub fn history<'py>(py: Python<'py>, ds: &sparkles::Dataset) -> PyResult<Bound<'py, PyDict>> {
    let h = py.detach(|| ds.store().history());
    let d = PyDict::new(py);
    d.set_item("head", h.head)?;
    d.set_item("reconstructable", PyList::new(py, h.reconstructable)?)?;
    d.set_item("bytes", h.bytes)?;
    d.set_item("retention", to_py(py, &h.retention)?)?;
    d.set_item("snapshots", h.snapshots)?;
    d.set_item("first_commit", h.first_commit)?;
    Ok(d)
}

pub fn set_retention<'py>(
    py: Python<'py>,
    ds: &sparkles::Dataset,
    keep_commits: Option<u64>,
    keep_age: Option<f64>,
    max_bytes: Option<u64>,
) -> PyResult<Bound<'py, PyDict>> {
    let r = Retention {
        keep_commits,
        keep_age_ms: keep_age.map(|s| (s * 1000.0).round() as u64),
        max_bytes,
    };
    py.detach(|| ds.store().set_retention(r)).py(py)?;
    history(py, ds)
}

pub fn clone_to<'py>(
    py: Python<'py>,
    ds: &sparkles::Dataset,
    directory: PathBuf,
    at: Option<At>,
    exclude_graphs: Vec<oxrdf::NamedNode>,
) -> PyResult<Bound<'py, PyDict>> {
    let opts = sparkles::store::CloneOptions {
        exclude_graphs,
        at,
        ..Default::default()
    };
    let r = py
        .detach(|| ds.store().clone_to(&directory, &opts))
        .py(py)?;
    let d = PyDict::new(py);
    d.set_item("path", directory.display().to_string())?;
    d.set_item("dataset_id", r.dataset_id.to_string())?;
    d.set_item("commit", r.forked_from.seq)?;
    d.set_item("source_quads", r.source_quads)?;
    d.set_item("quads", r.quads)?;
    d.set_item("graphs", r.graphs)?;
    d.set_item("millis", r.millis)?;
    Ok(d)
}

// --------------------------------------------------------------- text, vector ----

#[cfg(feature = "text")]
pub fn enable_text<'py>(
    py: Python<'py>,
    ds: &sparkles::Dataset,
    config: Option<&Bound<'py, PyAny>>,
) -> PyResult<Bound<'py, PyAny>> {
    let cfg: sparkles::text::TextConfig = match config.filter(|c| !c.is_none()) {
        Some(c) => from_py(c, "text index configuration")?,
        None => Default::default(),
    };
    let status = py.detach(|| ds.store().enable_text(cfg)).py(py)?;
    to_py(py, &status)
}

#[cfg(feature = "text")]
pub fn rebuild_text<'py>(py: Python<'py>, ds: &sparkles::Dataset) -> PyResult<Bound<'py, PyAny>> {
    let status = py.detach(|| ds.store().rebuild_text()).py(py)?;
    to_py(py, &status)
}

#[cfg(feature = "text")]
pub fn disable_text(py: Python<'_>, ds: &sparkles::Dataset) -> PyResult<()> {
    py.detach(|| ds.store().disable_text()).py(py)
}

#[cfg(feature = "text")]
pub fn text_status<'py>(
    py: Python<'py>,
    ds: &sparkles::Dataset,
) -> PyResult<Option<Bound<'py, PyAny>>> {
    ds.store().text_status().map(|s| to_py(py, &s)).transpose()
}

pub fn create_vector_index(
    py: Python<'_>,
    ds: &sparkles::Dataset,
    name: &str,
    predicate: oxrdf::NamedNode,
    dimension: usize,
    options: Option<&Bound<'_, PyAny>>,
) -> PyResult<bool> {
    let mut cfg = serde_json::to_value(sparkles::vector::config::VectorIndexConfig::new(
        predicate.as_str(),
        dimension,
    ))
    .map_err(|e| pyo3::exceptions::PyRuntimeError::new_err(e.to_string()))?;
    if let Some(o) = options.filter(|o| !o.is_none()) {
        let extra: serde_json::Map<String, serde_json::Value> = from_py(o, "vector index options")?;
        if let serde_json::Value::Object(m) = &mut cfg {
            m.extend(extra);
        }
    }
    let cfg: sparkles::vector::config::VectorIndexConfig = serde_json::from_value(cfg)
        .map_err(|e| PyValueError::new_err(format!("invalid vector index options: {e}")))?;
    py.detach(|| ds.store().create_vector_index(name, cfg))
        .py(py)
}

pub fn vector_index<'py>(
    py: Python<'py>,
    ds: &sparkles::Dataset,
    name: &str,
    wait: bool,
) -> PyResult<Option<Bound<'py, PyAny>>> {
    let s = if wait {
        py.detach(|| ds.store().wait_vector_index(name))
    } else {
        ds.store().vector_index(name)
    };
    s.map(|s| to_py(py, &s)).transpose()
}

pub fn vector_indexes<'py>(py: Python<'py>, ds: &sparkles::Dataset) -> PyResult<Bound<'py, PyAny>> {
    to_py(py, &ds.store().vector_indexes())
}

// ---------------------------------------------------------- write validation ----

/// The guard this binding installed, to report its status.
pub enum Guard {
    #[cfg(feature = "shacl")]
    Shacl(Arc<sparkles_shacl::guard::ShaclGuard>),
    #[cfg(feature = "shex")]
    Shex(Arc<sparkles_shex::guard::ShexGuard>),
    /// in a build without SHACL and ShEx, there is none
    #[cfg(not(any(feature = "shacl", feature = "shex")))]
    #[allow(dead_code)]
    None(std::convert::Infallible),
}

/// Install the write-time validation a persistent dataset's `validation.json` sets
/// up, as the server does when it opens one. Without the feature for its language, the
/// store keeps refusing writes.
pub fn install(store: &sparkles::store::Store) -> anyhow::Result<Option<Guard>> {
    #[cfg(feature = "shacl")]
    if let Some(g) = sparkles_shacl::guard::install(store)? {
        return Ok(Some(Guard::Shacl(g)));
    }
    #[cfg(feature = "shex")]
    if let Some(g) = sparkles_shex::guard::install(store)? {
        return Ok(Some(Guard::Shex(g)));
    }
    let _ = store;
    Ok(None)
}

/// Set, replace or (with `None` or mode `off`) remove the write-time validation.
/// Returns `{"status": "installed" | "not-conforming" | "removed", "summary": …}` and
/// the guard to keep.
#[allow(unused_variables)]
pub fn set_write_validation<'py>(
    py: Python<'py>,
    ds: &sparkles::Dataset,
    config: Option<&Bound<'py, PyAny>>,
) -> PyResult<(Bound<'py, PyDict>, Option<Option<Guard>>)> {
    let config = config.filter(|c| !c.is_none());
    let language = match config {
        Some(c) => c
            .get_item("language")
            .ok()
            .and_then(|l| l.extract::<String>().ok())
            .unwrap_or_else(|| "shacl".into()),
        None => "shacl".into(),
    };
    let d = PyDict::new(py);
    let store = ds.store();
    match language.as_str() {
        "shex" => {
            #[cfg(feature = "shex")]
            {
                use sparkles_shex::guard::{SetOutcome, ShexValidationConfig, set_config};
                let cfg: Option<ShexValidationConfig> = config
                    .map(|c| from_py(c, "write validation configuration"))
                    .transpose()?;
                let resolver = sparkles_shex::FileResolver::default();
                let out = py
                    .detach(|| set_config(store, cfg, &resolver))
                    .map_err(|e| crate::errors::anyhow(py, e))?;
                Ok(match out {
                    SetOutcome::Installed(g, s) => {
                        d.set_item("status", "installed")?;
                        d.set_item("summary", to_py(py, &s)?)?;
                        (d, Some(Some(Guard::Shex(g))))
                    }
                    SetOutcome::NotConforming(s) => {
                        d.set_item("status", "not-conforming")?;
                        d.set_item("summary", to_py(py, &s)?)?;
                        (d, None)
                    }
                    SetOutcome::Removed => {
                        d.set_item("status", "removed")?;
                        (d, Some(None))
                    }
                })
            }
            #[cfg(not(feature = "shex"))]
            {
                Err(crate::errors::missing_feature(py, "shex"))
            }
        }
        "shacl" => {
            #[cfg(feature = "shacl")]
            {
                use sparkles_shacl::guard::{SetOutcome, ValidationConfig, set_config};
                let cfg: Option<ValidationConfig> = config
                    .map(|c| from_py(c, "write validation configuration"))
                    .transpose()?;
                let out = py
                    .detach(|| set_config(store, cfg))
                    .map_err(|e| crate::errors::anyhow(py, e))?;
                Ok(match out {
                    SetOutcome::Installed(g, s) => {
                        d.set_item("status", "installed")?;
                        d.set_item("summary", to_py(py, &s)?)?;
                        (d, Some(Some(Guard::Shacl(g))))
                    }
                    SetOutcome::NotConforming(s) => {
                        d.set_item("status", "not-conforming")?;
                        d.set_item("summary", to_py(py, &s)?)?;
                        (d, None)
                    }
                    SetOutcome::Removed => {
                        d.set_item("status", "removed")?;
                        (d, Some(None))
                    }
                })
            }
            #[cfg(not(feature = "shacl"))]
            {
                Err(crate::errors::missing_feature(py, "shacl"))
            }
        }
        other => Err(PyValueError::new_err(format!(
            "unknown write validation language {other:?}: use shacl or shex"
        ))),
    }
}

/// The installed guard's configuration and status.
#[cfg_attr(
    not(any(feature = "shacl", feature = "shex")),
    allow(unreachable_code, unused_variables)
)]
pub fn guard_status<'py>(py: Python<'py>, g: &Guard) -> PyResult<Bound<'py, PyDict>> {
    let d = PyDict::new(py);
    match g {
        #[cfg(feature = "shacl")]
        Guard::Shacl(g) => {
            d.set_item("language", "shacl")?;
            d.set_item("config", to_py(py, g.config())?)?;
            d.set_item("status", to_py(py, &g.status())?)?;
        }
        #[cfg(feature = "shex")]
        Guard::Shex(g) => {
            d.set_item("language", "shex")?;
            d.set_item("config", to_py(py, g.config())?)?;
            d.set_item("status", to_py(py, &g.status())?)?;
        }
        #[cfg(not(any(feature = "shacl", feature = "shex")))]
        Guard::None(n) => match *n {},
    }
    Ok(d)
}

pub fn register(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_class::<PyCommit>()?;
    m.add_class::<PySnapshot>()?;
    Ok(())
}
