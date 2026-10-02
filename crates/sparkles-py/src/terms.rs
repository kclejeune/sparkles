//! RDF terms as Python classes, and conversions both ways (rdflib terms included).

use oxrdf::vocab::xsd;
use oxrdf::{
    BaseDirection, BlankNode, GraphName, Literal, NamedNode, NamedOrBlankNode, Quad, Term, Triple,
};
use pyo3::exceptions::{PyIndexError, PyTypeError, PyValueError};
use pyo3::prelude::*;
use pyo3::sync::PyOnceLock;
use pyo3::types::{PyBool, PyBytes, PyFloat, PyInt, PyString, PyTuple};

fn repr_str(py: Python<'_>, s: &str) -> PyResult<String> {
    Ok(PyString::new(py, s).repr()?.to_string())
}

// ---------------------------------------------------------------------- classes ----

/// An IRI (`rdflib.URIRef`).
#[pyclass(
    frozen,
    eq,
    hash,
    module = "sparkles",
    name = "NamedNode",
    skip_from_py_object
)]
#[derive(Clone, PartialEq, Eq, Hash)]
pub struct PyNamedNode {
    pub inner: NamedNode,
}

#[pymethods]
impl PyNamedNode {
    #[new]
    fn new(value: &str) -> PyResult<Self> {
        NamedNode::new(value)
            .map(|inner| PyNamedNode { inner })
            .map_err(|e| PyValueError::new_err(format!("invalid IRI <{value}>: {e}")))
    }

    #[getter]
    fn value(&self) -> &str {
        self.inner.as_str()
    }

    fn __str__(&self) -> String {
        self.inner.to_string()
    }

    fn __repr__(&self, py: Python<'_>) -> PyResult<String> {
        Ok(format!("NamedNode({})", repr_str(py, self.inner.as_str())?))
    }

    fn __getnewargs__(&self) -> (String,) {
        (self.inner.as_str().to_string(),)
    }
}

/// A blank node (`rdflib.BNode`).
#[pyclass(
    frozen,
    eq,
    hash,
    module = "sparkles",
    name = "BlankNode",
    skip_from_py_object
)]
#[derive(Clone, PartialEq, Eq, Hash)]
pub struct PyBlankNode {
    pub inner: BlankNode,
}

#[pymethods]
impl PyBlankNode {
    #[new]
    #[pyo3(signature = (value = None))]
    fn new(value: Option<&str>) -> PyResult<Self> {
        let inner = match value {
            None => BlankNode::default(),
            Some(v) => BlankNode::new(v)
                .map_err(|e| PyValueError::new_err(format!("invalid blank node id {v:?}: {e}")))?,
        };
        Ok(PyBlankNode { inner })
    }

    #[getter]
    fn value(&self) -> &str {
        self.inner.as_str()
    }

    fn __str__(&self) -> String {
        self.inner.to_string()
    }

    fn __repr__(&self, py: Python<'_>) -> PyResult<String> {
        Ok(format!("BlankNode({})", repr_str(py, self.inner.as_str())?))
    }

    fn __getnewargs__(&self) -> (String,) {
        (self.inner.as_str().to_string(),)
    }
}

/// The arguments of `_literal`: value, datatype IRI, language and direction.
type LiteralParts = (String, Option<String>, Option<String>, Option<&'static str>);

/// A literal: a string, a language-tagged string (with an RDF 1.2 direction) or a typed
/// value (`rdflib.Literal`).
#[pyclass(
    frozen,
    eq,
    hash,
    module = "sparkles",
    name = "Literal",
    skip_from_py_object
)]
#[derive(Clone, PartialEq, Eq, Hash)]
pub struct PyLiteral {
    pub inner: Literal,
}

