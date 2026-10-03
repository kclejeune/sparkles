//! A read-only GraphQL adapter for Sparkles datasets (spec C03).
//!
//! An administrator installs a mapping schema: GraphQL SDL whose types and fields name
//! RDF classes and predicates with `@rdf` directives ([`mapping`]). The server derives
//! the API schema clients see ([`api`]), with filters, orders, connections and
//! `node(id:)`. A request is parsed and validated with `apollo-compiler`, split into
//! fetch groups ([`plan`]), each one SPARQL query built as algebra ([`algebra`]), run on
//! one snapshot with the caller's query options, and assembled into the response
//! ([`exec`]). The number of queries depends on the document only, never on the number
//! of nodes in the answer.

pub mod algebra;
pub mod api;
pub mod config;
pub mod cursor;
pub mod draft;
pub mod error;
pub mod exec;
pub mod mapping;
pub mod names;
pub mod plan;
pub mod scalars;

use apollo_compiler::collections::HashMap as AHashMap;
use apollo_compiler::executable::OperationType;
use apollo_compiler::schema::Implementers;
use apollo_compiler::validation::Valid;
use apollo_compiler::{ExecutableDocument, Name, Schema, ast};
pub use config::{Catalog, Change, Config, Limits as SchemaLimits, Stored, Version};
pub use error::{Code, GqlError, Outcome};
use serde_json::{Map, Value as J, json};
use sha2::{Digest, Sha256};
use sparkles::guard::config::DataGraphSel;
use sparkles::history::At;
use sparkles::sparql::QueryOptions;
use sparkles::store::Snapshot;
use std::sync::Arc;
use std::time::Instant;

/// Largest document, in bytes (§7.1).
pub const MAX_DOCUMENT_BYTES: usize = 64 << 10;

/// Parsed and validated documents kept per schema.
const DOC_CACHE: usize = 256;

/// An installed configuration, compiled: the mapping, the API schema and a cache of
/// validated documents. Built once per version and shared by requests.
pub struct Compiled {
    pub config: Config,
    pub version: u64,
    pub mapping: mapping::Mapping,
    pub api: Valid<Schema>,
    /// the API schema as SDL (`GET /{ds}/graphql/schema`)
    pub api_sdl: String,
    implementers: AHashMap<Name, Implementers>,
    docs: quick_cache::sync::Cache<String, Arc<Valid<ExecutableDocument>>>,
}

/// Why a configuration was refused.
#[derive(Debug)]
pub enum PutError {
    Engine(sparkles::Error),
    Sdl(Vec<mapping::SdlError>),
}

impl PutError {
    pub fn message(&self) -> String {
        match self {
            PutError::Engine(e) => e.to_string(),
            PutError::Sdl(errs) => errs
                .iter()
                .map(|e| e.to_string())
                .collect::<Vec<_>>()
                .join("; "),
        }
    }
}

impl Compiled {
    /// Compile a configuration. Returns the warnings of §3.3 as well.
    pub fn new(
        config: Config,
        version: u64,
        backing: mapping::Backing,
    ) -> Result<(Compiled, Vec<String>), PutError> {
        let (m, _) = mapping::parse(&config.sdl, backing).map_err(PutError::Sdl)?;
        let text = api::sdl(&m);
        let api = Schema::parse_and_validate(&text, "api.graphql").map_err(|e| {
            PutError::Sdl(
                e.errors
                    .iter()
                    .map(|d| mapping::SdlError {
                        message: format!("the derived API schema is invalid: {}", d.error),
                        line: None,
                    })
                    .collect(),
            )
        })?;
        let warnings = m.warnings.clone();
        Ok((
            Compiled {
                api_sdl: api.to_string(),
                implementers: api.implementers_map(),
                api,
                mapping: m,
                config,
                version,
                docs: quick_cache::sync::Cache::new(DOC_CACHE),
            },
            warnings,
        ))
    }

