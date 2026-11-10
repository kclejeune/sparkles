//! SPARQL 1.1 query & update engine (ARQ equivalent).

pub mod aggext;
pub mod algorun;
pub mod algosvc;
pub mod arqpf;
pub mod cache;
pub mod catalog;
pub mod cdt;
pub mod charsets;
mod constkeys;
pub mod ctx;
pub mod cursor;
pub mod depth;
pub mod describe;
pub mod enhancer;
mod eqkey;
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
pub mod output;
pub mod pathsearch;
pub mod patternplan;
pub mod plan;
pub mod policy;
pub mod pool;
mod propertyext;
pub(crate) mod query_memory;
mod query_regex;
mod rangemask;
pub mod rdfs;
pub mod reasoning;
mod registeredagg;
pub mod results;
mod sample;
mod sortkey;
pub mod stats;
pub mod svccache;
pub mod table;
pub mod textpf;
mod unionpush;
pub mod update;
pub mod value;
mod valuespush;
mod vectortopk;
pub mod viewsvc;
mod zipper;

use crate::error::{Error, Result};
use crate::id::Id;
use crate::ioacct;
use crate::store::Snapshot;
pub use ctx::{Ctx, DatasetSpec, Optimizations};
pub use cursor::{
    AskResult, CursorOptions, CursorPlan, CursorStats, CursorStatus, ExecutionMode, FallbackPolicy,
    GraphBatch, GraphCursor, MaterializedResult, QueryBatch, QueryCursor, QueryExecution,
    ask_streaming, graph_cursor, query_cursor, query_execution, query_execution_with_plan,
    select_cursor, select_source_cursor,
};
pub use exec::{PlanFidelity, PlanInfo};
pub use policy::{Consumer, PolicyError, choose_execution};

/// `s` in Unicode Normalization Form C, as `fn:normalize-unicode` gives it.
pub fn nfc(s: &str) -> String {
    icu_normalizer::ComposingNormalizerBorrowed::new_nfc()
        .normalize(s)
        .into_owned()
}
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
    /// An absolute deadline, intersected with the relative timeout without renewal.
    pub deadline: Option<Instant>,
    /// Additional cancellation sources, intersected with `cancel` at work checks.
    pub cancellation: Option<crate::task::Cancellation>,
    /// options for write guards (updates)
    pub write: crate::guard::WriteOptions,
    /// protocol `default-graph-uri` (overrides FROM)
    pub default_graph_uris: Vec<String>,
    /// protocol `named-graph-uri` (overrides FROM NAMED)
    pub named_graph_uris: Vec<String>,
    pub base_iri: Option<String>,
    /// Maximum rows in an intermediate table or distinct CONSTRUCT graph output.
    pub max_rows: Option<usize>,
    /// Budget for the estimated memory of intermediate results and retained decoded
    /// graph output (`None`: unlimited);
    /// exceeding it fails with [`Error::BudgetExceeded`].
    pub max_memory_bytes: Option<u64>,
    /// The query's reservation in a server-wide memory pool (see [`pool`]). The server
    /// admits the request and passes the grant it got, and the query grows and shrinks
    /// it with its charges. An update's WHERE clauses share it, one at a time.
    pub grant: Option<Arc<pool::PoolGrant>>,
    /// A pool to take a grant from when `grant` is unset. The query then starts with an
    /// empty grant and fails with [`BudgetKind::ServerMemory`](crate::BudgetKind) when
    /// the pool cannot cover its charges. Nothing queues.
    pub pool: Option<Arc<pool::QueryPool>>,
    /// Where cursor operators may move state that outgrows the memory budget (see
    /// [`crate::spill`]). `None` turns spill off, and such operators fail with their
    /// memory budget.
    pub spill: Option<Arc<crate::spill::Spill>>,
    /// The disk this query's spill files may take, at most the server's limit for one
    /// query (`None`: that limit).
    pub max_spill_bytes: Option<u64>,
    /// Budget for the rows all operators of a query produce together, including
    /// distinct CONSTRUCT output (`None`: unlimited); exceeding it fails with
    /// [`Error::BudgetExceeded`]. An update's WHERE clauses share one count.
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
    /// Leave out the plan's references to the query's text, the `patterns`,
    /// `filterVars` and `joinVars` of spec U04 §11.3. A caller that never shows the plan
    /// sets it, so that a plain query does not name its patterns for nobody.
    pub no_plan_patterns: bool,
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
    /// Query-time reasoning (spec F17): the reasoner that derives, for this query, the
    /// facts its patterns need (see [`reasoning`]). With it, the query reads neither the
    /// inferred graph of `default_graph_extra` nor RDFS on read.
    pub reasoning: Option<Arc<dyn reasoning::ReasoningProvider>>,
    /// The named projections of the dataset, for `algo:projection` in a graph algorithm
    /// call (see [`crate::algo::catalog`]).
    pub algo_projections: Option<Arc<crate::algo::catalog::Catalog>>,
    /// The dataset's views, which `SERVICE view:read` reads (see [`viewsvc`]).
    pub views: Option<Arc<dyn viewsvc::ViewSource>>,
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
    /// The query endpoints of the process that runs the query. A SERVICE whose IRI one
    /// of them names exactly runs in this process instead of over HTTP (see
    /// [`LocalEndpoints`]).
    pub local_endpoints: Option<Arc<dyn LocalEndpoints>>,
    /// How many in-process SERVICE calls this query runs inside (0 for a request's own
    /// query). A call deeper than [`MAX_SERVICE_DEPTH`] fails.
    pub service_depth: u32,
}

/// The most in-process SERVICE calls that nest inside one another.
pub const MAX_SERVICE_DEPTH: u32 = 8;

/// The query endpoints of the process that runs a query, so that a SERVICE call to the
/// same server runs in process under the caller's grant, deadline and cancellation
/// instead of queueing behind the caller for admission (X08-P1-nested-service).
pub trait LocalEndpoints: Send + Sync + std::fmt::Debug {
    /// For an IRI that exactly names one of this process's query endpoints, the
    /// snapshot to query and the options the caller would have there, with the caller's
    /// view of the dataset. `None` leaves the call to HTTP. An error fails the call, for
    /// a caller that may not query the dataset it names or a dataset that is out of
    /// service.
    fn resolve(&self, iri: &str) -> Option<Result<LocalTarget>>;
}

/// The dataset an in-process SERVICE call queries (see [`LocalEndpoints`]).
pub struct LocalTarget {
    pub snap: Arc<Snapshot>,
    /// The caller's options on that dataset. The engine lowers their budgets to the
    /// calling query's and gives them its deadline, cancellation and grant.
    pub options: QueryOptions,
    /// Held until the call ends, such as the server's handle that keeps a dataset from
    /// being swapped out by a restore while a request reads it.
    pub hold: Option<Box<dyn std::any::Any + Send + Sync>>,
}

impl QueryOptions {
    /// The grant a query under these options holds: the one given, else an empty one from
    /// the pool.
    pub(crate) fn query_grant(&self) -> Option<Arc<pool::PoolGrant>> {
        match (&self.grant, &self.pool) {
            (Some(g), _) => Some(g.clone()),
            (None, Some(p)) => Some(Arc::new(p.empty_grant())),
            (None, None) => None,
        }
    }

    /// The earlier absolute or relative bound, anchored once by the caller.
    pub(crate) fn effective_deadline(&self, started: Instant) -> Result<Option<Instant>> {
        let relative = self
            .timeout
            .map(|timeout| {
                started
                    .checked_add(timeout)
                    .ok_or_else(|| Error::invalid("query timeout is too large"))
            })
            .transpose()?;
        Ok(earlier_deadline(self.deadline, relative))
    }
}

