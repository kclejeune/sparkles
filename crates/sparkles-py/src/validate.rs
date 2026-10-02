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
pub fn reason(
    py: Python<'_>,
    ds: sparkles::Dataset,
    profile: &str,
    rules: Option<String>,
) -> PyResult<PyReasonReport> {
    use sparkles_reasoner::{Profile, ReasonOptions};
    let profile = match rules {
        Some(text) => {
            sparkles_reasoner::parse_rules(&text).map_err(|e| errors::syntax(py, e.to_string()))?;
            Profile::Rules(text)
        }
        None => profile
            .parse::<Profile>()
            .map_err(|e| errors::invalid(py, e.to_string()))?,
    };
    let r = py
        .detach(|| sparkles_reasoner::materialize(ds.store(), &profile, &ReasonOptions::default()))
        .map_err(|e| errors::anyhow(py, e))?;
    Ok(PyReasonReport {
        profile: r.profile,
        rules: r.rules,
        iterations: r.iterations,
        inferred: r.inferred,
        millis: r.millis,
        warnings: r.warnings,
    })
}

#[cfg(not(feature = "reasoning"))]
pub fn reason(
    py: Python<'_>,
    _ds: sparkles::Dataset,
    _profile: &str,
    _rules: Option<String>,
) -> PyResult<PyReasonReport> {
    Err(errors::missing_feature(py, "reasoning"))
}

#[cfg(feature = "reasoning")]
pub fn clear_inferences(py: Python<'_>, ds: sparkles::Dataset) -> PyResult<u64> {
    py.detach(|| sparkles_reasoner::clear(ds.store()))
        .map_err(|e| errors::anyhow(py, e))
}

#[cfg(not(feature = "reasoning"))]
pub fn clear_inferences(py: Python<'_>, _ds: sparkles::Dataset) -> PyResult<u64> {
    Err(errors::missing_feature(py, "reasoning"))
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
    shapes_graph: Option<String>,
    data_graph: Option<String>,
    include_inferred: bool,
) -> PyResult<PyShaclReport> {
    use sparkles_shacl::{Shapes, ValidateOptions};
    let snap = ds.snapshot();
    let shapes = match (shapes, shapes_graph) {
        (Some(_), Some(_)) => {
            return Err(errors::invalid(py, "give shapes or shapes_graph, not both"));
        }
        (None, None) => return Err(errors::invalid(py, "give shapes or shapes_graph")),
        (Some(text), None) => py
            .detach(|| Shapes::parse(&text, format.unwrap_or(RdfFormat::Turtle), None))
            .map_err(|e| errors::syntax(py, format!("{e:#}")))?,
        (None, Some(g)) => py
            .detach(|| Shapes::from_store(&snap, Some(&g)))
            .map_err(|e| errors::anyhow(py, e))?,
    };
    let opts = ValidateOptions {
        data_graph,
        extra_graphs: if include_inferred {
            vec![INFERRED_GRAPH.to_string()]
        } else {
            Vec::new()
        },
        ..Default::default()
    };
    let (report, turtle) = py
        .detach(|| {
            sparkles_shacl::validate(&snap, &shapes, &opts).map(|r| {
                let t = r.to_turtle();
                (r, t)
            })
        })
        .map_err(|e| errors::anyhow(py, e))?;
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
    _shapes_graph: Option<String>,
    _data_graph: Option<String>,
    _include_inferred: bool,
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
    let parsed = sparkles_shex::parse_schema(&schema, None, hint)
        .map_err(|e| errors::syntax(py, format!("schema: {e}")))?;
    let compiled = py
        .detach(|| sparkles_shex::compile(&parsed, &FileResolver::default()))
        .map_err(|e| errors::syntax(py, format!("schema: {e}")))?;
    let map = if shape_map.trim_start().starts_with('[') {
        ShapeMap::from_json(&shape_map)
    } else {
        ShapeMap::parse(&shape_map, compiled.prefixes(), compiled.base())
    }
    .map_err(|e| errors::syntax(py, format!("shape map: {e}")))?;
    let opts = ValidateOptions {
        data_graph,
        extra_graphs: if include_inferred {
            vec![INFERRED_GRAPH.to_string()]
        } else {
            Vec::new()
        },
        ..Default::default()
    };
    let snap = ds.snapshot();
    let rm = py
        .detach(|| sparkles_shex::validate(&snap, &compiled, &map, &opts))
        .map_err(|e| errors::anyhow(py, e))?;
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
