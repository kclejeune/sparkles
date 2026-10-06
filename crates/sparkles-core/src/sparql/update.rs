//! SPARQL 1.1 Update (ARQ `modify` equivalent). All operations of a request run in a
//! single write transaction and see the effects of the previous operations.

use super::ctx::Ctx;
use super::plan::{ActiveGraph, Planner};
use super::{QueryOptions, Timing};
use crate::error::{Error, Result};
use crate::id::{Id, Tag};
use crate::index::Perm;
use crate::outbound::RequestBudget;
use crate::store::{Store, WriteTxn};
use oxrdf::{BlankNode, NamedNode, Term};
use oxrdfio::{RdfFormat, RdfParseError, RdfParser};
use rustc_hash::FxHashMap;
use serde::Serialize;
use spargebra::algebra::{GraphTarget, QueryDataset};
use spargebra::term::{
    GraphName, GraphNamePattern, GroundTerm, GroundTermPattern, NamedNodePattern, TermPattern,
};
use spargebra::{GraphUpdateOperation, SparqlParser};
use std::io::Read;
use std::sync::Arc;
use std::time::Instant;

#[derive(Debug, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdateStats {
    /// quads inserted, summed over the operations
    pub inserted: u64,
    /// quads deleted, summed over the operations
    pub deleted: u64,
    pub operations: usize,
    pub timing: Timing,
    /// the commit the request produced (or the unchanged head); not part of the default
    /// JSON body
    #[serde(skip)]
    pub commit: Option<crate::commit::Receipt>,
    /// Peak estimated memory of the WHERE evaluations (the largest of any operation).
    pub mem_peak_bytes: u64,
    /// Rows produced by the operators of every WHERE evaluation, summed.
    pub rows_produced: u64,
}

pub fn update(store: &Store, u: &str, opts: &QueryOptions) -> Result<UpdateStats> {
    update_as(store, u, opts, crate::commit::CommitKind::Update)
}

/// [`update`], recording the commit as `kind`.
pub fn update_as(
    store: &Store,
    u: &str,
    opts: &QueryOptions,
    kind: crate::commit::CommitKind,
) -> Result<UpdateStats> {
    let t0 = Instant::now();
    let parsed = parse_update(u, opts)?;
    let depth = super::depth::check_update(&parsed)?;
    super::depth::with_stack(depth, || run_update(store, &parsed, opts, kind, t0))
}

/// Run a SPARQL Update request inside an open write transaction without committing it.
/// The operations see the transaction's earlier changes, and the transaction commits or
/// discards them with the rest of its work. The request's timeout and cancellation apply
/// to its own operations. `opts.write` does not apply, because the transaction was
/// opened with write options of its own. A `text:query` call fails once the
/// transaction has changed data, because the full-text index covers committed data only.
pub fn update_in(txn: &mut WriteTxn<'_>, u: &str, opts: &QueryOptions) -> Result<UpdateStats> {
    let t0 = Instant::now();
    let parsed = parse_update(u, opts)?;
    let depth = super::depth::check_update(&parsed)?;
    super::depth::with_stack(depth, || {
        validate_geometry(&parsed)?;
        if let Some(access) = opts.graphs.as_ref() {
            for op in &parsed.operations {
                check_constant_graphs(access, op)?;
            }
        }
        let parse_ms = t0.elapsed().as_secs_f64() * 1000.0;
        let mut stats = UpdateStats {
            operations: parsed.operations.len(),
            ..Default::default()
        };
        let t1 = Instant::now();
        let req = Request {
            opts,
            deadline: opts.timeout.map(|t| t0 + t),
            base: parsed.base_iri.clone(),
            budget: RequestBudget::new(&opts.outbound),
            produced: Default::default(),
        };
        for op in &parsed.operations {
            req.check()?;
            run_op(txn, op, &req, &mut stats)?;
        }
        req.check()?;
        stats.rows_produced = req.produced.load(std::sync::atomic::Ordering::Relaxed);
        let exec_ms = t1.elapsed().as_secs_f64() * 1000.0;
        stats.timing = Timing {
            parse_ms,
            plan_ms: 0.0,
            exec_ms,
            serialize_ms: 0.0,
            total_ms: parse_ms + exec_ms,
        };
        Ok(stats)
    })
}

/// Parse an update request with the base IRI and prefixes of `opts`, and the protocol's
/// `using-graph-uri` and `using-named-graph-uri` from `opts`.
pub fn parse_update(u: &str, opts: &QueryOptions) -> Result<spargebra::Update> {
    let mut p = super::aggext::register(SparqlParser::new());
    if let Some(b) = &opts.base_iri {
        p = p
            .with_base_iri(b)
            .map_err(|e| Error::invalid(e.to_string()))?;
    }
    for (k, v) in &opts.prefixes {
        p = p
            .with_prefix(k, v)
            .map_err(|e| Error::invalid(e.to_string()))?;
    }
    let mut parsed = p.parse_update(u)?;
    protocol_dataset(&mut parsed, opts)?;
    Ok(parsed)
}

/// Malformed geometry constants fail before anything is written.
fn validate_geometry(parsed: &spargebra::Update) -> Result<()> {
    for op in &parsed.operations {
        if let GraphUpdateOperation::DeleteInsert { pattern, .. } = op {
            crate::geo::validate_query(pattern, &mut |_| {})?;
        }
    }
    Ok(())
}

