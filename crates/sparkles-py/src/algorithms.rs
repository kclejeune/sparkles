//! Graph algorithms: runs of PageRank, communities, similarity and embeddings, and the
//! dataset's named projections.
use crate::{admin, errors::EngineResult, handles::handle};
use pyo3::prelude::*;
use pyo3::types::PyDict;
use sparkles::sparql::QueryOptions;
use sparkles::sparql::algorun::AlgoRequest;

handle!(PyAlgorithms, "Algorithms");
handle!(PyProjections, "Projections");

fn parse_request(request: &Bound<'_, PyAny>) -> PyResult<AlgoRequest> {
    let v: serde_json::Value = admin::from_py(request, "algorithm request")?;
    AlgoRequest::from_json(v).map_err(|e| pyo3::exceptions::PyValueError::new_err(e.to_string()))
}

#[pymethods]
impl PyAlgorithms {
    /// The named projections.
    #[getter]
    fn projections(&self, py: Python<'_>) -> PyProjections {
        PyProjections {
            owner: self.owner.clone_ref(py),
        }
    }

    /// The SPARQL query that a request without `write` stands for.
    #[pyo3(signature=(request,*,summary=false))]
    fn query_text(
        &self,
        py: Python<'_>,
        request: &Bound<'_, PyAny>,
        summary: bool,
    ) -> PyResult<String> {
        let ds = self.ds(py)?;
        let req = parse_request(request)?;
        py.detach(|| ds.algorithms().query_text(&req, summary))
            .py(py)
    }

    /// Run a request without `write` and return the result of its query. The keyword
    /// arguments are those of `Dataset.query`.
    #[pyo3(signature=(request,**query_kwargs))]
    fn run<'py>(
        &self,
        py: Python<'py>,
        request: &Bound<'py, PyAny>,
        query_kwargs: Option<&Bound<'py, PyDict>>,
    ) -> PyResult<Bound<'py, PyAny>> {
        let text = self.query_text(py, request, false)?;
        self.owner
            .bind(py)
            .call_method("query", (text,), query_kwargs)
    }

    /// The statistics of a run as a dict (nodes, edges, iterations, …).
    fn stats<'py>(
        &self,
        py: Python<'py>,
        request: &Bound<'py, PyAny>,
    ) -> PyResult<Bound<'py, PyAny>> {
        let ds = self.ds(py)?;
        let req = parse_request(request)?;
        let text = py
            .detach(|| ds.algorithms().stats(&req, &QueryOptions::default()))
            .py(py)?;
        let v: serde_json::Value = serde_json::from_str(&text)
            .map_err(|e| pyo3::exceptions::PyRuntimeError::new_err(e.to_string()))?;
        admin::to_py(py, &v)
    }

    /// Run a request with `write` and write its results to the graph in batched commits.
    /// Returns the run's report.
    #[pyo3(signature=(request,*,author=None,cancel=None,progress=None,timeout=None))]
    fn run_write<'py>(
        &self,
        py: Python<'py>,
        request: &Bound<'py, PyAny>,
        author: Option<String>,
        cancel: Option<&Bound<'py, PyAny>>,
        progress: Option<&Bound<'py, PyAny>>,
        timeout: Option<f64>,
    ) -> PyResult<Bound<'py, PyAny>> {
        let ds = self.write(py)?;
        let req = parse_request(request)?;
        let report = crate::interrupt::controlled(py, cancel, progress, timeout, move |ctl| {
            ds.algorithms()
                .run_write(&req, &QueryOptions::default(), author.map(Into::into), &ctl)
        })?;
        admin::to_py(py, &report)
    }
}

#[pymethods]
impl PyProjections {
    fn list<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        let ds = self.ds(py)?;
        let list: Vec<_> = py
            .detach(|| ds.algorithms().projections().list())
            .into_iter()
            .map(|(name, s)| serde_json::json!({"name": name, "stored": s}))
            .collect();
        admin::to_py(py, &list)
    }

    fn get<'py>(&self, py: Python<'py>, name: &str) -> PyResult<Bound<'py, PyAny>> {
        let ds = self.ds(py)?;
        admin::to_py(py, &py.detach(|| ds.algorithms().projections().get(name)))
    }

    #[pyo3(signature=(name,definition,*,author=None,if_revision=None))]
    fn put<'py>(
        &self,
        py: Python<'py>,
        name: &str,
        definition: &Bound<'py, PyAny>,
        author: Option<String>,
        if_revision: Option<u64>,
    ) -> PyResult<Bound<'py, PyAny>> {
        let ds = self.write(py)?;
        let v: serde_json::Value = admin::from_py(definition, "projection definition")?;
        let def = sparkles::algo::catalog::ProjectionDef::from_json(v)
            .map_err(|e| pyo3::exceptions::PyValueError::new_err(e.to_string()))?;
        let (stored, created) = py
            .detach(|| {
                ds.algorithms()
                    .projections()
                    .put(name, def, author, if_revision)
            })
            .py(py)?;
        admin::to_py(
            py,
            &serde_json::json!({"stored": stored, "created": created}),
        )
    }

    #[pyo3(signature=(name,*,if_revision=None))]
    fn delete(&self, py: Python<'_>, name: &str, if_revision: Option<u64>) -> PyResult<bool> {
        let ds = self.write(py)?;
        py.detach(|| ds.algorithms().projections().delete(name, if_revision))
            .py(py)
    }
}

pub fn register(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_class::<PyAlgorithms>()?;
    m.add_class::<PyProjections>()?;
    Ok(())
}