    /// Plan a document without running it: the groups' paths and kinds (tests, tools).
    pub fn parse_document(
        &self,
        query: &str,
        operation_name: Option<&str>,
        get: bool,
    ) -> Result<Arc<Valid<ExecutableDocument>>, GqlError> {
        if query.len() > MAX_DOCUMENT_BYTES {
            return Err(GqlError::new(
                Code::ParseFailed,
                format!("the document is larger than {MAX_DOCUMENT_BYTES} bytes"),
            ));
        }
        let mut h = Sha256::new();
        h.update(query.as_bytes());
        h.update([0]);
        h.update(operation_name.unwrap_or("").as_bytes());
        let k: String = h.finalize().iter().map(|b| format!("{b:02x}")).collect();
        let ast = ast::Document::parse(query, "query.graphql").map_err(|e| {
            let mut errs = e.errors.iter();
            let first = errs.next();
            let mut g = GqlError::new(
                Code::ParseFailed,
                first
                    .as_ref()
                    .map_or("the document does not parse".into(), |d| {
                        d.error.to_string()
                    }),
            );
            if let Some(r) = first.as_ref().and_then(|d| d.line_column_range()) {
                g.locations.push((r.start.line, r.start.column));
            }
            g
        })?;
        if get {
            let ops: Vec<&ast::OperationDefinition> = ast
                .definitions
                .iter()
                .filter_map(|d| match d {
                    ast::Definition::OperationDefinition(o) => Some(o.as_ref()),
                    _ => None,
                })
                .collect();
            let selected = match operation_name {
                Some(n) => ops.iter().find(|o| o.name.as_ref().is_some_and(|x| x == n)),
                None if ops.len() == 1 => ops.first(),
                None => None,
            };
            if selected.is_some_and(|o| o.operation_type != OperationType::Query) {
                return Err(GqlError::new(
                    Code::MethodNotAllowed,
                    "GET runs queries only; send mutations with POST",
                ));
            }
        }
        if let Some(d) = self.docs.get(&k) {
            return Ok(d);
        }
        let doc = ast.to_executable_validate(&self.api).map_err(|e| {
            let errs: Vec<GqlError> = e
                .errors
                .iter()
                .map(|d| {
                    let mut g = GqlError::new(Code::ValidationFailed, d.error.to_string());
                    if let Some(r) = d.line_column_range() {
                        g.locations.push((r.start.line, r.start.column));
                    }
                    g
                })
                .collect();
            Multi(errs).into_one()
        })?;
        let doc = Arc::new(doc);
        self.docs.insert(k, doc.clone());
        Ok(doc)
    }

    /// The query options of the adapter's data graph (§3.1), from the caller's options.
    pub fn data_options(&self, base: &QueryOptions) -> QueryOptions {
        let mut o = base.clone();
        let inferred = sparkles::guard::config::INFERRED_GRAPH.to_string();
        let reasoning = self
            .config
            .reasoning
            .unwrap_or(!base.default_graph_extra.is_empty());
        o.named_graph_uris.clear();
        match &self.config.data_graph {
            DataGraphSel::Named(n) if n == "union" => {
                o.default_graph_uris = vec![sparkles::sparql::ctx::UNION_GRAPH_IRI.to_string()];
                o.default_graph_extra.clear();
            }
            DataGraphSel::Named(_) => {
                o.default_graph_uris.clear();
                o.default_graph_extra = if reasoning {
                    vec![inferred]
                } else {
                    Vec::new()
                };
            }
            DataGraphSel::Graphs(gs) => {
                o.default_graph_uris = gs.clone();
                if reasoning {
                    o.default_graph_uris.push(inferred);
                }
                o.default_graph_extra.clear();
            }
        }
        o
    }
}

/// Several errors that fail a request together (validation).
struct Multi(Vec<GqlError>);

