//! Reasoning and SHACL and ShEx validation, behind the features `reasoning`, `shacl`
//! and `shex`. Without a feature, its methods raise `UnsupportedError`.

#![cfg_attr(
    not(all(feature = "reasoning", feature = "shacl", feature = "shex")),
    allow(unused_imports)
)]

use crate::dataset::INFERRED_GRAPH;
use crate::errors;
use crate::terms::{PyNamedNode, term_to_py};
use oxrdf::{NamedNode, Term};
use oxrdfio::RdfFormat;
use pyo3::prelude::*;

// ------------------------------------------------------------------------ reasoning ----

/// What a `reason` run did.
#[pyclass(
    frozen,
    module = "sparkles",
    name = "ReasonReport",
    skip_from_py_object
)]
pub struct PyReasonReport {
    /// the profile, or `rules` for rule text
    #[pyo3(get)]
    profile: String,
    /// rules run
    #[pyo3(get)]
    rules: usize,
    #[pyo3(get)]
    iterations: usize,
    /// triples in the inferred graph after the run
    #[pyo3(get)]
    inferred: u64,
    #[pyo3(get)]
    millis: u64,
    #[pyo3(get)]
    warnings: Vec<String>,
}

#[pymethods]
impl PyReasonReport {
    fn __repr__(&self) -> String {
        format!(
            "ReasonReport(profile={:?}, rules={}, iterations={}, inferred={}, millis={})",
            self.profile, self.rules, self.iterations, self.inferred, self.millis
        )
    }
}

#[cfg(feature = "reasoning")]
impl From<sparkles_reasoner::ReasonReport> for PyReasonReport {
    fn from(r: sparkles_reasoner::ReasonReport) -> Self {
        Self {
            profile: r.profile,
            rules: r.rules,
            iterations: r.iterations,
            inferred: r.inferred,
            millis: r.millis,
            warnings: r.warnings,
        }
    }
}

// --------------------------------------------------------------------------- SHACL ----

/// One SHACL validation result.
#[pyclass(frozen, module = "sparkles", name = "ShaclResult", skip_from_py_object)]
pub struct PyShaclResult {
    focus_node: Term,
    path: Option<String>,
    value: Option<Term>,
    source_shape: Term,
    constraint_component: NamedNode,
    severity: NamedNode,
    message: Option<String>,
}

#[pymethods]
impl PyShaclResult {
    #[getter]
    fn focus_node<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        term_to_py(py, &self.focus_node)
    }

    /// The result path in SPARQL property path syntax, or `None`.
    #[getter]
    fn path(&self) -> Option<&str> {
        self.path.as_deref()
    }

    #[getter]
    fn value<'py>(&self, py: Python<'py>) -> PyResult<Option<Bound<'py, PyAny>>> {
        self.value.as_ref().map(|t| term_to_py(py, t)).transpose()
    }

    #[getter]
    fn source_shape<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        term_to_py(py, &self.source_shape)
    }

    #[getter]
    fn constraint_component(&self) -> PyNamedNode {
        PyNamedNode {
            inner: self.constraint_component.clone(),
        }
    }

    #[getter]
    fn severity(&self) -> PyNamedNode {
        PyNamedNode {
            inner: self.severity.clone(),
        }
    }

    #[getter]
    fn message(&self) -> Option<&str> {
        self.message.as_deref()
    }

    fn __repr__(&self) -> String {
        format!(
            "<ShaclResult focus={} component={} message={:?}>",
            self.focus_node, self.constraint_component, self.message
        )
    }
}

/// A SHACL validation report.
#[pyclass(frozen, module = "sparkles", name = "ShaclReport", skip_from_py_object)]
pub struct PyShaclReport {
    #[pyo3(get)]
    conforms: bool,
    results: Vec<Py<PyShaclResult>>,
    turtle: String,
}

#[pymethods]
impl PyShaclReport {
    #[getter]
    fn results(&self, py: Python<'_>) -> Vec<Py<PyShaclResult>> {
        self.results.iter().map(|r| r.clone_ref(py)).collect()
    }

    /// The W3C validation report graph as Turtle.
    fn to_turtle(&self) -> &str {
        &self.turtle
    }

    fn __bool__(&self) -> bool {
        self.conforms
    }

    fn __repr__(&self) -> String {
        format!(
            "<ShaclReport conforms={} results={}>",
            self.conforms,
            self.results.len()
        )
    }
}

