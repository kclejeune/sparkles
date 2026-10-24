//! Incremental views and live queries (spec F13).
use crate::{admin, errors::EngineResult, handles::handle};
use pyo3::prelude::*;
use pyo3::types::{PyDict, PyList, PyTuple};
use sparkles::incr::{LiveEvent, LiveQuery, LiveRow, ViewDef};
use std::sync::Arc;
use std::time::Duration;

handle!(PyViews, "Views");

#[pymethods]
impl PyViews {
    fn list<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        let ds = self.ds(py)?;
        admin::to_py(py, &py.detach(|| ds.views().list()))
    }
    fn get<'py>(&self, py: Python<'py>, name: &str) -> PyResult<Bound<'py, PyAny>> {
        let ds = self.ds(py)?;
        admin::to_py(py, &py.detach(|| ds.views().get(name)))
    }
    /// Define or replace a view from `query`, or from a definition mapping with
    /// `definition`.
    #[pyo3(signature=(name, query=None, *, definition=None, principal=None, wait=None))]
    fn put<'py>(
        &self,
        py: Python<'py>,
        name: &str,
        query: Option<String>,
        definition: Option<&Bound<'py, PyAny>>,
        principal: Option<String>,
        wait: Option<f64>,
    ) -> PyResult<Bound<'py, PyAny>> {
        let ds = self.write(py)?;
        let mut def: ViewDef = match (query, definition) {
            (Some(q), None) => ViewDef::table(q),
            (None, Some(d)) => admin::from_py(d, "view definition")?,
            _ => {
                return Err(pyo3::exceptions::PyValueError::new_err(
                    "give the query or a definition, not both",
                ));
            }
        };
        if principal.is_some() {
            def.principal = principal;
        }
        let info = py
            .detach(|| {
                let v = ds.views().put(name, def)?;
                match wait {
                    Some(s) => ds
                        .views()
                        .wait_built(name, Duration::from_secs_f64(s.max(0.0))),
                    None => Ok(v),
                }
            })
            .py(py)?;
        admin::to_py(py, &info)
    }
    fn delete(&self, py: Python<'_>, name: &str) -> PyResult<bool> {
        let ds = self.write(py)?;
        py.detach(|| ds.views().delete(name)).py(py)
    }
    fn rebuild(&self, py: Python<'_>, name: &str) -> PyResult<()> {
        let ds = self.write(py)?;
        py.detach(|| ds.views().rebuild(name)).py(py)
    }
    #[pyo3(signature=(name, timeout=60.0))]
    fn wait_built<'py>(
        &self,
        py: Python<'py>,
        name: &str,
        timeout: f64,
    ) -> PyResult<Bound<'py, PyAny>> {
        let ds = self.ds(py)?;
        let v = py
            .detach(|| {
                ds.views()
                    .wait_built(name, Duration::from_secs_f64(timeout.max(0.0)))
            })
            .py(py)?;
        admin::to_py(py, &v)
    }
    /// The rows of a view as `{"columns", "commit", "rows"}`, each row a tuple of terms
    /// (`None` for unbound) repeated by its multiplicity.
    fn read<'py>(&self, py: Python<'py>, name: &str) -> PyResult<Bound<'py, PyDict>> {
        let ds = self.ds(py)?;
        let r = py.detach(|| ds.views().read(name)).py(py)?;
        let out = PyDict::new(py);
        out.set_item("columns", &r.columns)?;
        out.set_item("commit", r.commit)?;
        let rows = PyList::empty(py);
        for (terms, n) in &r.rows {
            let t = terms_tuple(py, terms)?;
            for _ in 0..(*n).max(0) {
                rows.append(&t)?;
            }
        }
        out.set_item("rows", rows)?;
        Ok(out)
    }
    fn explain<'py>(&self, py: Python<'py>, query: &str) -> PyResult<Bound<'py, PyAny>> {
        let ds = self.ds(py)?;
        let (kind, plan) = py.detach(|| ds.views().explain(query)).py(py)?;
        admin::to_py(py, &serde_json::json!({ "kind": kind, "plan": plan }))
    }
}

fn terms_tuple<'py>(
    py: Python<'py>,
    terms: &[Option<oxrdf::Term>],
) -> PyResult<Bound<'py, PyTuple>> {
    let items: Vec<Bound<'py, PyAny>> = terms
        .iter()
        .map(|t| match t {
            Some(t) => crate::terms::term_to_py(py, t),
            None => Ok(py.None().into_bound(py)),
        })
        .collect::<PyResult<_>>()?;
    PyTuple::new(py, items)
}