#[pymethods]
impl PyLiteral {
    #[new]
    #[pyo3(signature = (value, *, datatype = None, language = None, direction = None))]
    fn new(
        value: &Bound<'_, PyAny>,
        datatype: Option<&Bound<'_, PyAny>>,
        language: Option<&str>,
        direction: Option<&str>,
    ) -> PyResult<Self> {
        let py = value.py();
        let Ok(text) = value.cast::<PyString>() else {
            if datatype.is_some() || language.is_some() || direction.is_some() {
                return Err(PyTypeError::new_err(
                    "datatype, language and direction need a str value",
                ));
            }
            return Ok(PyLiteral {
                inner: literal_from_native(py, value)?,
            });
        };
        let text = text.to_str()?;
        let inner = match (language, direction, datatype) {
            (Some(_), _, Some(_)) => {
                return Err(PyValueError::new_err(
                    "a literal has either a language or a datatype",
                ));
            }
            (None, Some(_), _) => {
                return Err(PyValueError::new_err("a direction needs a language"));
            }
            (Some(lang), None, None) => {
                Literal::new_language_tagged_literal(text, lang).map_err(|e| {
                    PyValueError::new_err(format!("invalid language tag {lang:?}: {e}"))
                })?
            }
            (Some(lang), Some(dir), None) => {
                let dir = match dir {
                    "ltr" => BaseDirection::Ltr,
                    "rtl" => BaseDirection::Rtl,
                    _ => {
                        return Err(PyValueError::new_err(format!(
                            "direction must be 'ltr' or 'rtl', not {dir:?}"
                        )));
                    }
                };
                Literal::new_directional_language_tagged_literal(text, lang, dir).map_err(|e| {
                    PyValueError::new_err(format!("invalid language tag {lang:?}: {e}"))
                })?
            }
            (None, None, Some(dt)) => Literal::new_typed_literal(text, iri_from_py(dt)?),
            (None, None, None) => Literal::new_simple_literal(text),
        };
        Ok(PyLiteral { inner })
    }

    /// The lexical form.
    #[getter]
    fn value(&self) -> &str {
        self.inner.value()
    }

    #[getter]
    fn datatype(&self) -> PyNamedNode {
        PyNamedNode {
            inner: self.inner.datatype().into_owned(),
        }
    }

    #[getter]
    fn language(&self) -> Option<&str> {
        self.inner.language()
    }

    #[getter]
    fn direction(&self) -> Option<&'static str> {
        self.inner.direction().map(|d| match d {
            BaseDirection::Ltr => "ltr",
            BaseDirection::Rtl => "rtl",
        })
    }

    /// The value as a native Python object for the common XSD datatypes, the lexical
    /// form otherwise.
    fn to_python<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        literal_to_native(py, &self.inner)
    }

    fn __str__(&self) -> String {
        self.inner.to_string()
    }

    /// Pickling: the constructor's keyword arguments, through `_literal`.
    fn __reduce__<'py>(&self, py: Python<'py>) -> PyResult<(Bound<'py, PyAny>, LiteralParts)> {
        let make = py.import("sparkles._sparkles")?.getattr("_literal")?;
        let datatype = match self.inner.language() {
            Some(_) => None,
            None => Some(self.inner.datatype().as_str().to_string()),
        };
        Ok((
            make,
            (
                self.inner.value().to_string(),
                datatype,
                self.inner.language().map(str::to_string),
                self.direction(),
            ),
        ))
    }

    fn __repr__(&self, py: Python<'_>) -> PyResult<String> {
        let v = repr_str(py, self.inner.value())?;
        Ok(match (self.inner.language(), self.direction()) {
            (Some(l), Some(d)) => format!(
                "Literal({v}, language={}, direction={})",
                repr_str(py, l)?,
                repr_str(py, d)?
            ),
            (Some(l), None) => format!("Literal({v}, language={})", repr_str(py, l)?),
            _ if self.inner.datatype() == xsd::STRING => format!("Literal({v})"),
            _ => format!(
                "Literal({v}, datatype=NamedNode({}))",
                repr_str(py, self.inner.datatype().as_str())?
            ),
        })
    }
}

/// An RDF triple, and an RDF 1.2 triple term.
#[pyclass(
    frozen,
    eq,
    hash,
    module = "sparkles",
    name = "Triple",
    skip_from_py_object
)]
#[derive(Clone, PartialEq, Eq, Hash)]
pub struct PyTriple {
    pub inner: Triple,
}

#[pymethods]
impl PyTriple {
    #[new]
    fn new(
        subject: &Bound<'_, PyAny>,
        predicate: &Bound<'_, PyAny>,
        object: &Bound<'_, PyAny>,
    ) -> PyResult<Self> {
        Ok(PyTriple {
            inner: Triple::new(
                subject_from_py(subject)?,
                named_node_from_py(predicate)?,
                term_from_py(object)?,
            ),
        })
    }