#[cfg(feature = "shacl")]
#[allow(clippy::too_many_arguments)]
pub fn shacl(
    py: Python<'_>,
    ds: sparkles::Dataset,
    shapes: Option<String>,
    format: Option<RdfFormat>,
    compact: bool,
    shapes_graph: Option<String>,
    data_graph: Option<String>,
    include_inferred: bool,
    cancel: Option<&Bound<'_, PyAny>>,
    progress: Option<&Bound<'_, PyAny>>,
    timeout: Option<f64>,
) -> PyResult<PyShaclReport> {
    use sparkles_shacl::{Shapes, ShapesSyntax, ValidateOptions};
    let syntax = if compact {
        ShapesSyntax::Compact
    } else {
        format.unwrap_or(RdfFormat::Turtle).into()
    };
    match (&shapes, &shapes_graph) {
        (Some(_), Some(_)) => {
            return Err(errors::invalid(py, "give shapes or shapes_graph, not both"));
        }
        (None, None) => return Err(errors::invalid(py, "give shapes or shapes_graph")),
        _ => {}
    }
    let (report, turtle) =
        crate::interrupt::controlled(py, cancel, progress, timeout, move |ctl| {
            ctl.progress.report(0.0, "parsing shapes");
            ctl.check()?;
            let snap = ds.snapshot();
            let shapes = match (shapes, shapes_graph) {
                (Some(text), None) => Shapes::parse(&text, syntax, None)
                    .map_err(|e| sparkles::Error::RdfParse(format!("{e:#}")))?,
                (None, Some(g)) => Shapes::from_store(&snap, Some(&g)).map_err(engine_error)?,
                _ => unreachable!(),
            };
            ctl.check()?;
            let opts = ValidateOptions {
                data_graph,
                extra_graphs: if include_inferred {
                    vec![INFERRED_GRAPH.into()]
                } else {
                    Vec::new()
                },
                cancel: Some(ctl.cancel.flag()),
                timeout: ctl
                    .deadline
                    .map(|d| d.saturating_duration_since(std::time::Instant::now())),
                ..Default::default()
            };
            let report = ds.validation().shacl(&shapes, &opts)?;
            let turtle = report.to_turtle();
            ctl.progress.report(1.0, "validated SHACL");
            Ok((report, turtle))
        })?;
    let results = report
        .results
        .into_iter()
        .map(|r| {
            let message = r.message().map(str::to_string);
            Py::new(
                py,
                PyShaclResult {
                    focus_node: r.focus_node,
                    path: r.result_path.map(|p| p.to_string()),
                    value: r.value,
                    source_shape: r.source_shape,
                    constraint_component: r.source_constraint_component,
                    severity: r.severity,
                    message,
                },
            )
        })
        .collect::<PyResult<Vec<_>>>()?;
    Ok(PyShaclReport {
        conforms: report.conforms,
        results,
        turtle,
    })
}

#[cfg(not(feature = "shacl"))]
#[allow(clippy::too_many_arguments)]
pub fn shacl(
    py: Python<'_>,
    _ds: sparkles::Dataset,
    _shapes: Option<String>,
    _format: Option<RdfFormat>,
    _compact: bool,
    _shapes_graph: Option<String>,
    _data_graph: Option<String>,
    _include_inferred: bool,
    _cancel: Option<&Bound<'_, PyAny>>,
    _progress: Option<&Bound<'_, PyAny>>,
    _timeout: Option<f64>,
) -> PyResult<PyShaclReport> {
    Err(errors::missing_feature(py, "shacl"))
}

// ---------------------------------------------------------------------------- ShEx ----

/// The result of one (node, shape) pair of a ShEx validation.
#[pyclass(frozen, module = "sparkles", name = "ShexResult", skip_from_py_object)]
pub struct PyShexResult {
    node: Term,
    /// the shape's IRI or blank-node label, or `START`
    #[pyo3(get)]
    shape: String,
    #[pyo3(get)]
    conformant: bool,
    #[pyo3(get)]
    reason: Option<String>,
}

#[pymethods]
impl PyShexResult {
    #[getter]
    fn node<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        term_to_py(py, &self.node)
    }

    fn __repr__(&self) -> String {
        format!(
            "<ShexResult node={} shape={} conformant={}>",
            self.node, self.shape, self.conformant
        )
    }
}

/// A ShEx result map.
#[pyclass(frozen, module = "sparkles", name = "ShexReport", skip_from_py_object)]
pub struct PyShexReport {
    #[pyo3(get)]
    conforms: bool,
    results: Vec<Py<PyShexResult>>,
    json: String,
}

