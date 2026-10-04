//! The dataset's GraphQL configuration (`graphql.json`, feature `graphql`): installing a
//! mapping schema, drafting one, and running requests against it.

use crate::Dataset;
use crate::error::{ComponentError, Error, Result};
use crate::guard::config::DataGraphSel;
use crate::history::{At, HistoryOptions};
use crate::sparql::QueryOptions;
use crate::store::{Snapshot, Store};
use crate::write_guard::WriteGuard;
pub use sparkles_graphql::config::Saved;
use sparkles_graphql::draft;
pub use sparkles_graphql::{
    Change, Compiled, Config, Options, PutError, Request, Response, Stored, Version,
};
use std::sync::Arc;
use std::time::{Duration, Instant};

/// The dataset's GraphQL configuration (from [`Dataset::graphql`]).
#[derive(Clone)]
pub struct GraphQl {
    pub(crate) ds: Dataset,
}

impl GraphQl {
    /// The configuration at `version`, or the current one; `None` when there is none.
    pub fn get(&self, version: Option<u64>) -> Option<Stored> {
        self.ds.state().graphql.get(version)
    }

    /// The configuration's versions, oldest first.
    pub fn versions(&self) -> Vec<Version> {
        self.ds.state().graphql.versions()
    }

    /// Remove the configuration and its versions (if its current version is
    /// `if_version`, when given); whether there was one.
    pub fn reset(&self, if_version: Option<u64>) -> Result<bool> {
        self.ds.state().graphql.delete(if_version)
    }

    /// Install `config` as the next version, after it compiles, and return it with the
    /// warnings about non-null fields that the write guard does not back. An unchanged
    /// configuration adds no version. A mapping schema that does not compile is a
    /// `graphql` component error with the code `invalid-schema`, whose source is the
    /// [`PutError`] with each error's line.
    pub fn put(&self, config: Config, change: Change) -> Result<(Saved, Vec<String>)> {
        let backing = Backing::of(self.ds.write_guard(), self.ds.store(), &config.data_graph);
        let backs = |c: &str, p: &str, i: bool| backing.backs(c, p, i);
        self.ds
            .state()
            .graphql
            .put(config, change, &backs)
            .map_err(|e| match e {
                PutError::Engine(e) => e,
                e => Error::Component(Box::new(
                    ComponentError::new(
                        "graphql",
                        "invalid-schema",
                        format!("the mapping schema is invalid: {}", e.message()),
                    )
                    .with_source(e),
                )),
            })
    }

    /// The installed configuration, compiled, or `None` when there is none.
    pub fn compiled(&self) -> Result<Option<Arc<Compiled>>> {
        self.ds
            .state()
            .graphql
            .compiled()
            .map_err(|e| Error::invalid(e.to_string()))
    }

    /// The API schema of the installed configuration as SDL, or `None` when there is
    /// none.
    pub fn sdl(&self) -> Result<Option<String>> {
        Ok(self.compiled()?.map(|c| c.api_sdl.clone()))
    }

    /// Run a GraphQL request against the installed configuration, at the head or at the
    /// state that `opts.at` and the request's cursors name. The dataset's query defaults
    /// fill the fields of `opts.query` that it leaves unset, as in
    /// [`Dataset::query_with`]. No installed configuration is [`Error::NotFound`].
    pub fn execute(&self, req: &Request, opts: &Options) -> Result<Response> {
        let store = self.ds.store();
        let history = HistoryOptions {
            cancel: opts.query.cancel.clone(),
            deadline: opts.query.timeout.map(|t| Instant::now() + t),
        };
        let resolve = |at: Option<&At>| match at {
            None => Ok(store.snapshot()),
            Some(at) => Ok(store.snapshot_at(at, &history)?.0),
        };
        self.execute_with(req, opts, &resolve)
    }

