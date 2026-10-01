//! SPARQL 1.1 query & update engine (ARQ equivalent).

pub mod cache;
pub mod ctx;
pub mod exec;
pub mod expr;
pub mod geopf;
mod keyfilter;
pub mod plan;
pub mod results;
pub mod table;
pub mod textpf;
pub mod update;
pub mod value;

use crate::error::{Error, Result};
use crate::id::Id;
use crate::index::Perm;
use crate::store::{Chunk, Snapshot};
pub use ctx::{Ctx, DatasetSpec, Optimizations};
pub use exec::PlanInfo;
use oxrdf::{BlankNode, NamedOrBlankNode, Term, Triple};
use plan::{ActiveGraph, Planner};
use rustc_hash::{FxHashMap, FxHashSet};
use serde::Serialize;
use spargebra::algebra::{GraphPattern, QueryDataset};
use spargebra::term::{NamedNodePattern, TermPattern, TriplePattern};
use spargebra::{Query, SparqlParser};
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::time::{Duration, Instant};
pub use table::Table;

#[derive(Clone, Debug, Default)]
pub struct QueryOptions {
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
    /// Blank nodes produced by this store (`_:b…` labels) resolve to their stored node.
    pub initial_bindings: Vec<(String, Term)>,
    /// prefixes made available to the query (Fuseki doesn't do this; the CLI does)
    pub prefixes: Vec<(String, String)>,
    /// Executor optimizations in effect (all on by default; see [`Optimizations`]).
    pub optimizations: Option<Optimizations>,
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
    pub triples: Vec<Triple>,
    pub plan: PlanInfo,
    pub timing: Timing,
    /// Peak estimated memory of intermediate results (see [`QueryOptions::max_memory_bytes`]).
    pub mem_peak_bytes: u64,
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
            _ => self.triples.len(),
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
    let mut p = SparqlParser::new();
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
    let (pattern, _, _) = split(&query);
    validate_scoping(pattern)?;
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
        | GP::Union { left, right }
        | GP::Minus { left, right }
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
        | GP::Service { inner, .. } => validate_scoping(inner),
        _ => Ok(()),
    }
}

