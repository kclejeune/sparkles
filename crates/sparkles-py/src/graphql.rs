//! GraphQL mapping configurations and execution.
#![cfg_attr(not(feature = "graphql"), allow(unused_variables, unused_imports))]
use crate::{
    admin,
    errors::{EngineResult, invalid},
    handles::handle,
    interrupt,
};
use pyo3::prelude::*;
handle!(PyGraphQl, "GraphQL");
handle!(PyGraphQlConfig, "GraphQLConfig");
#[pymethods]
impl PyGraphQl {
    #[getter]
    fn config(&self, py: Python<'_>) -> PyGraphQlConfig {
        PyGraphQlConfig {
            owner: self.owner.clone_ref(py),
        }
    }
    fn versions<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        let ds = self.ds(py)?;
        #[cfg(feature = "graphql")]
        {
            admin::to_py(py, &py.detach(|| ds.graphql().versions()))
        }
        #[cfg(not(feature = "graphql"))]
        {
            Err(crate::errors::new_err(
                py,
                "UnsupportedError",
                "graphql feature is disabled",
            ))
        }
    }
    fn sdl(&self, py: Python<'_>) -> PyResult<Option<String>> {
        let ds = self.ds(py)?;
        #[cfg(feature = "graphql")]
        {
            py.detach(|| ds.graphql().sdl()).py(py)
        }
        #[cfg(not(feature = "graphql"))]
        {
            Err(crate::errors::new_err(
                py,
                "UnsupportedError",
                "graphql feature is disabled",
            ))
        }
    }
    #[pyo3(signature=(query,variables=None,operation_name=None,*,at=None,cancel=None,progress=None,timeout=None))]
    #[allow(clippy::too_many_arguments)]
    fn execute<'py>(
        &self,
        py: Python<'py>,
        query: String,
        variables: Option<&Bound<'py, PyAny>>,
        operation_name: Option<String>,
        at: Option<&Bound<'py, PyAny>>,
        cancel: Option<&Bound<'py, PyAny>>,
        progress: Option<&Bound<'py, PyAny>>,
        timeout: Option<f64>,
    ) -> PyResult<Bound<'py, PyAny>> {
        let ds = self.ds(py)?;
        #[cfg(feature = "graphql")]
        {
            let variables: serde_json::Map<String, serde_json::Value> = variables
                .map(|v| admin::from_py(v, "GraphQL variables"))
                .transpose()?
                .unwrap_or_default();
            let req = sparkles::handles::graphql::Request {
                query,
                variables,
                operation_name,
            };
            let at = at.map(crate::dataset::at_from_py).transpose()?;
            let r = interrupt::controlled(py, cancel, progress, timeout, move |ctl| {
                let opts = sparkles::handles::graphql::Options {
                    at,
                    query: sparkles::sparql::QueryOptions {
                        cancel: Some(ctl.cancel.flag()),
                        timeout: ctl
                            .deadline
                            .map(|d| d.saturating_duration_since(std::time::Instant::now())),
                        ..Default::default()
                    },
                    ..Default::default()
                };
                let r = ds.graphql().execute(&req, &opts)?;
                ctl.check()?;
                ctl.progress.report(1.0, "executed GraphQL");
                Ok(r)
            })?;
            admin::to_py(py, &r.body)
        }
        #[cfg(not(feature = "graphql"))]
        {
            Err(crate::errors::new_err(
                py,
                "UnsupportedError",
                "graphql feature is disabled",
            ))
        }
    }
    #[pyo3(signature=(*,source=None,shapes_graph=None,graph="default",support=1.0,classes=None,min_instances=1,include_inferred=false,cancel=None,progress=None,timeout=600.0))]
    #[allow(clippy::too_many_arguments)]
    fn draft<'py>(
        &self,
        py: Python<'py>,
        source: Option<String>,
        shapes_graph: Option<String>,
        graph: &str,
        support: f64,
        classes: Option<Vec<String>>,
        min_instances: u64,
        include_inferred: bool,
        cancel: Option<&Bound<'py, PyAny>>,
        progress: Option<&Bound<'py, PyAny>>,
        timeout: f64,
    ) -> PyResult<Bound<'py, PyAny>> {
        let ds = self.ds(py)?;
        #[cfg(feature = "graphql")]
        {
            let graph =
                sparkles::schema::GraphSelection::parse(graph).map_err(|e| invalid(py, e))?;
            let r = interrupt::controlled(py, cancel, progress, Some(timeout), move |ctl| {
                let req = sparkles::handles::graphql::DraftRequest {
                    source,
                    shapes_graph,
                    graph,
                    support,
                    classes: classes.unwrap_or_default(),
                    min_instances,
                    reasoning: include_inferred,
                    timeout: ctl
                        .deadline
                        .unwrap()
                        .saturating_duration_since(std::time::Instant::now()),
                    ..Default::default()
                };
                let (r, commit) = ds.graphql().draft(ds.name().unwrap_or("dataset"), req)?;
                ctl.check()?;
                ctl.progress.report(1.0, "drafted GraphQL schema");
                Ok(
                    serde_json::json!({"source":r.source,"sdl":r.sdl,"types":r.types,"skipped":r.skipped,"commit":commit}),
                )
            })?;
            admin::to_py(py, &r)
        }
        #[cfg(not(feature = "graphql"))]
        {
            Err(crate::errors::new_err(
                py,
                "UnsupportedError",
                "graphql feature is disabled",
            ))
        }
    }
}
#[pymethods]
impl PyGraphQlConfig {
    #[pyo3(signature=(*,version=None))]
    fn get<'py>(&self, py: Python<'py>, version: Option<u64>) -> PyResult<Bound<'py, PyAny>> {
        let ds = self.ds(py)?;
        #[cfg(feature = "graphql")]
        {
            admin::to_py(py, &py.detach(|| ds.graphql().get(version)))
        }
        #[cfg(not(feature = "graphql"))]
        {
            Err(crate::errors::new_err(
                py,
                "UnsupportedError",
                "graphql feature is disabled",
            ))
        }
    }
    #[pyo3(signature=(config,*,author=None,message=None,if_version=None))]
    fn set<'py>(
        &self,
        py: Python<'py>,
        config: &Bound<'py, PyAny>,
        author: Option<String>,
        message: Option<String>,
        if_version: Option<u64>,
    ) -> PyResult<Bound<'py, PyAny>> {
        let ds = self.write(py)?;
        #[cfg(feature = "graphql")]
        {
            let cfg = admin::from_py(config, "GraphQL configuration")?;
            let change = sparkles::handles::graphql::Change {
                author,
                message,
                if_version,
                dataset_commit: Some(ds.head_commit().seq),
            };
            let (r, warnings) = py.detach(|| ds.graphql().put(cfg, change)).py(py)?;
            admin::to_py(
                py,
                &serde_json::json!({"stored":r.stored,"created":r.created,"changed":r.changed,"warnings":warnings}),
            )
        }
        #[cfg(not(feature = "graphql"))]
        {
            Err(crate::errors::new_err(
                py,
                "UnsupportedError",
                "graphql feature is disabled",
            ))
        }
    }
    #[pyo3(signature=(*,if_version=None))]
    fn reset(&self, py: Python<'_>, if_version: Option<u64>) -> PyResult<bool> {
        let ds = self.write(py)?;
        #[cfg(feature = "graphql")]
        {
            py.detach(|| ds.graphql().reset(if_version)).py(py)
        }
        #[cfg(not(feature = "graphql"))]
        {
            Err(crate::errors::new_err(
                py,
                "UnsupportedError",
                "graphql feature is disabled",
            ))
        }
    }
}
pub fn register(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_class::<PyGraphQl>()?;
    m.add_class::<PyGraphQlConfig>()?;
    Ok(())
}
