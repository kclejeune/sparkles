//! Materialized reasoning and RDFS on read.
#[cfg(feature = "reasoning")]
use crate::interrupt;
use crate::{
    admin,
    errors::{EngineResult, invalid},
    handles::handle,
};
use pyo3::prelude::*;
handle!(PyReasoning, "Reasoning");
handle!(PyRdfsSetting, "RdfsSetting");
#[pymethods]
impl PyReasoning {
    #[getter]
    fn rdfs(&self, py: Python<'_>) -> PyRdfsSetting {
        PyRdfsSetting {
            owner: self.owner.clone_ref(py),
        }
    }
    fn status<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        let ds = self.ds(py)?;
        admin::to_py(
            py,
            &py.detach(|| ds.reasoning().status())
                .map(|s| status_json(&s)),
        )
    }
    #[pyo3(signature=(profile="rdfs",*,rules=None,inputs=None,incremental=true,cancel=None,progress=None,timeout=None))]
    #[allow(clippy::too_many_arguments)]
    fn run(
        &self,
        py: Python<'_>,
        profile: &str,
        rules: Option<String>,
        inputs: Option<&Bound<'_, PyAny>>,
        incremental: bool,
        cancel: Option<&Bound<'_, PyAny>>,
        progress: Option<&Bound<'_, PyAny>>,
        timeout: Option<f64>,
    ) -> PyResult<crate::validate::PyReasonReport> {
        let ds = self.write(py)?;
        #[cfg(feature = "reasoning")]
        {
            let profile = match rules {
                Some(text) => {
                    sparkles_reasoner::parse_rules(&text)
                        .map_err(|e| crate::errors::syntax(py, e.to_string()))?;
                    sparkles_reasoner::Profile::Rules(text)
                }
                None => profile.parse().map_err(|e| invalid(py, format!("{e}")))?,
            };
            let req = sparkles::reasoning::ReasonRequest {
                profile,
                inputs: inputs
                    .map(|v| admin::from_py(v, "reasoning inputs"))
                    .transpose()?
                    .unwrap_or_default(),
                incremental,
                ..Default::default()
            };
            let r = interrupt::controlled(py, cancel, progress, timeout, move |ctl| {
                ds.reasoning().run_with(&req, &ctl)
            })?;
            Ok(r.report.into())
        }
        #[cfg(not(feature = "reasoning"))]
        {
            let _ = (
                ds,
                profile,
                rules,
                inputs,
                incremental,
                cancel,
                progress,
                timeout,
            );
            Err(crate::errors::new_err(
                py,
                "UnsupportedError",
                "reasoning feature is disabled",
            ))
        }
    }
    fn clear(&self, py: Python<'_>) -> PyResult<u64> {
        let ds = self.write(py)?;
        #[cfg(feature = "reasoning")]
        {
            py.detach(|| ds.reasoning().clear()).py(py)
        }
        #[cfg(not(feature = "reasoning"))]
        {
            let _ = ds;
            Err(crate::errors::new_err(
                py,
                "UnsupportedError",
                "reasoning feature is disabled",
            ))
        }
    }
    #[pyo3(signature=(*,checks=None,limit=100,include_inferred=false,graphs=None,cancel=None,progress=None,timeout=None))]
    #[allow(clippy::too_many_arguments)]
    fn diagnostics<'py>(
        &self,
        py: Python<'py>,
        checks: Option<Vec<String>>,
        limit: usize,
        include_inferred: bool,
        graphs: Option<Vec<String>>,
        cancel: Option<&Bound<'py, PyAny>>,
        progress: Option<&Bound<'py, PyAny>>,
        timeout: Option<f64>,
    ) -> PyResult<Bound<'py, PyAny>> {
        let ds = self.ds(py)?;
        #[cfg(feature = "reasoning")]
        {
            let r = interrupt::controlled(py, cancel, progress, timeout, move |ctl| {
                let opts = sparkles_reasoner::diagnostics::DiagnoseOptions {
                    checks: checks.unwrap_or_default(),
                    limit,
                    inferences: include_inferred,
                    graphs: graphs.unwrap_or_default(),
                    timeout: ctl
                        .deadline
                        .map(|d| d.saturating_duration_since(std::time::Instant::now())),
                    ..Default::default()
                };
                let r = ds.reasoning().diagnostics(&opts)?;
                ctl.check()?;
                ctl.progress.report(1.0, "diagnosed dataset");
                Ok(r)
            })?;
            admin::to_py(py, &r.report.to_json())
        }
        #[cfg(not(feature = "reasoning"))]
        {
            let _ = (
                ds,
                checks,
                limit,
                include_inferred,
                graphs,
                cancel,
                progress,
                timeout,
            );
            Err(crate::errors::new_err(
                py,
                "UnsupportedError",
                "reasoning feature is disabled",
            ))
        }
    }
}
#[pymethods]
impl PyRdfsSetting {
    fn get<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        use sparkles::sparql::rdfs::SchemaSource;
        let ds = self.ds(py)?;
        let r = ds.reasoning().rdfs().get().map(|r| match &r.source {
            SchemaSource::Graph(g) => {
                serde_json::json!({"source":"graph","graph":g.as_deref().unwrap_or("default")})
            }
            SchemaSource::Fixed(_) => serde_json::json!({"source":"fixed"}),
        });
        admin::to_py(py, &r)
    }
    #[pyo3(signature=(schema=None,*,graph=None,format=None))]
    fn set(
        &self,
        py: Python<'_>,
        schema: Option<&Bound<'_, PyAny>>,
        graph: Option<&Bound<'_, PyAny>>,
        format: Option<&Bound<'_, PyAny>>,
    ) -> PyResult<()> {
        use sparkles::reasoning::rdfs::NewSchema;
        let ds = self.write(py)?;
        let source = match (schema, graph) {
            (Some(text), None) => {
                let (src, _spool) = crate::io::source_from_py(
                    py,
                    Some(text),
                    format,
                    None,
                    None,
                    None,
                    None,
                    false,
                )?;
                let tmp = sparkles::Dataset::memory();
                py.detach(|| tmp.load_sources_receipt(vec![src])).py(py)?;
                let triples = tmp
                    .quads(None, None, None, None)
                    .map(|q| q.map(|q| oxrdf::Triple::new(q.subject, q.predicate, q.object)))
                    .collect::<sparkles::Result<Vec<_>>>()
                    .py(py)?;
                NewSchema::Triples(triples)
            }
            (None, Some(graph)) => {
                let g =
                    if graph.is_none() || graph.extract::<String>().is_ok_and(|v| v == "default") {
                        "default".into()
                    } else {
                        crate::terms::iri_from_py(graph)?.into_string()
                    };
                NewSchema::Graph(g)
            }
            _ => return Err(invalid(py, "give schema or graph")),
        };
        py.detach(|| ds.reasoning().rdfs().set(source)).py(py)
    }
    fn reset(&self, py: Python<'_>) -> PyResult<()> {
        let ds = self.write(py)?;
        py.detach(|| ds.reasoning().rdfs().reset()).py(py)
    }
}
pub fn register(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_class::<PyReasoning>()?;
    m.add_class::<PyRdfsSetting>()?;
    Ok(())
}

fn status_json(s: &sparkles::reasoning::ReasoningStatus) -> serde_json::Value {
    let mut j = serde_json::to_value(&s.record).expect("reasoning record serializes");
    j["head"] = s.head.into();
    j["stale"] = s.freshness.stale.into();
    j["commitsSince"] = serde_json::json!(s.freshness.commits_since);
    if let Some(reason) = &s.freshness.reason {
        j["staleReason"] = reason.clone().into();
    }
    j
}