impl Multi {
    fn into_one(self) -> GqlError {
        let mut it = self.0.into_iter();
        let mut first = it
            .next()
            .unwrap_or_else(|| GqlError::new(Code::ValidationFailed, "invalid document"));
        let rest: Vec<J> = it.map(|e| e.to_json()).collect();
        if !rest.is_empty() {
            first.extensions.insert("more".into(), J::Array(rest));
        }
        first
    }
}

/// A GraphQL request.
#[derive(Clone, Debug, Default)]
pub struct Request {
    pub query: String,
    pub operation_name: Option<String>,
    pub variables: Map<String, J>,
}

/// How a request runs.
#[derive(Clone)]
pub struct Options {
    /// the caller's query options: graph view, budgets, timeout, cancel flag, the
    /// endpoint's inferred graph
    pub query: QueryOptions,
    /// the server's ceilings
    pub limits: plan::Limits,
    /// the caller may administer the dataset (introspection when it is off)
    pub admin: bool,
    pub explain: bool,
    /// sent with `GET` (mutations answer 405)
    pub get: bool,
    /// the `at` parameter
    pub at: Option<At>,
    /// `--max-result-mb`, or the request's lower value
    pub max_result_bytes: Option<u64>,
}

impl Default for Options {
    fn default() -> Self {
        Options {
            query: QueryOptions::default(),
            limits: plan::Limits::default(),
            admin: true,
            explain: false,
            get: false,
            at: None,
            max_result_bytes: None,
        }
    }
}

/// A response, with what the server reports about it.
#[derive(Clone, Debug)]
pub struct Response {
    pub body: J,
    pub outcome: Outcome,
    /// the commit the request read
    pub commit: Option<u64>,
    /// engine executions
    pub groups: usize,
    /// SHA-256 of the document
    pub doc_hash: String,
    pub operation_name: Option<String>,
}

/// The snapshot of an `at` selector (`None`: the head).
pub type Resolve<'a> = &'a dyn Fn(Option<&At>) -> sparkles::Result<Arc<Snapshot>>;

/// Run a request against the snapshots `resolve` gives (§6.1): the head, the request's
/// `at`, or the commit its cursors name.
pub fn execute(c: &Compiled, req: &Request, opts: &Options, resolve: Resolve) -> Response {
    let doc_hash: String = Sha256::digest(req.query.as_bytes())
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    let mut resp = Response {
        body: J::Null,
        outcome: Outcome::Executed,
        commit: None,
        groups: 0,
        doc_hash,
        operation_name: req.operation_name.clone(),
    };
    match run(c, req, opts, resolve, &mut resp) {
        Ok(()) => {}
        Err(e) => {
            resp.outcome = Outcome::of(e.code);
            let mut body = json!({ "errors": [e.to_json()] });
            if let Some(commit) = resp.commit {
                body["extensions"] = json!({ "sparkles": { "commit": commit } });
            }
            resp.body = body;
        }
    }
    resp
}

/// Run a request against one snapshot (library use): cursors must name its commit.
pub fn execute_on(
    c: &Compiled,
    snapshot: Arc<Snapshot>,
    req: &Request,
    opts: &Options,
) -> Response {
    let resolve = move |_: Option<&At>| Ok(snapshot.clone());
    execute(c, req, opts, &resolve)
}

fn cursor_error(e: sparkles::Error, commit: u64) -> GqlError {
    match e {
        sparkles::Error::HistoryGone(g) => GqlError::new(
            Code::CursorExpired,
            format!("the cursor's commit {commit} is no longer readable"),
        )
        .with("commit", commit)
        .with(
            "oldestReadableCommit",
            g.reconstructable.first().map(|r| r.0),
        ),
        sparkles::Error::HistoryUnsupported(m) | sparkles::Error::NotFound(m) => GqlError::new(
            Code::CursorExpired,
            format!("the cursor's commit {commit} cannot be read: {m}"),
        )
        .with("commit", commit),
        e => GqlError::from_engine(e),
    }
}