pub(crate) fn earlier_deadline(a: Option<Instant>, b: Option<Instant>) -> Option<Instant> {
    a.into_iter().chain(b).min()
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

/// An opaque immutable result. Clones retain the same physical backing and
/// original query account; they neither copy payload nor mint receiving credit.
#[derive(Clone)]
pub struct QueryResult {
    pub kind: QueryKind,
    pub boolean: bool,
    pub timing: Timing,
    pub mem_peak_bytes: u64,
    pub rows_produced: u64,
    pub describe_truncated: bool,
    inner: Arc<ResultBacking>,
}

struct ResultBacking {
    variables: Option<Arc<cursor::Variables>>,
    table: Table,
    triples: Vec<Triple>,
    quads: Vec<Quad>,
    plan: PlanInfo,
    depth: usize,
    ctx: Arc<Ctx>,
    _table_credit: Option<table::FinalTableCredit>,
    _graph_credit: Option<ctx::OutputCredit>,
    _header: ctx::OutputCredit,
}
impl Drop for ResultBacking {
    fn drop(&mut self) {
        depth::with_stack(self.depth, || {
            self.triples.clear();
            self.quads.clear();
            self.plan.children.clear();
        });
    }
}

// Mutable construction is private. Freeze cannot lend mutable owning fields to
// callers or detach a credited payload, even when the public handle is cloned.
struct ResultParts {
    kind: QueryKind,
    variables: Option<Arc<cursor::Variables>>,
    table: Table,
    boolean: bool,
    triples: Vec<Triple>,
    quads: Vec<Quad>,
    plan: PlanInfo,
    timing: Timing,
    mem_peak_bytes: u64,
    rows_produced: u64,
    describe_truncated: bool,
    ctx: Arc<Ctx>,
    _table_credit: Option<table::FinalTableCredit>,
    _graph_credit: Option<ctx::OutputCredit>,
}
impl ResultParts {
    /// Publish the parts as a shared result. Only a graph result's term scan grows with
    /// the answer, so only a graph result times this into its execution, and the only
    /// check follows the header's admission. A SELECT or ASK result takes no timing here.
    fn freeze(self) -> Result<QueryResult> {
        let started = (!self.triples.is_empty() || !self.quads.is_empty()).then(Instant::now);
        fn levels(plan: &PlanInfo) -> usize {
            stacker::maybe_grow(exec::PLAN_RED_ZONE, exec::PLAN_STACK_SEGMENT, || {
                1 + plan.children.iter().map(levels).max().unwrap_or(0)
            })
        }
        let header = self
            .ctx
            .charge(cursor::arc_backing_bytes::<ResultBacking>(&self.ctx)?)?
            .into_output(&self.ctx);
        #[cfg(test)]
        {
            use crate::algo::memory::probe;
            probe::at(probe::Point::Allocation("query-result-backing", 1));
            if probe::fails("query-result-backing") {
                return Err(Error::invalid("injected result backing allocation failure"));
            }
        }
        let graph_depth = self
            .triples
            .iter()
            .map(|t| output::term_depth(&t.object))
            .chain(self.quads.iter().map(|q| output::term_depth(&q.object)))
            .max()
            .unwrap_or(0);
        let depth = levels(&self.plan).max(graph_depth);
        self.ctx.check()?;
        let mut result = QueryResult {
            kind: self.kind,
            boolean: self.boolean,
            timing: self.timing,
            mem_peak_bytes: self.mem_peak_bytes.max(self.ctx.mem_peak()),
            rows_produced: self.rows_produced,
            describe_truncated: self.describe_truncated,
            inner: Arc::new(ResultBacking {
                variables: self.variables,
                table: self.table,
                triples: self.triples,
                quads: self.quads,
                plan: self.plan,
                depth,
                ctx: self.ctx,
                _table_credit: self._table_credit,
                _graph_credit: self._graph_credit,
                _header: header,
            }),
        };
        if let Some(started) = started {
            let elapsed = started.elapsed().as_secs_f64() * 1000.0;
            result.timing.exec_ms += elapsed;
            result.timing.total_ms += elapsed;
        }
        Ok(result)
    }
}

impl QueryResult {
    // Test observability is absent from application/binding library builds.
    #[cfg(test)]
    pub(crate) fn test_context(&self) -> &Arc<Ctx> {
        &self.inner.ctx
    }
    #[cfg(test)]
    pub(crate) fn test_table(&self) -> &Table {
        &self.inner.table
    }
    // Explicit application copies used only by semantic comparison fixtures.
    // Ownership regressions use the actual public owner APIs, never this helper.
    #[cfg(test)]
    pub(crate) fn test_application_rows(&self) -> Vec<Vec<Option<Term>>> {
        self.rows()
            .expect("application test answer decoding")
            .iter()
            .map(|row| row.values().to_vec())
            .collect()
    }

    pub fn variables(&self) -> &[String] {
        self.inner
            .variables
            .as_ref()
            .map_or(&[], |variables| &variables.names)
    }
    pub fn table(&self) -> table::TableView<'_> {
        table::TableView::new(&self.inner.table)
    }
    pub fn triples(&self) -> &[Triple] {
        &self.inner.triples
    }
    pub fn quads(&self) -> &[Quad] {
        &self.inner.quads
    }
    pub fn plan(&self) -> &PlanInfo {
        &self.inner.plan
    }

    /// Whether this query actually called an algorithm.
    pub fn uses_algorithms(&self) -> bool {
        self.inner.ctx.calls_algorithm()
    }

    /// Borrow query-time reasoning metadata while the actual result stays live.
    pub fn reasoning(&self) -> Option<&reasoning::Applied> {
        self.inner.ctx.reasoning.as_deref()
    }

    /// Decode an ID into an immutable owner of its original query credit.
    /// New decoding checks current controls and can refuse; a previously decoded
    /// owner can still borrow its payload after the result or query has stopped.
    pub fn term(&self, id: Id) -> Result<Option<output::OwnedTerm>> {
        self.output_budget().decode(id)
    }

    /// The original receiving account for explicit, independently owned copies.
    pub fn output_budget(&self) -> output::OutputBudget {
        output::OutputBudget::new(self.inner.ctx.clone())
    }

    /// Decode a complete SELECT row before publishing its credited owner.
    pub fn row(&self, row: usize) -> Result<output::OwnedRow> {
        self.inner.ctx.check()?;
        if row >= self.inner.table.len() {
            return Err(Error::invalid("query result row is out of bounds"));
        }
        self.output_budget()
            .decode_rows(1, self.inner.table.width(), |_, column| {
                self.inner.table.get(row, column)
            })?
            .row_handle(0)
    }

    pub fn len(&self) -> usize {
        match self.kind {
            QueryKind::Select => self.inner.table.len(),
            QueryKind::Ask => 1,
            _ => self.inner.triples.len() + self.inner.quads.len(),
        }
    }
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Decode the complete SELECT answer under its original receiving account.
    /// The immutable returned backing retains all cells and row shape until the
    /// last row/cell/clone drops, including zero-width solutions.
    pub fn rows(&self) -> Result<output::OwnedRows> {
        self.output_budget().decode_rows(
            self.inner.table.len(),
            self.inner.table.width(),
            |row, column| self.inner.table.get(row, column),
        )
    }

    /// Decode only the selected half-open page under this actual result owner.
    /// Unselected payloads are not copied. Bounds refuse before allocating.
    pub fn rows_range(&self, range: std::ops::Range<usize>) -> Result<output::OwnedRows> {
        self.inner.ctx.check()?;
        if self.kind != QueryKind::Select
            || range.start > range.end
            || range.end > self.inner.table.len()
        {
            return Err(Error::invalid("query result SELECT range is out of bounds"));
        }
        self.output_budget()
            .decode_rows(range.len(), self.inner.table.width(), |row, column| {
                self.inner.table.get(range.start + row, column)
            })
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
    let (query, depth) = depth::checked_query(p.parse_query(q)?)?;
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
    make_ctx_input(snap, opts, dataset, base, None)
}

fn make_ctx_input(
    snap: Arc<Snapshot>,
    opts: &QueryOptions,
    dataset: Option<&QueryDataset>,
    base: Option<&oxiri::Iri<String>>,
    source: Option<crate::store::BoundReadSource>,
) -> Result<Ctx> {
    make_ctx_input_policy(snap, opts, dataset, base, source, None)
}

// Only the application root which just prepared this policy supplies the override.
// Checked public sources retain the existing exact-policy replacement refusal.
fn make_ctx_input_policy(
    snap: Arc<Snapshot>,
    opts: &QueryOptions,
    dataset: Option<&QueryDataset>,
    base: Option<&oxiri::Iri<String>>,
    source: Option<crate::store::BoundReadSource>,
    prepared_policy: Option<&Arc<crate::access::GraphAccess>>,
) -> Result<Ctx> {
    #[cfg(feature = "geo")]
    let op_vertices = snap.geo_op_vertices;
    let (mut ctx, graphs) = match source {
        Some(source) => make_source_ctx_policy(source, opts, prepared_policy)?,
        None => {
            let snap = with_union_default(snap, opts);
            // Legacy guards are bound before their mask, as before.
            let graphs = opts.graphs.as_ref().map(|a| a.bound(&snap));
            let snap = match &graphs {
                Some(a) => a.masked(&snap)?,
                None => snap,
            };
            (Ctx::new(snap), graphs)
        }
    };
    // the store's limit on one geometry operation
    #[cfg(feature = "geo")]
    ctx.geo.set_op_vertices(op_vertices);
    if ctx.read_source.is_none() {
        ctx.deadline = match opts.timeout {
            Some(_) => opts.effective_deadline(Instant::now())?,
            None => opts.deadline,
        };
    }
    ctx.cancellation = opts.cancellation.clone();
    if let Some(c) = &opts.cancel {
        ctx.cancel = c.clone();
    }
    if let Some(m) = opts.max_rows {
        ctx.max_rows = m;
    }
    if let Some(m) = opts.max_memory_bytes {
        ctx.mem_limit = m;
    }
    ctx.spill = opts
        .spill
        .as_ref()
        .map(|spill| Arc::new(spill.query(opts.max_spill_bytes)));
    if ctx.read_source.is_none() {
        ctx.set_grant(opts.query_grant());
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
        Some(r) => Some(r.schema_controlled(&ctx.snap, &mut || ctx.check())?),
        None => None,
    };
    ctx.use_cache = !opts.no_cache;
    ctx.plan_patterns = !opts.no_plan_patterns;
    ctx.algo_projections = opts.algo_projections.clone();
    ctx.views = opts.views.clone();
    ctx.extensions = opts.extensions.clone();
    if let Some(s) = &opts.service_scope {
        ctx.service_scope = s.clone();
    }
    ctx.local_endpoints = opts.local_endpoints.clone();
    ctx.service_depth = opts.service_depth;
    if let Some(o) = opts.optimizations {
        ctx.opt = o;
    }
    ctx.base_iri = base.cloned();
    let mut ds = DatasetSpec::default();
    if !opts.default_graph_uris.is_empty() || !opts.named_graph_uris.is_empty() {
        if opts
            .default_graph_uris
            .iter()
            .any(|g| g == ctx::UNION_GRAPH_IRI)
        {
            ds.union_default = true;
        } else if !opts.default_graph_uris.is_empty() {
            ds.default = Some(resolve_graphs(
                &mut ctx,
                opts.default_graph_uris.iter().map(String::as_str),
                opts.default_graph_uris.len(),
            )?);
        }
        if !opts.named_graph_uris.is_empty() {
            ds.named = Some(resolve_graphs(
                &mut ctx,
                opts.named_graph_uris.iter().map(String::as_str),
                opts.named_graph_uris.len(),
            )?);
        }
        // a protocol dataset replaces the store's: whichever part was not given is empty
        if (ds.default.is_some() || ds.union_default) && ds.named.is_none() {
            ds.named = Some(Vec::new());
        }
        if ds.named.is_some() && ds.default.is_none() && !ds.union_default {
            ds.default = Some(Vec::new());
        }
    } else if let Some(d) = dataset {
        ds.default = Some(resolve_graphs(
            &mut ctx,
            d.default.iter().map(|n| n.as_str()),
            d.default.len(),
        )?);
        ds.named = Some(resolve_graphs(
            &mut ctx,
            d.named.iter().flatten().map(|n| n.as_str()),
            d.named.as_ref().map_or(0, Vec::len),
        )?);
    }
    // extra graphs merged into the store's default graph (inference overlay)
    if ds.default.is_none() && !ds.union_default && !opts.default_graph_extra.is_empty() {
        ds.default = Some(resolve_graphs(
            &mut ctx,
            std::iter::once(ctx::DEFAULT_GRAPH_IRI)
                .chain(opts.default_graph_extra.iter().map(String::as_str)),
            opts.default_graph_extra
                .len()
                .checked_add(1)
                .ok_or_else(|| Error::invalid("default dataset size overflow"))?,
        )?);
    }
    ctx.dataset = ds;
    if ctx.read_source.is_some() {
        restrict_source_ctx(&mut ctx, graphs.as_ref())?;
    } else {
        restrict_ctx(&mut ctx, graphs.as_ref())?;
    }
    Ok(ctx)
}

pub(crate) fn source_grant(
    preparation: &crate::access::Preparation,
    opts: &QueryOptions,
) -> Result<Option<Arc<pool::PoolGrant>>> {
    let original = preparation.grant();
    if let Some(requested) = &opts.grant {
        if original.is_some_and(|grant| Arc::ptr_eq(grant, requested)) {
            return Ok(Some(requested.clone()));
        }
        let same_pool = original.is_some_and(|grant| Arc::ptr_eq(grant.pool(), requested.pool()));
        let (code, reason) = if same_pool {
            (
                "foreign-grant",
                sparkles_budget::ParentRefused::DifferentParent,
            )
        } else {
            (
                "foreign-domain",
                sparkles_budget::ParentRefused::ForeignDomain,
            )
        };
        return Err(Error::Component(Box::new(
            crate::error::ComponentError::new(
                "query-memory",
                code,
                "checked source retains its actual original grant",
            )
            .with_source(reason),
        )));
    }
    if let Some(pool) = &opts.pool
        && original.is_none_or(|grant| !Arc::ptr_eq(grant.pool(), pool))
    {
        return Err(Error::Component(Box::new(
            crate::error::ComponentError::new(
                "query-memory",
                "foreign-domain",
                "checked source belongs to another accounting pool",
            )
            .with_source(sparkles_budget::ParentRefused::ForeignDomain),
        )));
    }
    Ok(original.cloned())
}

#[cfg(test)]
fn make_source_ctx(
    source: crate::store::BoundReadSource,
    opts: &QueryOptions,
) -> Result<(Ctx, Option<Arc<crate::access::GraphAccess>>)> {
    make_source_ctx_policy(source, opts, None)
}

fn make_source_ctx_policy(
    source: crate::store::BoundReadSource,
    opts: &QueryOptions,
    prepared_policy: Option<&Arc<crate::access::GraphAccess>>,
) -> Result<(Ctx, Option<Arc<crate::access::GraphAccess>>)> {
    let deadline = opts.effective_deadline(Instant::now())?;
    let (snapshot, _, preparation) = source.query_input();
    let preparation = preparation.with_query_controls(
        opts.cancel.clone(),
        opts.cancellation.clone(),
        deadline,
    )?;
    let snapshot = snapshot.clone();
    // Covers the actual Ctx Arc, snapshot header, lazy ledger/owner bookkeeping,
    // and the fixed Arc-backed counters/default policy fields created by Ctx::new.
    // All variable-size query/graph data needs separate admission.
    let setup_bytes = std::mem::size_of::<Ctx>()
        .checked_add(std::mem::size_of::<Snapshot>())
        .and_then(|n| n.checked_add(4096))
        .and_then(|n| u64::try_from(n).ok())
        .ok_or_else(|| Error::invalid("checked context setup overflow"))?;
    // Validate real grant/pool identity before reserving or allocating a new Ctx.
    // A shared pool is not permission to relabel the source's existing parent.
    let grant = source_grant(&preparation, opts)?;
    let setup = preparation.reserve(setup_bytes)?;
    let parent = preparation.memory_parent()?;
    let mut ctx = Ctx::new(snapshot);
    ctx.read_source = Some(source);
    ctx.source_setup = Some(setup);
    ctx.source_preparation = Some(preparation.clone());
    ctx.deadline = deadline;
    ctx.cancellation = opts.cancellation.clone();
    if let Some(cancel) = &opts.cancel {
        ctx.cancel = cancel.clone();
    }
    ctx.set_grant(grant);
    ctx.set_memory_parent(parent)?;
    #[cfg(test)]
    checked_source_tests::observe_source_context(&ctx);
    ctx.check()?;
    ctx.snap = with_union_default(ctx.snap.clone(), opts);
    // Declare this sharing handle after Ctx: even an early refusal must destroy
    // every local policy clone before the original source's credit is returned.
    let prepared_access = ctx
        .read_source
        .as_ref()
        .expect("checked source")
        .query_input()
        .1
        .cloned();
    let access = match prepared_access {
        Some(access) => {
            if prepared_policy
                .or(opts.graphs.as_ref())
                .is_some_and(|requested| !Arc::ptr_eq(requested, &access))
            {
                return Err(Error::invalid(
                    "checked prepared access cannot be replaced; prepare the required policy first",
                ));
            }
            access
        }
        None => {
            let access = opts.graphs.as_ref().ok_or_else(|| {
                Error::NotPermitted(
                    "historical source query requires an explicit current access policy".into(),
                )
            })?;
            let view = access.masked_controlled(&ctx.snap, &preparation)?;
            let (snapshot, access, _) = view.query_input();
            ctx.snap = snapshot.clone();
            let access = access.clone();
            ctx.source_view = Some(view);
            access
        }
    };
    ctx.check()?;
    Ok((ctx, Some(access)))
}

fn resolve_graphs<'a>(
    ctx: &mut Ctx,
    iris: impl Iterator<Item = &'a str>,
    count: usize,
) -> Result<Vec<Id>> {
    let Some(_source) = ctx.read_source.as_ref() else {
        return Ok(iris.map(|iri| ctx.graph_id(iri)).collect());
    };
    let preparation = ctx
        .source_preparation
        .as_ref()
        .expect("source controls")
        .clone();
    let bytes = count
        .checked_mul(std::mem::size_of::<Id>())
        .and_then(|n| u64::try_from(n).ok())
        .ok_or_else(|| Error::invalid("dataset graph size overflow"))?;
    let held = preparation.reserve(bytes)?;
    let mut ids = Vec::new();
    ids.try_reserve_exact(count)
        .map_err(|_| Error::invalid("dataset graph allocation failed"))?;
    if ids.capacity() != count {
        return Err(Error::invalid("dataset graph capacity exceeds admission"));
    }
    for iri in iris {
        ctx.check()?;
        ids.push(ctx.graph_id(iri));
        ctx.check()?;
    }
    ctx.source_setup
        .as_mut()
        .expect("source setup owner")
        .merge(held);
    Ok(ids)
}

