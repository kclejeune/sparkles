//! `sparkles.Dataset`: a persistent or in-memory dataset. Every call that reads or
//! writes the store runs without the GIL.

use crate::errors::{EngineResult, invalid, new_err};
use crate::io::{
    Output, codec_from_py, format_from_py, format_of_output, output_from_py, serialize_quads,
    source_from_py, write_output,
};
use crate::results::{PyQuadIterator, PyQuerySolutions, PyQueryTriples, PyUpdateStats};
use crate::terms::{
    PyVariable, graph_from_py, graph_to_py, iri_from_py, named_node_from_py, opt, quad_from_py,
    subject_from_py, term_from_py,
};
use crate::txn::{PyTransaction, WriterSlot};
use oxrdf::{GraphName, NamedOrBlankNode, Quad, Term};
use pyo3::exceptions::PyTypeError;
use pyo3::prelude::*;
use pyo3::types::{PyBytes, PyString};
use sparkles::sparql::{QueryKind, QueryOptions};
use sparkles::store::{Store, StoreOptions};
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, RwLock};
use std::time::Duration;

/// The named graph that `reason` writes and `include_inferred` reads.
pub const INFERRED_GRAPH: &str = "urn:x-sparkles:inferred";

/// An RDF dataset: the default graph and named graphs, in memory or in a database
/// directory.
#[pyclass(frozen, module = "sparkles", name = "Dataset")]
pub struct PyDataset {
    inner: RwLock<Option<sparkles::Dataset>>,
    path: Option<String>,
    writer: WriterSlot,
}

/// Arguments shared by the query methods.
#[derive(Default)]
struct QueryArgs<'py> {
    base_iri: Option<String>,
    prefixes: Option<BTreeMap<String, String>>,
    bindings: Option<Bound<'py, PyAny>>,
    default_graph: Option<Bound<'py, PyAny>>,
    named_graphs: Option<Bound<'py, PyAny>>,
    include_inferred: bool,
    timeout: Option<f64>,
}

impl PyDataset {
    fn from_dataset(ds: sparkles::Dataset, path: Option<String>) -> PyDataset {
        PyDataset {
            inner: RwLock::new(Some(ds)),
            path,
            writer: Arc::new(Mutex::new(None)),
        }
    }

    fn open_at(py: Python<'_>, path: PathBuf, union_default_graph: bool) -> PyResult<PyDataset> {
        let opts = StoreOptions {
            union_default_graph,
            ..Default::default()
        };
        let shown = path.display().to_string();
        let ds = py
            .detach(|| sparkles::Dataset::open_with(&path, opts))
            .py(py)?;
        Ok(PyDataset::from_dataset(ds, Some(shown)))
    }

    fn in_memory(union_default_graph: bool) -> PyDataset {
        let opts = StoreOptions {
            union_default_graph,
            ..Default::default()
        };
        PyDataset::from_dataset(sparkles::Dataset::from_store(Store::in_memory(opts)), None)
    }

    /// The engine's handle (a cheap clone), or an error once closed.
    fn ds(&self, py: Python<'_>) -> PyResult<sparkles::Dataset> {
        self.inner
            .read()
            .unwrap()
            .clone()
            .ok_or_else(|| invalid(py, "the dataset is closed"))
    }

    /// The handle for a write: refused on the thread whose transaction is open, where
    /// it would wait for its own lock.
    fn ds_for_write(&self, py: Python<'_>) -> PyResult<sparkles::Dataset> {
        if *self.writer.lock().unwrap() == Some(std::thread::current().id()) {
            return Err(new_err(
                py,
                "ConflictError",
                "this thread has an open transaction on the dataset: write through the transaction",
            ));
        }
        self.ds(py)
    }