    #[getter]
    fn subject<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        subject_to_py(py, &self.inner.subject)
    }

    #[getter]
    fn predicate(&self) -> PyNamedNode {
        PyNamedNode {
            inner: self.inner.predicate.clone(),
        }
    }

    #[getter]
    fn object<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        term_to_py(py, &self.inner.object)
    }

    fn __len__(&self) -> usize {
        3
    }

    fn __getitem__<'py>(&self, py: Python<'py>, i: isize) -> PyResult<Bound<'py, PyAny>> {
        match if i < 0 { i + 3 } else { i } {
            0 => self.subject(py),
            1 => Ok(self.predicate().into_pyobject(py)?.into_any()),
            2 => self.object(py),
            _ => Err(PyIndexError::new_err("triple index out of range")),
        }
    }

    fn __iter__<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        Ok(self.tuple(py)?.try_iter()?.into_any())
    }

    fn __str__(&self) -> String {
        self.inner.to_string()
    }

    fn __repr__(&self, py: Python<'_>) -> PyResult<String> {
        Ok(format!(
            "Triple({}, {}, {})",
            self.subject(py)?.repr()?,
            self.predicate().__repr__(py)?,
            self.object(py)?.repr()?
        ))
    }

    fn __getnewargs__<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyTuple>> {
        self.tuple(py)
    }
}

impl PyTriple {
    fn tuple<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyTuple>> {
        PyTuple::new(
            py,
            [
                self.subject(py)?,
                self.predicate().into_pyobject(py)?.into_any(),
                self.object(py)?,
            ],
        )
    }
}

/// A triple in a graph: the default graph or a named one.
#[pyclass(
    frozen,
    eq,
    hash,
    module = "sparkles",
    name = "Quad",
    skip_from_py_object
)]
#[derive(Clone, PartialEq, Eq, Hash)]
pub struct PyQuad {
    pub inner: Quad,
}

#[pymethods]
impl PyQuad {
    #[new]
    #[pyo3(signature = (subject, predicate, object, graph_name = None))]
    fn new(
        subject: &Bound<'_, PyAny>,
        predicate: &Bound<'_, PyAny>,
        object: &Bound<'_, PyAny>,
        graph_name: Option<&Bound<'_, PyAny>>,
    ) -> PyResult<Self> {
        Ok(PyQuad {
            inner: Quad::new(
                subject_from_py(subject)?,
                named_node_from_py(predicate)?,
                term_from_py(object)?,
                match graph_name {
                    None => GraphName::DefaultGraph,
                    Some(g) => graph_from_py(g)?,
                },
            ),
        })
    }

    #[getter]
    fn subject<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        subject_to_py(py, &self.inner.subject)
    }

    #[getter]
    fn predicate(&self) -> PyNamedNode {
        PyNamedNode {
            inner: self.inner.predicate.clone(),
        }
    }

    #[getter]
    fn object<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        term_to_py(py, &self.inner.object)
    }

    #[getter]
    fn graph_name<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        graph_to_py(py, &self.inner.graph_name)
    }

    #[getter]
    fn triple(&self) -> PyTriple {
        PyTriple {
            inner: Triple::new(
                self.inner.subject.clone(),
                self.inner.predicate.clone(),
                self.inner.object.clone(),
            ),
        }
    }

    fn __len__(&self) -> usize {
        4
    }

    fn __getitem__<'py>(&self, py: Python<'py>, i: isize) -> PyResult<Bound<'py, PyAny>> {
        match if i < 0 { i + 4 } else { i } {
            0 => self.subject(py),
            1 => Ok(self.predicate().into_pyobject(py)?.into_any()),
            2 => self.object(py),
            3 => self.graph_name(py),
            _ => Err(PyIndexError::new_err("quad index out of range")),
        }
    }

    fn __iter__<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        Ok(self.tuple(py)?.try_iter()?.into_any())
    }

    fn __str__(&self) -> String {
        self.inner.to_string()
    }

    fn __repr__(&self, py: Python<'_>) -> PyResult<String> {
        Ok(format!(
            "Quad({}, {}, {}, {})",
            self.subject(py)?.repr()?,
            self.predicate().__repr__(py)?,
            self.object(py)?.repr()?,
            self.graph_name(py)?.repr()?
        ))
    }

    fn __getnewargs__<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyTuple>> {
        self.tuple(py)
    }
}

impl PyQuad {
    fn tuple<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyTuple>> {
        PyTuple::new(
            py,
            [
                self.subject(py)?,
                self.predicate().into_pyobject(py)?.into_any(),
                self.object(py)?,
                self.graph_name(py)?,
            ],
        )
    }
}

