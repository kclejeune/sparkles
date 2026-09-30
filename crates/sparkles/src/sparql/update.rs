//! SPARQL 1.1 Update (ARQ `modify` equivalent). All operations of a request run in a
//! single write transaction and see the effects of the previous operations.

use super::ctx::Ctx;
use super::plan::{ActiveGraph, Planner};
use super::{QueryOptions, Timing};
use crate::error::{Error, Result};
use crate::id::{Id, Tag};
use crate::index::Perm;
use crate::io::Source;
use crate::store::{Store, WriteTxn};
use oxrdf::{BlankNode, NamedNode, Term};
use rustc_hash::FxHashMap;
use serde::Serialize;
use spargebra::algebra::{GraphTarget, QueryDataset};
use spargebra::term::{
    GraphName, GraphNamePattern, GroundTerm, GroundTermPattern, NamedNodePattern, TermPattern,
};
use spargebra::{GraphUpdateOperation, SparqlParser};
use std::sync::Arc;
use std::time::Instant;

#[derive(Debug, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdateStats {
    pub inserted: u64,
    pub deleted: u64,
    pub operations: usize,
    pub timing: Timing,
}

pub fn update(store: &Store, u: &str, opts: &QueryOptions) -> Result<UpdateStats> {
    let t0 = Instant::now();
    let mut p = SparqlParser::new();
    if let Some(b) = &opts.base_iri {
        p = p.with_base_iri(b).map_err(|e| Error::invalid(e.to_string()))?;
    }
    for (k, v) in &opts.prefixes {
        p = p.with_prefix(k, v).map_err(|e| Error::invalid(e.to_string()))?;
    }
    let parsed = p.parse_update(u)?;
    let parse_ms = t0.elapsed().as_secs_f64() * 1000.0;
    let mut stats = UpdateStats {
        operations: parsed.operations.len(),
        ..Default::default()
    };
    let t1 = Instant::now();
    let mut txn = store.write();
    for op in &parsed.operations {
        run_op(&mut txn, op, opts, &mut stats, store)?;
    }
    txn.commit()?;
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

fn graph_id(txn: &mut WriteTxn<'_>, g: &GraphName) -> Result<Id> {
    Ok(match g {
        GraphName::DefaultGraph => Id::DEFAULT_GRAPH,
        GraphName::NamedNode(n) => txn.intern(&Term::NamedNode(n.clone()))?,
    })
}

fn run_op(txn: &mut WriteTxn<'_>, op: &GraphUpdateOperation, opts: &QueryOptions, stats: &mut UpdateStats, store: &Store) -> Result<()> {
    match op {
        GraphUpdateOperation::InsertData { data } => {
            let mut bnodes: FxHashMap<String, Id> = FxHashMap::default();
            for q in data {
                let s = match &q.subject {
                    oxrdf::NamedOrBlankNode::NamedNode(n) => txn.intern(&Term::NamedNode(n.clone()))?,
                    oxrdf::NamedOrBlankNode::BlankNode(b) => bnode(txn, &mut bnodes, b),
                };
                let p = txn.intern(&Term::NamedNode(q.predicate.clone()))?;
                let o = match &q.object {
                    Term::BlankNode(b) => bnode(txn, &mut bnodes, b),
                    t => txn.intern(t)?,
                };
                let g = graph_id(txn, &q.graph_name)?;
                if txn.insert([s, p, o, g])? {
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
                    #[allow(unreachable_patterns)]
                    _ => None,
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
        GraphUpdateOperation::DeleteInsert { delete, insert, using, pattern } => {
            let snap = Arc::new(txn.view());
            let mut ctx = Ctx::new(snap);
            if let Some(QueryDataset { default, named }) = using {
                ctx.dataset.default = Some(default.iter().map(|n| ctx.intern_term(&Term::NamedNode(n.clone()))).collect());
                ctx.dataset.named = Some(
                    named.iter().flatten().map(|n| ctx.intern_term(&Term::NamedNode(n.clone()))).collect(),
                );
            }
            ctx.allow_service = opts.allow_service;
            let node = Planner::new(&ctx).plan(pattern, &ActiveGraph::Default, Vec::new())?;
            let (table, _) = super::exec::execute(&ctx, &node)?;
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
                            GroundTermPattern::NamedNode(n) => ctx.snap.lookup_term(&Term::NamedNode(n.clone())),
                            GroundTermPattern::Literal(l) => ctx.snap.lookup_term(&Term::Literal(l.clone())),
                            #[allow(unreachable_patterns)]
                            _ => None,
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
                            TermPattern::NamedNode(n) => Some(txn.intern(&Term::NamedNode(n.clone()))?),
                            TermPattern::Literal(l) => Some(txn.intern(&Term::Literal(l.clone()))?),
                            #[allow(unreachable_patterns)]
                            _ => None,
                        })
                    };
                    let s = tp(txn, &q.subject)?;
                    let o = tp(txn, &q.object)?;
                    let p = match &q.predicate {
                        NamedNodePattern::NamedNode(n) => Some(txn.intern(&Term::NamedNode(n.clone()))?),
                        NamedNodePattern::Variable(v) => match get(&ctx, v.as_str(), i) {
                            Some(id) => to_store(txn, &ctx, id)?,
                            None => None,
                        },
                    };
                    let g = match &q.graph_name {
                        GraphNamePattern::DefaultGraph => Some(Id::DEFAULT_GRAPH),
                        GraphNamePattern::NamedNode(n) => Some(txn.intern(&Term::NamedNode(n.clone()))?),
                        GraphNamePattern::Variable(v) => match get(&ctx, v.as_str(), i) {
                            Some(id) => to_store(txn, &ctx, id)?,
                            None => None,
                        },
                    };
                    let (Some(s), Some(p), Some(o), Some(g)) = (s, p, o, g) else { continue };
                    // well-formedness: subject not a literal, predicate an IRI
                    let view = &ctx;
                    if view.kind(s) == super::ctx::TermKind::Literal
                        || view.kind(p) != super::ctx::TermKind::Iri && p.tag() != Tag::Delta
                    {
                        continue;
                    }
                    ins.push([s, p, o, g]);
                }
            }
            for q in dels {
                if txn.delete(q)? {
                    stats.deleted += 1;
                }
            }
            for q in ins {
                if txn.insert(q)? {
                    stats.inserted += 1;
                }
            }
        }
        GraphUpdateOperation::Load { silent, source, destination } => {
            let r = load(txn, source, destination, stats);
            if r.is_err() && !silent {
                return r;
            }
        }
        GraphUpdateOperation::Clear { graph, .. } | GraphUpdateOperation::Drop { graph, .. } => {
            let view = txn.view();
            let graphs: Vec<Id> = match graph {
                GraphTarget::DefaultGraph => vec![Id::DEFAULT_GRAPH],
                GraphTarget::NamedNode(n) => view.lookup_term(&Term::NamedNode(n.clone())).into_iter().collect(),
                GraphTarget::NamedGraphs => view.graph_ids()?,
                GraphTarget::AllGraphs => {
                    let mut v = view.graph_ids()?;
                    v.push(Id::DEFAULT_GRAPH);
                    v
                }
            };
            for g in graphs {
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

fn named_pat(ctx: &Ctx, p: &NamedNodePattern, get: impl Fn(&str) -> Option<Id>, lookup: bool) -> Option<Id> {
    match p {
        NamedNodePattern::NamedNode(n) if lookup => ctx.snap.lookup_term(&Term::NamedNode(n.clone())),
        NamedNodePattern::NamedNode(n) => Some(ctx.intern_term(&Term::NamedNode(n.clone()))),
        NamedNodePattern::Variable(v) => get(v.as_str()),
    }
}

fn graph_pat(ctx: &Ctx, g: &GraphNamePattern, get: impl Fn(&str) -> Option<Id>, lookup: bool) -> Option<Id> {
    match g {
        GraphNamePattern::DefaultGraph => Some(Id::DEFAULT_GRAPH),
        GraphNamePattern::NamedNode(n) if lookup => ctx.snap.lookup_term(&Term::NamedNode(n.clone())),
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

fn load(txn: &mut WriteTxn<'_>, source: &NamedNode, dest: &GraphName, stats: &mut UpdateStats) -> Result<()> {
    let url = source.as_str();
    let graph = match dest {
        GraphName::NamedNode(n) => Some(n.clone()),
        GraphName::DefaultGraph => None,
    };
    let src = if let Some(path) = url.strip_prefix("file://") {
        let mut s = Source::from_path(std::path::Path::new(path), graph)?;
        s.base = Some(url.to_string());
        s
    } else {
        let resp = reqwest::blocking::Client::new()
            .get(url)
            .header("Accept", "text/turtle, application/n-triples, application/n-quads, application/trig, application/rdf+xml, application/ld+json;q=0.9")
            .send()
            .map_err(|e| Error::invalid(format!("LOAD {url}: {e}")))?;
        let ct = resp.headers().get("content-type").and_then(|v| v.to_str().ok()).unwrap_or("").to_string();
        let format = crate::io::format_for_media_type(&ct)
            .or_else(|| crate::io::format_for_path(std::path::Path::new(url)).map(|f| f.0))
            .ok_or_else(|| Error::invalid(format!("LOAD {url}: unknown content type {ct}")))?;
        let body = resp.bytes().map_err(|e| Error::invalid(e.to_string()))?.to_vec();
        let mut s = Source::from_bytes(body, format, graph);
        s.base = Some(url.to_string());
        s
    };
    let (quads, _) = crate::io::parse_to_vec(&src)?;
    let mut labels = std::collections::HashMap::new();
    for q in &quads {
        let ids = txn.encode_quad(q, &mut labels)?;
        if txn.insert(ids)? {
            stats.inserted += 1;
        }
    }
    Ok(())
}