/// SPARQL 1.1 Protocol §2.2.3: for an update, [`QueryOptions::default_graph_uris`] and
/// [`QueryOptions::named_graph_uris`] are the `using-graph-uri` and
/// `using-named-graph-uri` parameters. They are the `USING` and `USING NAMED` of every
/// DELETE/INSERT operation, and an operation with `USING`, `USING NAMED` or `WITH` of
/// its own makes the request an error.
fn protocol_dataset(u: &mut spargebra::Update, opts: &QueryOptions) -> Result<()> {
    if opts.default_graph_uris.is_empty() && opts.named_graph_uris.is_empty() {
        return Ok(());
    }
    let iris = |v: &[String]| -> Result<Vec<NamedNode>> {
        v.iter()
            .map(|s| {
                NamedNode::new(s.clone())
                    .map_err(|e| Error::invalid(format!("invalid graph IRI <{s}>: {e}")))
            })
            .collect()
    };
    let default = iris(&opts.default_graph_uris)?;
    let named = iris(&opts.named_graph_uris)?;
    for op in &mut u.operations {
        if let GraphUpdateOperation::DeleteInsert { using, .. } = op {
            if using.is_some() {
                return Err(Error::invalid(
                    "using-graph-uri and using-named-graph-uri cannot be combined with USING, \
                     USING NAMED or WITH",
                ));
            }
            *using = Some(QueryDataset {
                default: default.clone(),
                named: Some(named.clone()),
            });
        }
    }
    Ok(())
}

/// Whether `u` parses as an update that only inserts or deletes data (`INSERT DATA`,
/// `DELETE DATA`): no pattern to evaluate, no LOAD, no graph management. Such an update
/// does a bounded amount of work for its size.
pub fn data_only(u: &str, opts: &QueryOptions) -> bool {
    let mut p = super::aggext::register(SparqlParser::new());
    if let Some(b) = &opts.base_iri {
        match p.with_base_iri(b) {
            Ok(q) => p = q,
            Err(_) => return false,
        }
    }
    for (k, v) in &opts.prefixes {
        match p.with_prefix(k, v) {
            Ok(q) => p = q,
            Err(_) => return false,
        }
    }
    p.parse_update(u).is_ok_and(|parsed| {
        !parsed.operations.is_empty()
            && parsed.operations.iter().all(|op| {
                matches!(
                    op,
                    GraphUpdateOperation::InsertData { .. }
                        | GraphUpdateOperation::DeleteData { .. }
                )
            })
    })
}

fn run_update(
    store: &Store,
    parsed: &spargebra::Update,
    opts: &QueryOptions,
    kind: crate::commit::CommitKind,
    t0: Instant,
) -> Result<UpdateStats> {
    // malformed geometry constants fail before the writer lock is taken
    validate_geometry(parsed)?;
    let parse_ms = t0.elapsed().as_secs_f64() * 1000.0;
    let mut stats = UpdateStats {
        operations: parsed.operations.len(),
        ..Default::default()
    };
    let t1 = Instant::now();
    let req = Request {
        opts,
        deadline: opts.timeout.map(|t| t0 + t),
        base: parsed.base_iri.clone(),
        budget: opts
            .outbound_budget
            .clone()
            .unwrap_or_else(|| RequestBudget::new(&opts.outbound)),
        produced: Default::default(),
    };
    // the request's cancellation and deadline also end the wait for the writer lock
    // and the write guard
    // graphs named by constants are checked before the writer lock is taken
    if let Some(access) = opts.graphs.as_ref() {
        for op in &parsed.operations {
            check_constant_graphs(access, op)?;
        }
    }
    let mut wopts = opts.write.clone();
    if wopts.graphs.is_none() {
        wopts.graphs = opts.graphs.clone();
    }
    if wopts.cancel.is_none() {
        wopts.cancel = opts.cancel.clone();
    }
    if wopts.deadline.is_none() {
        wopts.deadline = req.deadline;
    }
    let mut txn = store.try_write_with(kind, wopts)?;
    for op in &parsed.operations {
        req.check()?;
        run_op(&mut txn, op, &req, &mut stats)?;
    }
    // a request cancelled or timed out before this point publishes nothing
    req.check()?;
    stats.rows_produced = req.produced.load(std::sync::atomic::Ordering::Relaxed);
    stats.commit = Some(txn.commit()?);
    let exec_ms = t1.elapsed().as_secs_f64() * 1000.0;
    stats.timing = Timing {
        parse_ms,
        plan_ms: 0.0,
        exec_ms,
        serialize_ms: 0.0,
        total_ms: parse_ms + exec_ms,
    };
    Ok(stats)
}

/// Limits and context shared by every operation of one update request: one deadline for
/// the whole request, its cancellation flag, row, memory and outbound budgets, and the
/// parsed BASE.
struct Request<'a> {
    opts: &'a QueryOptions,
    deadline: Option<Instant>,
    base: Option<oxiri::Iri<String>>,
    /// what the LOADs and SERVICE calls of every operation spend
    budget: Arc<RequestBudget>,
    /// rows produced by the WHERE evaluations of every operation
    produced: Arc<std::sync::atomic::AtomicU64>,
}

