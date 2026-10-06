//! SPARQL 1.1 query & update engine (ARQ equivalent).

pub mod aggext;
pub mod arqpf;
pub mod cache;
pub mod catalog;
pub mod cdt;
pub mod charsets;
pub mod ctx;
pub mod depth;
pub mod describe;
pub mod enhancer;
pub mod exec;
mod exists;
pub mod expr;
mod exprcache;
pub mod extensions;
mod fnformat;
mod fnlib;
pub mod geojoin;
pub mod geopf;
pub mod georewrite;
pub mod history_svc;
pub mod hybrid;
pub mod indexjoin;
mod joinorder;
mod keyfilter;
mod keyprobe;
pub mod lateral;
pub mod pathsearch;
pub mod plan;
mod propertyext;
pub mod rdfs;
mod registeredagg;
pub mod results;
mod sample;
pub mod stats;
pub mod svccache;
pub mod table;
pub mod textpf;
pub mod update;
pub mod value;
mod vectortopk;

use crate::error::{Error, Result};
use crate::id::Id;
use crate::store::Snapshot;
pub use ctx::{Ctx, DatasetSpec, Optimizations};
pub use exec::PlanInfo;
use oxrdf::{BlankNode, GraphName, NamedOrBlankNode, Quad, Term, Triple};
use plan::{ActiveGraph, Planner};
use rustc_hash::{FxHashMap, FxHashSet};
use serde::Serialize;
use spargebra::algebra::{GraphPattern, QueryDataset};
use spargebra::term::{NamedNodePattern, TermPattern, TriplePattern};
use spargebra::{GraphTemplate, Query, SparqlParser};
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::time::{Duration, Instant};
pub use table::Table;