#[pymethods]
impl PyShexReport {
    #[getter]
    fn results(&self, py: Python<'_>) -> Vec<Py<PyShexResult>> {
        self.results.iter().map(|r| r.clone_ref(py)).collect()
    }

    /// The result map as JSON text (the format of `POST /{ds}/shex`).
    fn to_json(&self) -> &str {
        &self.json
    }

    fn __bool__(&self) -> bool {
        self.conforms
    }

    fn __repr__(&self) -> String {
        format!(
            "<ShexReport conforms={} results={}>",
            self.conforms,
            self.results.len()
        )
    }
}

#[cfg(feature = "shex")]
#[allow(clippy::too_many_arguments)]
pub fn shex(
    py: Python<'_>,
    ds: sparkles::Dataset,
    schema: String,
    shape_map: String,
    format: Option<String>,
    data_graph: Option<String>,
    include_inferred: bool,
    cancel: Option<&Bound<'_, PyAny>>,
    progress: Option<&Bound<'_, PyAny>>,
    timeout: Option<f64>,
) -> PyResult<PyShexReport> {
    use sparkles_shex::{
        FileResolver, SchemaFormat, ShapeLabel, ShapeMap, Status, ValidateOptions,
    };
    let hint = match format.as_deref() {
        None => None,
        Some(f) => Some(
            SchemaFormat::from_name(f)
                .ok_or_else(|| errors::invalid(py, format!("unknown ShEx schema format {f:?}")))?,
        ),
    };
    let rm = crate::interrupt::controlled(py, cancel, progress, timeout, move |ctl| {
        ctl.progress.report(0.0, "compiling schema");
        ctl.check()?;
        let parsed = sparkles_shex::parse_schema(&schema, None, hint)
            .map_err(|e| sparkles::Error::RdfParse(format!("schema: {e}")))?;
        let compiled = sparkles_shex::compile(&parsed, &FileResolver::default())
            .map_err(|e| sparkles::Error::RdfParse(format!("schema: {e:#}")))?;
        let map = if shape_map.trim_start().starts_with('[') {
            ShapeMap::from_json(&shape_map)
        } else {
            ShapeMap::parse(&shape_map, compiled.prefixes(), compiled.base())
        }
        .map_err(|e| sparkles::Error::RdfParse(format!("shape map: {e}")))?;
        ctl.check()?;
        let opts = ValidateOptions {
            data_graph,
            extra_graphs: if include_inferred {
                vec![INFERRED_GRAPH.into()]
            } else {
                Vec::new()
            },
            cancel: Some(ctl.cancel.flag()),
            timeout: ctl
                .deadline
                .map(|d| d.saturating_duration_since(std::time::Instant::now())),
            ..Default::default()
        };
        let rm = ds.validation().shex(&compiled, &map, &opts)?;
        ctl.progress.report(1.0, "validated ShEx");
        Ok(rm)
    })?;
    let json = rm.to_json().to_string();
    let results = rm
        .results
        .into_iter()
        .map(|r| {
            Py::new(
                py,
                PyShexResult {
                    node: r.node,
                    shape: match r.shape {
                        ShapeLabel::Iri(i) => i,
                        ShapeLabel::BNode(b) => format!("_:{b}"),
                        ShapeLabel::Start => "START".to_string(),
                    },
                    conformant: r.status == Status::Conformant,
                    reason: r.reason,
                },
            )
        })
        .collect::<PyResult<Vec<_>>>()?;
    Ok(PyShexReport {
        conforms: rm.conforms,
        results,
        json,
    })
}

#[cfg(not(feature = "shex"))]
#[allow(clippy::too_many_arguments)]
pub fn shex(
    py: Python<'_>,
    _ds: sparkles::Dataset,
    _schema: String,
    _shape_map: String,
    _format: Option<String>,
    _data_graph: Option<String>,
    _include_inferred: bool,
    _cancel: Option<&Bound<'_, PyAny>>,
    _progress: Option<&Bound<'_, PyAny>>,
    _timeout: Option<f64>,
) -> PyResult<PyShexReport> {
    Err(errors::missing_feature(py, "shex"))
}

pub fn register(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_class::<PyReasonReport>()?;
    m.add_class::<PyShaclReport>()?;
    m.add_class::<PyShaclResult>()?;
    m.add_class::<PyShexReport>()?;
    m.add_class::<PyShexResult>()?;
    Ok(())
}

#[cfg(feature = "shacl")]
fn engine_error(e: anyhow::Error) -> sparkles::Error {
    match e.downcast::<sparkles::Error>() {
        Ok(e) => e,
        Err(e) => sparkles::Error::invalid(format!("{e:#}")),
    }
}