impl Request<'_> {
    fn check(&self) -> Result<()> {
        if self
            .opts
            .cancel
            .as_ref()
            .is_some_and(|c| c.load(std::sync::atomic::Ordering::Relaxed))
        {
            return Err(Error::Cancelled);
        }
        if self.deadline.is_some_and(|d| Instant::now() > d) {
            return Err(Error::Timeout);
        }
        Ok(())
    }

    /// Query context for a WHERE clause, under the request's limits.
    fn ctx(&self, snap: Arc<crate::store::Snapshot>) -> Ctx {
        let snap = super::with_union_default(snap, self.opts);
        #[cfg(feature = "geo")]
        let op_vertices = snap.geo_op_vertices;
        let mut ctx = Ctx::new(snap);
        #[cfg(feature = "geo")]
        ctx.geo.set_op_vertices(op_vertices);
        ctx.deadline = self.deadline;
        if let Some(c) = &self.opts.cancel {
            ctx.cancel = c.clone();
        }
        if let Some(m) = self.opts.max_rows {
            ctx.max_rows = m;
        }
        if let Some(m) = self.opts.max_memory_bytes {
            ctx.mem_limit = m;
        }
        if let Some(m) = self.opts.max_rows_produced {
            ctx.max_rows_produced = m;
        }
        ctx.rows_produced = self.produced.clone();
        ctx.allow_service = self.opts.allow_service;
        ctx.forbid_service = self.opts.forbid_service;
        ctx.outbound = self.opts.outbound.clone();
        ctx.outbound_budget = self.budget.clone();
        if let Some(s) = &self.opts.service_scope {
            ctx.service_scope = s.clone();
        }
        if let Some(o) = self.opts.optimizations {
            ctx.opt = o;
        }
        ctx.base_iri = self.base.clone();
        ctx.extensions = self.opts.extensions.clone();
        ctx
    }
}