fn restrict_source_ctx(
    ctx: &mut Ctx,
    access: Option<&Arc<crate::access::GraphAccess>>,
) -> Result<()> {
    use crate::access::PreparationBuffer as Buffer;
    let Some(access) = access.filter(|access| !access.reads_everything()) else {
        return Ok(());
    };
    let preparation = ctx
        .source_preparation
        .as_ref()
        .expect("source controls")
        .clone();
    if access.reads_all() {
        ctx.graphs = Some(access.clone());
        return ctx.check();
    }
    let mut visible = Buffer::new(&preparation)?;
    let mut every = true;
    let mut last = None;
    preparation.scan(&ctx.snap, crate::index::Perm::Gspo, &[], |quad| {
        let graph = quad[3];
        if graph == Id::DEFAULT_GRAPH || last == Some(graph) {
            return Ok(true);
        }
        last = Some(graph);
        let allowed = preparation.with_term(&ctx.snap, graph, |term| {
            preparation.allows(&access.read, term)
        })?;
        if allowed {
            visible.push(graph)?;
        } else {
            every = false;
        }
        Ok(true)
    })?;
    #[cfg(test)]
    checked_source_tests::observe_union(&preparation, None);
    // GSPO visits distinct graph ids in order; do not populate an uncredited
    // snapshot policy cache or allocate an intermediate enumeration.
    // A failed second union must destroy the first union payload before its
    // actual credit returns. Locals drop in reverse declaration order.
    let mut kept = preparation.reserve(0)?;
    let mut dataset = std::mem::take(&mut ctx.dataset);
    let allowed = |graph: Id| -> Result<bool> {
        preparation.check()?;
        if graph == Id::DEFAULT_GRAPH {
            return Ok(access.read.default_graph());
        }
        if visible.binary_search(&graph).is_ok() {
            return Ok(true);
        }
        if matches!(
            graph.tag(),
            crate::id::Tag::BNode | crate::id::Tag::Undef | crate::id::Tag::Special
        ) {
            return Ok(false);
        }
        if graph.tag() == crate::id::Tag::Local {
            let credit = ctx.charge(0)?;
            let term = ctx.output_term(graph, &credit)?;
            preparation.allows(&access.read, term.as_ref())
        } else {
            preparation.with_term(&ctx.snap, graph, |term| {
                preparation.allows(&access.read, term)
            })
        }
    };
    let mut union = || -> Result<Vec<Id>> {
        let bytes = (visible.len() as u64)
            .checked_mul(std::mem::size_of::<Id>() as u64)
            .ok_or_else(|| Error::invalid("visible graph size overflow"))?;
        kept.grow_by(bytes)?;
        let mut ids = Vec::new();
        ids.try_reserve_exact(visible.len())
            .map_err(|_| Error::invalid("visible graph allocation failed"))?;
        if ids.capacity() != visible.len() {
            return Err(Error::invalid("visible graph capacity exceeds admission"));
        }
        for &graph in visible.iter() {
            preparation.check()?;
            ids.push(graph);
        }
        #[cfg(test)]
        checked_source_tests::observe_union(&preparation, Some((ids.as_ptr() as usize, bytes)));
        Ok(ids)
    };
    let filter = |mut ids: Vec<Id>| -> Result<Vec<Id>> {
        let mut out = 0;
        for index in 0..ids.len() {
            if allowed(ids[index])? {
                ids[out] = ids[index];
                out += 1;
            }
        }
        ids.truncate(out);
        Ok(ids)
    };
    dataset.default = match dataset.default.take() {
        None if every && (dataset.union_default || ctx.snap.union_default_graph) => None,
        _ if dataset.union_default => Some(union()?),
        None if ctx.snap.union_default_graph => Some(union()?),
        None if access.read.default_graph() => None,
        None => Some(Vec::new()),
        Some(ids) => Some(filter(ids)?),
    };
    if dataset.default.is_some() {
        dataset.union_default = false;
    }
    dataset.named = match dataset.named.take() {
        None if every => None,
        None => Some(union()?),
        Some(ids) => Some(filter(ids)?),
    };
    ctx.source_setup
        .as_mut()
        .expect("source setup owner")
        .merge(kept);
    ctx.dataset = dataset;
    ctx.graphs = Some(access.clone());
    ctx.check()
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
    let snap = ctx.snap.clone();
    // a view not bound yet is decided against the context's snapshot
    let a = &a.bound(&snap);
    let mut ds = std::mem::take(&mut ctx.dataset);
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
    query_with_plan(snap, q, opts).map_err(|f| f.error)
}

/// Query a genuine retained source. Current policy is still required for an
/// evidence source; a prepared source retains and applies its exact bound policy.
/// Neither source grants current authorization. Trusted callers must establish
/// current authority and source availability, and prepare a changed required
/// policy rather than bypass a rejected override by retrying without one.
/// The source and descendants share its original Preparation allowance. This
/// entry does not make legacy raw result extractors a bounded ownership API.
pub fn query_source(
    source: crate::store::BoundReadSource,
    store: &crate::store::Store,
    q: &str,
    opts: &QueryOptions,
) -> Result<QueryResult> {
    source.validate_query_instance(store)?;
    source.query_input().2.check()?;
    let started = Instant::now();
    let parsed = parse_query(q, opts.base_iri.as_deref(), &opts.prefixes)?;
    let parse_ms = started.elapsed().as_secs_f64() * 1000.0;
    let mut result = execute_retained_source(source, &parsed, opts, parse_ms)?;
    if result.kind == QueryKind::Select {
        select_star_order(q, &mut result)?;
    }
    Ok(result)
}