fn rows_py<'py>(py: Python<'py>, rows: &[LiveRow]) -> PyResult<Bound<'py, PyList>> {
    let out = PyList::empty(py);
    for r in rows {
        out.append((terms_tuple(py, &r.terms)?, r.count))?;
    }
    Ok(out)
}

/// An event as a dict: `event` is `snapshot`, `reset`, `diff` or `error`.
fn event_py<'py>(py: Python<'py>, e: &LiveEvent) -> PyResult<Bound<'py, PyDict>> {
    let d = PyDict::new(py);
    match e {
        LiveEvent::Snapshot {
            commit,
            vars,
            rows,
            value,
            reset,
        } => {
            d.set_item("event", if *reset { "reset" } else { "snapshot" })?;
            d.set_item("commit", commit)?;
            d.set_item("vars", vars.to_vec())?;
            match value {
                Some(v) => d.set_item("value", v)?,
                None => d.set_item("rows", rows_py(py, rows)?)?,
            }
        }
        LiveEvent::Diff {
            commit,
            from,
            removed,
            added,
        } => {
            d.set_item("event", "diff")?;
            d.set_item("commit", commit)?;
            d.set_item("from", from)?;
            d.set_item("removed", rows_py(py, removed)?)?;
            d.set_item("added", rows_py(py, added)?)?;
        }
        LiveEvent::Value {
            commit,
            from,
            value,
        } => {
            d.set_item("event", "diff")?;
            d.set_item("commit", commit)?;
            d.set_item("from", from)?;
            d.set_item("value", value)?;
        }
        LiveEvent::Error { code, message } => {
            d.set_item("event", "error")?;
            d.set_item("code", code)?;
            d.set_item("message", message)?;
        }
    }
    Ok(d)
}

/// A live query: iterate it for its events. Each row is a tuple of terms with its
/// count. Iteration waits for the next event and ends with the subscription.
#[pyclass(frozen, module = "sparkles", name = "LiveQuery")]
pub struct PyLiveQuery {
    pub(crate) live: std::sync::Mutex<Option<Arc<LiveQuery>>>,
}

impl PyLiveQuery {
    fn get(&self) -> Option<Arc<LiveQuery>> {
        self.live
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }
}

#[pymethods]
impl PyLiveQuery {
    #[getter]
    fn vars(&self) -> Vec<String> {
        self.get().map(|l| l.vars().to_vec()).unwrap_or_default()
    }
    fn __iter__(slf: PyRef<'_, Self>) -> PyRef<'_, Self> {
        slf
    }
    fn __next__<'py>(&self, py: Python<'py>) -> PyResult<Option<Bound<'py, PyDict>>> {
        let Some(live) = self.get() else {
            return Ok(None);
        };
        loop {
            // wait in slices, so that Ctrl-C reaches Python
            let e = py.detach(|| live.next_timeout(Duration::from_millis(200)));
            match e {
                Some(e) => return Ok(Some(event_py(py, &e)?)),
                None if live.is_closed() => return Ok(None),
                None => py.check_signals()?,
            }
        }
    }
    /// The next event, waiting at most `timeout` seconds; `None` when none arrived.
    #[pyo3(signature=(timeout=None))]
    fn next<'py>(
        &self,
        py: Python<'py>,
        timeout: Option<f64>,
    ) -> PyResult<Option<Bound<'py, PyDict>>> {
        let Some(live) = self.get() else {
            return Ok(None);
        };
        let e = match timeout {
            Some(t) => py.detach(|| live.next_timeout(Duration::from_secs_f64(t.max(0.0)))),
            None => return self.__next__(py),
        };
        e.map(|e| event_py(py, &e)).transpose()
    }
    /// End the subscription.
    fn close(&self) {
        self.live
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take();
    }
    fn __enter__(slf: PyRef<'_, Self>) -> PyRef<'_, Self> {
        slf
    }
    fn __exit__(
        &self,
        _t: Option<&Bound<'_, PyAny>>,
        _v: Option<&Bound<'_, PyAny>>,
        _tb: Option<&Bound<'_, PyAny>>,
    ) {
        self.close();
    }
}

pub fn register(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_class::<PyViews>()?;
    m.add_class::<PyLiveQuery>()?;
    Ok(())
}
