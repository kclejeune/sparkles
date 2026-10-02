//! Iterators over quads and query results. Terms become Python objects one at a time;
//! the work that reads the store runs in batches without the GIL.

use crate::errors::{EngineResult, invalid};
use crate::io::{format_from_py, format_of_output, output_from_py, serialize_quads, write_output};
use crate::terms::{PyVariable, quad_to_py, term_to_py, triple_to_py};
use oxrdf::{Quad, Term, Triple};
use pyo3::exceptions::{PyIndexError, PyKeyError, PyTypeError, PyValueError};
use pyo3::prelude::*;
use pyo3::types::{PyBytes, PyDict, PyString, PyTuple};
use sparkles::QuadIter;
use sparkles::sparql::QueryResult;
use sparkles::sparql::results::SolutionsFormat;
use std::collections::{BTreeMap, VecDeque};
use std::sync::{Arc, Mutex};

/// Items decoded per batch without the GIL.
const BATCH: usize = 1024;

// ------------------------------------------------------------------------- quads ----

enum QuadSource {
    /// already in memory (transactions)
    List(std::vec::IntoIter<Quad>),
    /// read from the store in batches
    Scan(Box<QuadIter>),
    /// parsed as the input is read (parse)
    Stream(Box<dyn Iterator<Item = sparkles::Result<Quad>> + Send>),
}

struct QuadState {
    source: QuadSource,
    buf: VecDeque<Quad>,
    done: bool,
    /// the error that ended the source, raised after the quads read before it
    error: Option<sparkles::Error>,
}

/// An iterator of `Quad`.
#[pyclass(module = "sparkles", name = "QuadIterator")]
pub struct PyQuadIterator {
    state: Mutex<QuadState>,
}

impl PyQuadIterator {
    pub fn from_vec(quads: Vec<Quad>) -> PyQuadIterator {
        PyQuadIterator::new(QuadSource::List(quads.into_iter()))
    }

    pub fn from_scan(it: QuadIter) -> PyQuadIterator {
        PyQuadIterator::new(QuadSource::Scan(Box::new(it)))
    }

    pub fn from_stream(
        it: Box<dyn Iterator<Item = sparkles::Result<Quad>> + Send>,
    ) -> PyQuadIterator {
        PyQuadIterator::new(QuadSource::Stream(it))
    }

    fn new(source: QuadSource) -> PyQuadIterator {
        PyQuadIterator {
            state: Mutex::new(QuadState {
                source,
                buf: VecDeque::new(),
                done: false,
                error: None,
            }),
        }
    }
}

#[pymethods]
impl PyQuadIterator {
    fn __iter__(slf: PyRef<'_, Self>) -> PyRef<'_, Self> {
        slf
    }

    fn __next__<'py>(&self, py: Python<'py>) -> PyResult<Option<Bound<'py, PyAny>>> {
        let next = py
            .detach(|| {
                let mut st = self.state.lock().unwrap();
                let st = &mut *st;
                if st.buf.is_empty() && !st.done {
                    let fallible: &mut dyn Iterator<Item = sparkles::Result<Quad>> =
                        match &mut st.source {
                            QuadSource::List(it) => {
                                st.buf.extend(it.by_ref().take(BATCH));
                                if st.buf.is_empty() {
                                    st.done = true;
                                }
                                return Ok(st.buf.pop_front());
                            }
                            QuadSource::Scan(it) => it.as_mut(),
                            QuadSource::Stream(it) => it.as_mut(),
                        };
                    for q in fallible.take(BATCH) {
                        match q {
                            Ok(q) => st.buf.push_back(q),
                            Err(e) => {
                                // the quads before the error come first
                                st.done = true;
                                st.error = Some(e);
                                break;
                            }
                        }
                    }
                    if st.buf.is_empty() {
                        st.done = true;
                    }
                }
                match st.buf.pop_front() {
                    Some(q) => Ok(Some(q)),
                    None => st.error.take().map_or(Ok(None), Err),
                }
            })
            .py(py)?;
        next.map(|q| quad_to_py(py, q)).transpose()
    }
}

// --------------------------------------------------------------------- solutions ----

struct SolState {
    /// `None` once `serialize` has written the solutions
    result: Option<QueryResult>,
    row: usize,
    buf: VecDeque<Vec<Option<Term>>>,
}

/// The solutions of a SELECT query: an iterator of `QuerySolution`.
#[pyclass(module = "sparkles", name = "QuerySolutions")]
pub struct PyQuerySolutions {
    vars: Arc<[String]>,
    state: Mutex<SolState>,
}

impl PyQuerySolutions {
    pub fn new(result: QueryResult) -> PyQuerySolutions {
        PyQuerySolutions {
            vars: result.vars.clone().into(),
            state: Mutex::new(SolState {
                result: Some(result),
                row: 0,
                buf: VecDeque::new(),
            }),
        }
    }
}

