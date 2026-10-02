//! Iterators over quads and query results. Terms become Python objects one at a time;
//! the work that reads the store runs in batches without the GIL.

use crate::errors::EngineResult;
use crate::terms::{PyVariable, quad_to_py, term_to_py, triple_to_py};
use oxrdf::{Quad, Term, Triple};
use pyo3::exceptions::{PyIndexError, PyKeyError, PyTypeError};
use pyo3::prelude::*;
use pyo3::types::{PyDict, PyString, PyTuple};
use sparkles::QuadIter;
use sparkles::sparql::QueryResult;
use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

/// Items decoded per batch without the GIL.
const BATCH: usize = 1024;

// ------------------------------------------------------------------------- quads ----

enum QuadSource {
    /// already in memory (parse, transactions)
    List(std::vec::IntoIter<Quad>),
    /// read from the store in batches
    Scan(Box<QuadIter>),
}

struct QuadState {
    source: QuadSource,
    buf: VecDeque<Quad>,
    done: bool,
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

    fn new(source: QuadSource) -> PyQuadIterator {
        PyQuadIterator {
            state: Mutex::new(QuadState {
                source,
                buf: VecDeque::new(),
                done: false,
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
                    match &mut st.source {
                        QuadSource::List(it) => st.buf.extend(it.by_ref().take(BATCH)),
                        QuadSource::Scan(it) => {
                            for q in it.by_ref().take(BATCH) {
                                match q {
                                    Ok(q) => st.buf.push_back(q),
                                    Err(e) => {
                                        st.done = true;
                                        return Err(e);
                                    }
                                }
                            }
                        }
                    }
                    if st.buf.is_empty() {
                        st.done = true;
                    }
                }
                Ok(st.buf.pop_front())
            })
            .py(py)?;
        next.map(|q| quad_to_py(py, q)).transpose()
    }
}

// --------------------------------------------------------------------- solutions ----

struct SolState {
    result: QueryResult,
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
                result,
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
            if st.buf.is_empty() {
                let r = &st.result;
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