    /// [`execute`](Self::execute) with the states that `resolve` opens: the head for
    /// `None`, else the state an `at` names.
    pub fn execute_with(
        &self,
        req: &Request,
        opts: &Options,
        resolve: sparkles_graphql::Resolve<'_>,
    ) -> Result<Response> {
        let c = self.compiled()?.ok_or_else(|| {
            Error::NotFound(format!(
                "no GraphQL schema is installed for {}",
                self.ds
                    .name()
                    .map_or("the dataset".into(), |n| format!("/{n}"))
            ))
        })?;
        let query = self.ds.with_query_defaults(&opts.query);
        let opts = match query {
            std::borrow::Cow::Borrowed(_) => std::borrow::Cow::Borrowed(opts),
            std::borrow::Cow::Owned(q) => std::borrow::Cow::Owned(Options {
                query: q,
                ..opts.clone()
            }),
        };
        Ok(sparkles_graphql::execute(&c, req, &opts, resolve))
    }

    /// A mapping schema drafted from SHACL shapes or from the data, with the commit it
    /// read. The data graph is the installed configuration's, and the shapes are those
    /// of `req.shapes_graph` or of the write guard.
    pub fn draft(&self, name: &str, req: DraftRequest) -> Result<(draft::Draft, u64)> {
        let data_graph = self
            .get(None)
            .map(|s| s.config.data_graph)
            .unwrap_or_default();
        draft_of(
            name,
            self.ds.store(),
            self.ds.write_guard(),
            &data_graph,
            req,
        )
    }
}

/// The write guard's SHACL configuration and its shapes as one graph: its shapes graphs
/// at the head, and its shapes file or inline shapes.
#[cfg(feature = "shacl")]
pub fn guard_shapes(
    guard: Option<WriteGuard>,
    store: &Store,
) -> Option<(sparkles_shacl::guard::ValidationConfig, oxrdf::Graph)> {
    #[allow(irrefutable_let_patterns)]
    let WriteGuard::Shacl(g) = guard? else {
        return None;
    };
    let cfg = g.config().clone();
    let mut graph = oxrdf::Graph::new();
    let snap = store.snapshot();
    for name in cfg.shapes.graphs.iter().flatten() {
        let Ok(n) = oxrdf::NamedNode::new(name.as_str()) else {
            continue;
        };
        let q = format!("CONSTRUCT {{ ?s ?p ?o }} WHERE {{ GRAPH {n} {{ ?s ?p ?o }} }}");
        if let Ok(r) = crate::sparql::query(snap.clone(), &q, &QueryOptions::default()) {
            for t in &r.triples {
                graph.insert(t);
            }
        }
    }
    let text = match (&cfg.shapes.inline, &cfg.shapes.file, store.root()) {
        (Some(t), _, _) => Some(t.clone()),
        (None, Some(_), Some(root)) => {
            std::fs::read_to_string(root.join(crate::guard::config::SHACL_SHAPES_FILE)).ok()
        }
        _ => None,
    };
    if let Some(text) = text {
        let syntax = cfg
            .shapes
            .format
            .as_deref()
            .and_then(sparkles_shacl::ShapesSyntax::from_media_type)
            .unwrap_or_default();
        if let Ok(g) = sparkles_shacl::shapes::Shapes::read_graph(&text, syntax, None) {
            for t in &g {
                graph.insert(t);
            }
        }
    }
    Some((cfg, graph))
}

/// Whether the guard enforces its shapes on a schema's data graph: `reject` mode, a
/// `strict` baseline and the same data graph.
#[cfg(feature = "shacl")]
fn enforces(g: &sparkles_shacl::guard::ValidationConfig, data_graph: &DataGraphSel) -> bool {
    g.mode == crate::guard::GuardMode::Reject
        && g.baseline.is_strict()
        && g.data_graph == *data_graph
}

/// The facts behind the non-null warnings: the `(class, path, inverse)` triples the
/// write guard requires a value for, and the data that says which classes are
/// superclasses.
pub struct Backing {
    required: Vec<(String, String, bool)>,
    snap: Arc<Snapshot>,
}