#[pymethods]
impl PyQuerySolutions {
    /// The projected variables, in column order.
    #[getter]
    fn variables(&self) -> Vec<PyVariable> {
        self.vars
            .iter()
            .map(|v| PyVariable { name: v.clone() })
            .collect()
    }

    fn __iter__(slf: PyRef<'_, Self>) -> PyRef<'_, Self> {
        slf
    }

    fn __next__(&self, py: Python<'_>) -> Option<PyQuerySolution> {
        let values = py.detach(|| {
            let mut st = self.state.lock().unwrap();
            let st = &mut *st;
            if st.buf.is_empty()
                && let Some(r) = &st.result
            {
                let end = (st.row + BATCH).min(r.table.len());
                for i in st.row..end {
                    st.buf.push_back(
                        (0..r.table.width())
                            .map(|c| r.term(r.table.get(i, c)))
                            .collect(),
                    );
                }
                st.row = end;
            }
            st.buf.pop_front()
        })?;
        Some(PyQuerySolution {
            vars: self.vars.clone(),
            values,
        })
    }

    /// Write the solutions as SPARQL results in `format` (`json`, `xml`, `csv` or `tsv`,
    /// or a media type). Returns bytes when `output` is `None`, writes a file for a path,
    /// and writes to a binary file object otherwise. It consumes the solutions, and it
    /// must come before any are iterated.
    #[pyo3(signature = (output = None, format = "json"))]
    fn serialize<'py>(
        &self,
        py: Python<'py>,
        output: Option<&Bound<'py, PyAny>>,
        format: &str,
    ) -> PyResult<Option<Bound<'py, PyBytes>>> {
        let Some(fmt) = SolutionsFormat::from_name(format) else {
            return Err(PyValueError::new_err(format!(
                "unknown SPARQL results format {format:?}: use json, xml, csv or tsv"
            )));
        };
        let out = output_from_py(output)?;
        let result = {
            let mut st = self.state.lock().unwrap();
            if st.row > 0 {
                return Err(invalid(py, "serialize the solutions before iterating them"));
            }
            st.result.take()
        };
        let Some(result) = result else {
            return Err(invalid(py, "the solutions have already been serialized"));
        };
        write_output(py, out, None, move |w| {
            sparkles::sparql::results::write_solutions(&result, fmt, w, None).map(|_| 0)
        })
    }

    fn __repr__(&self) -> String {
        format!("<QuerySolutions variables={:?}>", self.vars)
    }
}

/// One solution: a tuple of terms (`None` for unbound variables) that also maps
/// variable names to terms.
#[pyclass(frozen, module = "sparkles", name = "QuerySolution")]
pub struct PyQuerySolution {
    vars: Arc<[String]>,
    values: Vec<Option<Term>>,
}

impl PyQuerySolution {
    fn index(&self, key: &Bound<'_, PyAny>) -> PyResult<usize> {
        if let Ok(i) = key.extract::<isize>() {
            let n = self.values.len() as isize;
            let j = if i < 0 { i + n } else { i };
            if !(0..n).contains(&j) {
                return Err(PyIndexError::new_err("solution index out of range"));
            }
            return Ok(j as usize);
        }
        let name = if let Ok(v) = key.cast::<PyVariable>() {
            v.get().name.clone()
        } else if let Ok(s) = key.cast::<PyString>() {
            s.to_str()?.trim_start_matches(['?', '$']).to_string()
        } else {
            return Err(PyTypeError::new_err(
                "a solution is indexed by int, str or Variable",
            ));
        };
        self.vars
            .iter()
            .position(|v| *v == name)
            .ok_or_else(|| PyKeyError::new_err(name))
    }

    fn value<'py>(&self, py: Python<'py>, i: usize) -> PyResult<Option<Bound<'py, PyAny>>> {
        self.values[i]
            .as_ref()
            .map(|t| term_to_py(py, t))
            .transpose()
    }
}

