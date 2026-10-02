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
    let parsed = p.parse_update(u)?;
    let depth = super::depth::check_update(&parsed)?;
    super::depth::with_stack(depth, || run_update(store, &parsed, opts, kind, t0))
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
    for op in &parsed.operations {
        if let GraphUpdateOperation::DeleteInsert { pattern, .. } = op {
            crate::geo::validate_query(pattern, &mut |_| {})?;
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
    // the request's cancellation and deadline also end the wait for the writer lock
    // and the write guard
    let mut wopts = opts.write.clone();
    if wopts.cancel.is_none() {
        wopts.cancel = opts.cancel.clone();
    }
    if wopts.deadline.is_none() {
        wopts.deadline = req.deadline;
    }
    let mut txn = store.try_write_with(kind, wopts)?;
    for op in &parsed.operations {
        req.check()?;
        run_op(&mut txn, op, &req, &mut stats, store)?;
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
        if let Some(o) = self.opts.optimizations {
            ctx.opt = o;
        }
        ctx.base_iri = self.base.clone();
        ctx
    }
}

fn run_op(
    txn: &mut WriteTxn<'_>,
    op: &GraphUpdateOperation,
    req: &Request<'_>,
    stats: &mut UpdateStats,
    store: &Store,
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
                if let (Some(s), Some(p), Some(o), Some(g)) = (s, p, o, g)
                    && txn.delete([s, p, o, g])?
                {
                    stats.deleted += 1;
                }
            }
        }
        GraphUpdateOperation::DeleteInsert {
            delete,
            insert,
            using,
            pattern,
        } => {
            let snap = Arc::new(txn.view());
            let mut ctx = req.ctx(snap);
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
            let node = Planner::new(&ctx).plan(pattern, &ActiveGraph::Default, Vec::new())?;
            let (table, _) = super::exec::execute(&ctx, &node)?;
            stats.mem_peak_bytes = stats.mem_peak_bytes.max(ctx.mem_peak());
            let map = table.var_map(ctx.nvars());
            let get = |ctx: &Ctx, name: &str, i: usize| -> Option<Id> {
                let c = map.get(ctx.var(name) as usize).copied().flatten()?;
                let id = table.cols[c][i];
                (!id.is_undef()).then_some(id)
            };
            // resolve query-local ids into store ids
            let to_store = |txn: &mut WriteTxn<'_>, ctx: &Ctx, id: Id| -> Result<Option<Id>> {
                Ok(match id.tag() {
                    Tag::Local => match ctx.term(id) {
                        Some(t) => Some(txn.intern(&t)?),
                        None => None,
                    },
                    Tag::BNode if id.payload() & Id::LOCAL_BNODE_BIT != 0 => None,
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
                    let mut tp = |txn: &mut WriteTxn<'_>, t: &TermPattern| -> Result<Option<Id>> {
                        Ok(match t {
                            TermPattern::Variable(v) => match get(&ctx, v.as_str(), i) {
                                Some(id) => to_store(txn, &ctx, id)?,
                                None => None,
                            },
                            TermPattern::BlankNode(b) => Some(bnode(txn, &mut bnodes, b)),
                            TermPattern::NamedNode(n) => {
                                Some(txn.intern(&Term::NamedNode(n.clone()))?)
                            }
                            TermPattern::Literal(l) => Some(txn.intern(&Term::Literal(l.clone()))?),
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
                                    Some(term) => Some(txn.intern(&term)?),
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
                            Some(id) => to_store(txn, &ctx, id)?,
                            None => None,
                        },
                    };
                    let g = match &q.graph_name {
                        GraphNamePattern::DefaultGraph => Some(Id::DEFAULT_GRAPH),
                        GraphNamePattern::NamedNode(n) => {
                            Some(txn.intern(&Term::NamedNode(n.clone()))?)
                        }
                        GraphNamePattern::Variable(v) => match get(&ctx, v.as_str(), i) {
                            Some(id) => to_store(txn, &ctx, id)?,
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
                GraphTarget::NamedGraphs => view.graph_ids()?,
                GraphTarget::AllGraphs => {
                    let mut v = view.graph_ids()?;
                    v.push(Id::DEFAULT_GRAPH);
                    v
                }
            };
            for g in graphs {
                req.check()?;
                for k in view.scan_keys(Perm::Gspo, &[g.0])? {
                    if txn.delete(Perm::Gspo.to_quad(&k))? {
                        stats.deleted += 1;
                    }
                }
            }
        }
        GraphUpdateOperation::Create { .. } => {}
    }
    let _ = store;
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
        let (format, _) = crate::io::format_for_path(&path).ok_or_else(|| {
            Error::invalid(format!("cannot determine RDF format of {}", path.display()))
        })?;
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
                 application/rdf+xml, application/ld+json;q=0.9",
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
    let format = crate::io::format_for_media_type(&ct)
        .or_else(|| crate::io::format_for_path(url_path).map(|f| f.0))
        .ok_or_else(|| Error::invalid(format!("LOAD {url}: unknown content type {ct}")))?;
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
    format: RdfFormat,
    name: &str,
    into: &Into<'_>,
    stats: &mut UpdateStats,
    req: &Request<'_>,
) -> Result<()> {
    let mut parser = RdfParser::from_format(format)
        .with_base_iri(into.base)
        .map_err(|e| Error::invalid(e.to_string()))?;
    if let Some(g) = &into.graph {
        parser = parser.with_default_graph(oxrdf::GraphName::NamedNode(g.clone()));
    }
    let mut labels = std::collections::HashMap::new();
    let mut held = Vec::new();
    let r = crate::nesting::Guarded::rdf(r, format, name);
    for (i, q) in parser.for_reader(r).enumerate() {
        if i % 4096 == 4095 {
            req.check()?;
        }
        let q = q.map_err(|e| match e {
            RdfParseError::Io(e) => crate::codec::io_error(e),
            RdfParseError::Syntax(e) => Error::RdfParse(format!("{name}: {e}")),
        })?;
        let ids = txn.encode_quad(&q, &mut labels)?;
        if into.silent {
            held.push(ids);
        } else if txn.insert(ids)? {
            stats.inserted += 1;
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