#[derive(Clone, Debug, Default)]
pub struct QueryOptions {
    /// Immutable application scalar and aggregate callbacks resolved for this query only.
    pub extensions: Option<Arc<extensions::ExtensionRegistry>>,
    pub timeout: Option<Duration>,
    /// options for write guards (updates)
    pub write: crate::guard::WriteOptions,
    /// protocol `default-graph-uri` (overrides FROM)
    pub default_graph_uris: Vec<String>,
    /// protocol `named-graph-uri` (overrides FROM NAMED)
    pub named_graph_uris: Vec<String>,
    pub base_iri: Option<String>,
    pub max_rows: Option<usize>,
    /// Budget for the estimated memory of intermediate results (`None`: unlimited);
    /// exceeding it fails with [`Error::BudgetExceeded`].
    pub max_memory_bytes: Option<u64>,
    /// Budget for the rows all operators of a query produce together (`None`:
    /// unlimited); exceeding it fails with [`Error::BudgetExceeded`]. An update's WHERE
    /// clauses share one count.
    pub max_rows_produced: Option<u64>,
    pub allow_service: bool,
    /// Refuse SERVICE with [`Error::NotPermitted`] (the caller lacks the permission;
    /// `allow_service: false` means SERVICE is disabled for everyone).
    pub forbid_service: bool,
    /// Refuse `LOAD <http…>` with [`Error::NotPermitted`].
    pub forbid_remote_load: bool,
    /// Refuse `LOAD <file:…>` with [`Error::NotPermitted`] (the caller lacks the
    /// permission; see [`file_loads`](Self::file_loads) for what may be read at all).
    pub forbid_file_load: bool,
    /// Which files `LOAD <file:…>` may read (any, by default).
    pub file_loads: FileLoads,
    /// Where SERVICE and `LOAD <http…>` may connect, with their timeouts and response
    /// ceiling (the default refuses loopback, private and link-local destinations); a
    /// refused destination fails with [`Error::NotPermitted`].
    pub outbound: crate::outbound::OutboundPolicy,
    pub cancel: Option<Arc<AtomicBool>>,
    /// Graphs merged into the store's default graph when the query does not specify a
    /// dataset (used for the materialized-inference overlay).
    pub default_graph_extra: Vec<String>,
    /// Bypass the result cache (read and write).
    pub no_cache: bool,
    /// Pre-bound variables (Jena `QueryExec.substitution`): every occurrence of the
    /// variable is replaced by the term; projected variables report the bound value.
    /// A blank node with a stored node's label (`_:b…`) is that stored node. Any other
    /// label, such as a node a query minted (`_:q…`), is a new blank node of this query.
    pub initial_bindings: Vec<(String, Term)>,
    /// prefixes made available to the query (Fuseki doesn't do this; the CLI does)
    pub prefixes: Vec<(String, String)>,
    /// Executor optimizations in effect (all on by default; see [`Optimizations`]).
    pub optimizations: Option<Optimizations>,
    /// The graphs the request may read (and, in an update, write); `None` is every
    /// graph. See [`crate::access`].
    pub graphs: Option<Arc<crate::access::GraphAccess>>,
    /// RDFS on read: patterns match the RDFS closure of each graph with respect to this
    /// schema (see [`rdfs`]).
    pub rdfs: Option<Arc<rdfs::RdfsOnRead>>,
    /// The outbound budget the request spends (`None`: a new one from
    /// [`outbound`](Self::outbound)), shared by several requests that count as one.
    pub outbound_budget: Option<Arc<crate::outbound::RequestBudget>>,
    /// How DESCRIBE describes a resource (see [`describe`]).
    pub describe: describe::DescribeOptions,
    /// The count of rows produced that [`max_rows_produced`](Self::max_rows_produced)
    /// limits (`None`: a new one), shared by several queries that count as one request,
    /// such as the fetch groups of one GraphQL request.
    pub work: Option<Arc<std::sync::atomic::AtomicU64>>,
    /// Who the request runs for, as far as the cache of remote SERVICE results is
    /// concerned (see [`svccache`]): entries written under one scope are never read
    /// under another. The server sets it to the caller and the endpoint, so callers
    /// with different credentials or views never share results; `None` is one shared
    /// scope.
    pub service_scope: Option<Arc<str>>,
    /// Whether the default graph is the union of the named graphs for this request,
    /// overriding the store's [`StoreOptions::union_default_graph`](crate::store::StoreOptions)
    /// (`None`: the store's setting). Unlike a protocol dataset whose default graph is
    /// `urn:x-arq:UnionGraph`, it leaves the named graphs as they are.
    pub union_default_graph: Option<bool>,
    /// Whether these options already hold a dataset's query defaults, which are RDFS on
    /// read, the overlay of materialized inferences and the DESCRIBE setting.
    /// `sparkles::Dataset::query_options` returns options with this set, and a
    /// `sparkles::Dataset` then uses every field as given. When it is false, the
    /// dataset fills each of those fields that the caller left unset. The engine itself
    /// does not read it.
    pub defaults_applied: bool,
}

/// `snap` with the request's union default graph setting, if it overrides the store's.
pub fn with_union_default(snap: Arc<Snapshot>, opts: &QueryOptions) -> Arc<Snapshot> {
    match opts.union_default_graph {
        Some(u) if u != snap.union_default_graph => {
            let mut s = (*snap).clone();
            s.union_default_graph = u;
            Arc::new(s)
        }
        _ => snap,
    }
}

/// Which files `LOAD <file:…>` may read.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum FileLoads {
    /// Any file the process can read (the local command line, embedders).
    #[default]
    Anywhere,
    /// Regular files under this directory: the file's path, with `..` and symbolic
    /// links resolved, must be inside the directory's canonical path (see
    /// [`FileLoads::under`]).
    Under(std::path::PathBuf),
    /// None: `LOAD <file:…>` fails with [`Error::NotPermitted`].
    Disabled,
}

impl FileLoads {
    /// [`FileLoads::Under`] a directory, canonicalized (it must exist).
    pub fn under(dir: &std::path::Path) -> Result<FileLoads> {
        let d = std::fs::canonicalize(dir)?;
        if !d.is_dir() {
            return Err(Error::invalid(format!(
                "{} is not a directory",
                dir.display()
            )));
        }
        Ok(FileLoads::Under(d))
    }