    fn query_options(&self, args: &QueryArgs<'_>) -> PyResult<QueryOptions> {
        let mut opts = QueryOptions {
            base_iri: args.base_iri.clone(),
            prefixes: args
                .prefixes
                .clone()
                .unwrap_or_default()
                .into_iter()
                .collect(),
            timeout: args.timeout.map(Duration::from_secs_f64),
            ..Default::default()
        };
        if let Some(b) = &args.bindings {
            let items = b.call_method0("items")?;
            for item in items.try_iter()? {
                let item = item?;
                let (k, v): (Bound<'_, PyAny>, Bound<'_, PyAny>) = item.extract()?;
                let name = if let Ok(var) = k.cast::<PyVariable>() {
                    var.get().name.clone()
                } else if let Ok(s) = k.cast::<PyString>() {
                    s.to_str()?.trim_start_matches(['?', '$']).to_string()
                } else {
                    return Err(PyTypeError::new_err("binding names are str or Variable"));
                };
                opts.initial_bindings.push((name, term_from_py(&v)?));
            }
        }
        let iris = |ob: &Option<Bound<'_, PyAny>>| -> PyResult<Vec<String>> {
            match ob {
                None => Ok(Vec::new()),
                Some(ob) => ob
                    .try_iter()?
                    .map(|g| Ok(iri_from_py(&g?)?.into_string()))
                    .collect(),
            }
        };
        opts.default_graph_uris = iris(&args.default_graph)?;
        opts.named_graph_uris = iris(&args.named_graphs)?;
        if args.include_inferred {
            opts.default_graph_extra = vec![INFERRED_GRAPH.to_string()];
        }
        Ok(opts)
    }

    fn run_query<'py>(
        &self,
        py: Python<'py>,
        query: &str,
        args: QueryArgs<'py>,
        want: Option<&[QueryKind]>,
    ) -> PyResult<Bound<'py, PyAny>> {
        let ds = self.ds(py)?;
        let opts = self.query_options(&args)?;
        let r = py.detach(|| ds.query_with(query, &opts)).py(py)?;
        if let Some(want) = want
            && !want.contains(&r.kind)
        {
            let name = |k: &QueryKind| format!("{k:?}").to_uppercase();
            let names: Vec<String> = want.iter().map(name).collect();
            return Err(invalid(
                py,
                format!(
                    "not a {} query (it is a {} query)",
                    names.join(" or "),
                    name(&r.kind)
                ),
            ));
        }
        Ok(match r.kind {
            QueryKind::Select => PyQuerySolutions::new(r).into_pyobject(py)?.into_any(),
            QueryKind::Ask => pyo3::types::PyBool::new(py, r.boolean)
                .to_owned()
                .into_any(),
            QueryKind::Construct | QueryKind::Describe => {
                PyQueryTriples::new(r.triples).into_pyobject(py)?.into_any()
            }
        })
    }
}

#[pymethods]
impl PyDataset {
    /// `Dataset()` is in memory; `Dataset(path)` opens or creates a database directory.
    #[new]
    #[pyo3(signature = (path = None, *, union_default_graph = false))]
    fn new(py: Python<'_>, path: Option<PathBuf>, union_default_graph: bool) -> PyResult<Self> {
        match path {
            Some(p) => PyDataset::open_at(py, p, union_default_graph),
            None => Ok(PyDataset::in_memory(union_default_graph)),
        }
    }

    /// A new, empty in-memory dataset.
    #[staticmethod]
    #[pyo3(signature = (*, union_default_graph = false))]
    fn memory(union_default_graph: bool) -> Self {
        PyDataset::in_memory(union_default_graph)
    }

    /// Open (or create) a database directory; it stays locked while open.
    #[staticmethod]
    #[pyo3(signature = (path, *, union_default_graph = false))]
    fn open(py: Python<'_>, path: PathBuf, union_default_graph: bool) -> PyResult<Self> {
        PyDataset::open_at(py, path, union_default_graph)
    }

    /// The database directory, or `None` in memory.
    #[getter]
    fn path(&self) -> Option<&str> {
        self.path.as_deref()
    }

    /// Release this handle. The directory lock goes once the iterators and transactions
    /// still using the dataset are gone.
    fn close(&self, py: Python<'_>) {
        let ds = self.inner.write().unwrap().take();
        py.detach(|| drop(ds));
    }

    #[getter]
    fn closed(&self) -> bool {
        self.inner.read().unwrap().is_none()
    }