fn run_op(
    txn: &mut WriteTxn<'_>,
    op: &GraphUpdateOperation,
    req: &Request<'_>,
    stats: &mut UpdateStats,
) -> Result<()> {
    match op {
        GraphUpdateOperation::InsertData { data } => {
            let mut labels = std::collections::HashMap::new();
            for q in data {
                let quad = oxrdf::Quad::new(
                    q.subject.clone(),
                    q.predicate.clone(),
                    q.object.clone(),
                    match &q.graph_name {
                        GraphName::DefaultGraph => oxrdf::GraphName::DefaultGraph,
                        GraphName::NamedNode(n) => oxrdf::GraphName::NamedNode(n.clone()),
                    },
                );
                if stats.inserted % 4096 == 4095 {
                    req.check()?;
                }
                let ids = txn.encode_quad(&quad, &mut labels)?;
                if txn.insert(ids)? {
                    stats.inserted += 1;
                }
            }
        }
        GraphUpdateOperation::DeleteData { data } => {
            let view = txn.view();
            for q in data {
                let s = view.lookup_term(&Term::NamedNode(q.subject.clone()));
                let p = view.lookup_term(&Term::NamedNode(q.predicate.clone()));
                let o = match &q.object {
                    GroundTerm::NamedNode(n) => view.lookup_term(&Term::NamedNode(n.clone())),
                    GroundTerm::Literal(l) => view.lookup_term(&Term::Literal(l.clone())),
                    t @ GroundTerm::Triple(_) => view.lookup_term(&super::plan::ground_term(t)),
                };
                let g = match &q.graph_name {
                    GraphName::DefaultGraph => Some(Id::DEFAULT_GRAPH),
                    GraphName::NamedNode(n) => view.lookup_term(&Term::NamedNode(n.clone())),
                };
                if let (Some(s), Some(p), Some(o), Some(g)) = (s, p, o, g) {
                    if txn.delete([s, p, o, g])? {
                        stats.deleted += 1;
                    }
                } else {
                    // a quad of terms the store lacks deletes nothing, but protections are
                    // checked all the same, so that a refusal does not tell which exist
                    let graph = match &q.graph_name {
                        GraphName::DefaultGraph => None,
                        GraphName::NamedNode(n) => Some(Term::NamedNode(n.clone())),
                    };
                    let u = |x: Option<Id>| x.unwrap_or(Id::UNDEF);
                    txn.check_requested(
                        [u(s), u(p), u(o), u(g)],
                        q.predicate.as_str(),
                        graph.as_ref(),
                    )?;
                }
            }
        }
        GraphUpdateOperation::DeleteInsert {
            delete,
            insert,
            using,
            pattern,
        } => {
            // the WHERE clause reads what the graph view's protections leave visible
            let snap = txn.read_view()?;
            let mut ctx = req.ctx(snap);
            ctx.configure_extensions(pattern);
            if let Some(QueryDataset { default, named }) = using {
                ctx.dataset.default = Some(
                    default
                        .iter()
                        .map(|n| ctx.intern_term(&Term::NamedNode(n.clone())))
                        .collect(),
                );
                // WITH is encoded as USING without USING NAMED: named graphs stay visible
                ctx.dataset.named = named.as_ref().map(|n| {
                    n.iter()
                        .map(|n| ctx.intern_term(&Term::NamedNode(n.clone())))
                        .collect()
                });
            }
            // the WHERE clause reads the request's graph view only
            super::restrict_ctx(&mut ctx, req.opts.graphs.as_ref())?;
            if let Some(r) = &req.opts.rdfs {
                ctx.rdfs = Some(r.schema(&ctx.snap)?);
            }
            let pattern = super::rdfs::apply(&ctx, pattern);
            let node = Planner::new(&ctx).plan(&pattern, &ActiveGraph::Default, Vec::new())?;
            let executed = super::exec::execute(&ctx, &node);
            if let Err(error) = &executed
                && ctx.calls_extensions
            {
                ctx.fail_extension(super::extensions::ScalarError::from_engine(error));
            }
            let (table, _) = executed?;
            ctx.check()?;
            // a graph a template takes from a variable is checked for every solution,
            // before anything else of the quad is looked up
            let access = req.opts.graphs.clone();
            let check = |g: Option<Id>| -> Result<()> {
                match (&access, g) {
                    (Some(a), Some(g)) => check_graph_id(a, &ctx, g),
                    _ => Ok(()),
                }
            };
            stats.mem_peak_bytes = stats.mem_peak_bytes.max(ctx.mem_peak());
            let map = table.var_map(ctx.nvars());
            let get = |ctx: &Ctx, name: &str, i: usize| -> Option<Id> {
                let c = map.get(ctx.var(name) as usize).copied().flatten()?;
                let id = table.cols[c][i];
                (!id.is_undef()).then_some(id)
            };
            // A blank node the WHERE clause minted (BNODE(), a SERVICE result) is not in
            // the store: inserting it makes a new stored node, the same one wherever the
            // operation inserts it, and never one that exists already.
            let mut minted = Minted::default();
            // resolve query-local ids into store ids
            let to_store = |txn: &mut WriteTxn<'_>,
                            minted: &mut Minted,
                            ctx: &Ctx,
                            id: Id|
             -> Result<Option<Id>> {
                Ok(match id.tag() {
                    Tag::Local => match ctx.term(id) {
                        Some(t) => {
                            let t = minted.stored(txn, &t);
                            Some(txn.intern(&t)?)
                        }
                        None => None,
                    },
                    Tag::BNode => Some(minted.id(txn, id)),
                    _ => Some(id),
                })
            };
            let mut dels = Vec::new();
            let mut ins = Vec::new();
            for i in 0..table.len() {
                for q in delete {
                    let gt = |t: &GroundTermPattern| -> Option<Id> {
                        match t {
                            GroundTermPattern::Variable(v) => get(&ctx, v.as_str(), i),
                            GroundTermPattern::NamedNode(n) => {
                                ctx.snap.lookup_term(&Term::NamedNode(n.clone()))
                            }
                            GroundTermPattern::Literal(l) => {
                                ctx.snap.lookup_term(&Term::Literal(l.clone()))
                            }
                            GroundTermPattern::Triple(tp) => {
                                // a ground triple pattern is a template without blank nodes
                                let pat = ground_pattern_to_term_pattern(tp);
                                let term = super::instantiate(
                                    &pat,
                                    &mut |v| get(&ctx, v, i).and_then(|id| ctx.term(id)),
                                    &mut |_| Term::BlankNode(oxrdf::BlankNode::default()),
                                )?;
                                ctx.snap.lookup_term(&term)
                            }
                        }
                    };
                    if let GraphNamePattern::Variable(v) = &q.graph_name {
                        check(get(&ctx, v.as_str(), i))?;
                    }
                    let s = gt(&q.subject);
                    let p = named_pat(&ctx, &q.predicate, |n| get(&ctx, n, i), true);
                    let o = gt(&q.object);
                    let g = graph_pat(&ctx, &q.graph_name, |n| get(&ctx, n, i), true);
                    if let (Some(s), Some(p), Some(o), Some(g)) = (s, p, o, g) {
                        dels.push([s, p, o, g]);
                    }
                }
                let mut bnodes: FxHashMap<String, Id> = FxHashMap::default();
                for q in insert {
                    if let GraphNamePattern::Variable(v) = &q.graph_name {
                        check(get(&ctx, v.as_str(), i))?;
                    }
                    let mut tp = |txn: &mut WriteTxn<'_>, t: &TermPattern| -> Result<Option<Id>> {
                        Ok(match t {
                            TermPattern::Variable(v) => match get(&ctx, v.as_str(), i) {
                                Some(id) => to_store(txn, &mut minted, &ctx, id)?,
                                None => None,
                            },
                            TermPattern::BlankNode(b) => Some(bnode(txn, &mut bnodes, b)),
                            TermPattern::NamedNode(n) => {
                                Some(txn.intern(&Term::NamedNode(n.clone()))?)
                            }
                            TermPattern::Literal(l) => {
                                // the labels inside a composite literal of the template
                                // name the template's blank nodes, new per solution
                                let l = super::cdt::relabel_literal(l, &mut |b| {
                                    let b = BlankNode::new_unchecked(b);
                                    crate::id::bnode_label(bnode(txn, &mut bnodes, &b).payload())
                                })
                                .unwrap_or_else(|| l.clone());
                                Some(txn.intern(&Term::Literal(l))?)
                            }
                            t @ TermPattern::Triple(_) => {
                                let mut bn = |b: &str| {
                                    let id = match bnodes.get(b) {
                                        Some(id) => *id,
                                        None => {
                                            let id = txn.new_bnode();
                                            bnodes.insert(b.to_string(), id);
                                            id
                                        }
                                    };
                                    Term::BlankNode(crate::store::bnode_for(id))
                                };
                                match super::instantiate(
                                    t,
                                    &mut |v| get(&ctx, v, i).and_then(|id| ctx.term(id)),
                                    &mut bn,
                                ) {
                                    Some(term) => {
                                        let term = minted.stored(txn, &term);
                                        Some(txn.intern(&term)?)
                                    }
                                    None => None,
                                }
                            }
                        })
                    };
                    let s = tp(txn, &q.subject)?;
                    let o = tp(txn, &q.object)?;
                    let p = match &q.predicate {
                        NamedNodePattern::NamedNode(n) => {
                            Some(txn.intern(&Term::NamedNode(n.clone()))?)
                        }
                        NamedNodePattern::Variable(v) => match get(&ctx, v.as_str(), i) {
                            Some(id) => to_store(txn, &mut minted, &ctx, id)?,
                            None => None,
                        },
                    };
                    let g = match &q.graph_name {
                        GraphNamePattern::DefaultGraph => Some(Id::DEFAULT_GRAPH),
                        GraphNamePattern::NamedNode(n) => {
                            Some(txn.intern(&Term::NamedNode(n.clone()))?)
                        }
                        GraphNamePattern::Variable(v) => match get(&ctx, v.as_str(), i) {
                            Some(id) => to_store(txn, &mut minted, &ctx, id)?,
                            None => None,
                        },
                    };
                    let (Some(s), Some(p), Some(o), Some(g)) = (s, p, o, g) else {
                        continue;
                    };
                    // an instantiation that is not a valid RDF quad is skipped (SPARQL
                    // 1.1 Update §3.1.3): subject an IRI or blank node, predicate an IRI,
                    // graph an IRI. Kinds come from the stored keys, including terms this
                    // transaction just added to the delta vocabulary.
                    use super::ctx::TermKind;
                    if !matches!(ctx.kind(s), TermKind::Iri | TermKind::BNode)
                        || ctx.kind(p) != TermKind::Iri
                        || g != Id::DEFAULT_GRAPH && ctx.kind(g) != TermKind::Iri
                    {
                        continue;
                    }
                    ins.push([s, p, o, g]);
                }
            }
            req.check()?;
            for (i, q) in dels.into_iter().enumerate() {
                if i % 4096 == 4095 {
                    req.check()?;
                }
                if txn.delete(q)? {
                    stats.deleted += 1;
                }
            }
            for (i, q) in ins.into_iter().enumerate() {
                if i % 4096 == 4095 {
                    req.check()?;
                }
                if txn.insert(q)? {
                    stats.inserted += 1;
                }
            }
        }
        GraphUpdateOperation::Load {
            silent,
            source,
            destination,
        } => {
            let r = load(txn, source, destination, *silent, stats, req);
            // SILENT hides failures of the source, not a refusal, a spent budget or the
            // end of the request
            if matches!(
                r,
                Err(Error::NotPermitted(_)
                    | Error::BudgetExceeded(_)
                    | Error::Timeout
                    | Error::Cancelled)
            ) || (r.is_err() && !silent)
            {
                return r;
            }
        }
        GraphUpdateOperation::Clear { graph, .. } | GraphUpdateOperation::Drop { graph, .. } => {
            let view = txn.view();
            let graphs: Vec<Id> = match graph {
                GraphTarget::DefaultGraph => vec![Id::DEFAULT_GRAPH],
                GraphTarget::NamedNode(n) => view
                    .lookup_term(&Term::NamedNode(n.clone()))
                    .into_iter()
                    .collect(),
                // with a graph view: the graphs it sees, each of which must be writable
                GraphTarget::NamedGraphs | GraphTarget::AllGraphs
                    if let Some(a) = req.opts.graphs.as_ref().filter(|a| !a.read.is_all()) =>
                {
                    let mut v = a.visible_named(&view)?.to_vec();
                    if matches!(graph, GraphTarget::AllGraphs) && a.read.default_graph() {
                        v.push(Id::DEFAULT_GRAPH);
                    }
                    for g in &v {
                        a.check_write(&view, *g)?;
                    }
                    v
                }
                GraphTarget::NamedGraphs => view.graph_ids()?,
                GraphTarget::AllGraphs => {
                    let mut v = view.graph_ids()?;
                    v.push(Id::DEFAULT_GRAPH);
                    v
                }
            };
            // only the quads the view sees: protected ones it hides stay
            let read = txn.read_view()?;
            for g in graphs {
                req.check()?;
                for k in read.scan_keys(Perm::Gspo, &[g.0])? {
                    if txn.delete(Perm::Gspo.to_quad(&k))? {
                        stats.deleted += 1;
                    }
                }
            }
        }
        GraphUpdateOperation::Create { .. } => {}
    }
    Ok(())
}