fn make_ctx(
    snap: Arc<Snapshot>,
    opts: &QueryOptions,
    dataset: Option<&QueryDataset>,
    base: Option<&oxiri::Iri<String>>,
) -> Ctx {
    let mut ctx = Ctx::new(snap);
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
    ctx.allow_service = opts.allow_service;
    ctx.forbid_service = opts.forbid_service;
    ctx.outbound = opts.outbound.clone();
    ctx.outbound_budget = crate::outbound::RequestBudget::new(&opts.outbound);
    ctx.use_cache = !opts.no_cache;
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
    ctx
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
    let t1 = Instant::now();
    let (pattern, dataset, base) = split(parsed);
    let ctx = Arc::new(make_ctx(snap, opts, dataset, base));
    crate::geo::validate_query(pattern, &mut |w| ctx.warn(w))?;
    let mut planner = Planner::new(&ctx);
    let mut bound: Vec<(table::VarId, Id)> = Vec::new();
    for (name, term) in &opts.initial_bindings {
        let v = ctx.var(name.trim_start_matches(['?', '$']));
        let id = ctx.intern_term(term);
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
    let node = planner.plan(&pattern, &ActiveGraph::Default, Vec::new())?;
    let plan_ms = t1.elapsed().as_secs_f64() * 1000.0;
    let t2 = Instant::now();
    let (table, mut plan) = exec::execute(&ctx, &node)?;
    plan.warnings = ctx.warnings();
    let mut result = QueryResult {
        kind,
        vars: Vec::new(),
        table: Table::default(),
        boolean: false,
        triples: Vec::new(),
        plan,
        timing: Timing::default(),
        mem_peak_bytes: 0,
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
        Query::Construct { template, .. } => {
            result.triples = construct(&ctx, &table, template);
        }
        Query::Describe { .. } => {
            result.triples = describe(&ctx, &table)?;
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
    let (pattern, dataset, base) = split(&parsed);
    let ctx = make_ctx(snap, opts, dataset, base);
    crate::geo::validate_query(pattern, &mut |w| ctx.warn(w))?;
    let node = Planner::new(&ctx).plan(pattern, &ActiveGraph::Default, Vec::new())?;
    let mut info = exec::describe(&ctx, &node);
    info.warnings = ctx.warnings();
    Ok((parsed.to_sse(), info))
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

fn construct(ctx: &Ctx, t: &Table, template: &[TriplePattern]) -> Vec<Triple> {
    let map = t.var_map(ctx.nvars());
    let mut seen = FxHashSet::default();
    let mut out = Vec::new();
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
        for tp in template {
            let s = inst(&tp.subject, &mut bnodes);
            let p = match &tp.predicate {
                NamedNodePattern::NamedNode(n) => Some(Term::NamedNode(n.clone())),
                NamedNodePattern::Variable(v) => {
                    inst(&TermPattern::Variable(v.clone()), &mut bnodes)
                }
            };
            let o = inst(&tp.object, &mut bnodes);
            let (Some(s), Some(Term::NamedNode(p)), Some(o)) = (s, p, o) else {
                continue;
            };
            let s = match s {
                Term::NamedNode(n) => NamedOrBlankNode::NamedNode(n),
                Term::BlankNode(b) => NamedOrBlankNode::BlankNode(b),
                _ => continue,
            };
            let tr = Triple::new(s, p, o);
            if seen.insert(tr.clone()) {
                out.push(tr);
            }
        }
    }
    out
}

/// DESCRIBE: bounded description (outgoing triples + blank node closure), like
/// Jena's default `DescribeBNodeClosure`, over the default graph.
fn describe(ctx: &Ctx, t: &Table) -> Result<Vec<Triple>> {
    let mut resources: Vec<Id> = Vec::new();
    let mut seen_r = FxHashSet::default();
    for col in &t.cols {
        for id in col {
            if !id.is_undef()
                && matches!(ctx.kind(*id), ctx::TermKind::Iri | ctx::TermKind::BNode)
                && seen_r.insert(*id)
            {
                resources.push(*id);
            }
        }
    }
    let gf = match &ctx.dataset.default {
        Some(gs) => plan::GraphFilter::Set({
            let mut v: Vec<u64> = gs.iter().map(|g| g.0).collect();
            v.sort_unstable();
            v
        }),
        None if ctx.snap.union_default_graph => plan::GraphFilter::All,
        None => plan::GraphFilter::Default,
    };
    let mut out = Vec::new();
    let mut seen = FxHashSet::default();
    let mut queue = resources;
    let mut visited = FxHashSet::default();
    while let Some(r) = queue.pop() {
        ctx.check()?;
        if !visited.insert(r) || r.tag() == crate::id::Tag::Local {
            continue;
        }
        let mut keys = Vec::new();
        ctx.snap.scan(Perm::Spo, &[r.0], |c| {
            match c {
                Chunk::Block(b, s, e) => keys.extend((s..e).map(|i| b.key(i))),
                Chunk::Row(k) => keys.push(k),
            }
            Ok(true)
        })?;
        for k in keys {
            if !gf.accepts(k[3]) {
                continue;
            }
            let (s, p, o) = (Id(k[0]), Id(k[1]), Id(k[2]));
            if !seen.insert((s, p, o)) {
                continue;
            }
            if o.tag() == crate::id::Tag::BNode {
                queue.push(o);
            }
            if let Some(q) = ctx.snap.quad_to_terms(&[s, p, o, Id::DEFAULT_GRAPH]) {
                out.push(Triple::new(q.subject, q.predicate, q.object));
            }
        }
    }
    Ok(out)
}

#[cfg(test)]
mod opt_tests;
#[cfg(test)]
mod tests;
