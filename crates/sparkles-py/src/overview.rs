//! The graph overview of a dataset: its classes grouped by community, by property or
//! into members.
use crate::dataset::{QueryArgs, apply_defaults, query_options};
use crate::{admin, errors::EngineResult, handles::handle, interrupt};
use pyo3::prelude::*;
use sparkles::history::At;
use sparkles::overview::{FacetsRequest, GroupsRequest};
use std::collections::BTreeMap;
use std::time::Instant;

handle!(PyGraphOverview, "GraphOverview");

#[pymethods]
impl PyGraphOverview {
    /// Group the instances of a class, or of a group from an earlier answer, and count
    /// the edges between the groups, to their sibling groups and to the other classes.
    /// `within` is `{"class": iri}` or `{"group": id}`, `by` is `"auto"`, `"members"`,
    /// `{"community": iri}`, `{"property": iri, "buckets": n}` or
    /// `{"reports": {"level": n}}`. The last groups the whole dataset by its GraphRAG
    /// communities and is the one form that leaves `within` out. `at` is a commit
    /// number or a `time:` or `snapshot:` point. The other keyword arguments are those
    /// of `Dataset.query`. Returns the answer as a dict.
    #[pyo3(signature = (within = None, *, by = None, limit = None, at = None, base_iri = None, prefixes = None, default_graph = None, named_graphs = None, include_inferred = false, timeout = None, max_rows = None, max_memory_bytes = None, max_rows_produced = None, cancel = None))]
    #[allow(clippy::too_many_arguments)]
    fn groups<'py>(
        &self,
        py: Python<'py>,
        within: Option<&Bound<'py, PyAny>>,
        by: Option<&Bound<'py, PyAny>>,
        limit: Option<u64>,
        at: Option<Bound<'py, PyAny>>,
        base_iri: Option<String>,
        prefixes: Option<BTreeMap<String, String>>,
        default_graph: Option<Bound<'py, PyAny>>,
        named_graphs: Option<Bound<'py, PyAny>>,
        include_inferred: bool,
        timeout: Option<f64>,
        max_rows: Option<usize>,
        max_memory_bytes: Option<u64>,
        max_rows_produced: Option<u64>,
        cancel: Option<Bound<'py, PyAny>>,
    ) -> PyResult<Bound<'py, PyAny>> {
        let ds = self.ds(py)?;
        let mut body = serde_json::Map::new();
        if let Some(within) = within.filter(|w| !w.is_none()) {
            body.insert("within".into(), admin::from_py(within, "within")?);
        }
        if let Some(by) = by.filter(|b| !b.is_none()) {
            body.insert("by".into(), admin::from_py(by, "by")?);
        }
        if let Some(limit) = limit {
            body.insert("limit".into(), limit.into());
        }
        let mut req = GroupsRequest::from_json(&serde_json::Value::Object(body)).py(py)?;
        let args = QueryArgs {
            base_iri,
            prefixes,
            default_graph,
            named_graphs,
            include_inferred,
            timeout,
            max_rows,
            max_memory_bytes,
            max_rows_produced,
            cancel,
            at,
            ..Default::default()
        };
        let (mut opts, at) = query_options(&args)?;
        // one deadline for all of the overview's queries, not a timeout for each
        opts.deadline = opts
            .timeout
            .take()
            .and_then(|t| Instant::now().checked_add(t));
        apply_defaults(&mut opts, &ds.query_options(), &args)?;
        req.at = at.filter(|a| *a != At::Head);
        let token = args
            .cancel
            .as_ref()
            .is_some_and(|t| !t.is_none())
            .then(|| opts.cancel.clone().unwrap_or_default());
        let answer = interrupt::run_again(py, token, move |flag| {
            let mut opts = opts.clone();
            opts.cancel = Some(flag);
            ds.graph().groups(&req, &opts)
        })?;
        admin::to_py(py, &answer)
    }

    /// Count the values of literal and IRI properties of a set, as histograms and bar
    /// lists (spec U11 section 3.2). `within` is `{"class": iri}`, `{"nodes": [iri]}` or
    /// `{"query": text, "variable": name}`, `facets` the facets to count, `filters` the
    /// brushes and `candidates` asks for the properties worth a facet. These are the
    /// members of the HTTP route's body. `at` is a commit number or a `time:` or
    /// `snapshot:` point. The other keyword arguments are those of `Dataset.query`.
    /// Returns the answer as a dict.
    #[pyo3(signature = (within, *, facets = None, filters = None, candidates = false, at = None, base_iri = None, prefixes = None, default_graph = None, named_graphs = None, include_inferred = false, timeout = None, max_rows = None, max_memory_bytes = None, max_rows_produced = None, cancel = None))]
    #[allow(clippy::too_many_arguments)]
    fn facets<'py>(
        &self,
        py: Python<'py>,
        within: &Bound<'py, PyAny>,
        facets: Option<&Bound<'py, PyAny>>,
        filters: Option<&Bound<'py, PyAny>>,
        candidates: bool,
        at: Option<Bound<'py, PyAny>>,
        base_iri: Option<String>,
        prefixes: Option<BTreeMap<String, String>>,
        default_graph: Option<Bound<'py, PyAny>>,
        named_graphs: Option<Bound<'py, PyAny>>,
        include_inferred: bool,
        timeout: Option<f64>,
        max_rows: Option<usize>,
        max_memory_bytes: Option<u64>,
        max_rows_produced: Option<u64>,
        cancel: Option<Bound<'py, PyAny>>,
    ) -> PyResult<Bound<'py, PyAny>> {
        let ds = self.ds(py)?;
        let mut body = serde_json::Map::new();
        body.insert("within".into(), admin::from_py(within, "within")?);
        if let Some(f) = facets.filter(|f| !f.is_none()) {
            body.insert("facets".into(), admin::from_py(f, "facets")?);
        }
        if let Some(f) = filters.filter(|f| !f.is_none()) {
            body.insert("filters".into(), admin::from_py(f, "filters")?);
        }
        if candidates {
            body.insert("candidates".into(), true.into());
        }
        let mut req = FacetsRequest::from_json(&serde_json::Value::Object(body)).py(py)?;
        let args = QueryArgs {
            base_iri,
            prefixes,
            default_graph,
            named_graphs,
            include_inferred,
            timeout,
            max_rows,
            max_memory_bytes,
            max_rows_produced,
            cancel,
            at,
            ..Default::default()
        };
        let (mut opts, at) = query_options(&args)?;
        // one deadline for all of the facets' queries, not a timeout for each
        opts.deadline = opts
            .timeout
            .take()
            .and_then(|t| Instant::now().checked_add(t));
        apply_defaults(&mut opts, &ds.query_options(), &args)?;
        req.at = at.filter(|a| *a != At::Head);
        let token = args
            .cancel
            .as_ref()
            .is_some_and(|t| !t.is_none())
            .then(|| opts.cancel.clone().unwrap_or_default());
        let answer = interrupt::run_again(py, token, move |flag| {
            let mut opts = opts.clone();
            opts.cancel = Some(flag);
            ds.graph().facets(&req, &opts)
        })?;
        admin::to_py(py, &answer)
    }
}

pub fn register(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_class::<PyGraphOverview>()?;
    Ok(())
}