/// [`Error::NotPermitted`] unless graph `g` (an id of the WHERE clause's context) may
/// be written.
fn check_graph_id(access: &crate::access::GraphAccess, ctx: &Ctx, g: Id) -> Result<()> {
    if g == Id::DEFAULT_GRAPH {
        return if access.writable(None) {
            Ok(())
        } else {
            Err(crate::access::GraphAccess::refused(None))
        };
    }
    let t = match g.tag() {
        Tag::BNode => Some(Term::BlankNode(crate::store::bnode_for(g))),
        _ => ctx.term(g),
    };
    if access.writable(t.as_ref()) {
        Ok(())
    } else {
        Err(crate::access::GraphAccess::refused(t.as_ref()))
    }
}

/// The graphs an operation names by constants, checked against the request's graph view
/// before anything is read or written: data quads, template graphs, and the targets of
/// `LOAD … INTO`, `CLEAR`, `DROP` and `CREATE`.
fn check_constant_graphs(
    access: &crate::access::GraphAccess,
    op: &GraphUpdateOperation,
) -> Result<()> {
    let check = |g: Option<&NamedNode>| -> Result<()> {
        let t = g.map(|n| Term::NamedNode(n.clone()));
        if access.writable(t.as_ref()) {
            Ok(())
        } else {
            Err(crate::access::GraphAccess::refused(t.as_ref()))
        }
    };
    fn name(g: &GraphName) -> Option<&NamedNode> {
        match g {
            GraphName::NamedNode(n) => Some(n),
            GraphName::DefaultGraph => None,
        }
    }
    match op {
        GraphUpdateOperation::InsertData { data } => {
            for q in data {
                check(name(&q.graph_name))?;
            }
        }
        GraphUpdateOperation::DeleteData { data } => {
            for q in data {
                check(name(&q.graph_name))?;
            }
        }
        GraphUpdateOperation::DeleteInsert { delete, insert, .. } => {
            let pat = |g: &GraphNamePattern| -> Result<()> {
                match g {
                    GraphNamePattern::NamedNode(n) => check(Some(n)),
                    GraphNamePattern::DefaultGraph => check(None),
                    GraphNamePattern::Variable(_) => Ok(()),
                }
            };
            for q in delete {
                pat(&q.graph_name)?;
            }
            for q in insert {
                pat(&q.graph_name)?;
            }
        }
        GraphUpdateOperation::Load { destination, .. } => {
            if let GraphName::NamedNode(n) = destination {
                check(Some(n))?;
            }
        }
        GraphUpdateOperation::Clear { graph, .. } | GraphUpdateOperation::Drop { graph, .. } => {
            match graph {
                GraphTarget::NamedNode(n) => check(Some(n))?,
                GraphTarget::DefaultGraph => check(None)?,
                // the graphs of the view, checked when the operation runs
                GraphTarget::NamedGraphs | GraphTarget::AllGraphs => {}
            }
        }
        GraphUpdateOperation::Create { graph, .. } => check(Some(graph))?,
    }
    Ok(())
}

