//! `ORDER BY DESC(spk:cosine(?v, C)) LIMIT k` over one triple pattern `?s <p> ?v`, with
//! `spk:dot` alike, planned as an exact vector search of the `k` best rows (spec F04,
//! Phase 3).
//!
//! The order stays above the search and sorts its rows again, so the answer is the
//! generic plan's, up to the choice among rows tied at the `k`-th score. The search is
//! exact: it reads the predicate's packed vectors, never the HNSW graph, as the generic
//! plan reads every row. A row whose vector does not parse, or has another dimension,
//! gets an error from the function and sorts last in a descending order. Such rows
//! matter only when fewer than `k` rows have a score, and then the generic plan runs
//! instead (the search node's child). An ascending order, which puts those rows first,
//! and `spk:euclidean`, whose nearest rows come first in an ascending order, keep the
//! generic plan.

use super::ctx::Ctx;
use super::expr::{Expr, Func};
use super::plan::{GraphFilter, Kind, Node, PathEnd, ScanSpec, VectorQuery, VectorSpec};
use super::table::VarId;
use crate::id::Id;
use crate::vector::{self, Metric};

/// The ORDER BY with LIMIT `n`, its child rewritten when it is the shape above.
pub(super) fn vector_topk(n: Node, ctx: &Ctx) -> Node {
    match rewrite(&n, ctx) {
        Some(child) => {
            let mut n = n;
            n.cost += child.cost - n.children[0].cost;
            n.children[0] = child;
            n
        }
        None => n,
    }
}

fn rewrite(n: &Node, ctx: &Ctx) -> Option<Node> {
    let Kind::OrderBy {
        keys,
        limit: Some(k),
    } = &n.kind
    else {
        return None;
    };
    if !ctx.opt.vector_topk || keys.len() != 1 || keys[0].1 || !(1..=vector::MAX_K).contains(k) {
        return None;
    }
    // BINDs (which drop no row) over one scan
    let mut chain = Vec::new();
    let mut core = &n.children[0];
    while let Kind::Extend(..) = core.kind {
        chain.push(core);
        core = &core.children[0];
    }
    let Kind::Scan(scan) = &core.kind else {
        return None;
    };
    let key = match &keys[0].0 {
        Expr::Var(d) => chain.iter().find_map(|c| match &c.kind {
            Kind::Extend(v, e) if v == d => Some(e),
            _ => None,
        })?,
        e => e,
    };
    let (metric, v, query) = vector_call(key, ctx)?;
    let (s, p, o) = scan_triple(scan)?;
    // a constant subject would be applied after the search's top k
    if !matches!(o, PathEnd::Var(x) if x == v) || !matches!(s, PathEnd::Var(x) if x != v) {
        return None;
    }
    let PathEnd::Const(pred) = p else {
        return None;
    };
    // an index of another dimension refuses the query: keep the generic plan
    if let Some((_, c)) = ctx
        .snap
        .generation
        .vectors
        .configured_for(&ctx.snap, pred.0)
        && c.dimension != query.len()
    {
        return None;
    }
    let vars: Vec<VarId> = core.vars.clone();
    let spec = VectorSpec {
        pred: Some(pred),
        query: VectorQuery::Vector(query.clone().into()),
        k: *k,
        metric,
        subject: s,
        score: None,
        vector: Some(v),
        graph: scan.graph.clone(),
        graph_var: None,
        dedup: scan.graph.multi(),
        mode: vector::SearchMode {
            exact: true,
            ef: None,
        },
        distinct_subject: false,
        candidates: false,
        order_fallback: true,
    };
    let desc = format!(
        "{} ← {} {} k={k} dim={} exact, the best rows of ORDER BY",
        vars.iter()
            .map(|v| format!("?{}", ctx.var_name(*v)))
            .collect::<Vec<_>>()
            .join(" "),
        ctx.term(pred).map_or_else(|| "?".into(), |t| t.to_string()),
        metric.name(),
        query.len(),
    );
    let mut search = Node::leaf(Kind::VectorSearch(Box::new(spec)), vars, *k as f64, desc);
    search.cost = core.est.max(1.0);
    search.children.push(core.clone());
    // the BINDs above the search again
    let mut out = search;
    for c in chain.into_iter().rev() {
        let mut c = c.clone();
        c.est = out.est;
        c.cost = out.cost + out.est;
        c.children[0] = out;
        out = c;
    }
    Some(out)
}

/// `spk:cosine` or `spk:dot` of a variable and a constant vector, in either order.
fn vector_call(e: &Expr, ctx: &Ctx) -> Option<(Metric, VarId, Vec<f32>)> {
    let Expr::Call(Func::Ext(iri), args) = e else {
        return None;
    };
    let metric = match iri.strip_prefix(vector::NS)? {
        "cosine" => Metric::Cosine,
        "dot" => Metric::Dot,
        _ => return None,
    };
    let [a, b] = args.as_slice() else {
        return None;
    };
    let (v, c) = match (a, b) {
        (Expr::Var(v), c) | (c, Expr::Var(v)) => (*v, c),
        _ => return None,
    };
    let id = match c {
        Expr::Const(id) | Expr::Lit(id, _) => *id,
        _ => return None,
    };
    let Some(oxrdf::Term::Literal(l)) = ctx.term(id) else {
        return None;
    };
    if l.datatype().as_str() != vector::DATATYPE {
        return None;
    }
    let q = vector::parse(l.value()).ok()?;
    // a zero vector has no cosine: every row's key is an error
    if metric == Metric::Cosine && vector::norm(&q) == 0.0 {
        return None;
    }
    Some((metric, v, q))
}

/// The subject, predicate and object of a scan of one triple pattern without a graph
/// variable or a repeated variable.
fn scan_triple(scan: &ScanSpec) -> Option<(PathEnd, PathEnd, PathEnd)> {
    use crate::index::{G, O, P, S};
    if !scan.eqs.is_empty() {
        return None;
    }
    let at = |c: usize| -> Option<PathEnd> {
        let col = scan.perm.col_of(c);
        if col < scan.prefix.len() {
            return Some(PathEnd::Const(Id(scan.prefix[col])));
        }
        scan.cols
            .iter()
            .find(|(k, _)| *k == col)
            .map(|(_, v)| PathEnd::Var(*v))
    };
    // a graph bound by the scan's prefix or output is not the active graph's filter
    let gcol = scan.perm.col_of(G);
    if gcol < scan.prefix.len() || scan.cols.iter().any(|(k, _)| *k == gcol) {
        return None;
    }
    if matches!(scan.graph, GraphFilter::All) {
        // every graph without merging: duplicates per graph are separate rows
        return None;
    }
    Some((at(S)?, at(P)?, at(O)?))
}
