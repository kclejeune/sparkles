//! Dataset administration from Python: commits, named snapshots, retention and clones,
//! the full-text and vector indexes, and write-time validation. Configurations and
//! statuses that the engine reads and writes as JSON cross over as Python dicts in the
//! same camelCase shape (`text.json`, the vector index configuration, `validation.json`).

use pyo3::exceptions::PyValueError;
use pyo3::prelude::*;
use pyo3::sync::PyOnceLock;
use serde::Serialize;
use serde::de::DeserializeOwned;
use sparkles::commit::{CommitInfo, CommitRange};
use sparkles::history::NamedSnapshot;

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
    #[pyo3(get)]
    message: Option<String>,
    #[pyo3(get)]
    digest: Option<String>,
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
            message: None,
            digest: None,
        }
    }
}

impl PyCommit {
    pub fn annotated(mut self, annotation: Option<&sparkles::annotations::Annotation>) -> Self {
        if let Some(a) = annotation {
            self.message = a.message.as_ref().map(ToString::to_string);
            self.digest = a.digest_hex();
        }
        self
    }
}
impl From<sparkles::handles::CommitDetail> for PyCommit {
    fn from(c: sparkles::handles::CommitDetail) -> Self {
        PyCommit::from(c.commit).annotated(c.annotation.as_ref())
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
    let c = ds.head_commit();
    PyCommit::from(c).annotated(ds.history().annotation(c.seq).as_ref())
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
    Ok(page
        .commits
        .into_iter()
        .map(|c| PyCommit::from(c).annotated(ds.history().annotation(c.seq).as_ref()))
        .collect())
}

pub fn register(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_class::<PyCommit>()?;
    m.add_class::<PySnapshot>()?;
    Ok(())
}