fn run(
    c: &Compiled,
    req: &Request,
    opts: &Options,
    resolve: Resolve,
    resp: &mut Response,
) -> Result<(), GqlError> {
    let started = Instant::now();
    let doc = c.parse_document(&req.query, req.operation_name.as_deref(), opts.get)?;
    let op = doc
        .operations
        .get(req.operation_name.as_deref())
        .map_err(|e| GqlError::new(Code::ValidationFailed, e.message().to_string()))?;
    if !c.config.introspection && !opts.admin {
        for f in op.root_fields(&doc) {
            if matches!(f.name.as_str(), "__schema" | "__type") {
                return Err(GqlError::new(
                    Code::ValidationFailed,
                    "introspection is disabled for this schema",
                )
                .at(plan::location(f, &doc)));
            }
        }
    }
    let raw: apollo_compiler::response::JsonMap =
        serde_json::from_value(J::Object(req.variables.clone()))
            .map_err(|e| GqlError::new(Code::BadUserInput, format!("variables: {e}")))?;
    let vars = apollo_compiler::request::coerce_variable_values(&c.api, op, &raw)
        .map_err(|e| GqlError::new(Code::BadUserInput, e.message().to_string()))?;
    apollo_compiler::introspection::check_max_depth(&doc, op)
        .map_err(|e| GqlError::new(Code::TooComplex, e.message().to_string()))?;
    let limits = opts.limits.with(&c.config.limits);
    let plan = plan::Planner::new(c, &doc, &vars, limits).plan(op)?;
    // the snapshot: the cursors' commit, which `at` must agree with, or `at`, or the head
    let snap = match plan.commit {
        Some(commit) => match &opts.at {
            Some(at) => {
                let s = resolve(Some(at)).map_err(GqlError::from_engine)?;
                if s.commit != commit {
                    return Err(GqlError::new(
                        Code::CursorInvalid,
                        format!(
                            "the cursors name commit {commit}, and at names commit {}",
                            s.commit
                        ),
                    ));
                }
                s
            }
            None => {
                let head = resolve(None).map_err(GqlError::from_engine)?;
                if head.commit == commit {
                    head
                } else {
                    resolve(Some(&At::Commit(commit))).map_err(|e| cursor_error(e, commit))?
                }
            }
        },
        None => resolve(opts.at.as_ref()).map_err(GqlError::from_engine)?,
    };
    resp.commit = Some(snap.commit);
    let mut qopts = c.data_options(&opts.query);
    // one row count for every group of the request
    qopts.work = Some(Arc::new(std::sync::atomic::AtomicU64::new(0)));
    let deadline = qopts.timeout.map(|t| started + t);
    let data = exec::run_groups(c, &plan, snap.clone(), &qopts, deadline, opts.explain)?;
    resp.groups = data.iter().filter(|d| !d.skipped).count();
    let (out, errors, _nodes) = exec::assemble(
        c,
        &plan,
        &data,
        &doc,
        op,
        &vars,
        limits.max_nodes,
        snap.commit,
    )?;
    let mut body = Map::new();
    if !errors.is_empty() {
        body.insert(
            "errors".into(),
            errors.iter().map(GqlError::to_json).collect(),
        );
    }
    body.insert("data".into(), out.unwrap_or(J::Null));
    let mut ext = json!({ "commit": snap.commit });
    if opts.explain {
        ext["plan"] = exec::explain(&plan, &data);
        ext["groups"] = resp.groups.into();
    }
    body.insert("extensions".into(), json!({ "sparkles": ext }));
    let body = J::Object(body);
    if let Some(limit) = opts.max_result_bytes {
        let n = serde_json::to_vec(&body)
            .map(|v| v.len() as u64)
            .unwrap_or(0);
        if n > limit {
            return Err(GqlError::new(
                Code::BudgetExceeded,
                format!("the response is {n} bytes, more than the limit of {limit}"),
            )
            .with("budget", "result-bytes")
            .with("limit", limit)
            .with("requested", n));
        }
    }
    resp.body = body;
    Ok(())
}

#[cfg(test)]
mod tests;