fn ground_pattern_to_term_pattern(tp: &spargebra::term::GroundTriplePattern) -> TermPattern {
    let conv = |t: &GroundTermPattern| match t {
        GroundTermPattern::NamedNode(n) => TermPattern::NamedNode(n.clone()),
        GroundTermPattern::Literal(l) => TermPattern::Literal(l.clone()),
        GroundTermPattern::Variable(v) => TermPattern::Variable(v.clone()),
        GroundTermPattern::Triple(t) => ground_pattern_to_term_pattern(t),
    };
    TermPattern::Triple(Box::new(spargebra::term::TriplePattern {
        subject: conv(&tp.subject),
        predicate: tp.predicate.clone(),
        object: conv(&tp.object),
    }))
}

fn named_pat(
    ctx: &Ctx,
    p: &NamedNodePattern,
    get: impl Fn(&str) -> Option<Id>,
    lookup: bool,
) -> Option<Id> {
    match p {
        NamedNodePattern::NamedNode(n) if lookup => {
            ctx.snap.lookup_term(&Term::NamedNode(n.clone()))
        }
        NamedNodePattern::NamedNode(n) => Some(ctx.intern_term(&Term::NamedNode(n.clone()))),
        NamedNodePattern::Variable(v) => get(v.as_str()),
    }
}

fn graph_pat(
    ctx: &Ctx,
    g: &GraphNamePattern,
    get: impl Fn(&str) -> Option<Id>,
    lookup: bool,
) -> Option<Id> {
    match g {
        GraphNamePattern::DefaultGraph => Some(Id::DEFAULT_GRAPH),
        GraphNamePattern::NamedNode(n) if lookup => {
            ctx.snap.lookup_term(&Term::NamedNode(n.clone()))
        }
        GraphNamePattern::NamedNode(n) => Some(ctx.intern_term(&Term::NamedNode(n.clone()))),
        GraphNamePattern::Variable(v) => get(v.as_str()),
    }
}

/// The stored blank nodes that the minted blank nodes of one operation's solutions
/// become when inserted.
#[derive(Default)]
struct Minted(FxHashMap<u64, Id>);

impl Minted {
    /// The stored id of a blank node id: itself for a stored node, else (a node the
    /// WHERE clause minted) a new stored node, the same one for the same minted node.
    fn id(&mut self, txn: &mut WriteTxn<'_>, id: Id) -> Id {
        if id.payload() & Id::LOCAL_BNODE_BIT == 0 {
            return id;
        }
        *self
            .0
            .entry(id.payload())
            .or_insert_with(|| txn.new_bnode())
    }

    /// `t` with the minted blank nodes inside its triple terms replaced by stored ones.
    fn stored(&mut self, txn: &mut WriteTxn<'_>, t: &Term) -> Term {
        match t {
            Term::BlankNode(b) => match crate::id::parse_bnode_payload(b.as_str()) {
                Some(p) => Term::BlankNode(crate::store::bnode_for(self.id(txn, Id::bnode(p)))),
                None => t.clone(),
            },
            Term::Triple(tr) => {
                let s = match self.stored(txn, &tr.subject.clone().into()) {
                    Term::NamedNode(n) => oxrdf::NamedOrBlankNode::NamedNode(n),
                    Term::BlankNode(b) => oxrdf::NamedOrBlankNode::BlankNode(b),
                    _ => unreachable!("a subject stays a subject"),
                };
                let o = self.stored(txn, &tr.object);
                Term::Triple(Box::new(oxrdf::Triple::new(s, tr.predicate.clone(), o)))
            }
            // the labels inside a composite literal name blank nodes as the labels
            // outside it do. A label of no stored or minted node stays as it is.
            Term::Literal(l) => {
                super::cdt::relabel_literal(l, &mut |b| match crate::id::parse_bnode_payload(b) {
                    Some(p) => crate::id::bnode_label(self.id(txn, Id::bnode(p)).payload()),
                    None => b.to_string(),
                })
                .map_or_else(|| t.clone(), Term::Literal)
            }
            t => t.clone(),
        }
    }
}