    fn __enter__(slf: PyRef<'_, Self>) -> PyRef<'_, Self> {
        slf
    }

    #[pyo3(signature = (*_args))]
    fn __exit__(&self, py: Python<'_>, _args: &Bound<'_, pyo3::types::PyTuple>) -> bool {
        self.close(py);
        false
    }

    fn __repr__(&self) -> String {
        match (&self.path, self.closed()) {
            (_, true) => "<Dataset (closed)>".into(),
            (Some(p), false) => format!("<Dataset path={p:?}>"),
            (None, false) => "<Dataset in memory>".into(),
        }
    }

    // ---------------------------------------------------------------- loading ----

    /// Load RDF from `input` (str, bytes or a binary file object) or the file at
    /// `path`; returns the number of new quads.
    #[pyo3(signature = (input = None, format = None, *, path = None, base_iri = None, to_graph = None, compression = None, lenient = false))]
    #[allow(clippy::too_many_arguments)]
    fn load(
        &self,
        py: Python<'_>,
        input: Option<&Bound<'_, PyAny>>,
        format: Option<&Bound<'_, PyAny>>,
        path: Option<PathBuf>,
        base_iri: Option<String>,
        to_graph: Option<&Bound<'_, PyAny>>,
        compression: Option<&str>,
        lenient: bool,
    ) -> PyResult<u64> {
        let graph = opt(to_graph, iri_from_py)?;
        let src = source_from_py(
            py,
            input,
            format,
            path,
            base_iri,
            graph,
            compression,
            lenient,
        )?;
        let ds = self.ds_for_write(py)?;
        py.detach(|| ds.store().load(&[src])).py(py)
    }

    /// Load many files in one commit (the parallel bulk path); returns the new quads.
    #[pyo3(signature = (paths, *, to_graph = None))]
    fn load_files(
        &self,
        py: Python<'_>,
        paths: Vec<PathBuf>,
        to_graph: Option<&Bound<'_, PyAny>>,
    ) -> PyResult<u64> {
        let graph = opt(to_graph, iri_from_py)?;
        let sources = paths
            .iter()
            .map(|p| {
                source_from_py(
                    py,
                    None,
                    None,
                    Some(p.clone()),
                    None,
                    graph.clone(),
                    None,
                    false,
                )
            })
            .collect::<PyResult<Vec<_>>>()?;
        let ds = self.ds_for_write(py)?;
        py.detach(|| ds.store().load(&sources)).py(py)
    }

    // ----------------------------------------------------------------- SPARQL ----

    /// Run a SPARQL query: `QuerySolutions` for SELECT, `bool` for ASK, `QueryTriples`
    /// for CONSTRUCT and DESCRIBE.
    #[pyo3(signature = (query, *, base_iri = None, prefixes = None, bindings = None, default_graph = None, named_graphs = None, include_inferred = false, timeout = None))]
    #[allow(clippy::too_many_arguments)]
    fn query<'py>(
        &self,
        py: Python<'py>,
        query: &str,
        base_iri: Option<String>,
        prefixes: Option<BTreeMap<String, String>>,
        bindings: Option<Bound<'py, PyAny>>,
        default_graph: Option<Bound<'py, PyAny>>,
        named_graphs: Option<Bound<'py, PyAny>>,
        include_inferred: bool,
        timeout: Option<f64>,
    ) -> PyResult<Bound<'py, PyAny>> {
        let args = QueryArgs {
            base_iri,
            prefixes,
            bindings,
            default_graph,
            named_graphs,
            include_inferred,
            timeout,
        };
        self.run_query(py, query, args, None)
    }

    /// Run a SELECT query.
    #[pyo3(signature = (query, *, base_iri = None, prefixes = None, bindings = None, default_graph = None, named_graphs = None, include_inferred = false, timeout = None))]
    #[allow(clippy::too_many_arguments)]
    fn select<'py>(
        &self,
        py: Python<'py>,
        query: &str,
        base_iri: Option<String>,
        prefixes: Option<BTreeMap<String, String>>,
        bindings: Option<Bound<'py, PyAny>>,
        default_graph: Option<Bound<'py, PyAny>>,
        named_graphs: Option<Bound<'py, PyAny>>,
        include_inferred: bool,
        timeout: Option<f64>,
    ) -> PyResult<Bound<'py, PyAny>> {
        let args = QueryArgs {
            base_iri,
            prefixes,
            bindings,
            default_graph,
            named_graphs,
            include_inferred,
            timeout,
        };
        self.run_query(py, query, args, Some(&[QueryKind::Select]))
    }

    /// Run an ASK query.
    #[pyo3(signature = (query, *, base_iri = None, prefixes = None, bindings = None, default_graph = None, named_graphs = None, include_inferred = false, timeout = None))]
    #[allow(clippy::too_many_arguments)]
    fn ask<'py>(
        &self,
        py: Python<'py>,
        query: &str,
        base_iri: Option<String>,
        prefixes: Option<BTreeMap<String, String>>,
        bindings: Option<Bound<'py, PyAny>>,
        default_graph: Option<Bound<'py, PyAny>>,
        named_graphs: Option<Bound<'py, PyAny>>,
        include_inferred: bool,
        timeout: Option<f64>,
    ) -> PyResult<Bound<'py, PyAny>> {
        let args = QueryArgs {
            base_iri,
            prefixes,
            bindings,
            default_graph,
            named_graphs,
            include_inferred,
            timeout,
        };
        self.run_query(py, query, args, Some(&[QueryKind::Ask]))
    }

    /// Run a CONSTRUCT or DESCRIBE query.
    #[pyo3(signature = (query, *, base_iri = None, prefixes = None, bindings = None, default_graph = None, named_graphs = None, include_inferred = false, timeout = None))]
    #[allow(clippy::too_many_arguments)]
    fn construct<'py>(
        &self,
        py: Python<'py>,
        query: &str,
        base_iri: Option<String>,
        prefixes: Option<BTreeMap<String, String>>,
        bindings: Option<Bound<'py, PyAny>>,
        default_graph: Option<Bound<'py, PyAny>>,
        named_graphs: Option<Bound<'py, PyAny>>,
        include_inferred: bool,
        timeout: Option<f64>,
    ) -> PyResult<Bound<'py, PyAny>> {
        let args = QueryArgs {
            base_iri,
            prefixes,
            bindings,
            default_graph,
            named_graphs,
            include_inferred,
            timeout,
        };
        self.run_query(
            py,
            query,
            args,
            Some(&[QueryKind::Construct, QueryKind::Describe]),
        )
    }

    /// Run a SPARQL Update request in one transaction.
    #[pyo3(signature = (update, *, base_iri = None, prefixes = None))]
    fn update(
        &self,
        py: Python<'_>,
        update: &str,
        base_iri: Option<String>,
        prefixes: Option<BTreeMap<String, String>>,
    ) -> PyResult<PyUpdateStats> {
        let ds = self.ds_for_write(py)?;
        let opts = QueryOptions {
            base_iri,
            prefixes: prefixes.unwrap_or_default().into_iter().collect(),
            ..Default::default()
        };
        let s = py.detach(|| ds.update_with(update, &opts)).py(py)?;
        Ok(PyUpdateStats {
            inserted: s.inserted,
            deleted: s.deleted,
            operations: s.operations,
        })
    }

    // ------------------------------------------------------------------ quads ----

    /// Add a quad (or a triple, to the default graph); true if it was new.
    fn add(&self, py: Python<'_>, quad: &Bound<'_, PyAny>) -> PyResult<bool> {
        let q = quad_from_py(quad)?;
        let ds = self.ds_for_write(py)?;
        py.detach(|| ds.insert(q.as_ref())).py(py)
    }

    /// Remove a quad; true if it was present.
    fn remove(&self, py: Python<'_>, quad: &Bound<'_, PyAny>) -> PyResult<bool> {
        let q = quad_from_py(quad)?;
        let ds = self.ds_for_write(py)?;
        py.detach(|| ds.remove(q.as_ref())).py(py)
    }

    /// Add quads in one commit (triples go to the default graph); returns how many were
    /// new.
    fn extend(&self, py: Python<'_>, quads: &Bound<'_, PyAny>) -> PyResult<u64> {
        let quads = quads
            .try_iter()?
            .map(|q| quad_from_py(&q?))
            .collect::<PyResult<Vec<Quad>>>()?;
        let ds = self.ds_for_write(py)?;
        py.detach(|| ds.extend(quads.iter().map(Quad::as_ref)))
            .py(py)
    }

    /// The quads matching a pattern; `None` matches anything, and `graph_name=None`
    /// matches every graph.
    #[pyo3(signature = (subject = None, predicate = None, object = None, graph_name = None))]
    fn quads_for_pattern(
        &self,
        py: Python<'_>,
        subject: Option<&Bound<'_, PyAny>>,
        predicate: Option<&Bound<'_, PyAny>>,
        object: Option<&Bound<'_, PyAny>>,
        graph_name: Option<&Bound<'_, PyAny>>,
    ) -> PyResult<PyQuadIterator> {
        let s: Option<NamedOrBlankNode> = opt(subject, subject_from_py)?;
        let p = opt(predicate, named_node_from_py)?;
        let o: Option<Term> = opt(object, term_from_py)?;
        let g: Option<GraphName> = opt(graph_name, graph_from_py)?;
        let ds = self.ds(py)?;
        let it = py.detach(|| {
            ds.quads(
                g.as_ref().map(GraphName::as_ref),
                s.as_ref(),
                p.as_ref(),
                o.as_ref(),
            )
        });
        Ok(PyQuadIterator::from_scan(it))
    }

    fn __iter__(&self, py: Python<'_>) -> PyResult<PyQuadIterator> {
        self.quads_for_pattern(py, None, None, None, None)
    }

    fn __contains__(&self, py: Python<'_>, quad: &Bound<'_, PyAny>) -> PyResult<bool> {
        let q = quad_from_py(quad)?;
        let ds = self.ds(py)?;
        py.detach(|| ds.contains(q.as_ref())).py(py)
    }

    /// The number of quads.
    fn __len__(&self, py: Python<'_>) -> PyResult<usize> {
        let ds = self.ds(py)?;
        Ok(py.detach(|| ds.len()) as usize)
    }

    /// The names of the non-empty named graphs.
    fn named_graphs<'py>(&self, py: Python<'py>) -> PyResult<Vec<Bound<'py, PyAny>>> {
        let ds = self.ds(py)?;
        let names = py.detach(|| ds.graph_names()).py(py)?;
        names
            .into_iter()
            .map(|n| graph_to_py(py, &n.into()))
            .collect()
    }

    /// Remove every triple of a graph; returns how many were removed.
    fn clear_graph(&self, py: Python<'_>, graph_name: &Bound<'_, PyAny>) -> PyResult<u64> {
        let g = graph_from_py(graph_name)?;
        let ds = self.ds_for_write(py)?;
        py.detach(|| match &g {
            GraphName::DefaultGraph => ds.default_graph().clear(),
            GraphName::NamedNode(n) => ds.named_graph(n.as_str())?.clear(),
            GraphName::BlankNode(_) => {
                Err(sparkles::Error::unsupported("clearing a blank-node graph"))
            }
        })
        .py(py)
    }

    /// Remove every quad.
    fn clear(&self, py: Python<'_>) -> PyResult<()> {
        let ds = self.ds_for_write(py)?;
        py.detach(|| ds.update("CLEAR ALL")).py(py).map(|_| ())
    }

    /// Begin a write transaction (a context manager).
    fn transaction(&self, py: Python<'_>) -> PyResult<PyTransaction> {
        let ds = self.ds_for_write(py)?;
        PyTransaction::begin(py, ds, self.writer.clone())
    }

    // ----------------------------------------------------------- output, admin ----

    /// Serialize the dataset (a quad format: every graph; a triple format: the default
    /// graph or `from_graph`). Returns bytes when `output` is `None`.
    #[pyo3(signature = (output = None, format = None, *, from_graph = None, compression = None))]
    fn dump<'py>(
        &self,
        py: Python<'py>,
        output: Option<&Bound<'py, PyAny>>,
        format: Option<&Bound<'py, PyAny>>,
        from_graph: Option<&Bound<'py, PyAny>>,
        compression: Option<&str>,
    ) -> PyResult<Option<Bound<'py, PyBytes>>> {
        let out = output_from_py(output)?;
        let format = match format_from_py(format)?.or_else(|| format_of_output(&out)) {
            Some(f) => f,
            None => {
                return Err(invalid(
                    py,
                    "give a format (or an output path with an RDF extension)",
                ));
            }
        };
        let codec = codec_from_py(py, compression)?;
        let graph = opt(from_graph, graph_from_py)?;
        let ds = self.ds(py)?;
        if let Output::Path(p) = &out
            && let Some(dir) = p.parent()
            && !dir.as_os_str().is_empty()
            && !dir.exists()
        {
            return Err(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                format!("No such directory: {}", dir.display()),
            )
            .into());
        }
        write_output(py, out, codec, move |w| match graph {
            None => ds.dump(w, format),
            Some(g) => {
                let quads = ds.quads(Some(g.as_ref()), None, None, None);
                serialize_quads(w, format, ds.prefixes(), quads, false)
            }
        })
    }

    /// Merge the pending changes into a new index generation.
    fn compact(&self, py: Python<'_>) -> PyResult<()> {
        let ds = self.ds_for_write(py)?;
        py.detach(|| ds.compact()).py(py)
    }

    /// Write a compressed N-Quads backup into a directory; returns its path.
    fn backup(&self, py: Python<'_>, directory: PathBuf) -> PyResult<String> {
        let ds = self.ds(py)?;
        let p = py.detach(|| ds.backup(&directory)).py(py)?;
        Ok(p.display().to_string())
    }

    /// The dataset's prefixes, as `{prefix: namespace}`.
    #[getter]
    fn prefixes(&self, py: Python<'_>) -> PyResult<BTreeMap<String, String>> {
        Ok(self.ds(py)?.prefixes())
    }

    fn set_prefix(&self, py: Python<'_>, prefix: &str, iri: &str) -> PyResult<()> {
        let ds = self.ds(py)?;
        py.detach(|| ds.set_prefix(prefix, iri)).py(py)
    }

    // ------------------------------------------------------ reasoning, validation ----

    /// Materialize entailments into `urn:x-sparkles:inferred`.
    #[pyo3(signature = (profile = "rdfs", *, rules = None))]
    fn reason(
        &self,
        py: Python<'_>,
        profile: &str,
        rules: Option<String>,
    ) -> PyResult<crate::validate::PyReasonReport> {
        let ds = self.ds_for_write(py)?;
        crate::validate::reason(py, ds, profile, rules)
    }

    /// Remove the materialized entailments; returns how many triples were removed.
    fn clear_inferences(&self, py: Python<'_>) -> PyResult<u64> {
        let ds = self.ds_for_write(py)?;
        crate::validate::clear_inferences(py, ds)
    }

    /// Validate with SHACL shapes given as text, or read from `shapes_graph`.
    #[pyo3(signature = (shapes = None, *, format = None, shapes_graph = None, data_graph = None, include_inferred = false))]
    fn validate_shacl(
        &self,
        py: Python<'_>,
        shapes: Option<&Bound<'_, PyAny>>,
        format: Option<&Bound<'_, PyAny>>,
        shapes_graph: Option<&Bound<'_, PyAny>>,
        data_graph: Option<&Bound<'_, PyAny>>,
        include_inferred: bool,
    ) -> PyResult<crate::validate::PyShaclReport> {
        let ds = self.ds(py)?;
        let shapes_graph = opt(shapes_graph, iri_from_py)?.map(|n| n.into_string());
        let data_graph = opt(data_graph, iri_from_py)?.map(|n| n.into_string());
        let format = format_from_py(format)?;
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
            shapes_graph,
            data_graph,
            include_inferred,
        )
    }

    /// Validate with a ShEx schema (ShExC or ShExJ) and a shape map.
    #[pyo3(signature = (schema, shape_map, *, format = None, data_graph = None, include_inferred = false))]
    fn validate_shex(
        &self,
        py: Python<'_>,
        schema: String,
        shape_map: String,
        format: Option<String>,
        data_graph: Option<&Bound<'_, PyAny>>,
        include_inferred: bool,
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
        )
    }
}