    /// The local path of a `file:` URL, if these rules let it be read (the errors name
    /// `LOAD`): any path for `Anywhere`, relative ones (`file://data.ttl`) included; a
    /// regular file inside the directory, with `..` and symbolic links resolved, for
    /// `Under`; [`Error::NotPermitted`] otherwise.
    pub fn check(&self, url: &str) -> Result<std::path::PathBuf> {
        use std::path::PathBuf;
        let parsed = reqwest::Url::parse(url)
            .ok()
            .filter(|u| u.scheme() == "file")
            .and_then(|u| u.to_file_path().ok());
        let dir = match self {
            FileLoads::Anywhere => {
                return Ok(parsed
                    .unwrap_or_else(|| PathBuf::from(url.strip_prefix("file://").unwrap_or(url))));
            }
            FileLoads::Disabled => {
                return Err(Error::NotPermitted(
                    "LOAD <file:…> is not enabled: no load directory is configured".into(),
                ));
            }
            FileLoads::Under(dir) => dir,
        };
        let outside =
            || Error::NotPermitted(format!("LOAD <{url}>: not a file in the load directory"));
        // (the URL parser already resolved `..` segments)
        let path = parsed.ok_or_else(outside)?;
        match std::fs::canonicalize(&path) {
            // with symbolic links resolved, inside the directory, and a regular file (a
            // FIFO would block the reader)
            Ok(real)
                if real.starts_with(dir) && std::fs::metadata(&real).is_ok_and(|m| m.is_file()) =>
            {
                Ok(real)
            }
            // a missing file is told apart only inside the directory, so that files
            // elsewhere cannot be probed
            Err(_) if path.starts_with(dir) => {
                Err(Error::invalid(format!("LOAD <{url}>: no such file")))
            }
            _ => Err(outside()),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "UPPERCASE")]
pub enum QueryKind {
    Select,
    Ask,
    Construct,
    Describe,
}

#[derive(Clone, Debug, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Timing {
    pub parse_ms: f64,
    pub plan_ms: f64,
    pub exec_ms: f64,
    pub serialize_ms: f64,
    pub total_ms: f64,
}

pub struct QueryResult {
    pub kind: QueryKind,
    pub vars: Vec<String>,
    /// SELECT solutions; columns in `vars` order
    pub table: Table,
    pub boolean: bool,
    /// CONSTRUCT and DESCRIBE: the triples of the default graph
    pub triples: Vec<Triple>,
    /// CONSTRUCT with Jena ARQ's `GRAPH` template blocks: the quads in named graphs
    /// (the default graph's are in `triples`)
    pub quads: Vec<Quad>,
    pub plan: PlanInfo,
    pub timing: Timing,
    /// Peak estimated memory of intermediate results (see [`QueryOptions::max_memory_bytes`]).
    pub mem_peak_bytes: u64,
    /// Rows produced by all operators (see [`QueryOptions::max_rows_produced`]).
    pub rows_produced: u64,
    /// DESCRIBE: the result stopped at [`describe::DescribeOptions::max_triples`]
    pub describe_truncated: bool,
    pub ctx: Arc<Ctx>,
}

impl QueryResult {
    /// Decode an id of this result into a term.
    pub fn term(&self, id: Id) -> Option<Term> {
        if id.is_undef() {
            None
        } else {
            self.ctx.term(id)
        }
    }

    pub fn len(&self) -> usize {
        match self.kind {
            QueryKind::Select => self.table.len(),
            QueryKind::Ask => 1,
            _ => self.triples.len() + self.quads.len(),
        }
    }
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Solutions as terms (convenience for tests / small results).
    pub fn rows(&self) -> Vec<Vec<Option<Term>>> {
        (0..self.table.len())
            .map(|i| self.table.cols.iter().map(|c| self.term(c[i])).collect())
            .collect()
    }
}

pub fn parse_query(q: &str, base: Option<&str>, prefixes: &[(String, String)]) -> Result<Query> {
    let mut p = aggext::register(SparqlParser::new());
    if let Some(b) = base {
        p = p
            .with_base_iri(b)
            .map_err(|e| Error::invalid(e.to_string()))?;
    }
    for (k, v) in prefixes {
        p = p
            .with_prefix(k, v)
            .map_err(|e| Error::invalid(e.to_string()))?;
    }
    let query = p.parse_query(q)?;
    let depth = depth::check_query(&query)?;
    let (pattern, _, _) = split(&query);
    depth::with_stack(depth, || validate_scoping(pattern))?;
    Ok(query)
}