fn bnode(txn: &mut WriteTxn<'_>, map: &mut FxHashMap<String, Id>, b: &BlankNode) -> Id {
    if let Some(id) = map.get(b.as_str()) {
        return *id;
    }
    let id = txn.new_bnode();
    map.insert(b.as_str().to_string(), id);
    id
}

fn load(
    txn: &mut WriteTxn<'_>,
    source: &NamedNode,
    dest: &GraphName,
    silent: bool,
    stats: &mut UpdateStats,
    req: &Request<'_>,
) -> Result<()> {
    let opts = req.opts;
    let url = source.as_str();
    let into = Into {
        graph: match dest {
            GraphName::NamedNode(n) => Some(n.clone()),
            GraphName::DefaultGraph => None,
        },
        base: url,
        silent,
    };
    if url.starts_with("file:") {
        if opts.forbid_file_load {
            return Err(Error::NotPermitted(
                "LOAD <file:…> requires server-admin".into(),
            ));
        }
        let path = opts.file_loads.check(url)?;
        let format = if crate::trix::is_path(&path) {
            LoadSyntax::TriX
        } else {
            LoadSyntax::Rdf(
                crate::io::format_for_path(&path)
                    .ok_or_else(|| {
                        Error::invalid(format!("cannot determine RDF format of {}", path.display()))
                    })?
                    .0,
            )
        };
        let mut f = std::fs::File::open(&path)?;
        let name = path.display().to_string();
        let (codec, head) = crate::io::sniff_codec(&mut f, Some(&path), &name)?;
        let r = codec.reader(std::io::Cursor::new(head).chain(f), None)?;
        return insert_parsed(txn, r, format, &name, &into, stats, req);
    }
    if opts.forbid_remote_load {
        return Err(Error::NotPermitted(
            "LOAD <http…> requires the federate permission".into(),
        ));
    }
    let policy = &opts.outbound;
    // within the update's deadline too
    let timeout = req.deadline.map_or(policy.timeout, |d| {
        d.saturating_duration_since(Instant::now())
    });
    let resp = policy
        .send(&req.budget, url, timeout, |client, u| {
            client.get(u).header(
                "Accept",
                "text/turtle, application/n-triples, application/n-quads, application/trig, \
                 application/rdf+xml, application/ld+json;q=0.9, application/trix+xml;q=0.8",
            )
        })
        .map_err(|f| {
            f.into_error(&format!("LOAD <{url}>"), |m| {
                Error::invalid(format!("LOAD {url}: {m}"))
            })
        })?;
    if !resp.status.is_success() {
        return Err(Error::invalid(format!("LOAD {url}: {}", resp.status)));
    }
    let ct = resp.content_type;
    let url_path = std::path::Path::new(url);
    let format = if crate::trix::is_media_type(&ct) {
        LoadSyntax::TriX
    } else if let Some(f) = crate::io::format_for_media_type(&ct) {
        LoadSyntax::Rdf(f)
    } else if crate::trix::is_path(url_path) {
        LoadSyntax::TriX
    } else {
        LoadSyntax::Rdf(
            crate::io::format_for_path(url_path)
                .map(|f| f.0)
                .ok_or_else(|| Error::invalid(format!("LOAD {url}: unknown content type {ct}")))?,
        )
    };
    let mut body = resp.body;
    let (codec, head) =
        crate::io::sniff_codec(&mut body, Some(url_path), url).map_err(|e| read_error(url, e))?;
    // compressed data counts in the request's budget once decompressed, and is held to
    // the response ceiling then too
    let (body, count) = if codec == crate::codec::Codec::None {
        (body, None)
    } else {
        let (body, budget) = body.uncounted();
        (body, Some(budget))
    };
    let r = codec.reader(
        std::io::Cursor::new(head).chain(body),
        Some(policy.max_response_bytes),
    )?;
    let r: Box<dyn Read> = match count {
        Some(budget) => Box::new(crate::outbound::Counted { inner: r, budget }),
        None => r,
    };
    insert_parsed(txn, r, format, url, &into, stats, req).map_err(|e| read_error(url, e))
}

/// The syntax of a `LOAD`ed document: one oxrdfio reads, or TriX.
#[derive(Clone, Copy)]
enum LoadSyntax {
    Rdf(RdfFormat),
    TriX,
}

/// Where the quads of a `LOAD` go.
struct Into<'a> {
    graph: Option<NamedNode>,
    base: &'a str,
    /// insert the quads once all of them parsed: a failed `LOAD SILENT` leaves nothing
    /// behind (a failed `LOAD` fails the whole request)
    silent: bool,
}