/// Run query text over a source that this crate has just prepared, without the store
/// instance check of [`query_source`]. Permission preparation evaluates the patterns of
/// protections this way, so that their work is charged to the same preparation.
pub(crate) fn query_prepared_source(
    source: crate::store::BoundReadSource,
    q: &str,
    opts: &QueryOptions,
) -> Result<QueryResult> {
    source.query_input().2.check()?;
    let parsed = parse_query(q, opts.base_iri.as_deref(), &opts.prefixes)?;
    execute_retained_source(source, &parsed, opts, 0.0)
}

/// Execute an already parsed query using a genuine physical source and the
/// actual Store. The caller still supplies current policy and original controls;
/// retaining a source grants no authority and cannot relabel its paid parent.
pub fn execute_source(
    source: crate::store::BoundReadSource,
    store: &crate::store::Store,
    parsed: &Query,
    opts: &QueryOptions,
    parse_ms: f64,
) -> Result<QueryResult> {
    source.validate_query_instance(store)?;
    source.query_input().2.check()?;
    execute_retained_source(source, parsed, opts, parse_ms)
}

fn execute_retained_source(
    source: crate::store::BoundReadSource,
    parsed: &Query,
    opts: &QueryOptions,
    parse_ms: f64,
) -> Result<QueryResult> {
    source.query_input().2.check()?;
    let stack = depth::check_query(parsed)?;
    depth::with_stack(stack, || {
        let snapshot = source.query_input().0.clone();
        let start = Instant::now();
        let prepared = prepare_query_input(snapshot, parsed, opts, None, None, Some(source))?;
        let plan_ms = start.elapsed().as_secs_f64() * 1000.0;
        depth::with_stack(prepared.plan_depth, || {
            execute_prepared(parsed, opts, parse_ms, plan_ms, prepared)
                .map_err(|failed| failed.error)
        })
    })
}

/// A query that failed, with its plan as far as it ran when the failure came while it
/// ran (spec C18 §6.6.5). The nodes whose counts stop at the failure are marked
/// incomplete.
#[derive(Debug)]
pub struct Failed {
    pub error: Error,
    pub plan: Option<PlanInfo>,
}

impl From<Error> for Box<Failed> {
    fn from(error: Error) -> Self {
        Box::new(Failed { error, plan: None })
    }
}

/// [`query`], with the plan of a query that failed while it ran.
pub fn query_with_plan(
    snap: Arc<Snapshot>,
    q: &str,
    opts: &QueryOptions,
) -> std::result::Result<QueryResult, Box<Failed>> {
    let t0 = Instant::now();
    let parsed = parse_query(q, opts.base_iri.as_deref(), &opts.prefixes)?;
    let parse_ms = t0.elapsed().as_secs_f64() * 1000.0;
    let mut r = execute_query_with_plan(snap, &parsed, opts, parse_ms)?;
    if r.kind == QueryKind::Select {
        select_star_order(q, &mut r)?;
    }
    Ok(r)
}

/// `SELECT *`: spargebra projects variables in sorted order; Jena (and users) expect the
/// order of first appearance in the query text. [`query`] applies this; callers of
/// [`execute_query`] apply it themselves.
pub fn select_star_order(q: &str, r: &mut QueryResult) -> Result<()> {
    if !is_select_star(q) {
        return Ok(());
    }
    r.inner.ctx.check()?;
    if Arc::strong_count(&r.inner) != 1
        || r.inner
            .variables
            .as_ref()
            .is_some_and(|variables| Arc::strong_count(variables) != 1)
    {
        return Err(Error::invalid("cannot reorder a shared query result"));
    }
    let started = Instant::now();
    let names = r.variables();
    let longest = names.iter().map(String::len).max().unwrap_or(0);
    let bytes = names
        .len()
        .checked_mul(3 * std::mem::size_of::<usize>())
        .and_then(|bytes| longest.checked_add(64)?.checked_mul(4)?.checked_add(bytes))
        .and_then(|bytes| bytes.checked_add(256))
        .ok_or_else(|| r.inner.ctx.memory_exceeded(u64::MAX))?;
    let context = r.inner.ctx.clone();
    let _workspace =
        context.charge(u64::try_from(bytes).map_err(|_| context.memory_exceeded(u64::MAX))?)?;
    let Some(mut order) = select_star_columns_checked(q, names, || r.inner.ctx.check())? else {
        return Ok(());
    };
    r.inner.ctx.check()?;
    // No fallible operation remains once the first visible permutation starts.
    let backing = Arc::get_mut(&mut r.inner).expect("unique query result");
    let variables = backing
        .variables
        .as_mut()
        .map(|variables| Arc::get_mut(variables).expect("unique query variables"));
    let Some(variables) = variables else {
        return Ok(());
    };
    for first in 0..order.len() {
        let mut at = first;
        while order[at] != at {
            let next = order[at];
            order[at] = at;
            if next == first {
                break;
            }
            variables.names.swap(at, next);
            variables.ids.swap(at, next);
            backing.table.vars.swap(at, next);
            backing.table.cols.swap(at, next);
            at = next;
        }
    }
    r.mem_peak_bytes = r.mem_peak_bytes.max(context.mem_peak());
    let elapsed = started.elapsed().as_secs_f64() * 1000.0;
    r.timing.exec_ms += elapsed;
    r.timing.total_ms += elapsed;
    Ok(())
}

fn is_select_star(q: &str) -> bool {
    static RE: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    RE.get_or_init(|| regex::Regex::new(r"(?is)\bselect\s+(distinct\s+|reduced\s+)?\*").unwrap())
        .is_match(q)
}