/// SPARQL 1.1 §18.2.1: the target of `BIND(... AS ?v)` / `(expr AS ?v)` must not already
/// be in scope. spargebra does not check this; Jena and QLever reject such queries.
pub fn validate_scoping(gp: &GraphPattern) -> Result<()> {
    use GraphPattern as GP;
    match gp {
        GP::Extend {
            inner, variable, ..
        } => {
            let mut in_scope = false;
            inner.on_in_scope_variable(|v| in_scope |= v == variable);
            // `SELECT (agg AS ?v)`: ?v must not occur in the grouped WHERE clause either
            if let GP::Group { inner: body, .. } = &**inner {
                body.on_in_scope_variable(|v| in_scope |= v == variable);
            }
            if in_scope {
                return Err(Error::invalid(format!(
                    "variable ?{} is already in scope and cannot be the target of AS",
                    variable.as_str()
                )));
            }
            validate_scoping(inner)
        }
        GP::Join { left, right }
        | GP::Lateral { left, right }
        | GP::Union { left, right }
        | GP::Minus { left, right }
        | GP::SemiJoin { left, right }
        | GP::AntiJoin { left, right }
        | GP::LeftJoin { left, right, .. } => {
            validate_scoping(left)?;
            validate_scoping(right)
        }
        GP::Filter { inner, .. }
        | GP::Graph { inner, .. }
        | GP::OrderBy { inner, .. }
        | GP::Project { inner, .. }
        | GP::Distinct { inner }
        | GP::Reduced { inner }
        | GP::Slice { inner, .. }
        | GP::Group { inner, .. }
        | GP::Assign { inner, .. }
        | GP::Unfold { inner, .. }
        | GP::Service { inner, .. } => validate_scoping(inner),
        _ => Ok(()),
    }
}

fn make_ctx(
    snap: Arc<Snapshot>,
    opts: &QueryOptions,
    dataset: Option<&QueryDataset>,
    base: Option<&oxiri::Iri<String>>,
) -> Result<Ctx> {
    #[cfg(feature = "geo")]
    let op_vertices = snap.geo_op_vertices;
    let snap = with_union_default(snap, opts);
    // a view that hides triples reads the snapshot without them
    let snap = match &opts.graphs {
        Some(a) => a.masked(&snap)?,
        None => snap,
    };
    let mut ctx = Ctx::new(snap);
    // the store's limit on one geometry operation
    #[cfg(feature = "geo")]
    ctx.geo.set_op_vertices(op_vertices);
    ctx.deadline = opts.timeout.map(|t| Instant::now() + t);
    if let Some(c) = &opts.cancel {
        ctx.cancel = c.clone();
    }
    if let Some(m) = opts.max_rows {
        ctx.max_rows = m;
    }
    if let Some(m) = opts.max_memory_bytes {
        ctx.mem_limit = m;
    }
    if let Some(m) = opts.max_rows_produced {
        ctx.max_rows_produced = m;
    }
    if let Some(w) = &opts.work {
        ctx.rows_produced = w.clone();
    }
    ctx.allow_service = opts.allow_service;
    ctx.forbid_service = opts.forbid_service;
    ctx.outbound = opts.outbound.clone();
    ctx.outbound_budget = opts
        .outbound_budget
        .clone()
        .unwrap_or_else(|| crate::outbound::RequestBudget::new(&opts.outbound));
    ctx.rdfs = match &opts.rdfs {
        Some(r) => Some(r.schema(&ctx.snap)?),
        None => None,
    };
    ctx.use_cache = !opts.no_cache;
    ctx.extensions = opts.extensions.clone();
    if let Some(s) = &opts.service_scope {
        ctx.service_scope = s.clone();
    }
    if let Some(o) = opts.optimizations {
        ctx.opt = o;
    }
    ctx.base_iri = base.cloned();
    let resolve = |iris: &[String]| -> Vec<Id> { iris.iter().map(|i| ctx.graph_id(i)).collect() };
    let mut ds = DatasetSpec::default();
    if !opts.default_graph_uris.is_empty() || !opts.named_graph_uris.is_empty() {
        if opts
            .default_graph_uris
            .iter()
            .any(|g| g == ctx::UNION_GRAPH_IRI)
        {
            ds.union_default = true;
        } else if !opts.default_graph_uris.is_empty() {
            ds.default = Some(resolve(&opts.default_graph_uris));
        }
        if !opts.named_graph_uris.is_empty() {
            ds.named = Some(resolve(&opts.named_graph_uris));
        }
        // a protocol dataset replaces the store's: whichever part was not given is empty
        if (ds.default.is_some() || ds.union_default) && ds.named.is_none() {
            ds.named = Some(Vec::new());
        }
        if ds.named.is_some() && ds.default.is_none() && !ds.union_default {
            ds.default = Some(Vec::new());
        }
    } else if let Some(d) = dataset {
        ds.default = Some(resolve(
            &d.default
                .iter()
                .map(|n| n.as_str().to_string())
                .collect::<Vec<_>>(),
        ));
        ds.named = Some(resolve(
            &d.named
                .iter()
                .flatten()
                .map(|n| n.as_str().to_string())
                .collect::<Vec<_>>(),
        ));
    }
    // extra graphs merged into the store's default graph (inference overlay)
    if ds.default.is_none() && !ds.union_default && !opts.default_graph_extra.is_empty() {
        let mut d = vec![Id::DEFAULT_GRAPH];
        d.extend(resolve(&opts.default_graph_extra));
        ds.default = Some(d);
    }
    ctx.dataset = ds;
    restrict_ctx(&mut ctx, opts.graphs.as_ref())?;
    Ok(ctx)
}