/// Parse `r` as it streams in and insert its quads.
fn insert_parsed(
    txn: &mut WriteTxn<'_>,
    r: impl Read,
    syntax: LoadSyntax,
    name: &str,
    into: &Into<'_>,
    stats: &mut UpdateStats,
    req: &Request<'_>,
) -> Result<()> {
    let mut labels = std::collections::HashMap::new();
    let mut held = Vec::new();
    let mut i = 0usize;
    let mut insert = |q: oxrdf::Quad| -> Result<()> {
        if i % 4096 == 4095 {
            req.check()?;
        }
        i += 1;
        let ids = txn.encode_quad(&q, &mut labels)?;
        if into.silent {
            held.push(ids);
        } else if txn.insert(ids)? {
            stats.inserted += 1;
        }
        Ok(())
    };
    match syntax {
        LoadSyntax::Rdf(format) => {
            let mut parser = RdfParser::from_format(format)
                .with_base_iri(into.base)
                .map_err(|e| Error::invalid(e.to_string()))?;
            if let Some(g) = &into.graph {
                parser = parser.with_default_graph(oxrdf::GraphName::NamedNode(g.clone()));
            }
            let r = crate::nesting::Guarded::rdf(r, format, name);
            for q in parser.for_reader(r) {
                insert(q.map_err(|e| match e {
                    RdfParseError::Io(e) => crate::codec::io_error(e),
                    RdfParseError::Syntax(e) => Error::RdfParse(format!("{name}: {e}")),
                })?)?;
            }
        }
        LoadSyntax::TriX => {
            let r = std::io::BufReader::new(r);
            crate::trix::parse(r, Some(into.base), |mut q| -> Result<()> {
                if let (Some(g), oxrdf::GraphName::DefaultGraph) = (&into.graph, &q.graph_name) {
                    q.graph_name = oxrdf::GraphName::NamedNode(g.clone());
                }
                insert(q)
            })
            .map_err(|e| match e {
                Error::RdfParse(m) => Error::RdfParse(format!("{name}: {m}")),
                e => e,
            })?;
        }
    }
    for ids in held {
        if txn.insert(ids)? {
            stats.inserted += 1;
        }
    }
    Ok(())
}

/// A failed read of a `LOAD <http…>` body: the body's own words (a timeout, the size
/// ceiling, a broken connection) with the URL.
fn read_error(url: &str, e: Error) -> Error {
    match e {
        Error::Io(e) => Error::invalid(format!("LOAD {url}: {e}")),
        e => e,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn data_only_updates() {
        let o = QueryOptions::default();
        assert!(data_only("INSERT DATA { <urn:a> <urn:p> 1 }", &o));
        assert!(data_only(
            "PREFIX ex: <urn:> DELETE DATA { ex:a ex:p 1 } ; INSERT DATA { GRAPH ex:g { ex:a ex:p 2 } }",
            &o
        ));
        assert!(!data_only("DELETE WHERE { ?s ?p ?o }", &o));
        assert!(!data_only(
            "INSERT { <urn:a> <urn:p> ?o } WHERE { ?s <urn:q> ?o }",
            &o
        ));
        assert!(!data_only("LOAD <http://example.org/data.ttl>", &o));
        assert!(!data_only("CLEAR ALL", &o));
        assert!(!data_only("INSERT DATA { <urn:a> <urn:p> ", &o));
        assert!(!data_only("", &o));
    }
}

#[cfg(test)]
mod protocol_dataset_tests {
    use crate::sparql::{QueryOptions, query, update::update};
    use crate::store::{Store, StoreOptions};

    fn ask(s: &Store, q: &str) -> bool {
        query(s.snapshot(), q, &QueryOptions::default())
            .unwrap()
            .boolean
    }

    #[test]
    fn using_graph_uri_is_the_using_of_each_modify() {
        let s = Store::in_memory(StoreOptions::default());
        let data = "INSERT DATA { <urn:a> <urn:p> 1 . GRAPH <urn:g1> { <urn:b> <urn:p> 2 } \
                    GRAPH <urn:g2> { <urn:c> <urn:p> 3 } }";
        update(&s, data, &QueryOptions::default()).unwrap();
        let copy = "INSERT { GRAPH <urn:out> { ?s <urn:p> ?o } } WHERE { ?s <urn:p> ?o }";
        let opts = QueryOptions {
            default_graph_uris: vec!["urn:g1".into()],
            ..Default::default()
        };
        update(&s, copy, &opts).unwrap();
        assert!(ask(&s, "ASK { GRAPH <urn:out> { <urn:b> <urn:p> 2 } }"));
        assert!(!ask(&s, "ASK { GRAPH <urn:out> { <urn:a> <urn:p> 1 } }"));
        // only named graphs: the default graph is empty
        let named = QueryOptions {
            named_graph_uris: vec!["urn:g2".into()],
            ..Default::default()
        };
        let both = "INSERT { GRAPH <urn:out2> { ?s <urn:p> ?o } } \
                    WHERE { { ?s <urn:p> ?o } UNION { GRAPH ?g { ?s <urn:p> ?o } } }";
        let r = update(&s, both, &named).unwrap();
        assert_eq!(r.inserted, 1);
        assert!(ask(&s, "ASK { GRAPH <urn:out2> { <urn:c> <urn:p> 3 } }"));
        // USING or WITH of the request's own is an error
        let with = "WITH <urn:g1> DELETE { ?s ?p ?o } WHERE { ?s ?p ?o }";
        let e = update(&s, with, &opts);
        assert!(matches!(e, Err(crate::error::Error::Invalid(_))), "{e:?}");
        let using = "INSERT { <urn:x> <urn:y> <urn:z> } USING <urn:g2> WHERE {}";
        let e = update(&s, using, &opts);
        assert!(matches!(e, Err(crate::error::Error::Invalid(_))), "{e:?}");
    }
}
