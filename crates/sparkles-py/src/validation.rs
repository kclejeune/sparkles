//! SHACL/ShEx validation and write guards.
use crate::io::format_from_py;
use crate::terms::{iri_from_py, opt};
use crate::{
    admin,
    errors::{EngineResult, invalid},
    handles::handle,
};
use pyo3::prelude::*;
use pyo3::types::{PyBytes, PyString};
handle!(PyValidation, "Validation");
handle!(PyGuardSetting, "GuardSetting");
#[pymethods]
impl PyValidation {
    #[getter]
    fn guard(&self, py: Python<'_>) -> PyGuardSetting {
        PyGuardSetting {
            owner: self.owner.clone_ref(py),
        }
    }

    /// Validate with SHACL shapes given as text, or read from `shapes_graph`.
    #[pyo3(signature = (shapes = None, *, format = None, shapes_graph = None, data_graph = None, include_inferred = false, cancel = None, progress = None, timeout = None))]
    #[allow(clippy::too_many_arguments)]
    fn shacl(
        &self,
        py: Python<'_>,
        shapes: Option<&Bound<'_, PyAny>>,
        format: Option<&Bound<'_, PyAny>>,
        shapes_graph: Option<&Bound<'_, PyAny>>,
        data_graph: Option<&Bound<'_, PyAny>>,
        include_inferred: bool,
        cancel: Option<&Bound<'_, PyAny>>,
        progress: Option<&Bound<'_, PyAny>>,
        timeout: Option<f64>,
    ) -> PyResult<crate::validate::PyShaclReport> {
        let ds = self.ds(py)?;
        let shapes_graph = opt(shapes_graph, iri_from_py)?.map(|n| n.into_string());
        let data_graph = opt(data_graph, iri_from_py)?.map(|n| n.into_string());
        // "shaclc" (or `text/shaclc`) is the SHACL Compact Syntax; other names are RDF
        // formats
        let compact = match format.and_then(|f| f.cast::<PyString>().ok()) {
            Some(s) => matches!(
                s.to_str()?.trim().to_ascii_lowercase().as_str(),
                "shaclc" | "shc" | "text/shaclc"
            ),
            None => false,
        };
        let format = if compact {
            None
        } else {
            crate::io::rdf_only(format_from_py(format)?, "shapes")?
        };
        let shapes = match shapes.filter(|s| !s.is_none()) {
            None => None,
            Some(s) => Some(if let Ok(b) = s.cast::<PyBytes>() {
                String::from_utf8(b.as_bytes().to_vec())
                    .map_err(|_| invalid(py, "the shapes are not UTF-8"))?
            } else {
                s.extract::<String>()?
            }),
        };
        crate::validate::shacl(
            py,
            ds,
            shapes,
            format,
            compact,
            shapes_graph,
            data_graph,
            include_inferred,
            cancel,
            progress,
            timeout,
        )
    }

    /// Validate with a ShEx schema (ShExC or ShExJ) and a shape map.
    #[pyo3(signature = (schema, shape_map, *, format = None, data_graph = None, include_inferred = false, cancel = None, progress = None, timeout = None))]
    #[allow(clippy::too_many_arguments)]
    fn shex(
        &self,
        py: Python<'_>,
        schema: String,
        shape_map: String,
        format: Option<String>,
        data_graph: Option<&Bound<'_, PyAny>>,
        include_inferred: bool,
        cancel: Option<&Bound<'_, PyAny>>,
        progress: Option<&Bound<'_, PyAny>>,
        timeout: Option<f64>,
    ) -> PyResult<crate::validate::PyShexReport> {
        let ds = self.ds(py)?;
        let data_graph = opt(data_graph, iri_from_py)?.map(|n| n.into_string());
        crate::validate::shex(
            py,
            ds,
            schema,
            shape_map,
            format,
            data_graph,
            include_inferred,
            cancel,
            progress,
            timeout,
        )
    }
}
#[pymethods]
impl PyGuardSetting {
    fn get<'py>(&self, py: Python<'py>) -> PyResult<Option<Bound<'py, PyAny>>> {
        let ds = self.ds(py)?;
        ds.validation()
            .guard()
            .get()
            .map(|g| admin::to_py(py, &g.json()))
            .transpose()
    }
    fn reset(&self, py: Python<'_>) -> PyResult<()> {
        let ds = self.write(py)?;
        py.detach(|| ds.validation().guard().reset()).py(py)
    }
    fn set<'py>(&self, py: Python<'py>, config: &Bound<'py, PyAny>) -> PyResult<Bound<'py, PyAny>> {
        let ds = self.write(py)?;
        #[cfg(any(feature = "shacl", feature = "shex"))]
        {
            let language = config
                .get_item("language")
                .ok()
                .and_then(|v| v.extract::<String>().ok())
                .unwrap_or_else(|| "shacl".into());
            let outcome = match language.as_str() {
                #[cfg(feature = "shacl")]
                "shacl" => {
                    let c = admin::from_py(config, "SHACL guard")?;
                    py.detach(|| ds.validation().guard().set_shacl(c)).py(py)?
                }
                #[cfg(feature = "shex")]
                "shex" => {
                    let c = admin::from_py(config, "ShEx guard")?;
                    py.detach(|| {
                        ds.validation()
                            .guard()
                            .set_shex(c, &sparkles_shex::FileResolver::default())
                    })
                    .py(py)?
                }
                _ => {
                    return Err(crate::errors::new_err(
                        py,
                        "UnsupportedError",
                        format!("unavailable validation language: {language}"),
                    ));
                }
            };
            {
                use sparkles::handles::GuardOutcome;
                let j = match outcome {
                    GuardOutcome::Installed(s) => {
                        serde_json::json!({"status":"installed","summary":s})
                    }
                    GuardOutcome::NotConforming(s) => {
                        serde_json::json!({"status":"not-conforming","summary":s})
                    }
                    GuardOutcome::Removed => serde_json::json!({"status":"removed"}),
                    _ => serde_json::json!({"status":"unknown"}),
                };
                admin::to_py(py, &j)
            }
        }
        #[cfg(not(any(feature = "shacl", feature = "shex")))]
        {
            let _ = (ds, config);
            Err(crate::errors::new_err(
                py,
                "UnsupportedError",
                "validation features are disabled",
            ))
        }
    }
}
pub fn register(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_class::<PyValidation>()?;
    m.add_class::<PyGuardSetting>()?;
    Ok(())
}