/// Limit a query context's dataset to a graph view that does not read every graph (see
/// [`DatasetSpec::restrict`]).
pub(crate) fn restrict_ctx(
    ctx: &mut Ctx,
    graphs: Option<&Arc<crate::access::GraphAccess>>,
) -> Result<()> {
    let Some(a) = graphs.filter(|a| !a.reads_everything()) else {
        return Ok(());
    };
    let mut ds = std::mem::take(&mut ctx.dataset);
    let snap = ctx.snap.clone();
    ds.restrict(&snap, a, &|id| ctx.term(id))?;
    ctx.dataset = ds;
    ctx.graphs = Some(a.clone());
    Ok(())
}

fn split(
    q: &Query,
) -> (
    &GraphPattern,
    Option<&QueryDataset>,
    Option<&oxiri::Iri<String>>,
) {
    match q {
        Query::Select {
            pattern,
            dataset,
            base_iri,
        }
        | Query::Ask {
            pattern,
            dataset,
            base_iri,
        }
        | Query::Describe {
            pattern,
            dataset,
            base_iri,
        }
        | Query::Construct {
            pattern,
            dataset,
            base_iri,
            ..
        } => (pattern, dataset.as_ref(), base_iri.as_ref()),
    }
}

/// Execute a SPARQL query against a snapshot.
pub fn query(snap: Arc<Snapshot>, q: &str, opts: &QueryOptions) -> Result<QueryResult> {
    let t0 = Instant::now();
    let parsed = parse_query(q, opts.base_iri.as_deref(), &opts.prefixes)?;
    let parse_ms = t0.elapsed().as_secs_f64() * 1000.0;
    let mut r = execute_query(snap, &parsed, opts, parse_ms)?;
    if r.kind == QueryKind::Select {
        select_star_order(q, &mut r);
    }
    Ok(r)
}

/// `SELECT *`: spargebra projects variables in sorted order; Jena (and users) expect the
/// order of first appearance in the query text. [`query`] applies this; callers of
/// [`execute_query`] apply it themselves.
pub fn select_star_order(q: &str, r: &mut QueryResult) {
    static RE: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    let re = RE
        .get_or_init(|| regex::Regex::new(r"(?is)\bselect\s+(distinct\s+|reduced\s+)?\*").unwrap());
    if !re.is_match(q) {
        return;
    }
    let pos = |v: &str| {
        [format!("?{v}"), format!("${v}")]
            .iter()
            .filter_map(|pat| {
                q.match_indices(pat.as_str())
                    .find(|(i, _)| {
                        !q[i + pat.len()..].starts_with(|c: char| c.is_alphanumeric() || c == '_')
                    })
                    .map(|(i, _)| i)
            })
            .min()
            .unwrap_or(usize::MAX)
    };
    let mut order: Vec<usize> = (0..r.vars.len()).collect();
    order.sort_by_key(|&i| pos(&r.vars[i]));
    r.vars = order.iter().map(|&i| r.vars[i].clone()).collect();
    r.table.vars = order.iter().map(|&i| r.table.vars[i]).collect();
    r.table.cols = order
        .iter()
        .map(|&i| std::mem::take(&mut r.table.cols[i]))
        .collect();
}