#[pymethods]
impl PyQuerySolution {
    fn __getitem__<'py>(
        &self,
        py: Python<'py>,
        key: &Bound<'py, PyAny>,
    ) -> PyResult<Option<Bound<'py, PyAny>>> {
        let i = self.index(key)?;
        self.value(py, i)
    }

    /// The term of a variable, or `default` when it is unbound or not projected.
    #[pyo3(signature = (key, default = None))]
    fn get<'py>(
        &self,
        py: Python<'py>,
        key: &Bound<'py, PyAny>,
        default: Option<Bound<'py, PyAny>>,
    ) -> PyResult<Option<Bound<'py, PyAny>>> {
        match self.index(key) {
            Ok(i) => Ok(self.value(py, i)?.or(default)),
            Err(e) if e.is_instance_of::<PyKeyError>(py) => Ok(default),
            Err(e) => Err(e),
        }
    }

    fn __len__(&self) -> usize {
        self.values.len()
    }

    fn __iter__<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        Ok(self.tuple(py)?.try_iter()?.into_any())
    }

    fn __contains__(&self, name: &Bound<'_, PyAny>) -> bool {
        self.index(name)
            .is_ok_and(|i| name.extract::<isize>().is_err() && self.values[i].is_some())
    }

    /// The bound variables and their terms.
    fn as_dict<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyDict>> {
        let d = PyDict::new(py);
        for (v, t) in self.vars.iter().zip(&self.values) {
            if let Some(t) = t {
                d.set_item(v, term_to_py(py, t)?)?;
            }
        }
        Ok(d)
    }

    /// The projected variable names.
    fn keys(&self) -> Vec<String> {
        self.vars.to_vec()
    }

    fn __repr__(&self, py: Python<'_>) -> PyResult<String> {
        let mut parts = Vec::new();
        for (v, t) in self.vars.iter().zip(&self.values) {
            if let Some(t) = t {
                parts.push(format!("{v}={}", term_to_py(py, t)?.repr()?));
            }
        }
        Ok(format!("<QuerySolution {}>", parts.join(" ")))
    }
}

impl PyQuerySolution {
    fn tuple<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyTuple>> {
        let items = (0..self.values.len())
            .map(|i| self.value(py, i))
            .collect::<PyResult<Vec<_>>>()?;
        PyTuple::new(py, items)
    }
}

// ----------------------------------------------------------------------- triples ----

/// The triples of a CONSTRUCT or DESCRIBE query: an iterator of `Triple`.
#[pyclass(module = "sparkles", name = "QueryTriples")]
pub struct PyQueryTriples {
    triples: Mutex<std::vec::IntoIter<Triple>>,
}

impl PyQueryTriples {
    pub fn new(triples: Vec<Triple>) -> PyQueryTriples {
        PyQueryTriples {
            triples: Mutex::new(triples.into_iter()),
        }
    }
}

#[pymethods]
impl PyQueryTriples {
    fn __iter__(slf: PyRef<'_, Self>) -> PyRef<'_, Self> {
        slf
    }

    fn __next__<'py>(&self, py: Python<'py>) -> PyResult<Option<Bound<'py, PyAny>>> {
        let next = self.triples.lock().unwrap().next();
        next.map(|t| triple_to_py(py, t)).transpose()
    }

    /// Write the triples not yet iterated in an RDF format (Turtle by default). Returns
    /// bytes when `output` is `None`, writes a file for a path, and writes to a binary
    /// file object otherwise. It consumes the triples.
    #[pyo3(signature = (output = None, format = None, *, prefixes = None))]
    fn serialize<'py>(
        &self,
        py: Python<'py>,
        output: Option<&Bound<'py, PyAny>>,
        format: Option<&Bound<'py, PyAny>>,
        prefixes: Option<BTreeMap<String, String>>,
    ) -> PyResult<Option<Bound<'py, PyBytes>>> {
        let out = output_from_py(output)?;
        let format = format_from_py(format)?
            .or_else(|| format_of_output(&out))
            .unwrap_or(oxrdfio::RdfFormat::Turtle);
        let triples: Vec<Triple> = self.triples.lock().unwrap().by_ref().collect();
        let prefixes = prefixes.unwrap_or_default();
        write_output(py, out, None, move |w| {
            let quads = triples
                .into_iter()
                .map(|t| Ok(t.in_graph(oxrdf::GraphName::DefaultGraph)));
            serialize_quads(w, format, prefixes, quads, false)
        })
    }
}

// ------------------------------------------------------------------------ update ----

/// What a SPARQL Update request changed.
#[pyclass(frozen, module = "sparkles", name = "UpdateStats", skip_from_py_object)]
pub struct PyUpdateStats {
    /// quads inserted, summed over the operations
    #[pyo3(get)]
    pub inserted: u64,
    /// quads deleted, summed over the operations
    #[pyo3(get)]
    pub deleted: u64,
    #[pyo3(get)]
    pub operations: usize,
}

impl From<sparkles::sparql::update::UpdateStats> for PyUpdateStats {
    fn from(s: sparkles::sparql::update::UpdateStats) -> Self {
        PyUpdateStats {
            inserted: s.inserted,
            deleted: s.deleted,
            operations: s.operations,
        }
    }
}

#[pymethods]
impl PyUpdateStats {
    fn __repr__(&self) -> String {
        format!(
            "UpdateStats(inserted={}, deleted={}, operations={})",
            self.inserted, self.deleted, self.operations
        )
    }
}

pub fn register(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_class::<PyQuadIterator>()?;
    m.add_class::<PyQuerySolutions>()?;
    m.add_class::<PyQuerySolution>()?;
    m.add_class::<PyQueryTriples>()?;
    m.add_class::<PyUpdateStats>()?;
    Ok(())
}