/// The default graph of a dataset, as a graph name.
#[pyclass(
    frozen,
    eq,
    hash,
    module = "sparkles",
    name = "DefaultGraph",
    skip_from_py_object
)]
#[derive(Clone, PartialEq, Eq, Hash)]
pub struct PyDefaultGraph;

#[pymethods]
impl PyDefaultGraph {
    #[new]
    fn new() -> Self {
        PyDefaultGraph
    }

    #[getter]
    fn value(&self) -> &'static str {
        ""
    }

    fn __str__(&self) -> &'static str {
        "DEFAULT"
    }

    fn __repr__(&self) -> &'static str {
        "DefaultGraph()"
    }
}

/// A query variable, as in `QuerySolutions.variables`.
#[pyclass(
    frozen,
    eq,
    hash,
    module = "sparkles",
    name = "Variable",
    skip_from_py_object
)]
#[derive(Clone, PartialEq, Eq, Hash)]
pub struct PyVariable {
    pub name: String,
}

#[pymethods]
impl PyVariable {
    #[new]
    fn new(value: &str) -> PyResult<Self> {
        let name = value.trim_start_matches(['?', '$']);
        if name.is_empty() {
            return Err(PyValueError::new_err("empty variable name"));
        }
        Ok(PyVariable {
            name: name.to_string(),
        })
    }

    #[getter]
    fn value(&self) -> &str {
        &self.name
    }

    fn __str__(&self) -> String {
        format!("?{}", self.name)
    }

    fn __repr__(&self, py: Python<'_>) -> PyResult<String> {
        Ok(format!("Variable({})", repr_str(py, &self.name)?))
    }

    fn __getnewargs__(&self) -> (String,) {
        (self.name.clone(),)
    }
}

// ------------------------------------------------------------------ Rust → Python ----

pub fn term_to_py<'py>(py: Python<'py>, t: &Term) -> PyResult<Bound<'py, PyAny>> {
    Ok(match t {
        Term::NamedNode(n) => PyNamedNode { inner: n.clone() }
            .into_pyobject(py)?
            .into_any(),
        Term::BlankNode(b) => PyBlankNode { inner: b.clone() }
            .into_pyobject(py)?
            .into_any(),
        Term::Literal(l) => PyLiteral { inner: l.clone() }.into_pyobject(py)?.into_any(),
        Term::Triple(t) => PyTriple {
            inner: (**t).clone(),
        }
        .into_pyobject(py)?
        .into_any(),
    })
}

pub fn subject_to_py<'py>(py: Python<'py>, s: &NamedOrBlankNode) -> PyResult<Bound<'py, PyAny>> {
    Ok(match s {
        NamedOrBlankNode::NamedNode(n) => PyNamedNode { inner: n.clone() }
            .into_pyobject(py)?
            .into_any(),
        NamedOrBlankNode::BlankNode(b) => PyBlankNode { inner: b.clone() }
            .into_pyobject(py)?
            .into_any(),
    })
}

pub fn graph_to_py<'py>(py: Python<'py>, g: &GraphName) -> PyResult<Bound<'py, PyAny>> {
    Ok(match g {
        GraphName::DefaultGraph => PyDefaultGraph.into_pyobject(py)?.into_any(),
        GraphName::NamedNode(n) => PyNamedNode { inner: n.clone() }
            .into_pyobject(py)?
            .into_any(),
        GraphName::BlankNode(b) => PyBlankNode { inner: b.clone() }
            .into_pyobject(py)?
            .into_any(),
    })
}