impl Backing {
    /// What `guard` requires of the data of `data_graph` in `store`.
    pub fn of(guard: Option<WriteGuard>, store: &Store, data_graph: &DataGraphSel) -> Backing {
        #[cfg(feature = "shacl")]
        let required = match guard_shapes(guard, store) {
            Some((cfg, graph)) if enforces(&cfg, data_graph) => {
                let (types, _) = draft::read_shapes(&graph);
                types
                    .iter()
                    .flat_map(|t| {
                        t.fields
                            .iter()
                            .filter(|f| f.min_one)
                            .map(|f| (t.class.clone(), f.path.clone(), f.inverse))
                    })
                    .collect()
            }
            _ => Vec::new(),
        };
        #[cfg(not(feature = "shacl"))]
        let required = {
            let _ = (guard, data_graph);
            Vec::new()
        };
        Backing {
            required,
            snap: store.snapshot(),
        }
    }

    /// Whether a shape of the guard requires a value of `path` for the class or one of
    /// its superclasses.
    pub fn backs(&self, class: &str, path: &str, inverse: bool) -> bool {
        if self.required.is_empty() {
            return false;
        }
        let Ok(c) = oxrdf::NamedNode::new(class) else {
            return false;
        };
        let q = format!(
            "SELECT DISTINCT ?s WHERE {{ {c} <http://www.w3.org/2000/01/rdf-schema#subClassOf>* ?s }}"
        );
        let mut classes = vec![class.to_string()];
        if let Ok(r) = crate::sparql::query(self.snap.clone(), &q, &QueryOptions::default()) {
            for row in r.rows() {
                if let Some(oxrdf::Term::NamedNode(n)) = &row[0] {
                    classes.push(n.as_str().to_string());
                }
            }
        }
        self.required
            .iter()
            .any(|(k, p, i)| classes.contains(k) && p == path && *i == inverse)
    }
}

/// What a draft is made from.
#[derive(Clone, Debug)]
pub struct DraftRequest {
    /// `shapes` or `observed`; `None`: shapes when there is a shapes graph or a SHACL
    /// guard, else observed
    pub source: Option<String>,
    pub shapes_graph: Option<String>,
    pub graph: crate::schema::GraphSelection,
    pub support: f64,
    pub classes: Vec<String>,
    pub min_instances: u64,
    /// count the materialized inferences as data
    pub reasoning: bool,
    pub timeout: Duration,
    /// the graphs the caller may read (`None`: every graph)
    pub view: Option<Arc<crate::access::GraphAccess>>,
    pub max_entries: usize,
}

impl Default for DraftRequest {
    fn default() -> DraftRequest {
        DraftRequest {
            source: None,
            shapes_graph: None,
            graph: crate::schema::GraphSelection::Default,
            support: 1.0,
            classes: Vec::new(),
            min_instances: 1,
            reasoning: false,
            timeout: Duration::from_secs(600),
            view: None,
            max_entries: crate::schema::DEFAULT_MAX_ENTRIES,
        }
    }
}

