//! Batch conversion of quads and solutions to rdflib nodes for `sparkles.rdflib`.
//!
//! The store plugin hands rdflib one node per term. Building each one in Python from a
//! Sparkles term object costs several attribute reads and a dictionary lookup per term.
//! `_RdflibNodes` does the common cases here instead: it keeps the rdflib node of each
//! IRI and literal it has seen, and turns a whole batch of quads or solutions into tuples
//! of nodes in one call. Every other term (blank nodes, the plugin's own
//! `urn:x-sparkles:rdflib:` IRIs and triple terms) goes to the plugin's Python
//! conversion, whose answer can change with the plugin's state and is never kept here.

use crate::results::{PyQuadIterator, PyQuerySolutions};
use crate::terms::{graph_to_py, term_to_py};
use oxrdf::{Literal, NamedNode, NamedOrBlankNode, Term};
use pyo3::PyTraverseError;
use pyo3::exceptions::PyValueError;
use pyo3::gc::PyVisit;
use pyo3::prelude::*;
use pyo3::types::{PyList, PyTuple};
use std::collections::HashMap;
use std::sync::Mutex;

/// Nodes kept at most; the cache starts again when it is full, as the plugin's own does.
const CACHE_LIMIT: usize = 100_000;

#[derive(Default)]
struct Cache {
    iris: HashMap<String, Py<PyAny>>,
    literals: HashMap<Literal, Py<PyAny>>,
}

impl Cache {
    /// Start again when full. The old nodes are returned to be dropped after the lock
    /// is released, since dropping a Python object can run Python code.
    fn make_room(&mut self) -> Option<Cache> {
        (self.iris.len() + self.literals.len() >= CACHE_LIMIT).then(|| std::mem::take(self))
    }
}

#[pyclass(module = "sparkles", name = "_RdflibNodes")]
pub struct PyRdflibNodes {
    /// `rdflib.URIRef`
    uriref: Option<Py<PyAny>>,
    /// the plugin's conversion of one Sparkles term, a method of the store, which holds
    /// this object in turn (the cycle is visible to Python's garbage collector)
    convert: Option<Py<PyAny>>,
    /// IRIs with this prefix are the plugin's own and go to `convert`
    prefix: String,
    /// Held only to look up or insert, never while Python code runs, so that a thread
    /// waiting for it never holds the GIL that the holder needs.
    cache: Mutex<Cache>,
}

impl PyRdflibNodes {
    fn convert<'py>(&self, py: Python<'py>) -> PyResult<&Bound<'py, PyAny>> {
        self.convert
            .as_ref()
            .map(|c| c.bind(py))
            .ok_or_else(|| PyValueError::new_err("the node converter has been cleared"))
    }

    fn uriref<'py>(&self, py: Python<'py>) -> PyResult<&Bound<'py, PyAny>> {
        self.uriref
            .as_ref()
            .map(|c| c.bind(py))
            .ok_or_else(|| PyValueError::new_err("the node converter has been cleared"))
    }

    fn iri(&self, py: Python<'_>, n: &NamedNode) -> PyResult<Py<PyAny>> {
        let iri = n.as_str();
        if iri.starts_with(&self.prefix) {
            return Ok(self
                .convert(py)?
                .call1((term_to_py(py, &n.clone().into())?,))?
                .unbind());
        }
        if let Some(node) = self.cache.lock().unwrap().iris.get(iri) {
            return Ok(node.clone_ref(py));
        }
        let node = self.uriref(py)?.call1((iri,))?.unbind();
        let old = {
            let mut cache = self.cache.lock().unwrap();
            let old = cache.make_room();
            cache.iris.insert(iri.to_string(), node.clone_ref(py));
            old
        };
        drop(old);
        Ok(node)
    }

    fn node(&self, py: Python<'_>, t: &Term) -> PyResult<Py<PyAny>> {
        match t {
            Term::NamedNode(n) => self.iri(py, n),
            Term::Literal(l) => {
                if let Some(node) = self.cache.lock().unwrap().literals.get(l) {
                    return Ok(node.clone_ref(py));
                }
                let node = self.convert(py)?.call1((term_to_py(py, t)?,))?.unbind();
                let old = {
                    let mut cache = self.cache.lock().unwrap();
                    let old = cache.make_room();
                    cache.literals.insert(l.clone(), node.clone_ref(py));
                    old
                };
                drop(old);
                Ok(node)
            }
            _ => Ok(self.convert(py)?.call1((term_to_py(py, t)?,))?.unbind()),
        }
    }

    fn subject(&self, py: Python<'_>, s: &NamedOrBlankNode) -> PyResult<Py<PyAny>> {
        match s {
            NamedOrBlankNode::NamedNode(n) => self.iri(py, n),
            NamedOrBlankNode::BlankNode(b) => Ok(self
                .convert(py)?
                .call1((term_to_py(py, &b.clone().into())?,))?
                .unbind()),
        }
    }
}