pub fn execute_query(
    snap: Arc<Snapshot>,
    parsed: &Query,
    opts: &QueryOptions,
    parse_ms: f64,
) -> Result<QueryResult> {
    let depth = depth::check_query(parsed)?;
    depth::with_stack(depth, || execute_parsed(snap, parsed, opts, parse_ms))
}

fn execute_parsed(
    snap: Arc<Snapshot>,
    parsed: &Query,
    opts: &QueryOptions,
    parse_ms: f64,
) -> Result<QueryResult> {
    let t1 = Instant::now();
    let (pattern, dataset, base) = split(parsed);
    extensions::check_family(snap.dataset_id)?;
    let mut ctx = make_ctx(snap, opts, dataset, base)?;
    ctx.configure_extensions(pattern);
    let ctx = Arc::new(ctx);
    crate::geo::validate_query(pattern, &mut |w| ctx.warn(w))?;
    let mut planner = Planner::new(&ctx);
    planner.source = Some(parsed);
    let mut bound: Vec<(table::VarId, Id)> = Vec::new();
    for (name, term) in &opts.initial_bindings {
        let v = ctx.var(name.trim_start_matches(['?', '$']));
        let id = ctx.intern_outside_term(term);
        planner.subst.insert(v, id);
        bound.push((v, id));
    }
    let (kind, pattern) = match parsed {
        Query::Select { .. } => (QueryKind::Select, pattern.clone()),
        Query::Ask { .. } => (
            QueryKind::Ask,
            GraphPattern::Slice {
                inner: Box::new(pattern.clone()),
                start: 0,
                length: Some(1),
            },
        ),
        Query::Construct { .. } => (QueryKind::Construct, pattern.clone()),
        Query::Describe { .. } => (QueryKind::Describe, pattern.clone()),
    };
    let planned = rdfs::apply(&ctx, &pattern);
    let node = planner.plan(&planned, &ActiveGraph::Default, Vec::new())?;
    let plan_ms = t1.elapsed().as_secs_f64() * 1000.0;
    let t2 = Instant::now();
    let (table, mut plan) = exec::execute(&ctx, &node)?;
    plan.warnings = ctx.warnings();
    if ctx.graphs.is_some() {
        plan.redact();
    }
    let mut result = QueryResult {
        kind,
        vars: Vec::new(),
        table: Table::default(),
        boolean: false,
        triples: Vec::new(),
        quads: Vec::new(),
        plan,
        timing: Timing::default(),
        mem_peak_bytes: 0,
        rows_produced: 0,
        describe_truncated: false,
        ctx: ctx.clone(),
    };
    match parsed {
        Query::Select { .. } => {
            let vars: Vec<table::VarId> = match &pattern {
                GraphPattern::Project { variables, .. } => {
                    variables.iter().map(|v| ctx.var(v.as_str())).collect()
                }
                GraphPattern::Distinct { inner }
                | GraphPattern::Reduced { inner }
                | GraphPattern::Slice { inner, .. } => {
                    project_vars(inner, &ctx).unwrap_or_else(|| table.vars.clone())
                }
                _ => table.vars.clone(),
            };
            result.vars = vars.iter().map(|v| ctx.var_name(*v)).collect();
            result.table = table.project(&vars);
            // substituted variables report their bound value
            for (v, id) in &bound {
                if let Some(c) = result.table.col_of(*v) {
                    result.table.cols[c].iter_mut().for_each(|x| *x = *id);
                }
            }
        }
        Query::Ask { .. } => result.boolean = !table.is_empty(),
        Query::Construct {
            template,
            graph_templates,
            ..
        } => {
            (result.triples, result.quads) = construct(&ctx, &table, template, graph_templates);
        }
        Query::Describe { .. } => {
            // a dataset the query or the protocol gave, or the inference overlay
            let explicit = dataset.is_some()
                || !opts.default_graph_uris.is_empty()
                || !opts.named_graph_uris.is_empty()
                || !opts.default_graph_extra.is_empty();
            let d = describe::describe(&ctx, &table, &opts.describe, explicit)?;
            result.triples = d.triples;
            result.describe_truncated = d.truncated;
            result.plan.warnings = ctx.warnings();
        }
    }
    result.timing = Timing {
        parse_ms,
        plan_ms,
        exec_ms: t2.elapsed().as_secs_f64() * 1000.0,
        serialize_ms: 0.0,
        total_ms: parse_ms + t1.elapsed().as_secs_f64() * 1000.0,
    };
    result.mem_peak_bytes = ctx.mem_peak();
    result.rows_produced = ctx.rows_produced();
    ctx.check()?;
    Ok(result)
}

