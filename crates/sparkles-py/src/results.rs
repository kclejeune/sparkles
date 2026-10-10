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
use sparkles::id::Id;
use sparkles::sparql::QueryResult;
use sparkles::sparql::results::SolutionsFormat;
use sparkles::store::Snapshot;
use std::collections::{BTreeMap, VecDeque};
use std::sync::{Arc, Mutex};

/// Items decoded per batch without the GIL.
const BATCH: usize = 1024;
/// Quads of the first batch of a scan of a dataset in memory, which is read with the GIL
/// held: reading a few quads from memory takes less time than giving up the GIL and
/// taking it back.
const INLINE: usize = 16;

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
    /// the first refill reads `INLINE` quads with the GIL held (a scan of memory)
    inline: bool,
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

    /// A scan of a dataset in memory, whose first few quads are read with the GIL held.
    pub fn from_memory_scan(it: QuadIter) -> PyQuadIterator {
        let it = PyQuadIterator::new(QuadSource::Scan(Box::new(it)));
        it.state.lock().unwrap().inline = true;
        it
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
                inline: false,
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
        // A decoded quad is handed out without giving up the GIL. Only a refill, which
        // reads the store, runs detached. Nothing holds the lock while it needs the GIL.
        let ready = {
            let mut st = self.state.lock().unwrap();
            if st.inline {
                st.inline = false;
                refill(&mut st, INLINE);
                if st.buf.is_empty() {
                    return st.error.take().map_or(Ok(None), |e| Err(e).py(py));
                }
            }
            st.buf.pop_front()
        };
        if let Some(q) = ready {
            return quad_to_py(py, q).map(Some);
        }
        let next = py
            .detach(|| {
                let mut st = self.state.lock().unwrap();
                refill(&mut st, BATCH);
                match st.buf.pop_front() {
                    Some(q) => Ok(Some(q)),
                    None => st.error.take().map_or(Ok(None), Err),
                }
            })
            .py(py)?;
        next.map(|q| quad_to_py(py, q)).transpose()
    }
}

impl PyQuadIterator {
    /// The next batch of quads, empty once the source is drained (`sparkles.rdflib`
    /// converts whole batches). An error is raised after the quads read before it.
    pub(crate) fn take_batch(&self, py: Python<'_>) -> PyResult<Vec<Quad>> {
        {
            let mut st = self.state.lock().unwrap();
            if !st.buf.is_empty() {
                return Ok(st.buf.drain(..).collect());
            }
        }
        py.detach(|| {
            let mut st = self.state.lock().unwrap();
            refill(&mut st, BATCH);
            let quads: Vec<Quad> = st.buf.drain(..).collect();
            match st.error.take() {
                Some(e) if quads.is_empty() => Err(e),
                Some(e) => {
                    st.error = Some(e);
                    Ok(quads)
                }
                None => Ok(quads),
            }
        })
        .py(py)
    }
}

impl PyQuadIterator {
    /// The next batch of a scan as engine ids, with the snapshot that decodes them, for
    /// `sparkles.rdflib`, which converts each distinct id of a batch once. `None` when the
    /// source is not a scan, when decoded quads are waiting, or once the scan is done, and
    /// then `take_batch` serves the call, including any error the scan ended with.
    pub(crate) fn take_ids(&self, py: Python<'_>) -> Option<(Arc<Snapshot>, Vec<[Id; 4]>)> {
        let mut st = self.state.lock().unwrap();
        if !matches!(st.source, QuadSource::Scan(_)) || !st.buf.is_empty() || st.done {
            return None;
        }
        if st.inline {
            // a scan of memory: its first few quads are read with the GIL held
            st.inline = false;
            return read_ids(&mut st, INLINE);
        }
        drop(st);
        py.detach(|| read_ids(&mut self.state.lock().unwrap(), BATCH))
    }
}

/// Read up to `limit` quads of a scan as ids. A short batch ends the scan, as in `refill`.
fn read_ids(st: &mut QuadState, limit: usize) -> Option<(Arc<Snapshot>, Vec<[Id; 4]>)> {
    let QuadSource::Scan(it) = &mut st.source else {
        return None;
    };
    let snap = it.snapshot().clone();
    let mut ids = Vec::with_capacity(limit);
    while ids.len() < limit {
        match it.next_ids() {
            Some(Ok(q)) => ids.push(q),
            Some(Err(e)) => {
                st.done = true;
                st.error = Some(e);
                break;
            }
            None => {
                st.done = true;
                break;
            }
        }
    }
    Some((snap, ids))
}

