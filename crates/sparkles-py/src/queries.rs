//! Named, versioned, parameterized SPARQL queries.
use crate::{admin, errors::EngineResult, handles::handle};
use pyo3::prelude::*;
use pyo3::types::PyDict;
handle!(PyStoredQueries, "StoredQueries");
#[pymethods]
impl PyStoredQueries {
    fn list<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        let ds = self.ds(py)?;
        admin::to_py(py, &py.detach(|| ds.queries().list()))
    }
    #[pyo3(signature=(name,*,version=None))]
    fn get<'py>(
        &self,
        py: Python<'py>,
        name: &str,
        version: Option<u64>,
    ) -> PyResult<Bound<'py, PyAny>> {
        let ds = self.ds(py)?;
        admin::to_py(py, &py.detach(|| ds.queries().get(name, version)))
    }
    fn versions<'py>(&self, py: Python<'py>, name: &str) -> PyResult<Bound<'py, PyAny>> {
        let ds = self.ds(py)?;
        admin::to_py(py, &py.detach(|| ds.queries().versions(name)))
    }
    #[pyo3(signature=(name,query,*,parameters=None,description=None,results=None,mcp=true,author=None,message=None,if_version=None))]
    #[allow(clippy::too_many_arguments)]
    fn put<'py>(
        &self,
        py: Python<'py>,
        name: &str,
        query: String,
        parameters: Option<&Bound<'py, PyAny>>,
        description: Option<String>,
        results: Option<String>,
        mcp: bool,
        author: Option<String>,
        message: Option<String>,
        if_version: Option<u64>,
    ) -> PyResult<Bound<'py, PyAny>> {
        let ds = self.write(py)?;
        let def = sparkles::stored::Definition {
            query,
            parameters: parameters
                .map(|p| admin::from_py(p, "query parameters"))
                .transpose()?
                .unwrap_or_default(),
            description,
            results,
            mcp,
        };
        let change = sparkles::stored::Change {
            author,
            message,
            if_version,
            dataset_commit: Some(ds.head_commit().seq),
        };
        let r = py.detach(|| ds.queries().put(name, def, change)).py(py)?;
        admin::to_py(
            py,
            &serde_json::json!({"stored":r.stored,"created":r.created,"changed":r.changed}),
        )
    }
    #[pyo3(signature=(name,*,if_version=None))]
    fn delete(&self, py: Python<'_>, name: &str, if_version: Option<u64>) -> PyResult<bool> {
        let ds = self.write(py)?;
        py.detach(|| ds.queries().delete(name, if_version)).py(py)
    }
    #[pyo3(signature=(name,params=None,*,version=None,progress=None,**query_kwargs))]
    fn run<'py>(
        &self,
        py: Python<'py>,
        name: &str,
        params: Option<&Bound<'py, PyAny>>,
        version: Option<u64>,
        progress: Option<&Bound<'py, PyAny>>,
        query_kwargs: Option<&Bound<'py, PyDict>>,
    ) -> PyResult<Bound<'py, PyAny>> {
        let ds = self.ds(py)?;
        let params = params
            .map(|p| admin::from_py(p, "query parameter values"))
            .transpose()?
            .unwrap_or_default();
        let (stored, bindings) = py
            .detach(|| ds.queries().bind(name, version, &params))
            .py(py)?;
        let kwargs = match query_kwargs {
            Some(d) => d.copy()?,
            None => PyDict::new(py),
        };
        let values = match kwargs.get_item("bindings")? {
            Some(d) if !d.is_none() => d.cast::<PyDict>()?.copy()?,
            _ => PyDict::new(py),
        };
        for (name, value) in bindings {
            values.set_item(name, crate::terms::term_to_py(py, &value)?)?;
        }
        kwargs.set_item("bindings", values)?;
        let token = kwargs.get_item("cancel")?;
        let timeout = kwargs
            .get_item("timeout")?
            .filter(|t| !t.is_none())
            .map(|t| t.extract::<f64>())
            .transpose()?;
        let owner = self.owner.clone_ref(py);
        let kwargs = kwargs.unbind();
        let result =
            crate::interrupt::controlled(py, token.as_ref(), progress, timeout, move |ctl| {
                ctl.progress.report(0.0, "running stored query");
                ctl.check()?;
                Ok(Python::attach(|py| -> PyResult<Py<PyAny>> {
                    let kwargs = kwargs.bind(py);
                    kwargs.set_item(
                        "cancel",
                        Py::new(
                            py,
                            crate::interrupt::PyCancelToken {
                                flag: ctl.cancel.flag(),
                            },
                        )?,
                    )?;
                    owner
                        .bind(py)
                        .call_method("query", (stored.definition.query,), Some(kwargs))
                        .map(Bound::unbind)
                }))
            })??;
        Ok(result.into_bound(py))
    }
}
pub fn register(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_class::<PyStoredQueries>()?;
    Ok(())
}