fn project_vars(gp: &GraphPattern, ctx: &Ctx) -> Option<Vec<table::VarId>> {
    match gp {
        GraphPattern::Project { variables, .. } => {
            Some(variables.iter().map(|v| ctx.var(v.as_str())).collect())
        }
        GraphPattern::Distinct { inner }
        | GraphPattern::Reduced { inner }
        | GraphPattern::Slice { inner, .. } => project_vars(inner, ctx),
        _ => None,
    }
}

/// EXPLAIN: algebra (SSE) and the plan without executing it.
pub fn explain(snap: Arc<Snapshot>, q: &str, opts: &QueryOptions) -> Result<(String, PlanInfo)> {
    let parsed = parse_query(q, opts.base_iri.as_deref(), &opts.prefixes)?;
    let depth = depth::check_query(&parsed)?;
    depth::with_stack(depth, || {
        let (pattern, dataset, base) = split(&parsed);
        extensions::check_family(snap.dataset_id)?;
        let mut ctx = make_ctx(snap, opts, dataset, base)?;
        ctx.configure_extensions(pattern);
        crate::geo::validate_query(pattern, &mut |w| ctx.warn(w))?;
        let mut planner = Planner::new(&ctx);
        planner.source = Some(&parsed);
        let pattern = rdfs::apply(&ctx, pattern);
        let node = planner.plan(&pattern, &ActiveGraph::Default, Vec::new())?;
        let mut info = exec::describe(&ctx, &node);
        info.warnings = ctx.warnings();
        if ctx.graphs.is_some() {
            info.redact();
        }
        Ok((parsed.to_sse(), info))
    })
}

/// Instantiate a template term (CONSTRUCT / INSERT), recursing into RDF 1.2 triple
/// terms. `var` resolves variables, `bnode` maps template blank node labels.
pub fn instantiate(
    tp: &TermPattern,
    var: &mut dyn FnMut(&str) -> Option<Term>,
    bnode: &mut dyn FnMut(&str) -> Term,
) -> Option<Term> {
    Some(match tp {
        TermPattern::Variable(v) => var(v.as_str())?,
        TermPattern::BlankNode(b) => bnode(b.as_str()),
        TermPattern::NamedNode(n) => Term::NamedNode(n.clone()),
        TermPattern::Literal(l) => Term::Literal(l.clone()),
        TermPattern::Triple(t) => {
            let s = match instantiate(&t.subject, var, bnode)? {
                Term::NamedNode(n) => NamedOrBlankNode::NamedNode(n),
                Term::BlankNode(b) => NamedOrBlankNode::BlankNode(b),
                _ => return None,
            };
            let p = match &t.predicate {
                NamedNodePattern::NamedNode(n) => n.clone(),
                NamedNodePattern::Variable(v) => match var(v.as_str())? {
                    Term::NamedNode(n) => n,
                    _ => return None,
                },
            };
            let o = instantiate(&t.object, var, bnode)?;
            Term::Triple(Box::new(Triple::new(s, p, o)))
        }
    })
}