fn select_star_columns_checked(
    q: &str,
    vars: &[String],
    mut check: impl FnMut() -> Result<()>,
) -> Result<Option<Vec<usize>>> {
    // Callers have just checked. Each key scans the query text, so a long sort checks
    // every 64 keys, and a short one only once at the end.
    if !is_select_star(q) {
        return Ok(None);
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
    let mut order: Vec<usize> = (0..vars.len()).collect();
    let mut failure = None;
    let mut keys = 0usize;
    let mut position = |&i: &usize| {
        if failure.is_some() {
            return usize::MAX;
        }
        keys += 1;
        match if keys.is_multiple_of(64) {
            check()
        } else {
            Ok(())
        } {
            Ok(()) => pos(&vars[i]),
            Err(error) => {
                failure = Some(error);
                usize::MAX
            }
        }
    };
    if vars.len() <= 16 {
        order.sort_by_key(&mut position);
    } else {
        order.sort_by_cached_key(&mut position);
    }
    if let Some(error) = failure {
        return Err(error);
    }
    check()?;
    Ok(Some(order))
}

pub fn execute_query(
    snap: Arc<Snapshot>,
    parsed: &Query,
    opts: &QueryOptions,
    parse_ms: f64,
) -> Result<QueryResult> {
    execute_query_with_plan(snap, parsed, opts, parse_ms).map_err(|f| f.error)
}

/// [`execute_query`], with the plan of a query that failed while it ran.
pub fn execute_query_with_plan(
    snap: Arc<Snapshot>,
    parsed: &Query,
    opts: &QueryOptions,
    parse_ms: f64,
) -> std::result::Result<QueryResult, Box<Failed>> {
    let depth = depth::check_query(parsed)?;
    depth::with_stack(depth, || execute_parsed(snap, parsed, opts, parse_ms))
}

struct PreparedQuery {
    ctx: Arc<Ctx>,
    node: plan::Node,
    pattern: GraphPattern,
    bound: Vec<(table::VarId, Id)>,
    kind: QueryKind,
    /// the depth of `node`, which the evaluation and the drop of the plan need stack for
    plan_depth: usize,
}

/// With query-time reasoning, the options the context is made with: without the inferred
/// graph in the default graph and without RDFS on read (spec F17 §2.1, §4.4), and whether
/// RDFS on read was set and does not apply.
fn demand_options(opts: &QueryOptions) -> (Option<QueryOptions>, bool) {
    if opts.reasoning.is_none() {
        return (None, false);
    }
    let o = QueryOptions {
        default_graph_extra: Vec::new(),
        rdfs: None,
        ..opts.clone()
    };
    (Some(o), opts.rdfs.is_some())
}

/// Semantic preparation shared by eager execution and snapshot-owning cursors.
fn prepare_query(
    snap: Arc<Snapshot>,
    parsed: &Query,
    opts: &QueryOptions,
    cursor_started: Option<Instant>,
    automatic: Option<&CursorOptions>,
) -> Result<PreparedQuery> {
    prepare_query_input(snap, parsed, opts, cursor_started, automatic, None)
}

fn prepare_query_input(
    snap: Arc<Snapshot>,
    parsed: &Query,
    opts: &QueryOptions,
    cursor_started: Option<Instant>,
    automatic: Option<&CursorOptions>,
    source: Option<crate::store::BoundReadSource>,
) -> Result<PreparedQuery> {
    prepare_query_input_policy(snap, parsed, opts, cursor_started, automatic, source, None)
}

#[allow(clippy::too_many_arguments)]
fn prepare_query_input_policy(
    snap: Arc<Snapshot>,
    parsed: &Query,
    opts: &QueryOptions,
    cursor_started: Option<Instant>,
    automatic: Option<&CursorOptions>,
    source: Option<crate::store::BoundReadSource>,
    prepared_policy: Option<&Arc<crate::access::GraphAccess>>,
) -> Result<PreparedQuery> {
    let (pattern, dataset, base) = split(parsed);
    extensions::check_family(snap.dataset_id)?;
    let (demand_opts, rdfs_skipped) = demand_options(opts);
    let mut ctx = make_ctx_input_policy(
        snap,
        demand_opts.as_ref().unwrap_or(opts),
        dataset,
        base,
        source,
        prepared_policy,
    )?;
    if let Some(started) = cursor_started.filter(|_| automatic.is_none()) {
        ctx.enable_cursor_accounting()?;
        ctx.deadline = opts.effective_deadline(started)?;
    }
    ctx.configure_extensions(pattern);
    if let Some(provider) = &opts.reasoning {
        reasoning::check_form(parsed)?;
        reasoning::apply(
            &mut ctx,
            provider,
            pattern,
            opts,
            dataset,
            base,
            rdfs_skipped,
            true,
        )?;
    }
    // EXISTS is planned only when rows reach it, so the algebra decides, not the planner
    if algosvc::calls_algorithm(pattern) {
        ctx.note_algorithm_call();
    }
    let mut ctx = Arc::new(ctx);
    if cursor_started.is_some() && automatic.is_none() {
        ctx.attach_owner();
    }
    crate::geo::validate_query(pattern, &mut |w| ctx.warn(w))?;
    // the keys are visited only when asking for their groups can do something
    if ctx.opt.prefetch_constants
        && let Some(mut prefetch) = ctx.snap.generation.vocab.key_prefetch()
    {
        constkeys::for_each_key(pattern, &mut |key| prefetch.add(key));
        prefetch.finish();
    }
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
    ioacct::mark("plan");
    let node = planner.plan(&planned, &ActiveGraph::Default, Vec::new())?;
    // the count of the parsed query bounds the plan, and this makes it true of the plan
    let plan_depth = depth::check_plan(node.depth())?;
    ioacct::mark("planned");
    exec::prefetch_scans(&ctx, &node);
    if let Some(automatic) = automatic
        && bound.is_empty()
        && cursor::auto_streaming(&ctx, &node, kind, automatic)
    {
        let started = cursor_started.expect("automatic preparation start");
        let mutable = Arc::get_mut(&mut ctx)
            .ok_or_else(|| Error::invalid("automatic cursor context is shared"))?;
        mutable.enable_cursor_accounting()?;
        mutable.deadline = opts.effective_deadline(started)?;
    }
    // Retained charges release through the shared context in both accounting modes.
    // An eager context is attached once the automatic choice no longer needs it alone,
    // so its planning keeps what it retains uncharged, as before.
    ctx.attach_owner();
    ctx.check()?;
    Ok(PreparedQuery {
        ctx,
        node,
        pattern,
        bound,
        kind,
        plan_depth,
    })
}

fn execute_parsed(
    snap: Arc<Snapshot>,
    parsed: &Query,
    opts: &QueryOptions,
    parse_ms: f64,
) -> std::result::Result<QueryResult, Box<Failed>> {
    let t1 = Instant::now();
    let prepared = prepare_query(snap, parsed, opts, None, None)?;
    let plan_ms = t1.elapsed().as_secs_f64() * 1000.0;
    depth::with_stack(prepared.plan_depth, || {
        execute_prepared(parsed, opts, parse_ms, plan_ms, prepared)
    })
}

fn execute_prepared(
    parsed: &Query,
    opts: &QueryOptions,
    parse_ms: f64,
    plan_ms: f64,
    prepared: PreparedQuery,
) -> std::result::Result<QueryResult, Box<Failed>> {
    execute_prepared_parts(parsed, opts, parse_ms, plan_ms, prepared)?
        .freeze()
        .map_err(|error| Box::new(Failed { error, plan: None }))
}

/// The variables and retained table of an eager SELECT result. The body stays out of
/// line, with one error exit, so that the result path of a small query runs through a
/// short stretch of code rather than blocks spread across its caller.
#[inline(never)]
fn select_output(
    ctx: &Arc<Ctx>,
    pattern: &GraphPattern,
    materialized: table::Materialized<'_>,
) -> Result<(Arc<cursor::Variables>, table::RetainedMaterialized)> {
    let variables = cursor::Variables::new(ctx, pattern, &materialized.table.vars, "")?;
    let retained = materialized
        .project(ctx, &variables.ids)?
        .into_retained(ctx)?;
    Ok((variables, retained))
}

fn execute_prepared_parts(
    parsed: &Query,
    opts: &QueryOptions,
    parse_ms: f64,
    plan_ms: f64,
    prepared: PreparedQuery,
) -> std::result::Result<ResultParts, Box<Failed>> {
    let PreparedQuery {
        ctx,
        mut node,
        pattern,
        bound,
        kind,
        plan_depth: _,
    } = prepared;
    let (_, dataset, _) = split(parsed);
    let t2 = Instant::now();
    if kind == QueryKind::Ask {
        unionpush::order_ask(&ctx, &mut node);
    }
    // the plan of a failed query as far as it ran, under the root of a graph query
    let failed = |error: Error, plan: Option<PlanInfo>, stage: Option<Instant>| {
        let mut plan = plan.or_else(|| ctx.failed_plan());
        if let Some(p) = &mut plan {
            if matches!(kind, QueryKind::Construct | QueryKind::Describe) {
                let mut root = graph_root(kind, std::mem::take(p), stage.unwrap_or(t2));
                root.fidelity_mut().incomplete = true;
                *p = root;
            }
            p.warnings = ctx.warnings();
        }
        Box::new(Failed { error, plan })
    };
    let (materialized, mut plan) = match exec::execute_materialized(&ctx, &node) {
        Ok(r) => r,
        Err(e) => return Err(failed(e, None, None)),
    };
    ioacct::mark("executed");
    if let Some(a) = &ctx.reasoning {
        plan = a.wrap(plan);
    }
    plan.warnings = ctx.warnings();
    if ctx.graphs.is_some() {
        plan.redact();
    }
    let mut result = ResultParts {
        kind,
        variables: None,
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
        _table_credit: None,
        _graph_credit: None,
    };
    match parsed {
        Query::Select { .. } => {
            let (variables, retained) = select_output(&ctx, &pattern, materialized)
                .map_err(|error| failed(error, None, None))?;
            result.variables = Some(variables);
            result.table = retained.table;
            result._table_credit = retained.credit;
            // substituted variables report their bound value
            for (v, id) in &bound {
                if let Some(c) = result.table.col_of(*v) {
                    result.table.cols[c].iter_mut().for_each(|x| *x = *id);
                }
            }
        }
        Query::Ask { .. } => {
            result.boolean = !materialized.table.is_empty();
            drop(materialized);
        }
        Query::Construct {
            template,
            graph_templates,
            ..
        } => {
            let stage = Instant::now();
            let where_plan = std::mem::take(&mut result.plan);
            match construct(&ctx, &materialized.table, template, graph_templates) {
                Ok((triples, quads, credit)) => {
                    result.triples = triples;
                    result.quads = quads;
                    result._graph_credit = Some(credit);
                }
                Err(e) => return Err(failed(e, Some(where_plan), Some(stage))),
            }
            result.plan = graph_root(kind, where_plan, stage);
            result.plan.actual_rows = (result.triples.len() + result.quads.len()) as i64;
            drop(materialized);
        }
        Query::Describe { .. } => {
            // a dataset the query or the protocol gave, or the inference overlay
            let explicit = dataset.is_some()
                || !opts.default_graph_uris.is_empty()
                || !opts.named_graph_uris.is_empty()
                || !opts.default_graph_extra.is_empty();
            let stage = Instant::now();
            let where_plan = std::mem::take(&mut result.plan);
            let d = match describe::describe(&ctx, &materialized.table, &opts.describe, explicit) {
                Ok(d) => d,
                Err(e) => return Err(failed(e, Some(where_plan), Some(stage))),
            };
            result.describe_truncated = d.truncated;
            let (triples, credit) = match d.into_output(&ctx) {
                Ok(output) => output,
                Err(e) => return Err(failed(e, Some(where_plan), Some(stage))),
            };
            result.triples = triples;
            result._graph_credit = Some(credit);
            result.plan = graph_root(kind, where_plan, stage);
            result.plan.actual_rows = result.triples.len() as i64;
            result.plan.warnings = ctx.warnings();
            drop(materialized);
        }
    }
    let exec_ms = t2.elapsed().as_secs_f64() * 1000.0;
    result.timing = Timing {
        parse_ms,
        plan_ms,
        exec_ms,
        serialize_ms: 0.0,
        total_ms: parse_ms + plan_ms + exec_ms,
    };
    result.mem_peak_bytes = ctx.mem_peak();
    result.rows_produced = ctx.rows_produced();
    if let Err(error) = ctx.check() {
        let plan = Some(std::mem::take(&mut result.plan));
        return Err(Box::new(Failed { error, plan }));
    }
    // The operators' state goes with the plan, so the pool keeps only what the result
    // holds. The plan's retained charges, such as an EXISTS memo or key set, are
    // released before the shrink, which they would otherwise keep. A graph result
    // keeps its query's admitted high point until it is dropped (X14 stage 4), so
    // it is not shrunk to its graph credit.
    drop(node);
    ctx.release_work();
    if matches!(kind, QueryKind::Select | QueryKind::Ask)
        && let Err(error) = ctx.shrink_retained(if result._table_credit.is_some() {
            0
        } else {
            result.table.capacity_bytes()
        })
    {
        return Err(Box::new(Failed {
            error,
            plan: Some(std::mem::take(&mut result.plan)),
        }));
    }
    Ok(result)
}

/// The root of a graph query's plan: the `CONSTRUCT` or `DESCRIBE` stage over the plan
/// of its `WHERE` clause, timed from `stage`. Streaming graph cursors have the same
/// root, so both plans have the same shape (spec C18 §6.6.5).
fn graph_root(kind: QueryKind, mut where_plan: PlanInfo, stage: Instant) -> PlanInfo {
    let warnings = std::mem::take(&mut where_plan.warnings);
    let stage_ms = stage.elapsed().as_secs_f64() * 1000.0;
    PlanInfo {
        operator: format!("{kind:?}").to_uppercase(),
        columns: vec!["s".into(), "p".into(), "o".into(), "g".into()],
        estimated_rows: where_plan.estimated_rows,
        estimated_cost: where_plan.estimated_cost,
        actual_rows: -1,
        time_ms: where_plan.time_ms + stage_ms,
        warnings,
        fidelity: where_plan.fidelity().incomplete.then(|| {
            Box::new(PlanFidelity {
                incomplete: true,
                ..Default::default()
            })
        }),
        children: vec![where_plan],
        ..Default::default()
    }
}

/// EXPLAIN: algebra (SSE) and the plan without executing it.
pub fn explain(snap: Arc<Snapshot>, q: &str, opts: &QueryOptions) -> Result<(String, PlanInfo)> {
    let parsed = parse_query(q, opts.base_iri.as_deref(), &opts.prefixes)?;
    let depth = depth::check_query(&parsed)?;
    depth::with_stack(depth, || {
        let (pattern, dataset, base) = split(&parsed);
        extensions::check_family(snap.dataset_id)?;
        let (demand_opts, rdfs_skipped) = demand_options(opts);
        let mut ctx = make_ctx(snap, demand_opts.as_ref().unwrap_or(opts), dataset, base)?;
        ctx.configure_extensions(pattern);
        if let Some(provider) = &opts.reasoning {
            reasoning::check_form(&parsed)?;
            reasoning::apply(
                &mut ctx,
                provider,
                pattern,
                opts,
                dataset,
                base,
                rdfs_skipped,
                false,
            )?;
        }
        crate::geo::validate_query(pattern, &mut |w| ctx.warn(w))?;
        let mut planner = Planner::new(&ctx);
        planner.source = Some(&parsed);
        // the shape that execution plans: ASK as a slice of one solution
        let kind = match &parsed {
            Query::Select { .. } => QueryKind::Select,
            Query::Ask { .. } => QueryKind::Ask,
            Query::Construct { .. } => QueryKind::Construct,
            Query::Describe { .. } => QueryKind::Describe,
        };
        let pattern = match kind {
            QueryKind::Ask => GraphPattern::Slice {
                inner: Box::new(pattern.clone()),
                start: 0,
                length: Some(1),
            },
            _ => pattern.clone(),
        };
        let pattern = rdfs::apply(&ctx, &pattern);
        let node = planner.plan(&pattern, &ActiveGraph::Default, Vec::new())?;
        depth::check_plan(node.depth())?;
        let mut info = exec::try_describe(&ctx, &node)?;
        if let Some(a) = &ctx.reasoning {
            info = a.wrap(info);
        }
        if matches!(kind, QueryKind::Construct | QueryKind::Describe) {
            info = graph_root(kind, info, Instant::now());
            info.time_ms = 0.0;
        }
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
// Physical output containers precede their guard on every error and final drop.
struct Constructed<'a> {
    triples: Vec<Triple>,
    quads: Vec<Quad>,
    seen: FxHashSet<Triple>,
    seen_quads: FxHashSet<Quad>,
    seen_storage: u64,
    quad_storage: u64,
    credit: Option<ctx::Charge<'a>>,
}
impl Drop for Constructed<'_> {
    fn drop(&mut self) {
        let depth = self
            .triples
            .iter()
            .map(|t| output::term_depth(&t.object))
            .chain(self.quads.iter().map(|q| output::term_depth(&q.object)))
            .chain(self.seen.iter().map(|t| output::term_depth(&t.object)))
            .chain(
                self.seen_quads
                    .iter()
                    .map(|q| output::term_depth(&q.object)),
            )
            .max()
            .unwrap_or(0);
        depth::with_stack(depth, || {
            self.triples.clear();
            self.quads.clear();
            self.seen.clear();
            self.seen_quads.clear();
        });
    }
}

// Admit replacement storage while old and new allocations may coexist. The old
// envelope is released only after reserve has freed the previous allocation.
fn graph_vec_room<T>(ctx: &Ctx, held: &ctx::Charge<'_>, values: &mut Vec<T>) -> Result<()> {
    if values.len() < values.capacity() {
        return Ok(());
    }
    let old = values.capacity();
    let cap = old.saturating_mul(2).max(4);
    let bytes = cap
        .checked_mul(std::mem::size_of::<T>())
        .and_then(|n| u64::try_from(n).ok())
        .ok_or_else(|| ctx.memory_exceeded(u64::MAX))?;
    held.add(bytes)?;
    values
        .try_reserve_exact(cap - values.len())
        .map_err(|_| ctx.memory_exceeded(bytes))?;
    if values.capacity() > cap {
        return Err(Error::invalid("graph allocation exceeds admission"));
    }
    held.resize(held.bytes() - (old * std::mem::size_of::<T>()) as u64)?;
    Ok(())
}
/// The buckets hashbrown allocates for a table that must hold `items`, following
/// its `capacity_to_buckets`.
fn set_buckets(items: usize) -> Option<usize> {
    if items < 4 {
        Some(4)
    } else if items < 8 {
        Some(8)
    } else {
        items
            .checked_mul(8)
            .map(|n| n / 7)?
            .checked_next_power_of_two()
    }
}
/// The buckets of an allocated table with `capacity`, the inverse of hashbrown's
/// `bucket_mask_to_capacity`, which gives `buckets - 1` below 8 buckets and 7/8 of
/// them above. A table with no capacity has no allocation.
fn set_buckets_of(capacity: usize) -> Option<usize> {
    if capacity == 0 {
        Some(0)
    } else if capacity < 8 {
        capacity.checked_add(1)?.checked_next_power_of_two()
    } else {
        (capacity / 7).checked_mul(8)?.checked_next_power_of_two()
    }
}
/// The allocation of a hashbrown table of `T` with `buckets`: the values rounded up
/// to the control alignment, then one control byte per bucket and one trailing group.
/// A group of 16 bytes is exact with SSE2 and an upper bound for 8-byte groups.
fn set_table_bytes<T>(buckets: usize) -> Option<u64> {
    if buckets == 0 {
        return Some(0);
    }
    const GROUP: usize = 16;
    let align = std::mem::align_of::<T>().max(GROUP);
    let values = buckets
        .checked_mul(std::mem::size_of::<T>())?
        .checked_next_multiple_of(align)?;
    u64::try_from(values.checked_add(buckets)?.checked_add(GROUP)?).ok()
}
fn graph_set_room<T: std::hash::Hash + Eq>(
    ctx: &Ctx,
    held: &ctx::Charge<'_>,
    values: &mut FxHashSet<T>,
    storage: &mut u64,
) -> Result<()> {
    if values.len() < values.capacity() {
        return Ok(());
    }
    // The sets are insert-only, so a full table resizes to hold one more item than
    // its capacity, and the old and new tables coexist while it moves.
    let bytes = values
        .capacity()
        .checked_add(1)
        .and_then(set_buckets)
        .and_then(set_table_bytes::<T>)
        .ok_or_else(|| ctx.memory_exceeded(u64::MAX))?;
    held.add(bytes)?;
    values
        .try_reserve(1)
        .map_err(|_| ctx.memory_exceeded(bytes))?;
    let actual = set_buckets_of(values.capacity())
        .and_then(set_table_bytes::<T>)
        .ok_or_else(|| ctx.memory_exceeded(u64::MAX))?;
    if actual > bytes {
        return Err(Error::invalid("graph set exceeds admission"));
    }
    // The old table is freed, and the new one keeps its actual allocation.
    held.resize(held.bytes() - *storage - (bytes - actual))?;
    *storage = actual;
    Ok(())
}

#[cfg(test)]
#[test]
fn graph_set_charge_is_its_table_allocation() {
    use crate::test_alloc::record_largest;
    let mut set: FxHashSet<u64> = FxHashSet::default();
    for n in 0..3000u64 {
        if set.len() == set.capacity() {
            let bound = set_buckets(set.capacity() + 1)
                .and_then(set_table_bytes::<u64>)
                .unwrap();
            let largest = record_largest(|| set.reserve(1));
            let actual = set_buckets_of(set.capacity())
                .and_then(set_table_bytes::<u64>)
                .unwrap();
            // SSE2 groups are 16 bytes and the size is exact. Other groups are 8
            // bytes, and the size is an upper bound.
            if cfg!(all(
                any(target_arch = "x86", target_arch = "x86_64"),
                target_feature = "sse2"
            )) {
                assert_eq!(actual, largest as u64, "capacity {}", set.capacity());
            } else {
                assert!(actual >= largest as u64, "capacity {}", set.capacity());
            }
            assert_eq!(bound, actual, "capacity {}", set.capacity());
        }
        set.insert(n);
    }
}

fn construct(
    ctx: &Arc<Ctx>,
    t: &Table,
    template: &[TriplePattern],
    graphs: &[GraphTemplate],
) -> Result<(Vec<Triple>, Vec<Quad>, ctx::OutputCredit)> {
    ctx.check()?;
    let map_bytes = ctx
        .nvars()
        .checked_mul(std::mem::size_of::<Option<usize>>())
        .and_then(|n| u64::try_from(n).ok())
        .ok_or_else(|| ctx.memory_exceeded(u64::MAX))?;
    let _map_credit = ctx.charge(map_bytes)?;
    let map = t.var_map(ctx.nvars());
    let mut out = Constructed {
        triples: Vec::new(),
        quads: Vec::new(),
        seen: FxHashSet::default(),
        seen_quads: FxHashSet::default(),
        seen_storage: 0,
        quad_storage: 0,
        credit: Some(ctx.charge(0)?),
    };
    let mut work = 0usize;
    for i in 0..t.len() {
        construct_row(
            ctx,
            t,
            &map,
            i,
            template,
            graphs,
            &mut work,
            &mut |q, incoming| {
                let held = out.credit.as_ref().expect("live graph credit");
                if q.graph_name == GraphName::DefaultGraph {
                    let tr = Triple::new(q.subject, q.predicate, q.object);
                    if out.seen.contains(&tr) {
                        return Ok(());
                    }
                    ctx.check_rows(
                        out.triples
                            .len()
                            .saturating_add(out.quads.len())
                            .saturating_add(1),
                    )?;
                    ctx.produced(1)?;
                    held.adopt_charge(incoming)?;
                    // Incoming lexical ownership is moved, not decoded or charged again.
                    // Only the genuinely additional dedup copy is admitted here.
                    held.add(graph_triple_bytes(&tr))?;
                    graph_set_room(ctx, held, &mut out.seen, &mut out.seen_storage)?;
                    graph_vec_room(ctx, held, &mut out.triples)?;
                    out.seen.insert(tr.clone());
                    out.triples.push(tr);
                } else if !out.seen_quads.contains(&q) {
                    ctx.check_rows(
                        out.triples
                            .len()
                            .saturating_add(out.quads.len())
                            .saturating_add(1),
                    )?;
                    ctx.produced(1)?;
                    held.adopt_charge(incoming)?;
                    held.add(graph_quad_bytes(&q))?;
                    graph_set_room(ctx, held, &mut out.seen_quads, &mut out.quad_storage)?;
                    graph_vec_room(ctx, held, &mut out.quads)?;
                    out.seen_quads.insert(q.clone());
                    out.quads.push(q);
                }
                Ok(())
            },
        )?;
    }
    ctx.check()?;
    // Dispose traversal/dedup scratch before reconciling original admission down.
    out.seen = FxHashSet::default();
    out.seen_quads = FxHashSet::default();
    let retained = out.triples.iter().try_fold(
        (out.triples.capacity() as u64)
            .checked_mul(std::mem::size_of::<Triple>() as u64)
            .and_then(|n| {
                n.checked_add(
                    (out.quads.capacity() as u64).checked_mul(std::mem::size_of::<Quad>() as u64)?,
                )
            })
            .ok_or_else(|| ctx.memory_exceeded(u64::MAX))?,
        |n, t| {
            n.checked_add(graph_triple_bytes(t))
                .ok_or_else(|| ctx.memory_exceeded(u64::MAX))
        },
    )?;
    let retained = out.quads.iter().try_fold(retained, |n, q| {
        n.checked_add(graph_quad_bytes(q))
            .ok_or_else(|| ctx.memory_exceeded(u64::MAX))
    })?;
    let held = out.credit.as_ref().expect("live graph credit");
    if retained > held.bytes() {
        return Err(Error::invalid("construct exceeds original admission"));
    }
    held.resize(retained)?;
    ctx.check()?;
    let credit = out
        .credit
        .take()
        .expect("validated graph credit")
        .into_output(ctx);
    Ok((
        std::mem::take(&mut out.triples),
        std::mem::take(&mut out.quads),
        credit,
    ))
}

// Template blank-node identities are row work, not retained output. Their actual
// map allocation precedes its guard on drop; emitted clones have separate credit.
struct ConstructRow<'a> {
    bnodes: FxHashMap<String, BlankNode>,
    storage: u64,
    held: ctx::Charge<'a>,
}

/// Shared eager/cursor instantiation hands the emitter each quad together with
/// its preallocation guard. No emitter may retain the payload without adopting it.
#[allow(clippy::too_many_arguments)]
fn construct_row<'a>(
    ctx: &'a Ctx,
    t: &Table,
    map: &[Option<usize>],
    row: usize,
    template: &[TriplePattern],
    graphs: &[GraphTemplate],
    work: &mut usize,
    emit: &mut dyn FnMut(Quad, &mut Option<ctx::Charge<'a>>) -> Result<()>,
) -> Result<()> {
    let mut check = || {
        *work = work.wrapping_add(1);
        ctx.check()
    };
    check()?;
    let mut state = ConstructRow {
        bnodes: FxHashMap::default(),
        storage: 0,
        held: ctx.charge(0)?,
    };
    for tp in template {
        check()?;
        let mut held = Some(ctx.charge(0)?);
        if let Some(triple) =
            template_triple(ctx, t, map, row, tp, &mut state, held.as_ref().unwrap())?
        {
            emit(triple.in_graph(GraphName::DefaultGraph), &mut held)?;
        }
    }
    for graph in graphs {
        check()?;
        let name_credit = ctx.charge(0)?;
        let name = match template_term(ctx, t, map, row, &graph.name, &mut state, &name_credit)? {
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
        for tp in &graph.triples {
            check()?;
            let mut held = Some(ctx.charge(0)?);
            if let Some(triple) =
                template_triple(ctx, t, map, row, tp, &mut state, held.as_ref().unwrap())?
            {
                let bytes = match &name {
                    GraphName::NamedNode(n) => n.as_str().len() as u64 + 64,
                    GraphName::BlankNode(b) => b.as_str().len() as u64 + 64,
                    GraphName::DefaultGraph => 0,
                };
                held.as_ref().unwrap().add(bytes)?;
                emit(triple.in_graph(name.clone()), &mut held)?;
            }
        }
    }
    Ok(())
}

fn graph_allocation(ctx: &Ctx, label: &'static str) -> Result<()> {
    #[cfg(test)]
    {
        use crate::algo::memory::probe;
        probe::at(probe::Point::Allocation(label, 1));
        if probe::fails(label) {
            return Err(Error::invalid("injected construct allocation failure"));
        }
    }
    #[cfg(not(test))]
    let _ = label;
    ctx.check()
}

fn template_variable(
    ctx: &Ctx,
    t: &Table,
    map: &[Option<usize>],
    row: usize,
    name: &str,
    held: &ctx::Charge<'_>,
) -> Result<Option<Term>> {
    // Release the name read lock before accounting or any callback can reenter.
    let var = { ctx.var_names.read().iter().position(|v| v == name) };
    let Some(col) = var.and_then(|v| map.get(v).copied().flatten()) else {
        return Ok(None);
    };
    let id = t.cols[col][row];
    if id.is_undef() {
        return Ok(None);
    }
    // Admit the conservative decode bound before decoding, then keep only the
    // decoded term's actual graph bytes. A literal's lexical form is one copy.
    let estimate = ctx.decoded_bytes(id)?;
    held.add(estimate)?;
    graph_allocation(ctx, "construct-decode")?;
    let term = ctx.reserved_batch_term(id)?;
    let actual = term
        .as_ref()
        .map_or(0, |term| graph_term_bytes(term.as_ref()))
        .min(estimate);
    held.resize(held.bytes() - estimate + actual)?;
    Ok(term)
}
#[allow(clippy::too_many_arguments)]
fn template_term(
    ctx: &Ctx,
    t: &Table,
    map: &[Option<usize>],
    row: usize,
    tp: &TermPattern,
    state: &mut ConstructRow<'_>,
    held: &ctx::Charge<'_>,
) -> Result<Option<Term>> {
    ctx.check()?;
    stacker::maybe_grow(exec::PLAN_RED_ZONE, exec::PLAN_STACK_SEGMENT, || {
        Ok(Some(match tp {
            TermPattern::Variable(v) => {
                return template_variable(ctx, t, map, row, v.as_str(), held);
            }
            TermPattern::NamedNode(n) => {
                held.add(n.as_str().len() as u64 + 64)?;
                Term::NamedNode(n.clone())
            }
            TermPattern::Literal(l) => {
                held.add(
                    (l.value().len() as u64)
                        .checked_add(l.datatype().as_str().len() as u64)
                        .and_then(|n| n.checked_add(l.language().map_or(0, |s| s.len() as u64)))
                        .and_then(|n| n.checked_add(64))
                        .ok_or_else(|| ctx.memory_exceeded(u64::MAX))?,
                )?;
                graph_allocation(ctx, "construct-literal")?;
                Term::Literal(l.clone())
            }
            TermPattern::BlankNode(b) => {
                if !state.bnodes.contains_key(b.as_str()) {
                    state.held.add(b.as_str().len() as u64 + 128)?;
                    if state.bnodes.len() == state.bnodes.capacity() {
                        let slots = state
                            .bnodes
                            .len()
                            .checked_add(1)
                            .and_then(|n| n.checked_mul(8))
                            .and_then(usize::checked_next_power_of_two)
                            .ok_or_else(|| ctx.memory_exceeded(u64::MAX))?;
                        let bytes = (slots as u64)
                            .checked_mul((std::mem::size_of::<(String, BlankNode)>() + 16) as u64)
                            .and_then(|n| n.checked_add(128))
                            .ok_or_else(|| ctx.memory_exceeded(u64::MAX))?;
                        state.held.add(bytes)?;
                        state
                            .bnodes
                            .try_reserve(1)
                            .map_err(|_| ctx.memory_exceeded(bytes))?;
                        if state.bnodes.capacity() > slots {
                            return Err(Error::invalid("blank node map exceeds admission"));
                        }
                        state.held.resize(state.held.bytes() - state.storage)?;
                        state.storage = bytes;
                    }
                    state.bnodes.insert(
                        b.as_str().to_owned(),
                        crate::store::bnode_for(ctx.fresh_bnode()),
                    );
                }
                let node = state.bnodes.get(b.as_str()).expect("row blank node");
                held.add(node.as_str().len() as u64 + 64)?;
                Term::BlankNode(node.clone())
            }
            TermPattern::Triple(triple) => {
                let Some(value) = template_triple(ctx, t, map, row, triple, state, held)? else {
                    return Ok(None);
                };
                held.add(std::mem::size_of::<Triple>() as u64 + 64)?;
                Term::Triple(Box::new(value))
            }
        }))
    })
}
#[allow(clippy::too_many_arguments)]
fn template_triple(
    ctx: &Ctx,
    t: &Table,
    map: &[Option<usize>],
    row: usize,
    tp: &TriplePattern,
    state: &mut ConstructRow<'_>,
    held: &ctx::Charge<'_>,
) -> Result<Option<Triple>> {
    let s = template_term(ctx, t, map, row, &tp.subject, state, held)?;
    let p = match &tp.predicate {
        NamedNodePattern::NamedNode(n) => {
            held.add(n.as_str().len() as u64 + 64)?;
            Some(Term::NamedNode(n.clone()))
        }
        NamedNodePattern::Variable(v) => template_variable(ctx, t, map, row, v.as_str(), held)?,
    };
    let o = template_term(ctx, t, map, row, &tp.object, state, held)?;
    let (Some(s), Some(Term::NamedNode(p)), Some(o)) = (s, p, o) else {
        return Ok(None);
    };
    let subject = match s {
        Term::NamedNode(n) => NamedOrBlankNode::NamedNode(n),
        Term::BlankNode(b) => NamedOrBlankNode::BlankNode(b),
        _ => return Ok(None),
    };
    Ok(Some(Triple::new(subject, p, o)))
}

fn graph_quad_bytes(q: &Quad) -> u64 {
    graph_term_bytes(q.subject.as_ref().into())
        .saturating_add(graph_term_bytes(q.predicate.as_ref().into()))
        .saturating_add(graph_term_bytes(q.object.as_ref()))
        .saturating_add(match &q.graph_name {
            GraphName::NamedNode(n) => 64 + n.as_str().len() as u64,
            GraphName::BlankNode(b) => 64 + b.as_str().len() as u64,
            GraphName::DefaultGraph => 0,
        })
}

/// Conservative retained term estimate, using borrowed terms without cloning.
fn graph_term_bytes(mut t: oxrdf::TermRef<'_>) -> u64 {
    use oxrdf::TermRef;
    let mut bytes = 0u64;
    loop {
        let payload = match t {
            TermRef::NamedNode(n) => n.as_str().len() as u64,
            TermRef::BlankNode(b) => b.as_str().len() as u64,
            TermRef::Literal(l) => (l.value().len() as u64)
                .saturating_add(l.datatype().as_str().len() as u64)
                .saturating_add(l.language().map_or(0, |s| s.len() as u64)),
            TermRef::Triple(triple) => {
                let subject = match &triple.subject {
                    NamedOrBlankNode::NamedNode(n) => n.as_str().len(),
                    NamedOrBlankNode::BlankNode(b) => b.as_str().len(),
                };
                bytes = bytes
                    .saturating_add(192)
                    .saturating_add(subject as u64)
                    .saturating_add(triple.predicate.as_str().len() as u64);
                t = triple.object.as_ref();
                continue;
            }
        };
        return bytes.saturating_add(64).saturating_add(payload);
    }
}

fn graph_triple_bytes(t: &Triple) -> u64 {
    graph_term_bytes(t.subject.as_ref().into())
        .saturating_add(graph_term_bytes(t.predicate.as_ref().into()))
        .saturating_add(graph_term_bytes(t.object.as_ref()))
}

#[cfg(test)]
mod access_tests;
#[cfg(test)]
mod charsets_tests;
#[cfg(test)]
mod costcal_tests;
#[cfg(test)]
mod depth_tests;
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
mod page_ownership_tests;
#[cfg(test)]
mod pattern_tests;
#[cfg(test)]
mod plan_fidelity_tests;
#[cfg(test)]
mod plan_refs_tests;
#[cfg(test)]
mod sample_tests;
#[cfg(test)]
mod stats_tests;
#[cfg(test)]
mod strfilter_tests;
#[cfg(test)]
mod tests;

#[cfg(test)]
mod paid_final_table_tests {
    use super::*;
    use crate::store::{Store, StoreOptions};

    #[test]
    fn prepared_select_retains_actual_fast_table_credit_after_return() {
        let store = Store::in_memory(StoreOptions::default());
        let pool = pool::QueryPool::new(1 << 20);
        let grant = Arc::new(pool.empty_grant());
        query_memory::LEDGER_OPS.with(|n| n.set(0));
        let result = query(
            store.snapshot(),
            "SELECT ?value WHERE { VALUES ?value { UNDEF } }",
            &QueryOptions {
                no_cache: true,
                grant: Some(grant.clone()),
                max_memory_bytes: Some(1 << 16),
                ..Default::default()
            },
        )
        .unwrap();
        let bytes = algosvc::table_capacity(result.test_table()).unwrap();
        assert_eq!(result.test_table().cols, [vec![Id::UNDEF]]);
        assert!(
            result.test_context().live_bytes() >= bytes,
            "returned SELECT must retain actual table credit"
        );
        query_memory::LEDGER_OPS.with(|n| {
            assert_eq!(
                n.get(),
                0,
                "ordinary prepared output stays on its fast account"
            )
        });
        let owner = Arc::downgrade(result.test_context());
        drop(store);
        assert!(owner.upgrade().is_some());
        assert!(grant.used() >= bytes);
        drop(result);
        assert!(owner.upgrade().is_none());
        assert_eq!(grant.used(), 0);
    }
}

#[cfg(test)]
pub(crate) mod checked_source_tests;

#[cfg(test)]
mod result_backing_tests {
    use super::*;
    use crate::algo::memory::probe;
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn context(limit: u64) -> Arc<Ctx> {
        let store = crate::store::Store::in_memory(Default::default());
        let mut ctx = Ctx::new(store.snapshot());
        ctx.mem_limit = limit;
        Arc::new(ctx)
    }
    fn parts(ctx: Arc<Ctx>) -> ResultParts {
        ResultParts {
            kind: QueryKind::Ask,
            variables: None,
            table: Table::default(),
            boolean: true,
            triples: Vec::new(),
            quads: Vec::new(),
            plan: PlanInfo::default(),
            timing: Timing::default(),
            mem_peak_bytes: 0,
            rows_produced: 0,
            describe_truncated: false,
            ctx,
            _table_credit: None,
            _graph_credit: None,
        }
    }

    #[test]
    fn immutable_result_header_admits_before_allocation_and_clones_keep_fast_credit() {
        let ctx = context(u64::MAX);
        let bytes = cursor::arc_backing_bytes::<ResultBacking>(&ctx).unwrap();
        drop(ctx);
        for exact in [false, true] {
            let ctx = context(bytes - u64::from(!exact));
            let allocations = Arc::new(AtomicUsize::new(0));
            let observed = allocations.clone();
            let hook = probe::install(
                move |point| {
                    if let probe::Point::Allocation("query-result-backing", 1) = point {
                        observed.fetch_add(1, Ordering::Relaxed);
                    }
                },
                None,
            );
            query_memory::LEDGER_OPS.with(|counter| counter.set(0));
            let result = parts(ctx.clone()).freeze();
            drop(hook);
            assert_eq!(query_memory::LEDGER_OPS.with(|counter| counter.get()), 0);
            if exact {
                let result = result.unwrap();
                assert_eq!(allocations.load(Ordering::Relaxed), 1);
                assert_eq!(ctx.live_bytes(), bytes);
                let clone = result.clone();
                assert!(Arc::ptr_eq(&result.inner, &clone.inner));
                drop(result);
                assert_eq!(ctx.live_bytes(), bytes);
                assert!(clone.boolean && clone.variables().is_empty());
                drop(clone);
            } else {
                assert!(
                    matches!(result, Err(Error::BudgetExceeded(b)) if b.kind == crate::BudgetKind::Memory)
                );
                assert_eq!(allocations.load(Ordering::Relaxed), 0);
            }
            assert_eq!(ctx.live_bytes(), 0);
        }
    }

    #[test]
    fn freezing_a_result_makes_one_check() {
        let ctx = context(u64::MAX);
        let before = ctx::CHECKS.with(|n| n.get());
        let result = parts(ctx.clone()).freeze().unwrap();
        assert_eq!(ctx::CHECKS.with(|n| n.get()) - before, 1);
        drop(result);
    }

    #[test]
    fn immutable_result_header_allocation_failure_and_late_cancel_publish_nothing() {
        for cancelled in [false, true] {
            let ctx = context(4096);
            let flag = ctx.cancel.clone();
            let hook = probe::install(
                move |point| {
                    if cancelled && let probe::Point::Allocation("query-result-backing", 1) = point
                    {
                        flag.store(true, Ordering::Relaxed);
                    }
                },
                (!cancelled).then_some("query-result-backing"),
            );
            let result = parts(ctx.clone()).freeze();
            drop(hook);
            if cancelled {
                assert!(matches!(result, Err(Error::Cancelled)));
            } else {
                assert!(matches!(result, Err(Error::Invalid(_))));
            }
            assert_eq!(ctx.live_bytes(), 0);
        }
    }

    #[test]
    fn checked_presentation_refuses_shared_result_without_copying_or_partial_reorder() {
        let store = crate::store::Store::in_memory(Default::default());
        let mut result = query(
            store.snapshot(),
            "SELECT ?a ?z WHERE { VALUES (?a ?z) { (1 2) } }",
            &Default::default(),
        )
        .unwrap();
        let clone = result.clone();
        let names = result.variables().to_vec();
        let ctx = result.inner.ctx.clone();
        let before = ctx.live_bytes();
        assert!(matches!(
            select_star_order("SELECT * WHERE { VALUES (?z ?a) { (2 1) } }", &mut result),
            Err(Error::Invalid(_))
        ));
        assert_eq!(result.variables(), names);
        assert_eq!(clone.variables(), names);
        assert_eq!(ctx.live_bytes(), before);
        drop(clone);
        select_star_order("SELECT * WHERE { VALUES (?z ?a) { (2 1) } }", &mut result).unwrap();
        assert_eq!(result.variables(), ["z", "a"]);
        assert_eq!(result.table().get(0, 0).unwrap(), Id::from_i64(2).unwrap());
        assert_eq!(ctx.live_bytes(), before);
        drop(result);
        assert_eq!(ctx.live_bytes(), 0);
    }
}

#[cfg(test)]
mod eager_graph_lifecycle_tests {
    use super::*;
    use crate::store::{Store, StoreOptions};

    fn returned_graph_keeps_its_actual_live_admission(query_text: &str) {
        let store = Store::in_memory(StoreOptions::default());
        let mut write = store.write();
        let subject = write
            .intern(&oxrdf::NamedNode::new_unchecked("urn:s").into())
            .unwrap();
        let predicate = write
            .intern(&oxrdf::NamedNode::new_unchecked("urn:p").into())
            .unwrap();
        let object = write
            .intern(&oxrdf::Literal::new_simple_literal("x".repeat(64 << 10)).into())
            .unwrap();
        write
            .insert([subject, predicate, object, Id::DEFAULT_GRAPH])
            .unwrap();
        write.commit().unwrap();
        let pool = pool::QueryPool::new(16 << 20);
        let result = query(
            store.snapshot(),
            query_text,
            &QueryOptions {
                pool: Some(pool.clone()),
                max_memory_bytes: Some(4 << 20),
                no_cache: true,
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(result.triples().len(), 1);
        let Term::Literal(value) = &result.triples()[0].object else {
            panic!("stored literal");
        };
        assert_eq!(value.value().len(), 64 << 10);
        let context = result.test_context().clone();
        assert!(
            context.live_bytes() >= 64 << 10,
            "the returned physical64KiB graph must keep its original live credit; observed live={}, pool={}",
            context.live_bytes(),
            pool.granted()
        );
        drop(result);
        assert_eq!(context.live_bytes(), 0);
        drop(context);
        assert_eq!(pool.granted(), 0);
    }

    #[test]
    fn returned_construct_payload_keeps_its_original_live_credit() {
        returned_graph_keeps_its_actual_live_admission(
            "CONSTRUCT { <urn:s> <urn:copy> ?o } WHERE { <urn:s> <urn:p> ?o }",
        );
    }
    #[test]
    fn returned_describe_payload_keeps_its_original_live_credit() {
        returned_graph_keeps_its_actual_live_admission("DESCRIBE <urn:s>");
    }
    #[test]
    fn construct_literal_refuses_before_copy_and_rolls_back_late_failure_or_cancel() {
        use crate::algo::memory::probe;
        use crate::error::BudgetKind;
        use std::sync::atomic::{AtomicUsize, Ordering};
        let text = format!(
            "CONSTRUCT {{ <urn:s> <urn:p> \"{}\" }} WHERE {{}}",
            "x".repeat(64 << 10)
        );
        let Query::Construct { template, .. } = parse_query(&text, None, &[]).unwrap() else {
            unreachable!()
        };
        let store = Store::in_memory(StoreOptions::default());
        let mut context = Ctx::new(store.snapshot());
        context.mem_limit = 16 << 10;
        let context = Arc::new(context);
        let copied = Arc::new(AtomicUsize::new(0));
        let observed = copied.clone();
        let hook = probe::install(
            move |point| {
                if let probe::Point::Allocation("construct-literal", 1) = point {
                    observed.fetch_add(1, Ordering::Relaxed);
                }
            },
            None,
        );
        assert!(
            matches!(construct(&context,&Table::unit(),&template,&[]), Err(Error::BudgetExceeded(b)) if b.kind == BudgetKind::Memory)
        );
        drop(hook);
        assert_eq!(copied.load(Ordering::Relaxed), 0);
        assert_eq!(context.live_bytes(), 0);
        for cancel in [false, true] {
            let context = Arc::new(Ctx::new(store.snapshot()));
            let flag = context.cancel.clone();
            let hook = probe::install(
                move |point| {
                    if cancel && let probe::Point::Allocation("construct-literal", 1) = point {
                        flag.store(true, Ordering::Relaxed);
                    }
                },
                (!cancel).then_some("construct-literal"),
            );
            let result = construct(&context, &Table::unit(), &template, &[]);
            drop(hook);
            if cancel {
                assert!(matches!(result, Err(Error::Cancelled)));
            } else {
                assert!(matches!(result, Err(Error::Invalid(_))));
            }
            assert_eq!(context.live_bytes(), 0);
        }
    }
    #[test]
    fn eager_graph_clones_keep_payload_and_original_fast_owner_until_last_drop() {
        let store = Store::in_memory(StoreOptions::default());
        let pool = pool::QueryPool::new(16 << 20);
        let options = QueryOptions {
            pool: Some(pool.clone()),
            max_memory_bytes: Some(4 << 20),
            no_cache: true,
            ..Default::default()
        };
        let text = format!(
            "CONSTRUCT {{ _:b <urn:p> \"{}\" . _:b <urn:q> 1 }} WHERE {{}}",
            "x".repeat(64 << 10)
        );
        query_memory::LEDGER_OPS.with(|n| n.set(0));
        let result = query(store.snapshot(), &text, &options).unwrap();
        assert_eq!(query_memory::LEDGER_OPS.with(|n| n.get()), 0);
        assert_eq!(result.triples().len(), 2);
        assert_eq!(result.triples()[0].subject, result.triples()[1].subject);
        let weak = Arc::downgrade(result.test_context());
        let last = result.clone();
        drop(result);
        assert!(weak.upgrade().unwrap().live_bytes() >= 64 << 10);
        assert_eq!(last.triples().len(), 2);
        drop(last);
        assert!(weak.upgrade().is_none());
        assert_eq!(pool.granted(), 0);
    }
}

// Integration fixtures that inspect private output ownership remain unit tests.
#[cfg(test)]
#[path = "../../tests/private/query_aggregates.rs"]
mod fixture_query_aggregates;
#[cfg(test)]
#[path = "../../tests/private/query_pool.rs"]
mod fixture_query_pool;