#[pymethods]
impl PyRdflibNodes {
    #[new]
    fn new(uriref: Py<PyAny>, convert: Py<PyAny>, prefix: String) -> Self {
        PyRdflibNodes {
            uriref: Some(uriref),
            convert: Some(convert),
            prefix,
            cache: Mutex::new(Cache::default()),
        }
    }

    /// The next batch of `quads` as `(s, p, o)` tuples of rdflib nodes, or with
    /// `graphs` as `(s, p, o, graph)` with the graph as a Sparkles term. `None` once the
    /// iterator is drained.
    #[pyo3(signature = (quads, graphs = false))]
    fn triples<'py>(
        &self,
        py: Python<'py>,
        quads: &PyQuadIterator,
        graphs: bool,
    ) -> PyResult<Option<Bound<'py, PyList>>> {
        let batch = quads.take_batch(py)?;
        if batch.is_empty() {
            return Ok(None);
        }
        let mut rows = Vec::with_capacity(batch.len());
        for q in &batch {
            let s = self.subject(py, &q.subject)?;
            let p = self.iri(py, &q.predicate)?;
            let o = self.node(py, &q.object)?;
            let row = if graphs {
                PyTuple::new(py, [s, p, o, graph_to_py(py, &q.graph_name)?.unbind()])?
            } else {
                PyTuple::new(py, [s, p, o])?
            };
            rows.push(row);
        }
        Ok(Some(PyList::new(py, rows)?))
    }

    /// The next batch of `solutions` as tuples of rdflib nodes, `None` for an unbound
    /// variable. `None` once every solution has been handed out.
    fn solutions<'py>(
        &self,
        py: Python<'py>,
        solutions: &PyQuerySolutions,
    ) -> PyResult<Option<Bound<'py, PyList>>> {
        let batch = solutions.take_batch(py);
        if batch.is_empty() {
            return Ok(None);
        }
        let mut rows = Vec::with_capacity(batch.len());
        for row in &batch {
            let mut nodes = Vec::with_capacity(row.len());
            for t in row {
                nodes.push(match t {
                    Some(t) => self.node(py, t)?,
                    None => py.None(),
                });
            }
            rows.push(PyTuple::new(py, nodes)?);
        }
        Ok(Some(PyList::new(py, rows)?))
    }

    fn __traverse__(&self, visit: PyVisit<'_>) -> Result<(), PyTraverseError> {
        if let Some(c) = &self.convert {
            visit.call(c)?;
        }
        if let Some(u) = &self.uriref {
            visit.call(u)?;
        }
        Ok(())
    }

    fn __clear__(&mut self) {
        self.convert = None;
        self.uriref = None;
    }

    /// Forget the kept nodes.
    fn clear(&self) {
        let old = std::mem::take(&mut *self.cache.lock().unwrap());
        drop(old);
    }
}

pub fn register(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_class::<PyRdflibNodes>()
}