/// Instantiate a CONSTRUCT template per solution: the default graph's triples, and the
/// quads of Jena ARQ's `GRAPH` blocks. A block whose name is unbound, or not an IRI or a
/// blank node, gives nothing for that solution; `urn:x-arq:DefaultGraph` and
/// `urn:x-arq:DefaultGraphNode` name the default graph. Blank nodes, the graph names'
/// included, are fresh per solution.
fn construct(
    ctx: &Ctx,
    t: &Table,
    template: &[TriplePattern],
    graphs: &[GraphTemplate],
) -> (Vec<Triple>, Vec<Quad>) {
    let map = t.var_map(ctx.nvars());
    let mut seen = FxHashSet::default();
    let mut out = Vec::new();
    let mut seen_quads = FxHashSet::default();
    let mut quads = Vec::new();
    for i in 0..t.len() {
        let mut bnodes: FxHashMap<String, BlankNode> = FxHashMap::default();
        let inst = |tp: &TermPattern, bnodes: &mut FxHashMap<String, BlankNode>| -> Option<Term> {
            instantiate(
                tp,
                &mut |v| {
                    let c = map.get(ctx.var(v) as usize).copied().flatten()?;
                    let id = t.cols[c][i];
                    if id.is_undef() { None } else { ctx.term(id) }
                },
                &mut |b| {
                    Term::BlankNode(
                        bnodes
                            .entry(b.to_string())
                            .or_insert_with(|| crate::store::bnode_for(ctx.fresh_bnode()))
                            .clone(),
                    )
                },
            )
        };
        let triple = |tp: &TriplePattern, bnodes: &mut FxHashMap<String, BlankNode>| {
            let s = inst(&tp.subject, bnodes);
            let p = match &tp.predicate {
                NamedNodePattern::NamedNode(n) => Some(Term::NamedNode(n.clone())),
                NamedNodePattern::Variable(v) => inst(&TermPattern::Variable(v.clone()), bnodes),
            };
            let o = inst(&tp.object, bnodes);
            let (Some(s), Some(Term::NamedNode(p)), Some(o)) = (s, p, o) else {
                return None;
            };
            let s = match s {
                Term::NamedNode(n) => NamedOrBlankNode::NamedNode(n),
                Term::BlankNode(b) => NamedOrBlankNode::BlankNode(b),
                _ => return None,
            };
            Some(Triple::new(s, p, o))
        };
        for tp in template {
            if let Some(tr) = triple(tp, &mut bnodes)
                && seen.insert(tr.clone())
            {
                out.push(tr);
            }
        }
        for g in graphs {
            let name = match inst(&g.name, &mut bnodes) {
                Some(Term::NamedNode(n))
                    if n.as_str() == ctx::DEFAULT_GRAPH_IRI
                        || n.as_str() == ctx::DEFAULT_GRAPH_NODE_IRI =>
                {
                    GraphName::DefaultGraph
                }
                Some(Term::NamedNode(n)) => GraphName::NamedNode(n),
                Some(Term::BlankNode(b)) => GraphName::BlankNode(b),
                _ => continue,
            };
            for tp in &g.triples {
                let Some(tr) = triple(tp, &mut bnodes) else {
                    continue;
                };
                if name == GraphName::DefaultGraph {
                    if seen.insert(tr.clone()) {
                        out.push(tr);
                    }
                } else {
                    let q = tr.in_graph(name.clone());
                    if seen_quads.insert(q.clone()) {
                        quads.push(q);
                    }
                }
            }
        }
    }
    (out, quads)
}

#[cfg(test)]
mod access_tests;
#[cfg(test)]
mod charsets_tests;
#[cfg(test)]
mod costcal_tests;
#[cfg(test)]
mod exists_tests;
#[cfg(test)]
mod exprcache_tests;
#[cfg(test)]
mod indexjoin_tests;
#[cfg(test)]
mod join_tests;
#[cfg(test)]
mod joinorder_tests;
#[cfg(test)]
mod keyprobe_tests;
#[cfg(test)]
mod opt_tests;
#[cfg(test)]
mod sample_tests;
#[cfg(test)]
mod stats_tests;
#[cfg(test)]
mod strfilter_tests;
#[cfg(test)]
mod tests;