pub fn quad_to_py(py: Python<'_>, q: Quad) -> PyResult<Bound<'_, PyAny>> {
    Ok(PyQuad { inner: q }.into_pyobject(py)?.into_any())
}

pub fn triple_to_py(py: Python<'_>, t: Triple) -> PyResult<Bound<'_, PyAny>> {
    Ok(PyTriple { inner: t }.into_pyobject(py)?.into_any())
}

// ------------------------------------------------------------------ Python → Rust ----

/// The rdflib term class of `ob` (`URIRef`, `BNode`, `Literal` or `Variable`), found by
/// name in its type's MRO so that rdflib need not be imported.
fn rdflib_kind(ob: &Bound<'_, PyAny>) -> Option<&'static str> {
    let mro = ob.get_type().mro();
    for cls in mro.iter() {
        let module = cls.getattr("__module__").ok()?;
        if module.extract::<&str>().ok()? != "rdflib.term" {
            continue;
        }
        let name = cls.getattr("__name__").ok()?;
        match name.extract::<&str>().ok()? {
            "URIRef" => return Some("URIRef"),
            "BNode" => return Some("BNode"),
            "Literal" => return Some("Literal"),
            "Variable" => return Some("Variable"),
            _ => {}
        }
    }
    None
}

fn type_error(ob: &Bound<'_, PyAny>, want: &str) -> PyErr {
    let name = ob
        .get_type()
        .name()
        .map(|n| n.to_string())
        .unwrap_or_else(|_| "?".into());
    PyTypeError::new_err(format!("expected {want}, got {name}"))
}

fn rdflib_named(ob: &Bound<'_, PyAny>) -> PyResult<NamedNode> {
    let s = ob.str()?;
    let s = s.to_str()?;
    NamedNode::new(s).map_err(|e| PyValueError::new_err(format!("invalid IRI <{s}>: {e}")))
}

fn rdflib_blank(ob: &Bound<'_, PyAny>) -> PyResult<BlankNode> {
    let s = ob.str()?;
    let s = s.to_str()?;
    BlankNode::new(s)
        .map_err(|e| PyValueError::new_err(format!("invalid blank node id {s:?}: {e}")))
}

fn rdflib_literal(ob: &Bound<'_, PyAny>) -> PyResult<Literal> {
    let lexical = ob.str()?.to_str()?.to_string();
    let language: Option<String> = ob.getattr("language")?.extract()?;
    let datatype = ob.getattr("datatype")?;
    Ok(match language {
        Some(lang) => Literal::new_language_tagged_literal(lexical, &lang)
            .map_err(|e| PyValueError::new_err(format!("invalid language tag {lang:?}: {e}")))?,
        None if datatype.is_none() => Literal::new_simple_literal(lexical),
        None => Literal::new_typed_literal(lexical, rdflib_named(&datatype)?),
    })
}

/// An IRI: a `NamedNode`, an rdflib `URIRef` or a `str`.
pub fn iri_from_py(ob: &Bound<'_, PyAny>) -> PyResult<NamedNode> {
    if let Ok(n) = ob.cast::<PyNamedNode>() {
        return Ok(n.get().inner.clone());
    }
    if rdflib_kind(ob) == Some("URIRef") {
        return rdflib_named(ob);
    }
    if let Ok(s) = ob.cast::<PyString>() {
        let s = s.to_str()?;
        return NamedNode::new(s)
            .map_err(|e| PyValueError::new_err(format!("invalid IRI <{s}>: {e}")));
    }
    Err(type_error(ob, "NamedNode"))
}

/// A predicate or other IRI position that does not take a plain `str`.
pub fn named_node_from_py(ob: &Bound<'_, PyAny>) -> PyResult<NamedNode> {
    if ob.cast::<PyString>().is_ok() && rdflib_kind(ob).is_none() {
        return Err(type_error(ob, "NamedNode"));
    }
    iri_from_py(ob)
}

pub fn subject_from_py(ob: &Bound<'_, PyAny>) -> PyResult<NamedOrBlankNode> {
    if let Ok(n) = ob.cast::<PyNamedNode>() {
        return Ok(n.get().inner.clone().into());
    }
    if let Ok(b) = ob.cast::<PyBlankNode>() {
        return Ok(b.get().inner.clone().into());
    }
    match rdflib_kind(ob) {
        Some("URIRef") => Ok(rdflib_named(ob)?.into()),
        Some("BNode") => Ok(rdflib_blank(ob)?.into()),
        _ => Err(type_error(ob, "NamedNode or BlankNode")),
    }
}

pub fn term_from_py(ob: &Bound<'_, PyAny>) -> PyResult<Term> {
    if let Ok(n) = ob.cast::<PyNamedNode>() {
        return Ok(n.get().inner.clone().into());
    }
    if let Ok(b) = ob.cast::<PyBlankNode>() {
        return Ok(b.get().inner.clone().into());
    }
    if let Ok(l) = ob.cast::<PyLiteral>() {
        return Ok(l.get().inner.clone().into());
    }
    if let Ok(t) = ob.cast::<PyTriple>() {
        return Ok(t.get().inner.clone().into());
    }
    match rdflib_kind(ob) {
        Some("URIRef") => Ok(rdflib_named(ob)?.into()),
        Some("BNode") => Ok(rdflib_blank(ob)?.into()),
        Some("Literal") => Ok(rdflib_literal(ob)?.into()),
        _ => Err(type_error(ob, "NamedNode, BlankNode, Literal or Triple")),
    }
}

/// A graph name: `DefaultGraph`, a `NamedNode`, a `BlankNode`, an rdflib `URIRef` or
/// `BNode`, or a `str` IRI.
pub fn graph_from_py(ob: &Bound<'_, PyAny>) -> PyResult<GraphName> {
    if ob.cast::<PyDefaultGraph>().is_ok() {
        return Ok(GraphName::DefaultGraph);
    }
    if let Ok(b) = ob.cast::<PyBlankNode>() {
        return Ok(b.get().inner.clone().into());
    }
    if rdflib_kind(ob) == Some("BNode") {
        return Ok(rdflib_blank(ob)?.into());
    }
    iri_from_py(ob).map(GraphName::from).map_err(|e| {
        if e.is_instance_of::<PyTypeError>(ob.py()) {
            type_error(ob, "DefaultGraph, NamedNode, BlankNode or str")
        } else {
            e
        }
    })
}

/// A `Quad`, or a `Triple` in the default graph.
pub fn quad_from_py(ob: &Bound<'_, PyAny>) -> PyResult<Quad> {
    if let Ok(q) = ob.cast::<PyQuad>() {
        return Ok(q.get().inner.clone());
    }
    if let Ok(t) = ob.cast::<PyTriple>() {
        return Ok(t.get().inner.clone().in_graph(GraphName::DefaultGraph));
    }
    Err(type_error(ob, "Quad or Triple"))
}

/// An optional pattern position: `None` matches anything.
pub fn opt<T>(
    ob: Option<&Bound<'_, PyAny>>,
    f: impl Fn(&Bound<'_, PyAny>) -> PyResult<T>,
) -> PyResult<Option<T>> {
    match ob {
        Some(o) if !o.is_none() => f(o).map(Some),
        _ => Ok(None),
    }
}

// --------------------------------------------------------------- native values ----

/// Python classes the literal conversions test for.
struct PyTypes {
    datetime: Py<PyAny>,
    date: Py<PyAny>,
    time: Py<PyAny>,
    decimal: Py<PyAny>,
}

static PY_TYPES: PyOnceLock<PyTypes> = PyOnceLock::new();

/// `datetime.datetime`, `datetime.date`, `datetime.time` and `decimal.Decimal`.
fn py_types(py: Python<'_>) -> PyResult<&PyTypes> {
    PY_TYPES.get_or_try_init(py, || {
        let dt = py.import("datetime")?;
        let dec = py.import("decimal")?;
        Ok::<_, PyErr>(PyTypes {
            datetime: dt.getattr("datetime")?.unbind(),
            date: dt.getattr("date")?.unbind(),
            time: dt.getattr("time")?.unbind(),
            decimal: dec.getattr("Decimal")?.unbind(),
        })
    })
}

fn is_instance(ob: &Bound<'_, PyAny>, ty: &Py<PyAny>) -> PyResult<bool> {
    ob.is_instance(ty.bind(ob.py()))
}

/// A literal for a Python value (§4.2 of the spec).
fn literal_from_native(py: Python<'_>, v: &Bound<'_, PyAny>) -> PyResult<Literal> {
    let typed = |s: String, dt| Ok(Literal::new_typed_literal(s, dt));
    if let Ok(b) = v.cast::<PyBool>() {
        return typed(
            if b.is_true() { "true" } else { "false" }.into(),
            xsd::BOOLEAN,
        );
    }
    if v.cast::<PyInt>().is_ok() {
        return typed(v.str()?.to_string(), xsd::INTEGER);
    }
    if let Ok(f) = v.cast::<PyFloat>() {
        let x = f.value();
        let s = if x.is_nan() {
            "NaN".to_string()
        } else if x.is_infinite() {
            if x > 0.0 { "INF" } else { "-INF" }.to_string()
        } else {
            v.repr()?.to_string()
        };
        return typed(s, xsd::DOUBLE);
    }
    if let Ok(b) = v.cast::<PyBytes>() {
        let b64 = py
            .import("base64")?
            .call_method1("b64encode", (b,))?
            .call_method0("decode")?
            .extract::<String>()?;
        return typed(b64, xsd::BASE_64_BINARY);
    }
    let types = py_types(py)?;
    if is_instance(v, &types.decimal)? {
        if !v.call_method0("is_finite")?.is_truthy()? {
            return Err(PyValueError::new_err("xsd:decimal has no NaN or infinity"));
        }
        let s: String = py
            .import("builtins")?
            .call_method1("format", (v, "f"))?
            .extract()?;
        return typed(s, xsd::DECIMAL);
    }
    // datetime before date: a datetime is a date
    for (ty, dt) in [
        (&types.datetime, xsd::DATE_TIME),
        (&types.date, xsd::DATE),
        (&types.time, xsd::TIME),
    ] {
        if is_instance(v, ty)? {
            return typed(v.call_method0("isoformat")?.extract()?, dt);
        }
    }
    Err(type_error(
        v,
        "str, bool, int, float, Decimal, datetime, date, time or bytes",
    ))
}

/// The XSD integer types whose values are Python ints.
const INTEGER_TYPES: [&str; 13] = [
    "integer",
    "int",
    "long",
    "short",
    "byte",
    "nonNegativeInteger",
    "positiveInteger",
    "nonPositiveInteger",
    "negativeInteger",
    "unsignedLong",
    "unsignedInt",
    "unsignedShort",
    "unsignedByte",
];

/// The native value of a literal (§4.2), or its lexical form.
fn literal_to_native<'py>(py: Python<'py>, l: &Literal) -> PyResult<Bound<'py, PyAny>> {
    let lexical = || Ok(PyString::new(py, l.value()).into_any());
    let dt = l.datatype();
    let Some(local) = dt
        .as_str()
        .strip_prefix("http://www.w3.org/2001/XMLSchema#")
    else {
        return lexical();
    };
    let v = l.value().trim();
    let builtins = py.import("builtins")?;
    let parsed: PyResult<Bound<'py, PyAny>> = match local {
        "boolean" => match v {
            "true" | "1" => Ok(PyBool::new(py, true).to_owned().into_any()),
            "false" | "0" => Ok(PyBool::new(py, false).to_owned().into_any()),
            _ => return lexical(),
        },
        t if INTEGER_TYPES.contains(&t) => builtins.getattr("int")?.call1((v,)),
        "double" | "float" => builtins.getattr("float")?.call1((v,)),
        "decimal" => py_types(py)?.decimal.bind(py).call1((v,)),
        "dateTime" | "dateTimeStamp" => {
            let v = v
                .strip_suffix('Z')
                .map_or(v.to_string(), |s| format!("{s}+00:00"));
            py_types(py)?
                .datetime
                .bind(py)
                .call_method1("fromisoformat", (v,))
        }
        "date" => py_types(py)?
            .date
            .bind(py)
            .call_method1("fromisoformat", (v,)),
        "time" => {
            let v = v
                .strip_suffix('Z')
                .map_or(v.to_string(), |s| format!("{s}+00:00"));
            py_types(py)?
                .time
                .bind(py)
                .call_method1("fromisoformat", (v,))
        }
        "base64Binary" => py.import("base64")?.call_method1("b64decode", (v,)),
        "hexBinary" => py.get_type::<PyBytes>().call_method1("fromhex", (v,)),
        _ => return lexical(),
    };
    // an ill-typed lexical form gives the lexical form
    parsed.or_else(|_| lexical())
}

/// Unpickle a `Literal` (see `Literal.__reduce__`).
#[pyfunction]
#[pyo3(signature = (value, datatype, language, direction))]
fn _literal(
    value: &Bound<'_, PyAny>,
    datatype: Option<&Bound<'_, PyAny>>,
    language: Option<&str>,
    direction: Option<&str>,
) -> PyResult<PyLiteral> {
    PyLiteral::new(value, datatype, language, direction)
}

/// Register the term classes.
pub fn register(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_function(wrap_pyfunction!(_literal, m)?)?;
    m.add_class::<PyNamedNode>()?;
    m.add_class::<PyBlankNode>()?;
    m.add_class::<PyLiteral>()?;
    m.add_class::<PyTriple>()?;
    m.add_class::<PyQuad>()?;
    m.add_class::<PyDefaultGraph>()?;
    m.add_class::<PyVariable>()?;
    Ok(())
}