/// Read the next batch of up to `limit` quads from the source into an empty buffer. A
/// batch that comes out short ends the source, so that no later call has to ask it again.
fn refill(st: &mut QuadState, limit: usize) {
    if !st.buf.is_empty() || st.done {
        return;
    }
    let fallible: &mut dyn Iterator<Item = sparkles::Result<Quad>> = match &mut st.source {
        QuadSource::List(it) => {
            st.buf.extend(it.by_ref().take(limit));
            if st.buf.len() < limit {
                st.done = true;
            }
            return;
        }
        QuadSource::Scan(it) => it.as_mut(),
        QuadSource::Stream(it) => it.as_mut(),
    };
    let mut read = 0;
    for q in fallible.take(limit) {
        read += 1;
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
    if read < limit {
        st.done = true;
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
    /// The next batch of rows, empty once all have been handed out (`sparkles.rdflib`
    /// converts whole batches).
    pub(crate) fn take_batch(&self, py: Python<'_>) -> Vec<Vec<Option<Term>>> {
        {
            let mut st = self.state.lock().unwrap();
            if !st.buf.is_empty() {
                return st.buf.drain(..).collect();
            }
        }
        py.detach(|| {
            let mut st = self.state.lock().unwrap();
            fill_solutions(&mut st);
            st.buf.drain(..).collect()
        })
    }

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
        // as for quads, only a refill gives up the GIL
        let ready = self.state.lock().unwrap().buf.pop_front();
        if let Some(values) = ready {
            return Some(PyQuerySolution {
                vars: self.vars.clone(),
                values,
            });
        }
        let values = py.detach(|| {
            let mut st = self.state.lock().unwrap();
            fill_solutions(&mut st);
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

/// Decode the next batch of rows into an empty buffer.
fn fill_solutions(st: &mut SolState) {
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
}

/// One solution: a tuple of terms (`None` for unbound variables) that also maps
/// variable names to terms.
#[pyclass(frozen, module = "sparkles", name = "QuerySolution")]
pub struct PyQuerySolution {
    vars: Arc<[String]>,
    values: Vec<Option<Term>>,
}

impl PyQuerySolution {
    pub(crate) fn new(vars: Arc<[String]>, values: Vec<Option<Term>>) -> Self {
        Self { vars, values }
    }
    fn index(&self, key: &Bound<'_, PyAny>) -> PyResult<usize> {
        // A name is the common key. It is tried first, because a failed integer
        // conversion makes and discards a Python exception.
        let position = |name: &str| {
            self.vars
                .iter()
                .position(|v| v == name)
                .ok_or_else(|| PyKeyError::new_err(name.to_string()))
        };
        if let Ok(s) = key.cast::<PyString>() {
            return position(s.to_str()?.trim_start_matches(['?', '$']));
        }
        if let Ok(v) = key.cast::<PyVariable>() {
            return position(&v.get().name);
        }
        if let Ok(i) = key.extract::<isize>() {
            let n = self.values.len() as isize;
            let j = if i < 0 { i + n } else { i };
            if !(0..n).contains(&j) {
                return Err(PyIndexError::new_err("solution index out of range"));
            }
            return Ok(j as usize);
        }
        Err(PyTypeError::new_err(
            "a solution is indexed by int, str or Variable",
        ))
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
    /// the quads of a CONSTRUCT's `GRAPH` blocks (Jena ARQ)
    quads: Vec<Quad>,
}

impl PyQueryTriples {
    pub fn new(triples: Vec<Triple>, quads: Vec<Quad>) -> PyQueryTriples {
        PyQueryTriples {
            triples: Mutex::new(triples.into_iter()),
            quads,
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

    /// The quads in named graphs of a CONSTRUCT with Jena ARQ's `GRAPH` template blocks
    /// (the default graph's triples are the iteration).
    #[getter]
    fn quads<'py>(&self, py: Python<'py>) -> PyResult<Vec<Bound<'py, PyAny>>> {
        self.quads
            .iter()
            .map(|q| quad_to_py(py, q.clone()))
            .collect()
    }

    /// Write the triples not yet iterated in an RDF format (Turtle by default), and in a
    /// dataset format (TriG, N-Quads) the quads too. Returns bytes when `output` is
    /// `None`, writes a file for a path, and writes to a binary file object otherwise.
    /// It consumes the triples.
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
            .unwrap_or(crate::io::Fmt::Rdf(oxrdfio::RdfFormat::Turtle));
        let triples: Vec<Triple> = self.triples.lock().unwrap().by_ref().collect();
        let named = if format.supports_datasets() {
            self.quads.clone()
        } else {
            Vec::new()
        };
        let prefixes = prefixes.unwrap_or_default();
        write_output(py, out, None, move |w| {
            let quads = triples
                .into_iter()
                .map(|t| Ok(t.in_graph(oxrdf::GraphName::DefaultGraph)))
                .chain(named.into_iter().map(Ok));
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

/// What applying an RDF Patch did.
#[pyclass(frozen, module = "sparkles", name = "PatchStats", skip_from_py_object)]
pub struct PyPatchStats {
    /// whether the patch made a commit
    #[pyo3(get)]
    pub committed: bool,
    /// the commit, or the unchanged head
    #[pyo3(get)]
    pub commit: u64,
    /// `A` rows that added a quad
    #[pyo3(get)]
    pub inserted: u64,
    /// `D` rows that removed a quad
    #[pyo3(get)]
    pub deleted: u64,
    /// the rows read
    #[pyo3(get)]
    pub rows: u64,
    /// a `TA` row aborted the patch
    #[pyo3(get)]
    pub aborted: bool,
    /// the patch's `prev` named a commit of this dataset, which was the head
    #[pyo3(get)]
    pub prev_checked: bool,
    #[pyo3(get)]
    pub prefixes_set: u64,
    #[pyo3(get)]
    pub prefixes_removed: u64,
}

impl From<sparkles::store::PatchOutcome> for PyPatchStats {
    fn from(o: sparkles::store::PatchOutcome) -> Self {
        PyPatchStats {
            committed: o.receipt.committed,
            commit: o.receipt.commit.seq,
            inserted: o.inserted,
            deleted: o.deleted,
            rows: o.rows,
            aborted: o.aborted,
            prev_checked: o.prev_checked,
            prefixes_set: o.prefixes_set,
            prefixes_removed: o.prefixes_removed,
        }
    }
}

#[pymethods]
impl PyPatchStats {
    fn __repr__(&self) -> String {
        format!(
            "PatchStats(committed={}, commit={}, inserted={}, deleted={}, rows={}, aborted={})",
            if self.committed { "True" } else { "False" },
            self.commit,
            self.inserted,
            self.deleted,
            self.rows,
            if self.aborted { "True" } else { "False" },
        )
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
    m.add_class::<PyPatchStats>()?;
    Ok(())
}