/// A mapping schema of the dataset `name` in `store`, drafted as `r` asks, with the
/// commit it read (see [`GraphQl::draft`]).
pub fn draft_of(
    name: &str,
    store: &Store,
    guard: Option<WriteGuard>,
    data_graph: &DataGraphSel,
    r: DraftRequest,
) -> Result<(draft::Draft, u64)> {
    #[cfg(feature = "shacl")]
    let has_guard = guard
        .as_ref()
        .is_some_and(|v| v.language() == crate::guard::GuardLanguage::Shacl);
    #[cfg(not(feature = "shacl"))]
    let has_guard = false;
    let source = match r.source.as_deref() {
        Some("shapes") => "shapes",
        Some("observed") => "observed",
        None if r.shapes_graph.is_some() || has_guard => "shapes",
        None => "observed",
        Some(s) => {
            return Err(Error::invalid(format!(
                "source must be shapes or observed, not '{s}'"
            )));
        }
    };
    let qopts = QueryOptions {
        timeout: Some(r.timeout),
        graphs: r.view.clone(),
        ..Default::default()
    };
    let mut prefixes: Vec<(String, String)> = crate::io::standard_prefixes().into_iter().collect();
    prefixes.extend(store.prefixes());
    prefixes.sort();
    prefixes.dedup_by(|a, b| a.0 == b.0);
    let snap = store.snapshot();
    let commit = snap.commit;
    let from = if source == "shapes" {
        match &r.shapes_graph {
            Some(g) => format!("the SHACL shapes of the graph <{g}>"),
            None => "the SHACL shapes of the write-time guard".to_string(),
        }
    } else {
        "the shapes drafted from the data".to_string()
    };
    let header = format!(
        "A draft of a GraphQL mapping schema for /{name}, from {from} at commit {commit}.\nReview it, then install it with PUT /$/graphql/{name}."
    );
    let d = if source == "shapes" {
        let (graph, enforced) = match &r.shapes_graph {
            Some(g) => {
                let n = oxrdf::NamedNode::new(g.as_str())
                    .map_err(|e| Error::invalid(format!("shapesGraph: {e}")))?;
                let q = format!("CONSTRUCT {{ ?s ?p ?o }} WHERE {{ GRAPH {n} {{ ?s ?p ?o }} }}");
                let res = crate::sparql::query(snap.clone(), &q, &qopts)?;
                let mut graph = oxrdf::Graph::new();
                for t in &res.triples {
                    graph.insert(t);
                }
                (graph, false)
            }
            None => {
                #[cfg(feature = "shacl")]
                {
                    match guard_shapes(guard, store) {
                        Some((cfg, graph)) => {
                            let e = enforces(&cfg, data_graph);
                            (graph, e)
                        }
                        None => {
                            return Err(Error::invalid(
                                "the dataset has no SHACL write-time validation: name a shapes graph, or draft from the data (source observed)",
                            ));
                        }
                    }
                }
                #[cfg(not(feature = "shacl"))]
                {
                    let _ = (guard, data_graph);
                    return Err(Error::invalid("name a shapes graph"));
                }
            }
        };
        let (types, skipped) = draft::read_shapes(&graph);
        draft::render(types, skipped, &prefixes, enforced, &header, "shapes")
    } else {
        if !(r.support > 0.0 && r.support <= 1.0) {
            return Err(Error::invalid("support must be a number in (0, 1]"));
        }
        let o = crate::schema::draft::DraftOptions {
            schema: crate::schema::SchemaOptions {
                graph: r.graph,
                inferred_graph: Some(crate::reasoning::INFERRED_GRAPH.to_string()),
                include_inferred: r.reasoning,
                max_entries: r.max_entries,
                graphs: r.view,
                deadline: Some(Instant::now() + r.timeout),
                ..Default::default()
            },
            dataset: name.to_string(),
            support: r.support,
            classes: r.classes,
            min_instances: r.min_instances,
            max_in: crate::schema::draft::DEFAULT_MAX_IN,
            max_count: 1,
            closed: false,
            base: crate::schema::draft::default_base(name),
            prefixes: prefixes.clone(),
        };
        let shapes =
            crate::schema::draft_shapes(&snap, &o).map_err(|e| Error::invalid(e.to_string()))?;
        let big = |class: &str, pred: &str| {
            crate::sparql::query(snap.clone(), &draft::big_integer_query(class, pred), &qopts)
                .is_ok_and(|r| r.boolean)
        };
        let types = draft::read_observed(&shapes, &big);
        draft::render(types, Vec::new(), &prefixes, false, &header, "observed")
    };
    Ok((d, commit))
}
