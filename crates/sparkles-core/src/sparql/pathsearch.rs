//! Path search: paths as solutions (spec F07).
//!
//! ```sparql
//! PREFIX path: <urn:x-sparkles:path#>
//! SERVICE path:search {
//!   [] path:source ?a ; path:target ?b ; path:predicate foaf:knows ;
//!      path:algorithm path:allShortest ; path:maxLength 6 ;
//!      path:pathIndex ?path ; path:edgeIndex ?i ;
//!      path:edgeSubject ?s ; path:edgePredicate ?p ; path:edgeObject ?o .
//! }
//! ```
//!
//! The call is planned as a `PathSearch` leaf. When its source or target is a variable
//! the leaf reads the rest of its join group (child 0) and extends each of its rows with
//! the paths of that row's (source, target) pair. Paths are simple: no node repeats,
//! except that the last node may be the first (a cycle). Edges are read from the
//! permutations directly: `PSO`/`POS` per predicate, or `SPO`/`OSP` for every predicate.
//! A predicate can have a direction of its own, as in `path:predicate (ex:p
//! path:backward)`. Edges can instead come from a nested pattern in the block, whose
//! solutions bind `path:start` and `path:end` (the leaf's last child).
//! `path:excludePredicate` drops the triples of some predicates from the edges of every
//! predicate as they are read, and `path:maxDegree` keeps a search from expanding a node
//! with more edges in the call's direction than the limit, unless it is the search's root,
//! so no path passes through such a hub (spec U09 Phase 1, F07 §4.9). The degree comes
//! from the index's counts of the node's ranges when every quad of the store is in the
//! active graph, and otherwise from reading the node's edges up to one past the limit.
//!
//! The searches: bidirectional breadth-first search for one unweighted pair, one
//! breadth-first search per source (or per target) for several or unbound ends,
//! Dijkstra's algorithm with weights (from both ends for one pair and for Yen's spurs,
//! and Pareto-pruned over `(node, length)` pairs when `path:maxLength` is set), and a
//! depth-first enumeration pruned by the distance to the targets for all paths.

use super::ctx::{Charge, Ctx};
use super::plan::{ActiveGraph, GraphFilter, Kind, Node, PT, PathEnd, Planner};
use super::table::{Table, VarId};
use super::value::Value;
use crate::error::{Error, Result};
use crate::id::Id;
use crate::index::{Perm, pad};
use crate::store::Chunk;
use oxrdf::Term;
use rustc_hash::{FxBuildHasher, FxHashMap, FxHashSet};
use spargebra::algebra::GraphPattern;
use spargebra::term::{NamedNodePattern, TermPattern, TriplePattern};
use sparkles_budget::sizes::vec_grown;
use std::cell::{Cell, RefCell};
use std::cmp::{Ordering, Reverse};
use std::collections::{BTreeMap, BTreeSet, BinaryHeap};
use std::rc::Rc;

/// The namespace of the parameters (`PREFIX path: <urn:x-sparkles:path#>`).
pub const NS: &str = "urn:x-sparkles:path#";
/// The SERVICE IRI of a path search.
pub const SEARCH: &str = "urn:x-sparkles:path#search";
const RDF_REIFIES: &str = "http://www.w3.org/1999/02/22-rdf-syntax-ns#reifies";
/// The nodes one search may visit without `path:maxVisited`.
pub const DEFAULT_MAX_VISITED: u64 = 10_000_000;
/// The id that stands for the source as the last node of a cycle (or for the target as
/// the first node of one, in a backward search). No stored id is all ones.
const VIRT: u64 = u64::MAX;
/// A frontier at least `rows / SWEEP_RATIO` long is expanded by one pass over the
/// predicate's rows instead of one seek per node (as in the transitive path operator).
const SWEEP_RATIO: u64 = 512;
/// Estimated bytes per visited node (map entry and parent) and per cached neighbour.
const NODE_BYTES: u64 = 48;
const STEP_BYTES: u64 = 24;
/// Estimated bytes per entry of the settling-order map `path:allShortest` builds, a
/// `u64` key and a `u32` rank padded to 16 bytes, with the table's control byte and
/// room to grow.
const RANK_BYTES: u64 = 32;
/// The ratio of the largest to the smallest positive weight up to which the search for
/// every cheapest path from both ends adds costs exactly.
const WIDE_WEIGHTS: f64 = (1u64 << 28) as f64;
/// Estimated bytes per node whose degree `path:maxDegree` checked (a map entry).
const HUB_BYTES: u64 = 16;

/// Whether a SERVICE name is the path search.
pub fn is_search(name: &NamedNodePattern) -> bool {
    matches!(name, NamedNodePattern::NamedNode(n) if n.as_str() == SEARCH)
}

fn bad(m: impl std::fmt::Display) -> Error {
    Error::invalid(format!("path:search: {m}"))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Algorithm {
    Shortest,
    AllShortest,
    KShortest,
    All,
}

impl Algorithm {
    fn name(self) -> &'static str {
        match self {
            Algorithm::Shortest => "shortest",
            Algorithm::AllShortest => "allShortest",
            Algorithm::KShortest => "kShortest",
            Algorithm::All => "all",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Direction {
    Forward,
    Backward,
    Both,
}

/// A planned path search.
#[derive(Clone, Debug)]
pub struct PathSearchSpec {
    pub algorithm: Algorithm,
    pub source: PathEnd,
    pub target: PathEnd,
    /// predicate ids; `None` means every predicate (an empty list: no edges)
    pub predicates: Option<Vec<u64>>,
    /// the direction of each predicate of `predicates` (in the same order)
    pub directions: Vec<Direction>,
    /// the direction of every predicate, and of the edges of a nested pattern
    pub direction: Direction,
    /// predicate ids whose triples are not edges (`path:excludePredicate`), sorted; only
    /// with every predicate
    pub exclude: Vec<u64>,
    /// a node whose degree in the call's direction is above this is not expanded, unless
    /// it is the root of a search (`path:maxDegree`)
    pub max_degree: Option<u64>,
    /// edges from a nested pattern (the leaf's last child): the variables of an edge's
    /// start and end
    pub edge_pattern: Option<(VarId, VarId)>,
    pub min_len: u32,
    pub max_len: Option<u32>,
    /// paths per pair in `kShortest`
    pub k: usize,
    /// paths of the whole call
    pub limit: Option<usize>,
    pub max_visited: u64,
    /// the weight property: `Some(None)` when the store does not have it
    pub weight: Option<Option<u64>>,
    pub default_weight: f64,
    /// `rdf:reifies`, if the store has it
    pub reifies: Option<u64>,
    pub path_index: Option<VarId>,
    pub edge_index: Option<VarId>,
    pub edge_s: Option<VarId>,
    pub edge_p: Option<VarId>,
    pub edge_o: Option<VarId>,
    /// the edge as a triple term `<<( s p o )>>`
    pub edge: Option<VarId>,
    pub length: Option<VarId>,
    pub cost: Option<VarId>,
    pub graph: GraphFilter,
}

impl PathSearchSpec {
    /// Whether the search reads the rest of its group: a variable source or target.
    pub fn needs_input(&self) -> bool {
        matches!(self.source, PathEnd::Var(_)) || matches!(self.target, PathEnd::Var(_))
    }

    /// One row per edge (an edge variable is asked for), else one row per path.
    fn per_edge(&self) -> bool {
        self.edge_index.is_some()
            || self.edge_s.is_some()
            || self.edge_p.is_some()
            || self.edge_o.is_some()
            || self.edge.is_some()
    }

    fn weighted(&self) -> bool {
        self.weight.is_some()
    }
}

// ----------------------------------------------------------------- planning ------

/// The parts of a search's block: the configuration triples (those with a `path:`
/// predicate, list arguments taken out), and the rest, the pattern of the edges if any.
pub(super) fn split_block(
    inner: &GraphPattern,
    ns: &str,
    what: &str,
) -> Result<(Vec<TriplePattern>, Option<GraphPattern>)> {
    // a FILTER of the block filters the pattern of the edges
    if let GraphPattern::Filter { expr, inner } = inner {
        let (config, pattern) = split_block(inner, ns, what)?;
        let Some(pattern) = pattern else {
            return Err(bad(
                "a FILTER in the block needs a nested pattern of edges to filter",
            ));
        };
        let filtered = GraphPattern::Filter {
            expr: expr.clone(),
            inner: Box::new(pattern),
        };
        return Ok((config, Some(filtered)));
    }
    fn parts<'g>(gp: &'g GraphPattern, out: &mut Vec<&'g GraphPattern>) {
        match gp {
            GraphPattern::Join { left, right } => {
                parts(left, out);
                parts(right, out);
            }
            _ => out.push(gp),
        }
    }
    let mut all = Vec::new();
    parts(inner, &mut all);
    let mut config = Vec::new();
    let mut rest: Vec<GraphPattern> = Vec::new();
    for gp in all {
        let GraphPattern::Bgp { patterns } = gp else {
            rest.push(gp.clone());
            continue;
        };
        let mut patterns = patterns.clone();
        let is_param = |t: &TriplePattern| matches!(&t.predicate, NamedNodePattern::NamedNode(n) if n.as_str().starts_with(ns));
        // a list argument's rdf:first / rdf:rest triples are part of its parameter
        let heads: Vec<TermPattern> = patterns
            .iter()
            .filter(|t| is_param(t))
            .map(|t| t.object.clone())
            .filter(|o| matches!(o, TermPattern::BlankNode(_)))
            .collect();
        let mut lists = Vec::new();
        for h in heads {
            if let Some(items) = super::textpf::take_list(&mut patterns, &h, what)? {
                lists.push((h, items));
            }
        }
        let mut edges = Vec::new();
        for t in patterns {
            if is_param(&t) {
                let o = lists
                    .iter()
                    .find(|(h, _)| *h == t.object)
                    .map(|(_, items)| items);
                config.push(match o {
                    // the list, as one object the parameter decodes
                    Some(items) => TriplePattern {
                        object: list_term(items),
                        ..t
                    },
                    None => t,
                });
            } else {
                edges.push(t);
            }
        }
        if !edges.is_empty() {
            rest.push(GraphPattern::Bgp { patterns: edges });
        }
    }
    let pattern = rest.into_iter().reduce(|l, r| GraphPattern::Join {
        left: Box::new(l),
        right: Box::new(r),
    });
    Ok((config, pattern))
}

/// A list argument kept in one term until its parameter decodes it: a triple term whose
/// subject is a marker blank node, predicate `rdf:first` and object the first element,
/// nested in the subject for the next elements.
fn list_term(items: &[TermPattern]) -> TermPattern {
    let mut t = TermPattern::BlankNode(spargebra::term::BlankNode::new_unchecked(LIST_END));
    for item in items.iter().rev() {
        t = TermPattern::Triple(Box::new(TriplePattern {
            subject: t,
            predicate: NamedNodePattern::NamedNode(oxrdf::vocab::rdf::FIRST.into_owned()),
            object: item.clone(),
        }));
    }
    t
}

/// The elements of a list kept by [`list_term`], or `None` when `t` is not one.
pub(super) fn list_items(t: &TermPattern) -> Option<Vec<TermPattern>> {
    let mut items = Vec::new();
    let mut t = t;
    loop {
        match t {
            TermPattern::BlankNode(b) if b.as_str() == LIST_END => return Some(items),
            TermPattern::Triple(tp) if matches!(&tp.predicate, NamedNodePattern::NamedNode(n) if *n == oxrdf::vocab::rdf::FIRST) =>
            {
                items.push(tp.object.clone());
                t = &tp.subject;
            }
            _ => return None,
        }
    }
}

/// The label of the marker that ends a list kept by [`list_term`].
const LIST_END: &str = "sparklesPathListEnd";

/// A `SERVICE path:search { … }` call as a leaf (with its nested pattern of edges as its
/// child).
pub(super) fn path_search_leaf(
    p: &mut Planner<'_>,
    inner: &GraphPattern,
    g: &ActiveGraph,
) -> Result<Node> {
    let (patterns, edge_gp) = split_block(inner, NS, "path:search")?;
    let patterns = &patterns;
    if patterns.is_empty() {
        return Err(bad("missing path:source and path:target"));
    }
    let subject = &patterns[0].subject;
    let ctx = p.ctx;
    let mut algorithm = None;
    let (mut source, mut target) = (None, None);
    let mut preds: Option<Vec<u64>> = None;
    let mut dirs: Vec<Option<Direction>> = Vec::new();
    let mut pred_names = Vec::new();
    let mut exclude: Vec<u64> = Vec::new();
    let mut exclude_names: Vec<String> = Vec::new();
    let mut max_degree = None;
    let mut direction = None;
    let (mut start, mut end_var) = (None, None);
    let (mut min_len, mut max_len, mut k, mut limit, mut max_visited) =
        (None, None, None, None, None);
    let (mut weight, mut default_weight) = (None, None);
    let mut outs: [Option<VarId>; 8] = [None; 8];
    const OUT_NAMES: [&str; 8] = [
        "pathIndex",
        "edgeIndex",
        "edgeSubject",
        "edgePredicate",
        "edgeObject",
        "length",
        "cost",
        "edge",
    ];
    fn once<T>(slot: &mut Option<T>, v: T, name: &str) -> Result<()> {
        if slot.replace(v).is_some() {
            return Err(bad(format!("path:{name} is given twice")));
        }
        Ok(())
    }
    let direction_of = |t: &TermPattern, name: &str| -> Result<Direction> {
        Ok(match t {
            TermPattern::NamedNode(n) => match n.as_str().strip_prefix(NS) {
                Some("forward") => Direction::Forward,
                Some("backward") => Direction::Backward,
                Some("both") => Direction::Both,
                _ => {
                    return Err(bad(format!(
                        "unknown direction {n} in path:{name} (path:forward, path:backward or path:both)"
                    )));
                }
            },
            _ => return Err(bad(format!("path:{name} takes a path: IRI"))),
        })
    };
    let iri_in_ns = |t: &TermPattern, name: &str| -> Result<String> {
        match t {
            TermPattern::NamedNode(n) if n.as_str().starts_with(NS) => {
                Ok(n.as_str()[NS.len()..].to_string())
            }
            _ => Err(bad(format!("path:{name} takes a path: IRI"))),
        }
    };
    let integer = |t: &TermPattern, name: &str| -> Result<u64> {
        match t {
            TermPattern::Literal(l)
                if l.datatype()
                    .as_str()
                    .starts_with("http://www.w3.org/2001/XMLSchema#")
                    && l.datatype() != oxrdf::vocab::xsd::STRING =>
            {
                l.value()
                    .trim()
                    .parse::<u64>()
                    .map_err(|_| bad(format!("path:{name} takes a non-negative integer")))
            }
            TermPattern::Variable(_) | TermPattern::BlankNode(_) => {
                Err(bad(format!("path:{name} must be a constant")))
            }
            _ => Err(bad(format!("path:{name} takes a non-negative integer"))),
        }
    };
    let output = |t: &TermPattern, name: &str| -> Result<VarId> {
        match t {
            TermPattern::Variable(_) | TermPattern::BlankNode(_) => match p.term_pattern(t) {
                PT::V(v) => Ok(v),
                PT::C(_) => Err(bad(format!("path:{name} must be an unbound variable"))),
            },
            _ => Err(bad(format!("path:{name} must be a variable"))),
        }
    };
    for tp in patterns {
        if tp.subject != *subject {
            return Err(bad(
                "the configuration triples must share one subject (one search per SERVICE)",
            ));
        }
        let NamedNodePattern::NamedNode(pn) = &tp.predicate else {
            return Err(bad("a parameter must be a path: IRI, not a variable"));
        };
        let Some(name) = pn.as_str().strip_prefix(NS) else {
            return Err(bad(format!("unknown parameter <{}>", pn.as_str())));
        };
        let o = &tp.object;
        match name {
            "source" | "target" => {
                let end = match o {
                    TermPattern::Triple(_) => return Err(bad(format!("path:{name} takes a term"))),
                    _ => match p.term_pattern(o) {
                        PT::C(id) => PathEnd::Const(id),
                        PT::V(v) => PathEnd::Var(v),
                    },
                };
                once(
                    if name == "source" {
                        &mut source
                    } else {
                        &mut target
                    },
                    end,
                    name,
                )?;
            }
            "algorithm" => {
                let a = match iri_in_ns(o, name)?.as_str() {
                    "shortest" => Algorithm::Shortest,
                    "allShortest" => Algorithm::AllShortest,
                    "kShortest" => Algorithm::KShortest,
                    "all" => Algorithm::All,
                    other => {
                        return Err(bad(format!(
                            "unknown algorithm path:{other} (path:shortest, path:allShortest, path:kShortest or path:all)"
                        )));
                    }
                };
                once(&mut algorithm, a, name)?;
            }
            "direction" => {
                let d = match iri_in_ns(o, name)?.as_str() {
                    "forward" => Direction::Forward,
                    "backward" => Direction::Backward,
                    "both" => Direction::Both,
                    other => {
                        return Err(bad(format!(
                            "unknown direction path:{other} (path:forward, path:backward or path:both)"
                        )));
                    }
                };
                once(&mut direction, d, name)?;
            }
            "predicate" => {
                // an IRI, or a list of an IRI and its own direction
                let (n, d) = match (o, list_items(o)) {
                    (TermPattern::NamedNode(n), _) => (n.clone(), None),
                    (_, Some(items)) => match items.as_slice() {
                        [TermPattern::NamedNode(n), d] => (n.clone(), Some(direction_of(d, name)?)),
                        _ => {
                            return Err(bad(
                                "path:predicate takes an IRI, or a list of an IRI and a direction such as (ex:p path:backward)",
                            ));
                        }
                    },
                    _ => return Err(bad("path:predicate takes an IRI")),
                };
                if pred_names.iter().any(|(x, _)| x == n.as_str()) {
                    return Err(bad(format!("path:predicate {n} is given twice")));
                }
                pred_names.push((n.as_str().to_string(), d));
                let list = preds.get_or_insert_with(Vec::new);
                if let Some(id) = ctx.snap.lookup_iri(n.as_str())
                    && !list.contains(&id.0)
                {
                    list.push(id.0);
                    dirs.push(d);
                }
            }
            "excludePredicate" => {
                let TermPattern::NamedNode(n) = o else {
                    return Err(bad("path:excludePredicate takes an IRI"));
                };
                if exclude_names.iter().any(|x| x == n.as_str()) {
                    return Err(bad(format!("path:excludePredicate {n} is given twice")));
                }
                exclude_names.push(n.as_str().to_string());
                // a predicate the store does not have has no triples to skip
                if let Some(id) = ctx.snap.lookup_iri(n.as_str()) {
                    exclude.push(id.0);
                }
            }
            "maxDegree" => once(&mut max_degree, integer(o, name)?, name)?,
            "start" => once(&mut start, output(o, name)?, name)?,
            "end" => once(&mut end_var, output(o, name)?, name)?,
            "minLength" => once(&mut min_len, integer(o, name)?, name)?,
            "maxLength" => once(&mut max_len, integer(o, name)?, name)?,
            "k" => once(&mut k, integer(o, name)?, name)?,
            "limit" => once(&mut limit, integer(o, name)?, name)?,
            "maxVisited" => once(&mut max_visited, integer(o, name)?, name)?,
            "weight" => {
                let TermPattern::NamedNode(n) = o else {
                    return Err(bad("path:weight takes an IRI"));
                };
                once(
                    &mut weight,
                    (n.as_str().to_string(), ctx.snap.lookup_iri(n.as_str())),
                    name,
                )?;
            }
            "defaultWeight" => {
                let w = match o {
                    TermPattern::Literal(l) => super::value::approx_f64(&Value::from_literal(l)),
                    _ => None,
                };
                match w {
                    Some(w) if w >= 0.0 && w.is_finite() => once(&mut default_weight, w, name)?,
                    _ => return Err(bad("path:defaultWeight takes a non-negative number")),
                }
            }
            _ => {
                let Some(i) = OUT_NAMES.iter().position(|n| *n == name) else {
                    return Err(bad(format!("unknown parameter path:{name}")));
                };
                once(&mut outs[i], output(o, name)?, name)?;
            }
        }
    }
    let source = source.ok_or_else(|| bad("missing path:source"))?;
    let target = target.ok_or_else(|| bad("missing path:target"))?;
    // edges from a nested pattern: its variables of an edge's ends, no predicates
    let edge_pattern = match (&edge_gp, start, end_var) {
        (None, None, None) => None,
        (Some(_), Some(s), Some(e)) if s != e => {
            if !pred_names.is_empty() {
                return Err(bad(
                    "path:predicate and a nested pattern of edges cannot be combined; put the predicates in the pattern",
                ));
            }
            if !exclude_names.is_empty() {
                return Err(bad(
                    "path:excludePredicate and a nested pattern of edges cannot be combined; filter the pattern instead",
                ));
            }
            for (i, what) in [(3, "path:edgePredicate"), (7, "path:edge")] {
                if outs[i].is_some() {
                    return Err(bad(format!(
                        "{what} needs edges from predicates; the edges of a nested pattern have no triple"
                    )));
                }
            }
            if weight.is_some() {
                return Err(bad(
                    "path:weight needs edges from predicates; the edges of a nested pattern have no triple",
                ));
            }
            Some((s, e))
        }
        (Some(_), Some(_), Some(_)) => {
            return Err(bad("path:start and path:end need two variables"));
        }
        (Some(_), _, _) => {
            return Err(bad(
                "a nested pattern of edges needs path:start and path:end, the variables of each edge's ends",
            ));
        }
        (None, _, _) => {
            return Err(bad(
                "path:start and path:end name the variables of a nested pattern of edges, and the block has none",
            ));
        }
    };
    if !pred_names.is_empty() && !exclude_names.is_empty() {
        return Err(bad(
            "path:excludePredicate cannot be combined with path:predicate; name the predicates to follow or the ones to skip",
        ));
    }
    if max_degree == Some(0) {
        return Err(bad("path:maxDegree must be positive"));
    }
    exclude.sort_unstable();
    exclude.dedup();
    let algorithm = algorithm.unwrap_or(Algorithm::Shortest);
    let min_len =
        u32::try_from(min_len.unwrap_or(1)).map_err(|_| bad("path:minLength is too large"))?;
    let max_len = max_len
        .map(|m| u32::try_from(m).map_err(|_| bad("path:maxLength is too large")))
        .transpose()?;
    if matches!(algorithm, Algorithm::Shortest | Algorithm::AllShortest) && min_len > 1 {
        return Err(bad(
            "path:minLength above 1 needs path:kShortest or path:all",
        ));
    }
    let k = match (algorithm, k) {
        (Algorithm::KShortest, None) => return Err(bad("path:kShortest needs path:k")),
        (Algorithm::KShortest, Some(0)) => return Err(bad("path:k must be positive")),
        (Algorithm::KShortest, Some(k)) => usize::try_from(k).unwrap_or(usize::MAX),
        (_, Some(_)) => return Err(bad("path:k applies to path:kShortest only")),
        (_, None) => 1,
    };
    if algorithm == Algorithm::All && max_len.is_none() {
        return Err(bad("path:all needs path:maxLength"));
    }
    if limit == Some(0) {
        return Err(bad("path:limit must be positive"));
    }
    if max_visited == Some(0) {
        return Err(bad("path:maxVisited must be positive"));
    }
    if default_weight.is_some() && weight.is_none() {
        return Err(bad("path:defaultWeight needs path:weight"));
    }
    // each output its own variable, none of them the source or the target
    let mut seen: Vec<VarId> = Vec::new();
    for v in outs.iter().flatten() {
        let clash = seen.contains(v)
            || matches!(source, PathEnd::Var(s) if s == *v)
            || matches!(target, PathEnd::Var(t) if t == *v);
        if clash {
            return Err(bad(format!(
                "?{} is bound by two parameters; each output needs its own variable",
                ctx.var_name(*v)
            )));
        }
        seen.push(*v);
    }
    let mut vars: Vec<VarId> = Vec::new();
    for v in [&source, &target]
        .into_iter()
        .filter_map(|e| match e {
            PathEnd::Var(v) => Some(*v),
            PathEnd::Const(_) => None,
        })
        .chain(outs.iter().flatten().copied())
    {
        if !vars.contains(&v) {
            vars.push(v);
        }
    }
    let Some((graph, graph_var)) = p.graph_filter(g)? else {
        return Ok(Node::empty(vars));
    };
    if graph_var.is_some() {
        return Err(bad(
            "GRAPH ?g around a path search is not supported; name the graph or use the default graph",
        ));
    }
    let direction = direction.unwrap_or(Direction::Forward);
    let directions: Vec<Direction> = dirs.iter().map(|d| d.unwrap_or(direction)).collect();
    let end = |e: &PathEnd| match e {
        PathEnd::Var(v) => format!("?{}", ctx.var_name(*v)),
        PathEnd::Const(id) => ctx
            .plan_term(*id)
            .map_or_else(|| format!("{id:?}"), |t| t.to_string()),
    };
    let desc = format!(
        "{} {} → {} via {}{} {}{}{}{}{}{}{}",
        algorithm.name(),
        end(&source),
        end(&target),
        if edge_pattern.is_some() {
            "the nested pattern".to_string()
        } else if pred_names.is_empty() {
            "every predicate".to_string()
        } else {
            pred_names
                .iter()
                .map(|(p, d)| match d {
                    None => format!("<{p}>"),
                    Some(Direction::Forward) => format!("<{p}>(forward)"),
                    Some(Direction::Backward) => format!("<{p}>(backward)"),
                    Some(Direction::Both) => format!("<{p}>(both)"),
                })
                .collect::<Vec<_>>()
                .join(" ")
        },
        if exclude_names.is_empty() {
            String::new()
        } else {
            format!(
                " except {}",
                exclude_names
                    .iter()
                    .map(|p| format!("<{p}>"))
                    .collect::<Vec<_>>()
                    .join(" ")
            )
        },
        match direction {
            Direction::Forward => "forward",
            Direction::Backward => "backward",
            Direction::Both => "both ways",
        },
        if min_len != 1 {
            format!(" min={min_len}")
        } else {
            String::new()
        },
        max_len.map_or(String::new(), |m| format!(" max={m}")),
        if algorithm == Algorithm::KShortest {
            format!(" k={k}")
        } else {
            String::new()
        },
        limit.map_or(String::new(), |l| format!(" limit={l}")),
        max_degree.map_or(String::new(), |d| format!(" maxDegree={d}")),
        weight
            .as_ref()
            .map_or(String::new(), |(w, _)| format!(" weight=<{w}>")),
    );
    let edge_vars = [outs[1], outs[2], outs[3], outs[4], outs[7]];
    // the pattern's own variables stay inside it
    let edges_node = match &edge_gp {
        Some(gp) if edge_pattern.is_some() => {
            let n = p.plan(gp, g, Vec::new())?;
            for (v, name) in [
                (edge_pattern.unwrap().0, "start"),
                (edge_pattern.unwrap().1, "end"),
            ] {
                if !n.vars.contains(&v) {
                    return Err(bad(format!(
                        "path:{name} ?{} is not a variable of the nested pattern",
                        ctx.var_name(v)
                    )));
                }
            }
            Some(n)
        }
        _ => None,
    };
    let spec = PathSearchSpec {
        algorithm,
        source,
        target,
        predicates: if edge_pattern.is_some() {
            Some(Vec::new())
        } else {
            preds
        },
        directions,
        edge_pattern,
        direction,
        exclude,
        max_degree,
        min_len,
        max_len,
        k,
        limit: limit.map(|l| usize::try_from(l).unwrap_or(usize::MAX)),
        max_visited: max_visited.unwrap_or(DEFAULT_MAX_VISITED),
        weight: weight.map(|(_, id)| id.map(|i| i.0)),
        default_weight: default_weight.unwrap_or(1.0),
        reifies: ctx.snap.lookup_iri(RDF_REIFIES).map(|i| i.0),
        path_index: outs[0],
        edge_index: outs[1],
        edge_s: outs[2],
        edge_p: outs[3],
        edge_o: outs[4],
        edge: outs[7],
        length: outs[5],
        cost: outs[6],
        graph,
    };
    let est = 100.0;
    let mut n = Node::leaf(Kind::PathSearch(Box::new(spec)), vars, est, desc);
    if let Some(e) = edges_node {
        n.cost += e.cost;
        n.children.push(e);
    }
    // a path of length zero has no edge
    if min_len == 0 {
        n.certain.retain(|v| !edge_vars.contains(&Some(*v)));
    }
    n.cost = 10_000.0;
    Ok(n)
}

/// The search attached to the rest of its group (`left`), which binds its source or
/// target.
pub(super) fn attach(p: &Planner<'_>, left: Node, search: Node) -> Result<Node> {
    let Kind::PathSearch(spec) = &search.kind else {
        unreachable!("a path search");
    };
    let unbound = |e: &PathEnd| matches!(e, PathEnd::Var(v) if !left.vars.contains(v));
    if unbound(&spec.source) && unbound(&spec.target) {
        let name = |e: &PathEnd| match e {
            PathEnd::Var(v) => p.ctx.var_name(*v),
            PathEnd::Const(_) => String::new(),
        };
        return Err(bad(format!(
            "neither path:source ?{} nor path:target ?{} is bound by the rest of the group; bind one, for example with VALUES",
            name(&spec.source),
            name(&spec.target)
        )));
    }
    let mut vars = left.vars.clone();
    for v in &search.vars {
        if !vars.contains(v) {
            vars.push(*v);
        }
    }
    let mut certain = left.certain.clone();
    certain.extend(search.certain.iter().copied());
    let est = (left.est * 10.0).max(1.0);
    // the input first, then the nested pattern of edges if any
    let mut children = vec![left];
    children.extend(search.children);
    Ok(Node {
        dist: vars.iter().map(|&v| (v, est)).collect(),
        cost: children[0].cost + est * 100.0,
        vars,
        certain,
        sorted: Vec::new(),
        est,
        desc: search.desc,
        kind: search.kind,
        children,
    })
}

// ----------------------------------------------------------------- execution ------

/// A neighbour: the node, the edge's predicate, and whether the stored triple runs
/// against the path (its subject is the later node).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
struct Step {
    node: u64,
    p: u64,
    rev: bool,
}

/// Where [`Engine::scan_lane`] puts the edges it reads: a list of them, or a count for
/// the check of `path:maxDegree`, which must not hold a hub's edges in memory.
trait Sink {
    /// Whether the scan checks the deadline and cancellation once per chunk, because it
    /// does not return to a caller that checks them between nodes.
    const CHECKS: bool;
    fn put(&mut self, x: u64, step: Step);
    fn len(&self) -> usize;
}

impl Sink for Vec<(u64, Step)> {
    const CHECKS: bool = false;
    #[inline]
    fn put(&mut self, x: u64, step: Step) {
        self.push((x, step));
    }
    #[inline]
    fn len(&self) -> usize {
        Vec::len(self)
    }
}

/// The number of edges a scan read.
struct Count(usize);

impl Sink for Count {
    const CHECKS: bool = true;
    #[inline]
    fn put(&mut self, _: u64, _: Step) {
        self.0 += 1;
    }
    #[inline]
    fn len(&self) -> usize {
        self.0
    }
}

/// An edge of a path, from `from` to `to` in path order.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
struct Edge {
    from: u64,
    to: u64,
    p: u64,
    rev: bool,
}

impl Edge {
    /// The stored triple.
    fn triple(&self) -> (u64, u64, u64) {
        if self.rev {
            (self.to, self.p, self.from)
        } else {
            (self.from, self.p, self.to)
        }
    }
    fn map(self, f: impl Fn(u64) -> u64) -> Edge {
        Edge {
            from: f(self.from),
            to: f(self.to),
            ..self
        }
    }
}

/// A link of a search tree: the node one edge nearer the root.
#[derive(Clone, Copy, Debug)]
struct Parent {
    node: u64,
    p: u64,
    rev: bool,
}

/// The parents of the nodes a search reached. In a forward tree a parent precedes its
/// node on the path, in a backward tree it follows it.
struct Tree {
    root: u64,
    forward: bool,
    /// node → (first parent, BFS level)
    first: FxHashMap<u64, (Parent, u32)>,
    /// further parents of the same level or cost
    extra: FxHashMap<u64, Vec<Parent>>,
    /// the nodes reached, with their level, in the order the search finished them: by
    /// level and id (breadth first) or by cost (Dijkstra)
    order: Vec<(u64, u32)>,
}

/// The work of one path enumeration, checked against the query's deadline and
/// cancellation every 4096 units.
struct Budget<'c> {
    steps: u64,
    check: &'c dyn Fn() -> Result<()>,
}

impl Budget<'_> {
    fn tick(&mut self) -> Result<()> {
        self.steps += 1;
        if self.steps.is_multiple_of(4096) {
            (self.check)()?;
        }
        Ok(())
    }
}

/// What `Tree::paths` needs to skip parents that cannot reach the root: the cost of
/// each node and the position at which the search settled it.
struct Prune<'a> {
    cost: &'a FxHashMap<u64, Cost>,
    rank: FxHashMap<u64, u32>,
}

impl Tree {
    fn new(root: u64, forward: bool) -> Tree {
        Tree {
            root,
            forward,
            first: FxHashMap::default(),
            extra: FxHashMap::default(),
            order: Vec::new(),
        }
    }
    /// The position of each node in the order the search settled it.
    fn ranks(&self) -> FxHashMap<u64, u32> {
        self.order
            .iter()
            .enumerate()
            .map(|(i, &(n, _))| (n, i as u32))
            .collect()
    }
    fn contains(&self, x: u64) -> bool {
        x == self.root || self.first.contains_key(&x)
    }
    fn parent_at(&self, x: u64, i: usize, all: bool) -> Option<Parent> {
        match i {
            0 => self.first.get(&x).map(|e| e.0),
            _ if all => self.extra.get(&x).and_then(|v| v.get(i - 1)).copied(),
            _ => None,
        }
    }

    /// The paths between the root and `m` through the recorded parents, at most `cap`,
    /// each in path order.
    fn paths(
        &self,
        m: u64,
        all: bool,
        cap: usize,
        prune: Option<&Prune>,
        check: &dyn Fn() -> Result<()>,
    ) -> Result<Vec<Vec<Edge>>> {
        // with one parent per node the parents form a tree, and every one reaches the root
        let prune = prune.filter(|_| all);
        let rank_of = |n: u64| prune.and_then(|p| p.rank.get(&n).copied()).unwrap_or(0);
        let mut out = Vec::new();
        if cap == 0 || !self.contains(m) {
            return Ok(out);
        }
        let mut stack: Vec<(u64, usize)> = vec![(m, 0)];
        // low[j] is the least rank of the nodes stack[0..=j]
        let mut low: Vec<u32> = vec![rank_of(m)];
        let mut links: Vec<(u64, Parent)> = Vec::new();
        // zero-weight edges can make nodes parents of each other, so a parent already
        // on the path is skipped
        let mut on: FxHashSet<u64> = FxHashSet::default();
        on.insert(m);
        let mut budget = Budget { steps: 0, check };
        while let Some(top) = stack.last_mut() {
            budget.tick()?;
            let x = top.0;
            if x == self.root {
                let edges: Vec<Edge> = if self.forward {
                    links
                        .iter()
                        .rev()
                        .map(|(x, q)| Edge {
                            from: q.node,
                            to: *x,
                            p: q.p,
                            rev: q.rev,
                        })
                        .collect()
                } else {
                    links
                        .iter()
                        .map(|(x, q)| Edge {
                            from: *x,
                            to: q.node,
                            p: q.p,
                            rev: q.rev,
                        })
                        .collect()
                };
                out.push(edges);
                stack.pop();
                low.pop();
                links.pop();
                on.remove(&x);
                if out.len() >= cap {
                    break;
                }
                continue;
            }
            let i = top.1;
            top.1 += 1;
            match self.parent_at(x, i, all) {
                Some(q) if on.contains(&q.node) && q.node != self.root => {}
                Some(q) => {
                    let low_top = low.last().copied().unwrap_or(0);
                    if let Some(pr) = prune
                        && !self.escapes(x, q.node, &on, low_top, pr, &mut budget)?
                    {
                        continue;
                    }
                    links.push((x, q));
                    stack.push((q.node, 0));
                    low.push(low_top.min(rank_of(q.node)));
                    on.insert(q.node);
                }
                None => {
                    stack.pop();
                    low.pop();
                    links.pop();
                    on.remove(&x);
                }
            }
        }
        Ok(out)
    }

    /// Whether parent `q` of `x` reaches the root through recorded parents without a
    /// node of `on`. A parent the search settled before every node on the path, whose
    /// least rank is `low`, escapes at once, because its first parents were settled
    /// still earlier. Costs never rise toward the root, so a cheaper parent escapes at
    /// once too, through its first parents. Otherwise the recorded parents are searched,
    /// and each parent the search examines counts against the deadline.
    fn escapes(
        &self,
        x: u64,
        q: u64,
        on: &FxHashSet<u64>,
        low: u32,
        pr: &Prune,
        budget: &mut Budget<'_>,
    ) -> Result<bool> {
        let cost = pr.cost;
        if q == self.root {
            return Ok(true);
        }
        // first parents were settled before their nodes, so the chain of first parents
        // from a node settled before every node on the path stays off the path
        if pr.rank.get(&q).is_some_and(|&r| r < low) {
            return Ok(true);
        }
        let (Some(&c), Some(&cq)) = (cost.get(&x), cost.get(&q)) else {
            return Ok(true);
        };
        if cq < c {
            return Ok(true);
        }
        let mut seen: FxHashSet<u64> = FxHashSet::default();
        seen.insert(q);
        let mut todo = vec![q];
        while let Some(z) = todo.pop() {
            let mut i = 0;
            while let Some(p) = self.parent_at(z, i, true) {
                i += 1;
                budget.tick()?;
                let y = p.node;
                if y == self.root {
                    return Ok(true);
                }
                if on.contains(&y) {
                    continue;
                }
                match cost.get(&y) {
                    Some(&cy) if cy < c => return Ok(true),
                    None => return Ok(true),
                    _ => {}
                }
                if seen.insert(y) {
                    todo.push(y);
                }
            }
        }
        Ok(false)
    }
}

/// A sum of non-negative weights kept as a double-double: the rounded sum and the error
/// of rounding it, from Knuth's TwoSum. A search from both ends adds a path's weights from
/// each end, and plain floating point can round the two orders apart enough to rank a
/// dearer path first when the weights differ greatly in size. With about 106 bits the
/// orders agree unless two costs differ by less than about 2^-106 of their size.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Cost {
    hi: f64,
    lo: f64,
}

impl Cost {
    const ZERO: Cost = Cost { hi: 0.0, lo: 0.0 };

    fn of(w: f64) -> Cost {
        // a weight of -0 counts as 0
        Cost {
            hi: w + 0.0,
            lo: 0.0,
        }
    }

    const INFINITE: Cost = Cost {
        hi: f64::INFINITY,
        lo: 0.0,
    };

    fn add(self, o: Cost) -> Cost {
        if self.hi.is_infinite() || o.hi.is_infinite() {
            return Cost::INFINITE;
        }
        if (self.hi + o.hi).is_finite() {
            return self.sum(o);
        }
        // the rounded parts reach past the largest double, but the rounding errors can
        // bring the sum back under it. Halving is exact, so the halves are added without
        // overflow and the sum doubled, which overflows only when the sum itself rounds
        // past the largest double.
        let half = |c: Cost| Cost {
            hi: c.hi * 0.5,
            lo: c.lo * 0.5,
        };
        let h = half(self).sum(half(o));
        let hi = h.hi * 2.0;
        if !hi.is_finite() {
            return Cost::INFINITE;
        }
        Cost { hi, lo: h.lo * 2.0 }
    }

    /// TwoSum of the rounded parts, the rounding errors added to the error, and Fast2Sum
    /// to renormalize.
    fn sum(self, o: Cost) -> Cost {
        let (a, b) = (self.hi, o.hi);
        // s + e is exactly a + b
        let s = a + b;
        let bb = s - a;
        let e = (a - (s - bb)) + (b - bb) + self.lo + o.lo;
        // |s| ≥ |e|
        let hi = s + e;
        if !hi.is_finite() {
            return Cost::INFINITE;
        }
        // adding +0 turns a -0 into +0, so that equal costs compare equal under `Ord`
        Cost {
            hi,
            lo: e - (hi - s) + 0.0,
        }
    }
}

impl Eq for Cost {}

impl PartialOrd for Cost {
    fn partial_cmp(&self, o: &Self) -> Option<Ordering> {
        Some(self.cmp(o))
    }
}

impl Ord for Cost {
    fn cmp(&self, o: &Self) -> Ordering {
        self.hi.total_cmp(&o.hi).then(self.lo.total_cmp(&o.lo))
    }
}

/// Every path of a front half from `front`, the edge `e` and a back half from `back`,
/// given to `emit` until it returns false, which this returns too. Each path counts
/// against `budget`, since the product can be far larger than the halves.
fn join_halves(
    front: &[Vec<Edge>],
    e: Edge,
    back: &[Vec<Edge>],
    budget: &mut Budget<'_>,
    emit: &mut dyn FnMut(Vec<Edge>) -> Result<bool>,
) -> Result<bool> {
    for a in front {
        for z in back {
            budget.tick()?;
            let mut p = Vec::with_capacity(a.len() + 1 + z.len());
            p.extend_from_slice(a);
            p.push(e);
            p.extend_from_slice(z);
            if !emit(p)? {
                return Ok(false);
            }
        }
    }
    Ok(true)
}

/// A path from `s` with every cycle cut out, so each node appears once. Two halves of a
/// bidirectional search can share a node only through edges of weight zero, so the cut
/// path costs the same.
fn without_loops(s: u64, edges: Vec<Edge>) -> Vec<Edge> {
    let mut out: Vec<Edge> = Vec::with_capacity(edges.len());
    // the position in `out` after which each node is reached
    let mut at: FxHashMap<u64, usize> = FxHashMap::default();
    at.insert(s, 0);
    for e in edges {
        if let Some(&i) = at.get(&e.to) {
            for d in out.drain(i..) {
                at.remove(&d.to);
            }
            at.insert(e.to, i);
            continue;
        }
        out.push(e);
        at.insert(e.to, out.len());
    }
    out
}

/// Locate where `(len, cost)` belongs in a Pareto frontier sorted by strictly increasing
/// `len` and strictly decreasing `cost`. Returns `None` when an existing label has
/// `l <= len` and `c <= cost`, `Some((pos, true))` to replace `v[pos]` (same length,
/// strictly cheaper), or `Some((pos, false))` to insert at `pos`.
fn pareto_locate(v: &[(u32, Cost)], len: u32, cost: Cost) -> Option<(usize, bool)> {
    let pos = v.partition_point(|&(l, _)| l < len);
    if pos > 0 && v[pos - 1].1 <= cost {
        return None;
    }
    if pos < v.len() && v[pos].0 == len {
        return (cost < v[pos].1).then_some((pos, true));
    }
    Some((pos, false))
}

/// Insert `(len, cost)` into a Pareto frontier, pruning any longer labels with cost
/// `>= cost`. Returns `true` when the label is not dominated and was kept.
#[cfg(test)]
fn pareto_insert(v: &mut Vec<(u32, Cost)>, len: u32, cost: Cost) -> bool {
    let Some((pos, replace)) = pareto_locate(v, len, cost) else {
        return false;
    };
    if replace {
        v[pos].1 = cost;
        let mut end = pos + 1;
        while end < v.len() && v[end].1 >= cost {
            end += 1;
        }
        if end > pos + 1 {
            v.drain(pos + 1..end);
        }
        return true;
    }
    let mut end = pos;
    while end < v.len() && v[end].1 >= cost {
        end += 1;
    }
    if end > pos {
        v[pos] = (len, cost);
        if end > pos + 1 {
            v.drain(pos + 1..end);
        }
    } else {
        v.insert(pos, (len, cost));
    }
    true
}

/// The cheapest label in a Pareto frontier with length `<= max_l`. Because lengths
/// strictly increase and costs strictly decrease, this is the last entry with `l <= max_l`.
#[inline]
fn pareto_best_within(v: &[(u32, Cost)], max_l: u32) -> Option<(u32, Cost)> {
    let pos = v.partition_point(|&(l, _)| l <= max_l);
    (pos > 0).then(|| v[pos - 1])
}

#[inline]
fn opt_cost_min(a: Option<Cost>, b: Option<Cost>) -> Option<Cost> {
    match (a, b) {
        (Some(x), Some(y)) => Some(x.min(y)),
        (Some(x), None) | (None, Some(x)) => Some(x),
        (None, None) => None,
    }
}

#[inline]
fn opt_cost_add(a: Option<Cost>, b: Option<Cost>) -> Option<Cost> {
    match (a, b) {
        (Some(x), Some(y)) => Some(x.add(y)),
        _ => None,
    }
}

/// Accumulated prefix cost and positive weight bounds carried into a length-bounded
/// pair or Yen spur search so wide-weight detection and source-order fallback match the
/// full path from the original source.
#[derive(Clone, Copy)]
struct PrefixCost {
    cost: Cost,
    wmin: f64,
    wmax: f64,
    total_max_len: u32,
}

impl PrefixCost {
    const fn zero(total_max_len: u32) -> Self {
        Self {
            cost: Cost::ZERO,
            wmin: f64::INFINITY,
            wmax: 0.0,
            total_max_len,
        }
    }
}

/// Outcome of a one-ended length-bounded weighted search: either a Pareto-pruned parent
/// tree or, for a reverse search where wide weights or overflow occurred alongside at least
/// one multi-route node, the reverse tree together with exact minimum backward edge
/// distances (`bwd_dist`) and the shortest length at which each multi-route node has a
/// second non-backtracking route (`direct_multi`) so unique-route sources still reconstruct
/// directly while sources with a competing route within `max_len` re-solve in source order.
enum BoundedTreeOutcome {
    Tree(ChargedBoundedTree),
    ReverseWide {
        order: Vec<(u64, u32)>,
        parent: HbMap<(u64, u32), Parent>,
        bwd_dist: HbMap<u64, u32>,
        direct_multi: HbMap<u64, u32>,
    },
}

/// Whether the ratio of positive edge weights or the budget `max_len` can make
/// double-double `Cost::add` round differently across bidirectional split points.
#[inline]
fn bounded_weights_too_wide(max_len: u32, wmin: f64, wmax: f64) -> bool {
    if max_len <= 1 || wmax <= 0.0 || !wmin.is_finite() {
        return false;
    }
    let ratio = wmax / wmin;
    ratio > WIDE_WEIGHTS || (max_len as f64) * ratio > ((1u64 << 51) as f64)
}

/// Record a non-self-loop reverse relaxation reaching `y` at length `nl` from `(u, l)`.
/// Direct 1-hop backtracks to `(u, l)`'s own reverse parent `y` cannot extend into a
/// simple path unless `u` itself has another route to the target; defer them in
/// `backtrack_parent` (at most one entry per settled node `u`) and propagate after the
/// reverse search only if `u` becomes multi-route.
#[allow(clippy::too_many_arguments)]
fn record_reverse_multi_relax<C: ByteCharge>(
    u: u64,
    l: u32,
    y: u64,
    nl: u32,
    parent: &HbMap<(u64, u32), Parent>,
    dist: &HbMap<u64, Vec<(u32, Cost)>>,
    virt_first_len: &mut Option<u32>,
    direct_multi: &mut HbMap<u64, u32>,
    backtrack_parent: &mut HbMap<u64, u64>,
    charge: &mut C,
    base: u64,
) -> Result<u64> {
    debug_assert_ne!(y, u);
    if l > 0 && parent.get(&(u, l)).is_some_and(|p| p.node == y) {
        match backtrack_parent.get(&u).copied() {
            Some(prev_y) if prev_y == y => return Ok(base),
            None => {
                let (_, next_base) = insert_hb_map(backtrack_parent, u, y, charge, base)?;
                return Ok(next_base);
            }
            Some(_) => {
                // `u` was settled at two different lengths with distinct parents, so `u`
                // already has multiple routes; fall through to record `y` conservatively.
            }
        }
    }
    let first_len = if y == VIRT {
        match *virt_first_len {
            None => {
                *virt_first_len = Some(nl);
                return Ok(base);
            }
            Some(fl) => {
                if nl < fl {
                    *virt_first_len = Some(nl);
                }
                fl
            }
        }
    } else {
        let Some(fl) = dist.get(&y).and_then(|v| v.first()).map(|&(fl, _)| fl) else {
            return Ok(base);
        };
        fl
    };
    let alt_len = first_len.max(nl);
    if direct_multi.get(&y).is_some_and(|&prev| prev <= alt_len) {
        return Ok(base);
    }
    let (_, next_base) = insert_hb_map(direct_multi, y, alt_len, charge, base)?;
    Ok(next_base)
}

/// Propagate `direct_multi` across deferred 1-hop backtrack edges (`u -> parent(u)`) when
/// `u` gained an alternative route to the target. Because `backtrack_parent` has out-degree
/// at most one and `next_alt` strictly increases up to `max_len`, each seed walk terminates
/// in at most `max_len` steps.
#[allow(clippy::too_many_arguments)]
fn propagate_reverse_backtracks<C: ByteCharge>(
    ctx: &Ctx,
    max_len: u32,
    best_len: &HbMap<u64, u32>,
    backtrack_parent: &HbMap<u64, u64>,
    direct_multi: &mut HbMap<u64, u32>,
    charge: &mut C,
    base: u64,
) -> Result<u64> {
    if direct_multi.is_empty() || backtrack_parent.is_empty() {
        return Ok(base);
    }
    let seeds_bytes = vec_bytes::<(u64, u32)>(direct_multi.len());
    charge.resize(base.saturating_add(seeds_bytes))?;
    let mut seeds = Vec::with_capacity(direct_multi.len());
    seeds.extend(direct_multi.iter().map(|(&k, &v)| (k, v)));
    let actual_seeds_bytes = vec_bytes::<(u64, u32)>(seeds.capacity());
    let mut cur_base = base.saturating_add(actual_seeds_bytes);
    charge.resize(cur_base)?;
    for (mut cur, mut alt) in seeds {
        let mut steps = 0usize;
        while let Some(&y) = backtrack_parent.get(&cur) {
            steps += 1;
            if steps.is_multiple_of(4096) {
                ctx.check()?;
            }
            let next_alt = alt
                .saturating_add(1)
                .max(best_len.get(&y).copied().unwrap_or(0));
            if next_alt > max_len {
                break;
            }
            if direct_multi.get(&y).is_some_and(|&prev| prev <= next_alt) {
                break;
            }
            (_, cur_base) = insert_hb_map(direct_multi, y, next_alt, charge, cur_base)?;
            cur = y;
            alt = next_alt;
        }
    }
    let final_base = cur_base.saturating_sub(actual_seeds_bytes);
    charge.resize(final_base)?;
    Ok(final_base)
}

/// Whether any node along the reverse parent chain from `(m, len)` back to `(root, 0)`
/// (excluding `root`) has an alternative route to `root` short enough to fit within the
/// remaining edge budget `max_len - prefix` from `m`. When this returns `false`, `m` has a
/// unique simple route of length `<= max_len` to `root` and needs no source-order
/// re-ranking.
fn bounded_chain_has_multi(
    ctx: &Ctx,
    parent: &HbMap<(u64, u32), Parent>,
    direct_multi: &HbMap<u64, u32>,
    m: u64,
    len: u32,
    max_len: u32,
) -> Result<bool> {
    let mut cur = m;
    let mut l = len;
    let mut prefix = 0u32;
    let mut steps = 0usize;
    while l > 0 {
        steps += 1;
        if steps.is_multiple_of(4096) {
            ctx.check()?;
        }
        let rem_budget = max_len.saturating_sub(prefix);
        if direct_multi
            .get(&cur)
            .is_some_and(|&alt_len| alt_len <= rem_budget)
        {
            return Ok(true);
        }
        cur = parent[&(cur, l)].node;
        l -= 1;
        prefix += 1;
    }
    Ok(false)
}

/// Reconstruct a single path of `len` edges from `(m, len)` back to `(root, 0)` in a
/// length-indexed parent map, mapping `VIRT` back to `root`.
fn reconstruct_bounded(
    ctx: &Ctx,
    root: u64,
    forward: bool,
    parent: &HbMap<(u64, u32), Parent>,
    m: u64,
    len: u32,
) -> Result<Vec<Edge>> {
    let real = |x: u64| if x == VIRT { root } else { x };
    let mut edges = Vec::with_capacity(len as usize);
    let mut cur = m;
    let mut l = len;
    let mut steps = 0usize;
    while l > 0 {
        steps += 1;
        if steps.is_multiple_of(4096) {
            ctx.check()?;
        }
        let q = parent[&(cur, l)];
        let e = if forward {
            Edge {
                from: q.node,
                to: cur,
                p: q.p,
                rev: q.rev,
            }
        } else {
            Edge {
                from: cur,
                to: q.node,
                p: q.p,
                rev: q.rev,
            }
        };
        edges.push(e.map(real));
        cur = q.node;
        l -= 1;
    }
    if forward {
        edges.reverse();
    }
    Ok(edges)
}

/// One index range that holds edges: `perm` with the predicate fixed or not, or the
/// edges of a nested pattern (`pattern`).
#[derive(Clone, Copy)]
struct Lane {
    perm: Perm,
    p: Option<u64>,
    /// the lane is read by the subject of its triples (else by the object), or by the
    /// start of a pattern's edges (else by the end)
    subject_side: bool,
    pattern: bool,
}

/// A lane of the neighbours on one side, whether its triples run against the path, and
/// whether it skips self-loops (the second side of a predicate followed both ways reads
/// them from the first).
#[derive(Clone, Copy)]
struct Side {
    lane: Lane,
    rev: bool,
    skip_self: bool,
}

/// The edges of a nested pattern, by their start and by their end (sorted, distinct).
#[derive(Default)]
struct PatternEdges {
    by_start: FxHashMap<u64, Vec<u64>>,
    by_end: FxHashMap<u64, Vec<u64>>,
}

impl PatternEdges {
    fn of(t: &Table, (s, e): (VarId, VarId)) -> PatternEdges {
        let mut out = PatternEdges::default();
        let (Some(sc), Some(ec)) = (t.col_of(s), t.col_of(e)) else {
            return out;
        };
        for i in 0..t.len() {
            let (a, b) = (t.cols[sc][i], t.cols[ec][i]);
            if a.is_undef() || b.is_undef() {
                continue;
            }
            out.by_start.entry(a.0).or_default().push(b.0);
            out.by_end.entry(b.0).or_default().push(a.0);
        }
        for v in out.by_start.values_mut().chain(out.by_end.values_mut()) {
            v.sort_unstable();
            v.dedup();
        }
        out
    }
}

impl Lane {
    /// (key column of the node read, column of the neighbour, column of the predicate)
    fn cols(&self) -> (usize, usize, Option<usize>) {
        match (self.p, self.subject_side) {
            // PSO [p, s, o] / POS [p, o, s]
            (Some(_), _) => (1, 2, None),
            // SPO [s, p, o]
            (None, true) => (0, 2, Some(1)),
            // OSP [o, s, p]
            (None, false) => (0, 1, Some(2)),
        }
    }
}

/// How this call's view counts degrees: from the index alone when every quad of the
/// store passes the graph filter, which is one count of the store's graphs per call.
fn degree_count(ctx: &Ctx, graph: &GraphFilter) -> Result<DegreeCount> {
    let snap = &ctx.snap;
    let total = snap.count(Perm::Gspo, &[])?;
    if snap.count(Perm::Gspo, &[Id::DEFAULT_GRAPH.0])? == total {
        return Ok(if graph.accepts(Id::DEFAULT_GRAPH.0) {
            DegreeCount::Index
        } else {
            DegreeCount::Zero
        });
    }
    if let GraphFilter::One(g) = graph
        && snap.count(Perm::Gspo, &[*g])? == total
    {
        return Ok(DegreeCount::Index);
    }
    Ok(DegreeCount::Scan)
}

/// What a spur search of Yen's algorithm may not use.
struct Bans<'b> {
    nodes: &'b FxHashSet<u64>,
    edges: &'b FxHashSet<Edge>,
}

/// The neighbours of each node (and direction) a search looked up.
type Neighbours = FxHashMap<(u64, bool), Rc<[Step]>>;
/// Min-heap of `(cost, len, node)` labels for Pareto-pruned bounded search.
type BoundedHeap = BinaryHeap<Reverse<(Cost, u32, u64)>>;
/// One spur path returned by `Engine::spur`, with its cost and optional search-scoped charge.
type SpurPath<'a> = (Vec<Edge>, f64, Option<Charge<'a>>);

/// A path found: its ends, edges and cost.
struct Found {
    start: u64,
    end: u64,
    /// the edges, when an output needs them (edge variables or weights)
    edges: Vec<Edge>,
    len: usize,
    cost: f64,
}

/// The state of one call: index access, caches, budgets and counters.
struct Engine<'a> {
    ctx: &'a Ctx,
    spec: &'a PathSearchSpec,
    dedup: bool,
    /// the lanes of a node's successors and of its predecessors
    succ: Vec<Side>,
    pred: Vec<Side>,
    pattern: PatternEdges,
    cache: RefCell<Neighbours>,
    weights: RefCell<FxHashMap<(u64, u64, u64), f64>>,
    charge: Charge<'a>,
    /// nodes visited by the current search, and by all of them
    visited: Cell<u64>,
    visited_total: Cell<u64>,
    searches: Cell<u64>,
    sweeps: Cell<u64>,
    /// paths the call may still return
    left: Cell<usize>,
    /// with `path:maxDegree`: whether each node checked is a hub
    hubs: RefCell<FxHashMap<u64, bool>>,
    /// the roots of the current search, which are expanded even when they are hubs
    roots: RefCell<Vec<u64>>,
    /// expansions of a hub that were skipped
    skipped_hubs: Cell<u64>,
    /// how a node's degree is counted in this call's view
    degree: DegreeCount,
    /// some lane could leave a literal, which F07 §4.1 forbids: a successor lane read by
    /// the object of its triples, or a predecessor lane read by the subject, whose
    /// predecessors are objects. Literals are then checked before they are expanded. A
    /// forward search has neither, so it never checks.
    sinks: bool,
}

/// How `path:maxDegree` counts a node's degree.
#[derive(Clone, Copy, PartialEq, Eq)]
enum DegreeCount {
    /// every quad of the store passes the call's graph filter, so the index counts of a
    /// node's ranges, less the skipped predicates and self-loops, are its degree
    Index,
    /// no quad of the store passes the filter, so every degree is 0
    Zero,
    /// rows of other graphs or repeated triples may be counted: an index count that is
    /// not above the limit still proves a node is not a hub, and otherwise the node's
    /// edges are read until one more than the limit is found
    Scan,
}

impl<'a> Engine<'a> {
    fn new(ctx: &'a Ctx, spec: &'a PathSearchSpec, edges: Option<&Table>) -> Result<Engine<'a>> {
        // (subject lane, object lane, direction) of each predicate, of every predicate,
        // or of the nested pattern's edges
        let lane = |p: Option<u64>, subject_side: bool, pattern: bool| {
            let perm = match (p.is_some(), subject_side) {
                (true, true) => Perm::Pso,
                (true, false) => Perm::Pos,
                (false, true) => Perm::Spo,
                (false, false) => Perm::Osp,
            };
            Lane {
                perm,
                p,
                subject_side,
                pattern,
            }
        };
        let groups: Vec<(Lane, Lane, Direction)> = match (&spec.edge_pattern, &spec.predicates) {
            (Some(_), _) => vec![(
                lane(None, true, true),
                lane(None, false, true),
                spec.direction,
            )],
            (None, Some(ps)) => ps
                .iter()
                .zip(&spec.directions)
                .map(|(&p, &d)| (lane(Some(p), true, false), lane(Some(p), false, false), d))
                .collect(),
            (None, None) => vec![(
                lane(None, true, false),
                lane(None, false, false),
                spec.direction,
            )],
        };
        // each predicate's first side, then the second sides of those followed both ways
        // (the order of the lanes before directions per predicate)
        let (mut succ, mut pred) = (Vec::new(), Vec::new());
        let side = |lane: Lane, rev: bool, skip_self: bool| Side {
            lane,
            rev,
            skip_self,
        };
        for &(s, o, d) in &groups {
            match d {
                Direction::Forward | Direction::Both => {
                    succ.push(side(s, false, false));
                    pred.push(side(o, false, false));
                }
                Direction::Backward => {
                    succ.push(side(o, true, false));
                    pred.push(side(s, true, false));
                }
            }
        }
        for &(s, o, d) in &groups {
            if d == Direction::Both {
                succ.push(side(o, true, true));
                pred.push(side(s, true, true));
            }
        }
        let pattern = match (edges, spec.edge_pattern) {
            (Some(t), Some(vars)) => PatternEdges::of(t, vars),
            _ => PatternEdges::default(),
        };
        let sinks = succ
            .iter()
            .any(|s: &Side| s.lane.pattern || !s.lane.subject_side)
            || pred
                .iter()
                .any(|s: &Side| s.lane.pattern || s.lane.subject_side);
        let degree = match spec.max_degree {
            None => DegreeCount::Scan,
            Some(_) => degree_count(ctx, &spec.graph)?,
        };
        Ok(Engine {
            ctx,
            spec,
            dedup: spec.graph.multi(),
            succ,
            pred,
            pattern,
            cache: Default::default(),
            weights: Default::default(),
            charge: ctx.charge(0)?,
            visited: Cell::new(0),
            visited_total: Cell::new(0),
            searches: Cell::new(0),
            sweeps: Cell::new(0),
            left: Cell::new(spec.limit.unwrap_or(usize::MAX)),
            hubs: Default::default(),
            roots: Default::default(),
            skipped_hubs: Cell::new(0),
            degree,
            sinks,
        })
    }

    /// The roots of the search that starts: they are expanded even when they are hubs.
    fn set_roots(&self, roots: &[u64]) {
        if self.spec.max_degree.is_some() || self.sinks {
            let mut r = self.roots.borrow_mut();
            r.clear();
            r.extend_from_slice(roots);
        }
    }

    /// Whether `x` is a literal that the search may not expand: no edge leaves a literal,
    /// so it is never expanded to its successors, and to its predecessors only as the
    /// root of a search, the end of the paths.
    fn sink(&self, x: u64, succ: bool) -> bool {
        self.sinks
            && self.ctx.snap.term_kind(Id(x)) == crate::store::TermKind::Literal
            && (succ || !self.roots.borrow().contains(&x))
    }

    /// Whether the search may not expand `x`: `path:maxDegree` is set, `x` is not a
    /// root of the current search, and its degree is above the limit.
    fn blocked(&self, x: u64) -> Result<bool> {
        if self.spec.max_degree.is_none() || self.roots.borrow().contains(&x) {
            return Ok(false);
        }
        self.is_hub(x)
    }

    /// Whether the degree of `x` in the call's direction is above `path:maxDegree`,
    /// worked out once per node of the call.
    fn is_hub(&self, x: u64) -> Result<bool> {
        let Some(limit) = self.spec.max_degree else {
            return Ok(false);
        };
        if let Some(&h) = self.hubs.borrow().get(&x) {
            return Ok(h);
        }
        let h = self.degree_above(x, limit)?;
        self.charge.add(HUB_BYTES)?;
        self.hubs.borrow_mut().insert(x, h);
        Ok(h)
    }

    /// Whether `x` has more than `limit` edges that the search could follow from it in
    /// the call's direction: its successors, whichever way the search reached it, so a
    /// node is a hub or not whatever side of a bidirectional search meets it. The edges
    /// are counted after the predicates are chosen and skipped and in the call's view,
    /// with a self-loop followed both ways counted once, as the search lists them.
    fn degree_above(&self, x: u64, limit: u64) -> Result<bool> {
        if self.degree == DegreeCount::Zero {
            return Ok(false);
        }
        let snap = &self.ctx.snap;
        let key = |lane: &Lane, x: u64| -> Vec<u64> { lane.p.into_iter().chain([x]).collect() };
        // an upper bound from the index: rows of every graph, skipped predicates and
        // self-loops included
        let mut upper = 0u64;
        for s in &self.succ {
            if s.lane.pattern {
                upper += self.pattern_degree(s, x);
            } else {
                upper += snap.count(s.lane.perm, &key(&s.lane, x))?;
            }
        }
        if upper <= limit {
            return Ok(false);
        }
        // a nested pattern's edges are counted exactly above
        if self.succ.iter().all(|s| s.lane.pattern) {
            return Ok(true);
        }
        if self.degree == DegreeCount::Index {
            let excl = &self.spec.exclude;
            let mut less = 0u64;
            for s in &self.succ {
                let lane = &s.lane;
                if lane.p.is_none() {
                    // the triples of a skipped predicate in this lane
                    let by = if lane.subject_side {
                        Perm::Pso
                    } else {
                        Perm::Pos
                    };
                    for &pe in excl {
                        less += snap.count(by, &[pe, x])?;
                    }
                }
                if s.skip_self {
                    // a self-loop is listed by the first side only
                    less += match lane.p {
                        Some(p) => snap.count(Perm::Pso, &[p, x, x])?,
                        None => {
                            let mut loops = snap.count(Perm::Sop, &[x, x])?;
                            for &pe in excl {
                                loops = loops.saturating_sub(snap.count(Perm::Pso, &[pe, x, x])?);
                            }
                            loops
                        }
                    };
                }
            }
            return Ok(upper.saturating_sub(less) > limit);
        }
        // count the edges until one more than the limit is found, without keeping them
        let cap = usize::try_from(limit)
            .unwrap_or(usize::MAX)
            .saturating_add(1);
        let mut out = Count(0);
        for s in &self.succ {
            if s.lane.pattern {
                out.0 = out.0.saturating_add(self.pattern_degree(s, x) as usize);
            } else {
                self.scan_lane(&s.lane, &[x], s.rev, s.skip_self, &mut out, cap)?;
            }
            if out.0 >= cap {
                return Ok(true);
            }
        }
        Ok(false)
    }

    /// The exact number of edges that leave `x` on a side of a nested pattern.
    fn pattern_degree(&self, s: &Side, x: u64) -> u64 {
        let map = if s.lane.subject_side {
            &self.pattern.by_start
        } else {
            &self.pattern.by_end
        };
        let ns = map.get(&x).map_or(&[][..], Vec::as_slice);
        let n = ns.len() as u64;
        if s.skip_self && ns.binary_search(&x).is_ok() {
            n - 1
        } else {
            n
        }
    }

    /// Start a new search: its visited count starts at zero.
    fn begin(&self) {
        self.visited.set(0);
        self.searches.set(self.searches.get() + 1);
    }

    /// The pruning data for the paths of `tree` when all the cheapest ones are wanted. The
    /// settling-order map trades memory for speed, so its estimated size is charged to the
    /// memory budget before it is built.
    fn prune<'c>(
        &self,
        tree: &Tree,
        cost: Option<&'c FxHashMap<u64, Cost>>,
        all: bool,
    ) -> Result<Option<Prune<'c>>> {
        let Some(cost) = cost.filter(|_| all) else {
            return Ok(None);
        };
        self.charge.add(tree.order.len() as u64 * RANK_BYTES)?;
        Ok(Some(Prune {
            cost,
            rank: tree.ranks(),
        }))
    }

    /// Count `n` more visited nodes against `path:maxVisited` and the memory budget.
    fn visit(&self, n: u64) -> Result<()> {
        self.visit_count(n)?;
        self.charge.add(n * NODE_BYTES)
    }

    /// Count `n` more visited nodes against `path:maxVisited` without charging the
    /// call-wide estimate (for searches that pre-admit their working set on a
    /// search-scoped `Charge`).
    fn visit_count(&self, n: u64) -> Result<()> {
        let v = self.visited.get() + n;
        self.visited.set(v);
        self.visited_total.set(self.visited_total.get() + n);
        if v > self.spec.max_visited {
            return Err(bad(format!(
                "a search visited more than {} nodes; raise path:maxVisited, lower path:maxLength or add predicates",
                self.spec.max_visited
            )));
        }
        Ok(())
    }

    /// The neighbours of every node of a sorted, duplicate-free frontier, as
    /// (node, neighbour) pairs: its successors on a path when `succ`, else its
    /// predecessors.
    fn expand(&self, frontier: &[u64], succ: bool, out: &mut Vec<(u64, Step)>) -> Result<()> {
        if self.spec.max_degree.is_none() && !self.sinks {
            return self.expand_all(frontier, succ, out);
        }
        // a literal is not left, and a hub is not expanded unless it is a root
        let mut kept: Option<Vec<u64>> = None;
        for (i, &x) in frontier.iter().enumerate() {
            let skip = if self.sink(x, succ) {
                true
            } else if self.blocked(x)? {
                self.skipped_hubs.set(self.skipped_hubs.get() + 1);
                true
            } else {
                false
            };
            if skip {
                kept.get_or_insert_with(|| frontier[..i].to_vec());
            } else if let Some(k) = &mut kept {
                k.push(x);
            }
        }
        match kept {
            None => self.expand_all(frontier, succ, out),
            Some(k) => self.expand_all(&k, succ, out),
        }
    }

    /// [`expand`](Self::expand) without the check of `path:maxDegree`.
    fn expand_all(&self, frontier: &[u64], succ: bool, out: &mut Vec<(u64, Step)>) -> Result<()> {
        if frontier.is_empty() {
            return Ok(());
        }
        let sides = if succ { &self.succ } else { &self.pred };
        for s in sides {
            let start = out.len();
            // with both directions a self-loop is one edge, read from the first side
            if s.lane.pattern {
                self.pattern_lane(&s.lane, frontier, s.rev, s.skip_self, out);
            } else {
                self.scan_lane(&s.lane, frontier, s.rev, s.skip_self, out, usize::MAX)?;
            }
            // a predecessor's edge leaves it, so a literal is never one. Only a lane read
            // by the subject, or a pattern, gives objects as predecessors.
            if !succ && (s.lane.pattern || s.lane.subject_side) {
                let snap = &self.ctx.snap;
                let mut w = start;
                for r in start..out.len() {
                    if snap.term_kind(Id(out[r].1.node)) != crate::store::TermKind::Literal {
                        out[w] = out[r];
                        w += 1;
                    }
                }
                out.truncate(w);
            }
        }
        self.ctx.check()
    }

    /// Expand one node with pre-admitted growth of `out` on `work`.
    fn expand_one_charged(
        &self,
        x: u64,
        succ: bool,
        out: &mut Vec<(u64, Step)>,
        work: &mut Charge<'a>,
        outer_bytes: u64,
    ) -> Result<()> {
        if self.spec.max_degree.is_some() || self.sinks {
            if self.sink(x, succ) {
                return Ok(());
            }
            if self.blocked(x)? {
                self.skipped_hubs.set(self.skipped_hubs.get() + 1);
                return Ok(());
            }
        }
        self.expand_one_all_charged(x, succ, out, work, outer_bytes)
    }

    /// [`expand_one_charged`](Self::expand_one_charged) without hub or literal filtering.
    fn expand_one_all_charged(
        &self,
        x: u64,
        succ: bool,
        out: &mut Vec<(u64, Step)>,
        work: &mut Charge<'a>,
        outer_bytes: u64,
    ) -> Result<()> {
        let sides = if succ { &self.succ } else { &self.pred };
        let mut sweeps = self.sweeps.get();
        for s in sides {
            let start = out.len();
            if s.lane.pattern {
                let map = if s.lane.subject_side {
                    &self.pattern.by_start
                } else {
                    &self.pattern.by_end
                };
                let mut cur_base =
                    outer_bytes.saturating_add(vec_bytes::<(u64, Step)>(out.capacity()));
                let mut scanned = 0usize;
                for &node in map.get(&x).map_or(&[][..], Vec::as_slice) {
                    scanned += 1;
                    if scanned.is_multiple_of(4096) {
                        self.ctx.check()?;
                    }
                    if !(s.skip_self && node == x) {
                        cur_base = push_vec(
                            out,
                            (
                                x,
                                Step {
                                    node,
                                    p: 0,
                                    rev: s.rev,
                                },
                            ),
                            work,
                            cur_base,
                        )?;
                    }
                }
            } else {
                scan_lane_charged(
                    self.dedup,
                    &mut sweeps,
                    self.ctx,
                    self.spec,
                    &s.lane,
                    &[x],
                    s.rev,
                    s.skip_self,
                    out,
                    usize::MAX,
                    Some((work, outer_bytes)),
                )?;
            }
            if !succ && (s.lane.pattern || s.lane.subject_side) {
                let snap = &self.ctx.snap;
                let mut w = start;
                for r in start..out.len() {
                    if r.is_multiple_of(4096) {
                        self.ctx.check()?;
                    }
                    if snap.term_kind(Id(out[r].1.node)) != crate::store::TermKind::Literal {
                        out[w] = out[r];
                        w += 1;
                    }
                }
                out.truncate(w);
            }
        }
        self.sweeps.set(sweeps);
        self.ctx.check()
    }

    /// [`scan_lane`](Self::scan_lane) for the edges of a nested pattern.
    fn pattern_lane(
        &self,
        lane: &Lane,
        frontier: &[u64],
        rev: bool,
        skip_self: bool,
        out: &mut Vec<(u64, Step)>,
    ) {
        let map = if lane.subject_side {
            &self.pattern.by_start
        } else {
            &self.pattern.by_end
        };
        for &x in frontier {
            for &node in map.get(&x).map_or(&[][..], Vec::as_slice) {
                if !(skip_self && node == x) {
                    out.push((x, Step { node, p: 0, rev }));
                }
            }
        }
    }

    /// The edges of the nodes of `frontier` in one lane. `cap` stops a scan of one node
    /// once `out` holds that many rows (the check of `path:maxDegree`).
    #[allow(clippy::too_many_arguments)]
    fn scan_lane<O: Sink>(
        &self,
        lane: &Lane,
        frontier: &[u64],
        rev: bool,
        skip_self: bool,
        out: &mut O,
        cap: usize,
    ) -> Result<()> {
        let snap = &self.ctx.snap;
        let (kc, nc, pc) = lane.cols();
        let gc = lane.perm.col_of(crate::index::G);
        let prefix: Vec<u64> = lane.p.into_iter().collect();
        let mask = (1 << kc) | (1 << nc) | (1 << gc) | pc.map_or(0, |c| 1 << c);
        let graph = &self.spec.graph;
        let dedup = self.dedup;
        let fixed_p = lane.p.unwrap_or(0);
        // the skipped predicates, in the lanes of every predicate
        let excl: &[u64] = if lane.p.is_none() {
            &self.spec.exclude
        } else {
            &[]
        };
        let mut last: Option<(u64, u64, u64)> = None;
        let mut emit = |x: u64, node: u64, p: u64, g: u64, out: &mut O| {
            if !graph.accepts(g) || (skip_self && node == x) {
                return;
            }
            if !excl.is_empty() && excl.binary_search(&p).is_ok() {
                return;
            }
            if dedup && last == Some((x, node, p)) {
                return;
            }
            last = Some((x, node, p));
            out.put(x, Step { node, p, rev });
        };
        let rows = match lane.p {
            Some(p) => snap.estimate(lane.perm, &[p]),
            None => snap.len(),
        };
        let sweep = self.ctx.opt.batched_paths
            && frontier.len() >= 64
            && (frontier.len() as u64).saturating_mul(SWEEP_RATIO) >= rows;
        if !sweep {
            for &x in frontier {
                let mut key = prefix.clone();
                key.push(x);
                snap.scan_between_cols(lane.perm, pad(&key, 0), pad(&key, u64::MAX), mask, |c| {
                    match c {
                        Chunk::Block(b, s, e) => {
                            for i in s..e {
                                let p = pc.map_or(fixed_p, |c| b.cols[c][i]);
                                emit(x, b.cols[nc][i], p, b.cols[gc][i], out);
                            }
                        }
                        Chunk::Row(k) => emit(x, k[nc], pc.map_or(fixed_p, |c| k[c]), k[gc], out),
                    }
                    if O::CHECKS {
                        self.ctx.check()?;
                    }
                    Ok(out.len() < cap)
                })?;
            }
            return Ok(());
        }
        // one pass over the rows between the first and the last node, merged with the
        // frontier
        self.sweeps.set(self.sweeps.get() + 1);
        let (first, last_node) = (frontier[0], frontier[frontier.len() - 1]);
        let (mut lo, mut hi) = (prefix.clone(), prefix);
        lo.push(first);
        hi.push(last_node);
        let mut j = 0;
        snap.scan_between_cols(lane.perm, pad(&lo, 0), pad(&hi, u64::MAX), mask, |c| {
            match c {
                Chunk::Block(b, s, e) => {
                    let keys = &b.cols[kc][s..e];
                    let mut i = 0;
                    while i < keys.len() && j < frontier.len() {
                        let (k, f) = (keys[i], frontier[j]);
                        if k < f {
                            i += keys[i..].partition_point(|&x| x < f);
                        } else if k > f {
                            j += frontier[j..].partition_point(|&x| x < k);
                        } else {
                            // the run may continue in the next chunk
                            while i < keys.len() && keys[i] == f {
                                let r = s + i;
                                let p = pc.map_or(fixed_p, |c| b.cols[c][r]);
                                emit(f, b.cols[nc][r], p, b.cols[gc][r], out);
                                i += 1;
                            }
                        }
                    }
                }
                Chunk::Row(k) => {
                    j += frontier[j..].partition_point(|&x| x < k[kc]);
                    if j < frontier.len() && frontier[j] == k[kc] {
                        emit(k[kc], k[nc], pc.map_or(fixed_p, |c| k[c]), k[gc], out);
                    }
                }
            }
            Ok(j < frontier.len())
        })?;
        Ok(())
    }

    /// The neighbours of one node, cached for the call.
    fn neighbours(&self, x: u64, succ: bool) -> Result<Rc<[Step]>> {
        // a hub has no neighbours for this search, but it may be another search's root,
        // so the cache keeps the neighbours a root would have
        if self.sink(x, succ) {
            return Ok(Rc::from(Vec::new()));
        }
        if self.blocked(x)? {
            self.skipped_hubs.set(self.skipped_hubs.get() + 1);
            return Ok(Rc::from(Vec::new()));
        }
        if let Some(n) = self.cache.borrow().get(&(x, succ)) {
            return Ok(n.clone());
        }
        let mut out = Vec::new();
        self.expand_all(&[x], succ, &mut out)?;
        let steps: Rc<[Step]> = out.into_iter().map(|(_, s)| s).collect();
        self.charge
            .add(steps.len() as u64 * STEP_BYTES + NODE_BYTES)?;
        self.cache.borrow_mut().insert((x, succ), steps.clone());
        Ok(steps)
    }

    /// Cached neighbours of one node with pre-admitted scan scratch on `work` and
    /// pre-admitted cache storage on `self.charge`.
    fn neighbours_charged(
        &self,
        x: u64,
        succ: bool,
        work: &mut Charge<'a>,
        outer_bytes: u64,
    ) -> Result<Rc<[Step]>> {
        if self.sink(x, succ) {
            return Ok(Rc::from(Vec::new()));
        }
        if self.blocked(x)? {
            self.skipped_hubs.set(self.skipped_hubs.get() + 1);
            return Ok(Rc::from(Vec::new()));
        }
        if let Some(n) = self.cache.borrow().get(&(x, succ)) {
            return Ok(n.clone());
        }
        let mut out = Vec::new();
        self.expand_one_all_charged(x, succ, &mut out, work, outer_bytes)?;
        let rc_bytes = (out.len() as u64)
            .saturating_mul(size_of::<Step>() as u64)
            .saturating_add((2 * size_of::<usize>()) as u64);
        self.charge.add(rc_bytes.saturating_add(NODE_BYTES))?;
        let steps: Rc<[Step]> = out.into_iter().map(|(_, s)| s).collect();
        work.resize(outer_bytes)?;
        self.cache.borrow_mut().insert((x, succ), steps.clone());
        Ok(steps)
    }

    /// The weight of an edge: the smallest value of the weight property on a reifier of
    /// its triple term, else the default.
    fn weight(&self, e: &Edge) -> Result<f64> {
        let Some(w) = self.spec.weight else {
            return Ok(1.0);
        };
        // no reifier or no weight property in the store: every edge has the default
        if w.is_none() || self.spec.reifies.is_none() {
            return Ok(self.spec.default_weight);
        }
        let key = e.triple();
        if let Some(&x) = self.weights.borrow().get(&key) {
            return Ok(x);
        }
        let x = self.lookup_weight(w, key)?;
        self.charge.add(NODE_BYTES)?;
        self.weights.borrow_mut().insert(key, x);
        Ok(x)
    }

    fn lookup_weight(&self, w: Option<u64>, (s, p, o): (u64, u64, u64)) -> Result<f64> {
        let default = self.spec.default_weight;
        let (Some(w), Some(reifies)) = (w, self.spec.reifies) else {
            return Ok(default);
        };
        let ctx = self.ctx;
        let tt = {
            let decode = ctx.charge(0)?;
            let terms = (
                ctx.term_checked_into(Id(s), &decode)?,
                ctx.term_checked_into(Id(p), &decode)?,
                ctx.term_checked_into(Id(o), &decode)?,
            );
            let (Some(s), Some(Term::NamedNode(p)), Some(o)) = terms else {
                return Ok(default);
            };
            let s = match s {
                Term::NamedNode(n) => oxrdf::NamedOrBlankNode::NamedNode(n),
                Term::BlankNode(b) => oxrdf::NamedOrBlankNode::BlankNode(b),
                _ => return Ok(default),
            };
            let tt = Term::Triple(Box::new(oxrdf::Triple::new(s, p, o)));
            let Some(tt) = ctx.snap.lookup_term(&tt) else {
                return Ok(default);
            };
            tt
        };
        let graph = &self.spec.graph;
        let mut best: Option<f64> = None;
        let mut last_reifier: Option<u64> = None;
        let mut scanned = 0usize;
        let scan_weights = |reifier: u64, best: &mut Option<f64>| -> Result<()> {
            let mut inner_scanned = 0usize;
            let mut handle_weight = |val_id: u64, g: u64| -> Result<()> {
                if !graph.accepts(g) {
                    return Ok(());
                }
                let v = ctx
                    .value_checked(Id(val_id))?
                    .as_ref()
                    .and_then(super::value::approx_f64);
                match v {
                    Some(v) if v >= 0.0 && v.is_finite() => {
                        *best = Some(best.map_or(v, |b: f64| b.min(v)));
                        Ok(())
                    }
                    _ => {
                        let t = ctx
                            .plan_term(Id(val_id))
                            .map_or_else(|| "?".to_string(), |t| t.to_string());
                        Err(bad(format!("the weight {t} is not a non-negative number")))
                    }
                }
            };
            ctx.snap.scan_checked(
                Perm::Pso,
                &[w, reifier],
                || ctx.check(),
                |c| {
                    match c {
                        Chunk::Block(b, s_idx, e_idx) => {
                            for i in s_idx..e_idx {
                                inner_scanned += 1;
                                if inner_scanned.is_multiple_of(4096) {
                                    ctx.check()?;
                                }
                                handle_weight(b.cols[2][i], b.cols[3][i])?;
                            }
                        }
                        Chunk::Row(k) => {
                            inner_scanned += 1;
                            if inner_scanned.is_multiple_of(4096) {
                                ctx.check()?;
                            }
                            handle_weight(k[2], k[3])?;
                        }
                    }
                    Ok(true)
                },
            )
        };
        ctx.snap.scan_checked(
            Perm::Pos,
            &[reifies, tt.0],
            || ctx.check(),
            |c| {
                match c {
                    Chunk::Block(b, s_idx, e_idx) => {
                        for i in s_idx..e_idx {
                            scanned += 1;
                            if scanned.is_multiple_of(4096) {
                                ctx.check()?;
                            }
                            if graph.accepts(b.cols[3][i]) {
                                let r = b.cols[2][i];
                                if last_reifier != Some(r) {
                                    last_reifier = Some(r);
                                    scan_weights(r, &mut best)?;
                                }
                            }
                        }
                    }
                    Chunk::Row(k) => {
                        scanned += 1;
                        if scanned.is_multiple_of(4096) {
                            ctx.check()?;
                        }
                        if graph.accepts(k[3]) {
                            let r = k[2];
                            if last_reifier != Some(r) {
                                last_reifier = Some(r);
                                scan_weights(r, &mut best)?;
                            }
                        }
                    }
                }
                Ok(true)
            },
        )?;
        Ok(best.unwrap_or(default))
    }

    /// A path's cost as reported: the double-double sum of its weights from the start,
    /// rounded to a double, so that equally cheap paths report the same cost whatever
    /// order their weights come in.
    fn path_cost(&self, edges: &[Edge]) -> Result<f64> {
        Ok(self.exact_cost(edges, |x| x)?.hi)
    }

    /// Grow a breadth-first tree by one level. `virt` is the node the virtual id stands
    /// for: when the tree's root is virtual it is expanded as that node, and when
    /// `close` is set a neighbour equal to it is recorded as the virtual id (the end of
    /// a cycle) and never expanded. Returns the nodes of the new level.
    #[allow(clippy::too_many_arguments)]
    fn grow(
        &self,
        tree: &mut Tree,
        frontier: &[u64],
        depth: u32,
        all: bool,
        virt: u64,
        close: bool,
        bans: Option<&Bans>,
    ) -> Result<Vec<u64>> {
        let mut scan: Vec<u64> = Vec::with_capacity(frontier.len());
        let mut virt_root = false;
        for &x in frontier {
            if x == VIRT {
                // the virtual root of a backward tree; a virtual end is never expanded
                if tree.root == VIRT {
                    virt_root = true;
                }
            } else if !(tree.root == VIRT && x == virt) {
                scan.push(x);
            }
        }
        let mut found = Vec::new();
        self.expand(&scan, tree.forward, &mut found)?;
        if virt_root {
            let mut more = Vec::new();
            self.expand(&[virt], tree.forward, &mut more)?;
            found.extend(more.into_iter().map(|(_, s)| (VIRT, s)));
        }
        let mut next = Vec::new();
        for (x, st) in found {
            let y = if close && st.node == virt {
                VIRT
            } else {
                st.node
            };
            if y == tree.root {
                continue;
            }
            if let Some(b) = bans {
                let e = if tree.forward {
                    Edge {
                        from: x,
                        to: y,
                        p: st.p,
                        rev: st.rev,
                    }
                } else {
                    Edge {
                        from: y,
                        to: x,
                        p: st.p,
                        rev: st.rev,
                    }
                };
                if b.nodes.contains(&y) || b.edges.contains(&e) {
                    continue;
                }
            }
            let parent = Parent {
                node: x,
                p: st.p,
                rev: st.rev,
            };
            match tree.first.entry(y) {
                std::collections::hash_map::Entry::Vacant(v) => {
                    v.insert((parent, depth + 1));
                    next.push(y);
                }
                std::collections::hash_map::Entry::Occupied(o) if all && o.get().1 == depth + 1 => {
                    tree.extra.entry(y).or_default().push(parent);
                }
                std::collections::hash_map::Entry::Occupied(_) => {}
            }
        }
        self.visit(next.len() as u64)?;
        next.sort_unstable();
        tree.order.extend(next.iter().map(|&y| (y, depth + 1)));
        Ok(next)
    }

    /// Unweighted shortest paths between one pair by bidirectional breadth-first
    /// search.
    fn bidirectional(&self, s: u64, t: u64, all: bool, cap: usize) -> Result<Vec<Vec<Edge>>> {
        let tt = if s == t { VIRT } else { t };
        let real = |x: u64| if x == VIRT { s } else { x };
        Ok(self
            .bidi(s, tt, s, all, cap, None, self.spec.max_len)?
            .into_iter()
            .map(|p| p.into_iter().map(|e| e.map(real)).collect())
            .collect())
    }

    /// Bidirectional breadth-first search from `s` to `tt`, which is the virtual id
    /// when the path closes a cycle at `virt`. Paths keep the virtual id. `bans` are
    /// the nodes and edges a spur search of Yen's algorithm may not use.
    #[allow(clippy::too_many_arguments)]
    fn bidi(
        &self,
        s: u64,
        tt: u64,
        virt: u64,
        all: bool,
        cap: usize,
        bans: Option<&Bans>,
        max: Option<u32>,
    ) -> Result<Vec<Vec<Edge>>> {
        let cycle = tt == VIRT;
        self.set_roots(&[s, if cycle { virt } else { tt }]);
        let mut f = Tree::new(s, true);
        let mut b = Tree::new(tt, false);
        let (mut ff, mut bf) = (vec![s], vec![tt]);
        let (mut fd, mut bd) = (0u32, 0u32);
        let max = max.unwrap_or(u32::MAX);
        self.visit(2)?;
        loop {
            if fd.saturating_add(bd) >= max || ff.is_empty() || bf.is_empty() {
                return Ok(Vec::new());
            }
            let forward = ff.len() <= bf.len();
            let (new, other) = if forward {
                ff = self.grow(&mut f, &ff, fd, all, virt, cycle, bans)?;
                fd += 1;
                (&ff, &b)
            } else {
                bf = self.grow(&mut b, &bf, bd, all, virt, false, bans)?;
                bd += 1;
                (&bf, &f)
            };
            let mut meet: Vec<u64> = new.iter().copied().filter(|y| other.contains(*y)).collect();
            // a path may not pass through a hub or a literal, and neither side expanded
            // one
            if self.spec.max_degree.is_some() || self.sinks {
                let mut kept = Vec::with_capacity(meet.len());
                for m in meet {
                    if m == s || m == tt || !(self.sink(m, true) || self.blocked(m)?) {
                        kept.push(m);
                    }
                }
                meet = kept;
            }
            if meet.is_empty() {
                continue;
            }
            let mut out = Vec::new();
            for m in meet {
                let left = cap.saturating_sub(out.len());
                if left == 0 {
                    break;
                }
                let fp = f.paths(m, all, left, None, &|| self.ctx.check())?;
                let bp = b.paths(m, all, left, None, &|| self.ctx.check())?;
                'combine: for a in &fp {
                    for z in &bp {
                        let mut e: Vec<Edge> = a.iter().chain(z).copied().collect();
                        e.shrink_to_fit();
                        out.push(e);
                        if out.len() >= cap {
                            break 'combine;
                        }
                    }
                }
                if !all {
                    break;
                }
            }
            return Ok(out);
        }
    }

    /// A breadth-first search from `root` (forward when `forward`), to the nodes of
    /// `targets` or, without targets, to every node. Returns the tree; `close` records
    /// the root reached again as the virtual id.
    fn bfs(
        &self,
        root: u64,
        forward: bool,
        targets: Option<&FxHashSet<u64>>,
        all: bool,
        close: bool,
    ) -> Result<Tree> {
        self.set_roots(&[root]);
        let mut tree = Tree::new(root, forward);
        let mut frontier = vec![root];
        let mut depth = 0u32;
        let max = self.spec.max_len.unwrap_or(u32::MAX);
        let mut missing: FxHashSet<u64> = targets
            .map(|t| {
                t.iter()
                    .map(|&x| if close && x == root { VIRT } else { x })
                    .collect()
            })
            .unwrap_or_default();
        self.visit(1)?;
        while !frontier.is_empty() && depth < max {
            frontier = self.grow(&mut tree, &frontier, depth, all, root, close, None)?;
            depth += 1;
            if targets.is_some() {
                for y in &frontier {
                    missing.remove(y);
                }
                if missing.is_empty() {
                    break;
                }
            }
        }
        Ok(tree)
    }

    /// Dijkstra's algorithm from `root`, until every target is settled (or every node,
    /// without targets). `banned` nodes and edges are skipped (Yen's spur searches).
    #[allow(clippy::too_many_arguments)]
    fn dijkstra(
        &self,
        root: u64,
        forward: bool,
        targets: Option<&FxHashSet<u64>>,
        all: bool,
        close: bool,
        banned_nodes: Option<&FxHashSet<u64>>,
        banned_edges: Option<&FxHashSet<Edge>>,
        virt: u64,
    ) -> Result<(Tree, FxHashMap<u64, Cost>)> {
        self.set_roots(&[if root == VIRT { virt } else { root }]);
        let mut tree = Tree::new(root, forward);
        // double-doubles, so that ties are decided on the weights' sums and not on the
        // order the search added them in, as in the search from both ends
        let mut dist: FxHashMap<u64, Cost> = FxHashMap::default();
        let mut settled: FxHashSet<u64> = FxHashSet::default();
        let mut heap = BinaryHeap::new();
        let mut missing: FxHashSet<u64> = targets
            .map(|t| {
                t.iter()
                    .map(|&x| if close && x == virt { VIRT } else { x })
                    .collect()
            })
            .unwrap_or_default();
        dist.insert(root, Cost::ZERO);
        heap.push(Reverse((Cost::ZERO, root)));
        let mut n = 0u64;
        // once every target is settled, a search for all the cheapest paths goes on
        // through the nodes of the last cost: a zero-weight edge from one of them can
        // still reach a settled target at that cost
        let mut last_cost: Option<Cost> = None;
        while let Some(Reverse((d, u))) = heap.pop() {
            if last_cost.is_some_and(|c| d > c) {
                break;
            }
            if !settled.insert(u) {
                continue;
            }
            if u != root {
                tree.order.push((u, 0));
            }
            self.visit(1)?;
            n += 1;
            if n.is_multiple_of(4096) {
                self.ctx.check()?;
            }
            if targets.is_some() {
                missing.remove(&u);
                if missing.is_empty() {
                    if !all {
                        break;
                    }
                    last_cost = Some(d);
                }
            }
            // a virtual end is not expanded
            if u == VIRT && root != VIRT {
                continue;
            }
            let scan = if u == VIRT { virt } else { u };
            // a plain search expands each node once; Yen's spur searches revisit them
            let steps: Rc<[Step]> = if banned_nodes.is_some() {
                self.neighbours(scan, forward)?
            } else {
                let mut out = Vec::new();
                self.expand(&[scan], forward, &mut out)?;
                out.into_iter().map(|(_, s)| s).collect()
            };
            for st in steps.iter() {
                let y = if close && st.node == virt {
                    VIRT
                } else {
                    st.node
                };
                if banned_nodes.is_some_and(|b| b.contains(&y)) {
                    continue;
                }
                // a settled node has its final cost, and only a parent of the same cost
                // is still of use, which a zero-weight edge from a node settled later can be
                let tied = settled.contains(&y);
                if tied && !all {
                    continue;
                }
                let e = if forward {
                    Edge {
                        from: u,
                        to: y,
                        p: st.p,
                        rev: st.rev,
                    }
                } else {
                    Edge {
                        from: y,
                        to: u,
                        p: st.p,
                        rev: st.rev,
                    }
                };
                if banned_edges.is_some_and(|b| b.contains(&e)) {
                    continue;
                }
                let real = |x: u64| if x == VIRT { virt } else { x };
                let nd = d.add(Cost::of(self.weight(&e.map(real))?));
                let parent = Parent {
                    node: u,
                    p: st.p,
                    rev: st.rev,
                };
                match dist.get(&y) {
                    Some(&cur) if nd > cur => {}
                    Some(&cur) if nd == cur => {
                        if all && y != root {
                            tree.extra.entry(y).or_default().push(parent);
                        }
                    }
                    _ if tied => {}
                    _ => {
                        dist.insert(y, nd);
                        tree.first.insert(y, (parent, 0));
                        tree.extra.remove(&y);
                        heap.push(Reverse((nd, y)));
                    }
                }
            }
        }
        // only settled nodes have final parents
        tree.first.retain(|k, _| settled.contains(k));
        dist.retain(|k, _| settled.contains(k));
        Ok((tree, dist))
    }

    /// The cheapest path from `s` to `t`, two different real nodes, by Dijkstra's
    /// algorithm from both ends (ROADMAP item 150). The forward search settles nodes by
    /// their cost from `s` and the backward search by their cost to `t`, and the side with
    /// the smaller queue goes next. Every edge either side relaxes onto a node the other
    /// side has reached gives a path, and the cheapest one so far is kept. The search
    /// stops once the two queues' least costs add up to at least that path's cost, since
    /// no path through an unsettled node can be cheaper, so on a graph where most paths
    /// are short each side settles only the nodes near its end. A meeting node must be
    /// one a path may pass through: not a hub and not a literal, unless it is an end.
    /// `banned` nodes and edges are skipped on both sides, for Yen's spur searches.
    fn dijkstra_pair(
        &self,
        s: u64,
        t: u64,
        banned_nodes: Option<&FxHashSet<u64>>,
        banned_edges: Option<&FxHashSet<Edge>>,
    ) -> Result<Option<(Vec<Edge>, f64)>> {
        debug_assert!(s != t && s != VIRT && t != VIRT);
        self.set_roots(&[s, t]);
        let mut trees = [Tree::new(s, true), Tree::new(t, false)];
        // the costs are double-doubles, so both ends add a path's weights alike
        let mut dist: [FxHashMap<u64, Cost>; 2] = Default::default();
        let mut settled: [FxHashSet<u64>; 2] = Default::default();
        let mut heaps: [BinaryHeap<Reverse<(Cost, u64)>>; 2] = Default::default();
        for (i, r) in [s, t].into_iter().enumerate() {
            dist[i].insert(r, Cost::ZERO);
            heaps[i].push(Reverse((Cost::ZERO, r)));
        }
        // the cheapest path found so far, by its cost and the node where the sides meet
        let mut best: Option<(Cost, u64)> = None;
        let mut n = 0u64;
        loop {
            for i in 0..2 {
                while let Some(&Reverse((_, u))) = heaps[i].peek() {
                    if !settled[i].contains(&u) {
                        break;
                    }
                    heaps[i].pop();
                }
            }
            let (Some(&Reverse((df, _))), Some(&Reverse((db, _)))) =
                (heaps[0].peek(), heaps[1].peek())
            else {
                // one side has settled everything it can reach
                break;
            };
            if best.is_some_and(|(mu, _)| df.add(db) >= mu) {
                break;
            }
            let side = usize::from(heaps[0].len() > heaps[1].len());
            let forward = side == 0;
            let Reverse((d, u)) = heaps[side].pop().unwrap();
            settled[side].insert(u);
            if u != trees[side].root {
                trees[side].order.push((u, 0));
            }
            self.visit(1)?;
            n += 1;
            if n.is_multiple_of(4096) {
                self.ctx.check()?;
            }
            // a plain search expands each node once; Yen's spur searches revisit them
            let steps: Rc<[Step]> = if banned_nodes.is_some() {
                self.neighbours(u, forward)?
            } else {
                let mut out = Vec::new();
                self.expand(&[u], forward, &mut out)?;
                out.into_iter().map(|(_, s)| s).collect()
            };
            for st in steps.iter() {
                let y = st.node;
                if settled[side].contains(&y) || banned_nodes.is_some_and(|b| b.contains(&y)) {
                    continue;
                }
                let e = if forward {
                    Edge {
                        from: u,
                        to: y,
                        p: st.p,
                        rev: st.rev,
                    }
                } else {
                    Edge {
                        from: y,
                        to: u,
                        p: st.p,
                        rev: st.rev,
                    }
                };
                if banned_edges.is_some_and(|b| b.contains(&e)) {
                    continue;
                }
                let nd = d.add(Cost::of(self.weight(&e)?));
                if dist[side].get(&y).is_some_and(|&c| nd >= c) {
                    continue;
                }
                dist[side].insert(y, nd);
                trees[side].first.insert(
                    y,
                    (
                        Parent {
                            node: u,
                            p: st.p,
                            rev: st.rev,
                        },
                        0,
                    ),
                );
                heaps[side].push(Reverse((nd, y)));
                if let Some(&o) = dist[1 - side].get(&y)
                    && best.is_none_or(|(mu, _)| nd.add(o) < mu)
                    && (y == s || y == t || !(self.sink(y, true) || self.blocked(y)?))
                {
                    best = Some((nd.add(o), y));
                }
            }
        }
        let Some((_, m)) = best else {
            return Ok(None);
        };
        // the parents of a node only ever get cheaper, so the two halves through the
        // meeting node cost at most the path found, which no path beats
        let check = || self.ctx.check();
        let (Some(a), Some(z)) = (
            trees[0].paths(m, false, 1, None, &check)?.pop(),
            trees[1].paths(m, false, 1, None, &check)?.pop(),
        ) else {
            return Ok(None);
        };
        let path = without_loops(s, a.into_iter().chain(z).collect());
        let cost = self.path_cost(&path)?;
        Ok(Some((path, cost)))
    }

    /// Every cheapest path from `s` to `t` by Dijkstra's algorithm from both ends
    /// (ROADMAP item 277), given to `emit` one at a time until it returns false, so
    /// `path:shortest` takes the first. When `s`
    /// and `t` are the same node the paths are the cheapest cycles through it, and the
    /// backward search starts from the virtual id, which stands for `s` as the end.
    ///
    /// Both sides keep every parent of equal cost, as [`dijkstra`](Self::dijkstra) does,
    /// and neither expands the other's root, since no path passes through an end. The
    /// search stops once the least costs of the two queues add up to more than the
    /// cheapest meeting and both are above zero. Every node with a cost below its side's
    /// least queued cost, `Tf` forward and `Tb` backward, has then been settled with its
    /// final parents. Call these nodes the forward and the backward region. A node on a
    /// cheapest path has `df + db = μ < Tf + Tb`, so it lies in one of the regions. Costs
    /// rise along a cheapest path from `s`, so the nodes of the forward region on it form
    /// a prefix, and the rest lie in the backward region. Each cheapest path is therefore
    /// the forward region's cheapest path to its last node `u` in that region, the edge to
    /// the next node `v`, and the backward region's cheapest path from `v` to `t`, or a
    /// path inside the forward region. The halves are joined only at such an edge, and
    /// only with backward halves that stay out of the forward region. Each path then has
    /// one split, so no path is found twice, and the halves share no node, so it is simple.
    fn dijkstra_pair_all(
        &self,
        s: u64,
        t: u64,
        all: bool,
        emit: &mut dyn FnMut(Vec<Edge>) -> Result<bool>,
    ) -> Result<bool> {
        let cycle = s == t;
        let tt = if cycle { VIRT } else { t };
        let real = |x: u64| if x == VIRT { s } else { x };
        self.set_roots(&[s, t]);
        let mut trees = [Tree::new(s, true), Tree::new(tt, false)];
        let mut dist: [FxHashMap<u64, Cost>; 2] = Default::default();
        let mut settled: [FxHashSet<u64>; 2] = Default::default();
        let mut heaps: [BinaryHeap<Reverse<(Cost, u64)>>; 2] = Default::default();
        for (i, r) in [s, tt].into_iter().enumerate() {
            dist[i].insert(r, Cost::ZERO);
            heaps[i].push(Reverse((Cost::ZERO, r)));
        }
        // a node a path may pass through: an end, or a node neither a hub nor a literal
        let passable = |x: u64| -> Result<bool> {
            Ok(x == s || x == tt || !(self.sink(x, true) || self.blocked(x)?))
        };
        // the edges, in path order, that join a node the forward side reached to one the
        // backward side reached, and the cheapest path through one of them so far
        let mut meets: FxHashSet<Edge> = FxHashSet::default();
        let mut best: Option<Cost> = None;
        let mut best_edge: Option<Edge> = None;
        let mut n = 0u64;
        // the least cost in each queue, infinite when it is empty
        let mut tops = [Cost::INFINITE; 2];
        // the smallest and largest positive weights relaxed
        let (mut wmin, mut wmax) = (f64::INFINITY, 0.0f64);
        loop {
            for i in 0..2 {
                while let Some(&Reverse((_, u))) = heaps[i].peek() {
                    if !settled[i].contains(&u) {
                        break;
                    }
                    heaps[i].pop();
                }
                tops[i] = heaps[i].peek().map_or(Cost::INFINITE, |r| r.0.0);
            }
            // a side whose queue is empty has settled every node it reaches. Without a
            // meeting there is then no path. With one, the other side goes on until its
            // least cost is above zero, so that its region holds at least its root.
            let empty = [heaps[0].is_empty(), heaps[1].is_empty()];
            if (empty[0] && empty[1]) || (best.is_none() && (empty[0] || empty[1])) {
                break;
            }
            // one path needs no ties, so the test of the cheapest path from both ends
            // suffices
            if !all && best.is_some_and(|mu| tops[0].add(tops[1]) >= mu) {
                break;
            }
            if best.is_some_and(|mu| tops[0].add(tops[1]) > mu)
                && tops[0] > Cost::ZERO
                && tops[1] > Cost::ZERO
            {
                break;
            }
            let side = if empty[0] || empty[1] {
                usize::from(empty[0])
            } else {
                usize::from(heaps[0].len() > heaps[1].len())
            };
            let forward = side == 0;
            let Reverse((d, u)) = heaps[side].pop().unwrap();
            settled[side].insert(u);
            if u != trees[side].root {
                trees[side].order.push((u, 0));
            }
            self.visit(1)?;
            n += 1;
            if n.is_multiple_of(4096) {
                self.ctx.check()?;
            }
            // no path passes through an end
            if u == if forward { tt } else { s } {
                continue;
            }
            let scan = real(u);
            let mut out = Vec::new();
            self.expand(&[scan], forward, &mut out)?;
            for (_, st) in out {
                let y = if forward && cycle && st.node == s {
                    VIRT
                } else {
                    st.node
                };
                if y == trees[side].root {
                    continue;
                }
                let e = if forward {
                    Edge {
                        from: u,
                        to: y,
                        p: st.p,
                        rev: st.rev,
                    }
                } else {
                    Edge {
                        from: y,
                        to: u,
                        p: st.p,
                        rev: st.rev,
                    }
                };
                let wv = self.weight(&e.map(real))?;
                if wv > 0.0 {
                    wmin = wmin.min(wv);
                    wmax = wmax.max(wv);
                }
                let w = Cost::of(wv);
                let nd = d.add(w);
                let parent = Parent {
                    node: u,
                    p: st.p,
                    rev: st.rev,
                };
                match dist[side].get(&y) {
                    Some(&cur) if nd > cur => {}
                    Some(&cur) if nd == cur => {
                        if all {
                            trees[side].extra.entry(y).or_default().push(parent);
                        }
                    }
                    // a settled node has its final cost
                    _ if settled[side].contains(&y) => {}
                    _ => {
                        dist[side].insert(y, nd);
                        trees[side].first.insert(y, (parent, 0));
                        trees[side].extra.remove(&y);
                        heaps[side].push(Reverse((nd, y)));
                    }
                }
                if let Some(&o) = dist[1 - side].get(&y)
                    && passable(e.from)?
                    && passable(e.to)?
                {
                    // the cost of a path through the edge, added in path order from the
                    // forward end's cost, whichever side found it
                    let c = if forward { nd.add(o) } else { o.add(w).add(d) };
                    if best.is_none_or(|mu| c < mu) {
                        best = Some(c);
                        best_edge = Some(e);
                    }
                    if all && meets.insert(e) {
                        self.charge.add(STEP_BYTES)?;
                    }
                }
            }
        }
        if best.is_none() {
            return Ok(true);
        }
        // The sums are exact while the weights lie within 2^28 of each other, since a
        // search visits fewer than 2^24 nodes by default and a double-double holds about
        // 106 bits. Otherwise the two sides can round a path's cost apart, so the search
        // from the source alone answers, which adds every cost in one order.
        if wmax > wmin * WIDE_WEIGHTS {
            return Ok(false);
        }
        let to_real = |p: Vec<Edge>| -> Vec<Edge> { p.into_iter().map(|e| e.map(real)).collect() };
        let check = || self.ctx.check();
        if !all {
            // path:shortest: the first parents through the cheapest meeting, which only
            // got cheaper after it was found, so the halves cost at most the meeting's
            // cost. Zero-weight edges can make them share a node, which is cut out.
            let (Some(mu), Some(e)) = (best, best_edge) else {
                return Ok(false);
            };
            if mu.hi.is_infinite() {
                return Ok(false);
            }
            let (Some(a), Some(z)) = (
                trees[0].paths(e.from, false, 1, None, &check)?.pop(),
                trees[1].paths(e.to, false, 1, None, &check)?.pop(),
            ) else {
                return Ok(false);
            };
            let mut p = a;
            p.push(e);
            p.extend(z);
            emit(to_real(without_loops(s, p)))?;
            return Ok(true);
        }
        // only settled nodes have final parents
        for i in 0..2 {
            trees[i].first.retain(|k, _| settled[i].contains(k));
            dist[i].retain(|k, _| settled[i].contains(k));
        }
        let region = |i: usize, x: u64| dist[i].get(&x).is_some_and(|&c| c < tops[i]);
        // the cheapest cost among the joins, from the final costs
        let at_end = dist[0].get(&tt).copied().filter(|_| region(0, tt));
        let mut joins: Vec<(Edge, Cost)> = Vec::new();
        for &e in &meets {
            if !region(0, e.from) || region(0, e.to) || !region(1, e.to) {
                continue;
            }
            let w = Cost::of(self.weight(&e.map(real))?);
            joins.push((e, dist[0][&e.from].add(w).add(dist[1][&e.to])));
        }
        drop(meets);
        // a meeting with no join, or a cost past the largest double, leaves the answer
        // to the search from the source alone
        let Some(mu) = joins.iter().map(|j| j.1).chain(at_end).min() else {
            return Ok(false);
        };
        if mu.hi.is_infinite() {
            return Ok(false);
        }
        joins.retain(|j| j.1 == mu);
        joins.sort_unstable_by_key(|(e, _)| (e.from, e.to, e.p, e.rev));
        // path:shortest follows first parents, which reach the root in linear time
        let prunes = [
            self.prune(&trees[0], Some(&dist[0]), all)?,
            self.prune(&trees[1], Some(&dist[1]), all)?,
        ];
        let cap = |left: usize| if all { left } else { left.min(1) };
        if at_end == Some(mu) {
            let left = self.left.get();
            for p in trees[0].paths(tt, all, cap(left), prunes[0].as_ref(), &check)? {
                if !emit(to_real(p))? {
                    return Ok(true);
                }
            }
        }
        // the backward halves from each node, kept while their joins are emitted
        let mut back: FxHashMap<u64, Rc<[Vec<Edge>]>> = FxHashMap::default();
        // the product of the halves can be far larger than either, so its paths count
        // against the deadline and cancellation as the halves' steps do
        let mut joined = Budget {
            steps: 0,
            check: &check,
        };
        let mut i = 0;
        while i < joins.len() {
            let u = joins[i].0.from;
            let left = self.left.get();
            if left == 0 {
                return Ok(true);
            }
            let front = trees[0].paths(u, all, cap(left), prunes[0].as_ref(), &check)?;
            while i < joins.len() && joins[i].0.from == u {
                let e = joins[i].0;
                i += 1;
                let v = e.to;
                let halves = match back.get(&v) {
                    Some(h) => h.clone(),
                    None => {
                        let mut h =
                            trees[1].paths(v, all, cap(left), prunes[1].as_ref(), &check)?;
                        // a half that enters the forward region belongs to another split
                        h.retain(|p| p.iter().all(|x| !region(0, x.to)));
                        self.charge
                            .add(h.iter().map(|p| p.len() as u64 * 32 + 24).sum())?;
                        let h: Rc<[Vec<Edge>]> = h.into();
                        back.insert(v, h.clone());
                        h
                    }
                };
                if !join_halves(&front, e, &halves, &mut joined, &mut |p| emit(to_real(p)))? {
                    return Ok(true);
                }
            }
        }
        Ok(true)
    }

    /// Pareto-pruned weighted shortest paths of at most `max_len` edges from `root`
    /// (`forward`) or into `root` (`!forward`) to every reachable node (ROADMAP item 275).
    /// Labels are popped in non-decreasing `(cost, len)` order and kept only when no
    /// already-settled label at the node is shorter or equal in length (since it is also
    /// cheaper or equal in cost). Because lengths strictly increase along every edge,
    /// parent links form a DAG and each node's first settled label yields a simple path of
    /// minimum cost within `max_len`. Working memory is pre-admitted on a search-scoped
    /// `Charge` that stays live for the returned `(order, parent)` tree.
    fn dijkstra_bounded(
        &self,
        root: u64,
        forward: bool,
        max_len: u32,
    ) -> Result<(BoundedTreeOutcome, Charge<'a>)> {
        self.set_roots(&[root]);
        let mut work = self.ctx.charge(0)?;
        if max_len == 0 {
            return Ok((
                BoundedTreeOutcome::Tree((Vec::new(), HbMap::default())),
                work,
            ));
        }
        let real = |x: u64| if x == VIRT { root } else { x };
        let mut best_len: HbMap<u64, u32> = HbMap::default();
        let mut dist: HbMap<u64, Vec<(u32, Cost)>> = HbMap::default();
        let mut dist_vec_bytes = 0u64;
        let mut parent: HbMap<(u64, u32), Parent> = HbMap::default();
        let mut order: Vec<(u64, u32)> = Vec::new();
        let mut heap: BoundedHeap = BinaryHeap::new();
        let mut direct_multi: HbMap<u64, u32> = HbMap::default();
        let mut backtrack_parent: HbMap<u64, u64> = HbMap::default();
        let mut virt_first_len: Option<u32> = None;
        let mut best_virt: Option<(Cost, u32)> = None;
        let (mut wmin, mut wmax) = (f64::INFINITY, 0.0f64);
        let mut wide = false;
        let track_reverse_wide = !forward && max_len > 1;
        #[allow(clippy::too_many_arguments)]
        let local_bytes = |best_len: &HbMap<u64, u32>,
                           dist: &HbMap<u64, Vec<(u32, Cost)>>,
                           dist_vec_bytes: u64,
                           parent: &HbMap<(u64, u32), Parent>,
                           order: &Vec<(u64, u32)>,
                           heap: &BoundedHeap,
                           direct_multi: &HbMap<u64, u32>,
                           backtrack_parent: &HbMap<u64, u64>| {
            map_bytes(best_len)
                .saturating_add(map_bytes(dist))
                .saturating_add(dist_vec_bytes)
                .saturating_add(map_bytes(parent))
                .saturating_add(vec_bytes::<(u64, u32)>(order.capacity()))
                .saturating_add(vec_bytes::<Reverse<(Cost, u32, u64)>>(heap.capacity()))
                .saturating_add(map_bytes(direct_multi))
                .saturating_add(map_bytes(backtrack_parent))
        };
        let mut base = 0u64;
        (_, base) = pareto_insert_charged(
            &mut dist,
            &mut dist_vec_bytes,
            root,
            0,
            Cost::ZERO,
            &mut work,
            base,
        )?;
        let _ = push_heap(&mut heap, Reverse((Cost::ZERO, 0, root)), &mut work, base)?;
        let mut n = 0u64;
        while let Some(Reverse((d, l, u))) = heap.pop() {
            n += 1;
            if n.is_multiple_of(4096) {
                self.ctx.check()?;
            }
            if best_len.get(&u).is_some_and(|&bl| u == VIRT || l >= bl) {
                continue;
            }
            let cur_base = local_bytes(
                &best_len,
                &dist,
                dist_vec_bytes,
                &parent,
                &order,
                &heap,
                &direct_multi,
                &backtrack_parent,
            );
            let (prev, next_base) = insert_hb_map(&mut best_len, u, l, &mut work, cur_base)?;
            if prev.is_none() && u != root {
                let _ = push_vec(&mut order, (u, l), &mut work, next_base)?;
            }
            self.visit_count(1)?;
            if u == VIRT || l >= max_len {
                continue;
            }
            let loc = local_bytes(
                &best_len,
                &dist,
                dist_vec_bytes,
                &parent,
                &order,
                &heap,
                &direct_multi,
                &backtrack_parent,
            );
            let mut out_steps = Vec::new();
            self.expand_one_charged(u, forward, &mut out_steps, &mut work, loc)?;
            let out_bytes = vec_bytes::<(u64, Step)>(out_steps.capacity());
            let nl = l + 1;
            for (idx, (_, st)) in out_steps.iter().copied().enumerate() {
                if idx.is_multiple_of(4096) {
                    #[cfg(any(test, feature = "failpoints"))]
                    test_hooks::fire_traversal_poll();
                    self.ctx.check()?;
                }
                let y = if st.node == root { VIRT } else { st.node };
                if y == u {
                    continue;
                }
                if track_reverse_wide {
                    let cur_base = local_bytes(
                        &best_len,
                        &dist,
                        dist_vec_bytes,
                        &parent,
                        &order,
                        &heap,
                        &direct_multi,
                        &backtrack_parent,
                    )
                    .saturating_add(out_bytes);
                    let _ = record_reverse_multi_relax(
                        u,
                        l,
                        y,
                        nl,
                        &parent,
                        &dist,
                        &mut virt_first_len,
                        &mut direct_multi,
                        &mut backtrack_parent,
                        &mut work,
                        cur_base,
                    )?;
                }
                if best_len.get(&y).is_some_and(|&bl| y == VIRT || nl >= bl) {
                    continue;
                }
                let e = if forward {
                    Edge {
                        from: u,
                        to: y,
                        p: st.p,
                        rev: st.rev,
                    }
                } else {
                    Edge {
                        from: y,
                        to: u,
                        p: st.p,
                        rev: st.rev,
                    }
                };
                let wv = self.weight(&e.map(real))?;
                if track_reverse_wide && !wide && wv > 0.0 {
                    wmin = wmin.min(wv);
                    wmax = wmax.max(wv);
                    if bounded_weights_too_wide(max_len, wmin, wmax) {
                        wide = true;
                    }
                }
                let nd = d.add(Cost::of(wv));
                if track_reverse_wide && !wide && nd.hi.is_infinite() {
                    wide = true;
                }
                let mut cur_base = local_bytes(
                    &best_len,
                    &dist,
                    dist_vec_bytes,
                    &parent,
                    &order,
                    &heap,
                    &direct_multi,
                    &backtrack_parent,
                )
                .saturating_add(out_bytes);
                if y == VIRT {
                    if best_virt.is_some_and(|(bc, bl)| (nd, nl) >= (bc, bl)) {
                        continue;
                    }
                    best_virt = Some((nd, nl));
                } else {
                    let (kept, next_base) = pareto_insert_charged(
                        &mut dist,
                        &mut dist_vec_bytes,
                        y,
                        nl,
                        nd,
                        &mut work,
                        cur_base,
                    )?;
                    if !kept {
                        continue;
                    }
                    cur_base = next_base;
                }
                (_, cur_base) = insert_hb_map(
                    &mut parent,
                    (y, nl),
                    Parent {
                        node: u,
                        p: st.p,
                        rev: st.rev,
                    },
                    &mut work,
                    cur_base,
                )?;
                let _ = push_heap(&mut heap, Reverse((nd, nl, y)), &mut work, cur_base)?;
            }
            drop(out_steps);
            work.resize(local_bytes(
                &best_len,
                &dist,
                dist_vec_bytes,
                &parent,
                &order,
                &heap,
                &direct_multi,
                &backtrack_parent,
            ))?;
        }
        drop(dist);
        drop(heap);
        if wide && !direct_multi.is_empty() {
            let prop_base = map_bytes(&parent)
                .saturating_add(vec_bytes::<(u64, u32)>(order.capacity()))
                .saturating_add(map_bytes(&best_len))
                .saturating_add(map_bytes(&direct_multi))
                .saturating_add(map_bytes(&backtrack_parent));
            let _ = propagate_reverse_backtracks(
                self.ctx,
                max_len,
                &best_len,
                &backtrack_parent,
                &mut direct_multi,
                &mut work,
                prop_base,
            )?;
            drop(backtrack_parent);
            work.resize(
                map_bytes(&parent)
                    .saturating_add(vec_bytes::<(u64, u32)>(order.capacity()))
                    .saturating_add(map_bytes(&best_len))
                    .saturating_add(map_bytes(&direct_multi)),
            )?;
            return Ok((
                BoundedTreeOutcome::ReverseWide {
                    order,
                    parent,
                    bwd_dist: best_len,
                    direct_multi,
                },
                work,
            ));
        }
        drop(best_len);
        drop(direct_multi);
        drop(backtrack_parent);
        work.resize(map_bytes(&parent).saturating_add(vec_bytes::<(u64, u32)>(order.capacity())))?;
        Ok((BoundedTreeOutcome::Tree((order, parent)), work))
    }

    /// One cheapest simple path from `from` to `to` of at most `max_len` edges by
    /// bidirectional Pareto-pruned search over `(node, length)` pairs (ROADMAP item 275).
    /// When `to == VIRT` the path closes a cycle at `virt`, and the returned edges keep
    /// `VIRT` as the final target. Each side searches up to half the length budget
    /// (`max_f = ceil(max_len / 2)`, `max_b = floor(max_len / 2)`), keeping a label only
    /// when no other label at the node is both cheaper-or-equal and shorter-or-equal.
    /// Labels at a side's half-length boundary are never expanded and never enter its heap;
    /// `bounds[side]` records the least cost relaxed at that boundary so the lower bound
    /// `min(top[0] + min(top[1], bound[1]), top[1] + min(top[0], bound[0]))` remains exact
    /// while `Cost::add` is exact. When wide weights or overflow could make split sums
    /// disagree with source-order sums, falls back to [`dijkstra_pair_bounded_forward`].
    #[allow(clippy::too_many_arguments)]
    fn dijkstra_pair_bounded(
        &self,
        from: u64,
        to: u64,
        virt: u64,
        banned_nodes: Option<&FxHashSet<u64>>,
        banned_edges: Option<&FxHashSet<Edge>>,
        max_len: u32,
        prefix: PrefixCost,
    ) -> Result<Option<(Vec<Edge>, Charge<'a>)>> {
        debug_assert!(from != to);
        let cycle = to == VIRT;
        let real = |x: u64| if x == VIRT { virt } else { x };
        self.set_roots(&[from, real(to)]);
        if max_len == 0 {
            return Ok(None);
        }
        if prefix.total_max_len > 1
            && (prefix.cost.hi.is_infinite()
                || bounded_weights_too_wide(prefix.total_max_len, prefix.wmin, prefix.wmax))
        {
            return self.dijkstra_pair_bounded_forward(
                from,
                to,
                virt,
                banned_nodes,
                banned_edges,
                max_len,
                prefix.cost,
                None,
            );
        }
        let max_side = [max_len.div_ceil(2), max_len / 2];
        let mut work = self.ctx.charge(0)?;
        let mut dist: [HbMap<u64, Vec<(u32, Cost)>>; 2] = Default::default();
        let mut dist_vec_bytes = [0u64; 2];
        let mut best_len: [HbMap<u64, u32>; 2] = Default::default();
        let mut parent: [HbMap<(u64, u32), Parent>; 2] = Default::default();
        let mut heaps: [BoundedHeap; 2] = Default::default();
        let mut bounds: [Option<Cost>; 2] = [None, None];
        let local_bytes = |dist: &[HbMap<u64, Vec<(u32, Cost)>>; 2],
                           dist_vec_bytes: &[u64; 2],
                           best_len: &[HbMap<u64, u32>; 2],
                           parent: &[HbMap<(u64, u32), Parent>; 2],
                           heaps: &[BoundedHeap; 2]| {
            map_bytes(&dist[0])
                .saturating_add(map_bytes(&dist[1]))
                .saturating_add(dist_vec_bytes[0])
                .saturating_add(dist_vec_bytes[1])
                .saturating_add(map_bytes(&best_len[0]))
                .saturating_add(map_bytes(&best_len[1]))
                .saturating_add(map_bytes(&parent[0]))
                .saturating_add(map_bytes(&parent[1]))
                .saturating_add(vec_bytes::<Reverse<(Cost, u32, u64)>>(heaps[0].capacity()))
                .saturating_add(vec_bytes::<Reverse<(Cost, u32, u64)>>(heaps[1].capacity()))
        };
        let mut base = 0u64;
        for (i, r) in [from, to].into_iter().enumerate() {
            (_, base) = pareto_insert_charged(
                &mut dist[i],
                &mut dist_vec_bytes[i],
                r,
                0,
                Cost::ZERO,
                &mut work,
                base,
            )?;
            if max_side[i] > 0 {
                base = push_heap(&mut heaps[i], Reverse((Cost::ZERO, 0, r)), &mut work, base)?;
            } else {
                (_, base) = insert_hb_map(&mut best_len[i], r, 0, &mut work, base)?;
                bounds[i] = Some(Cost::ZERO);
            }
        }
        let mut best: Option<(Cost, u64, (u32, u32))> = None;
        let mut n = 0u64;
        let (mut wmin, mut wmax) = (prefix.wmin, prefix.wmax);
        let mut wide = false;
        'search: loop {
            let mut tops: [Option<Cost>; 2] = [None, None];
            for i in 0..2 {
                while let Some(&Reverse((_, l, u))) = heaps[i].peek() {
                    if best_len[i].get(&u).is_none_or(|&bl| l < bl) {
                        break;
                    }
                    heaps[i].pop();
                    n += 1;
                    if n.is_multiple_of(4096) {
                        self.ctx.check()?;
                    }
                }
                tops[i] = heaps[i].peek().map(|r| r.0.0);
            }
            let lb = opt_cost_min(
                opt_cost_add(tops[0], opt_cost_min(tops[1], bounds[1])),
                opt_cost_add(tops[1], opt_cost_min(tops[0], bounds[0])),
            );
            let Some(lb) = lb else {
                break;
            };
            if prefix.total_max_len > 1
                && (lb.hi.is_infinite() || prefix.cost.add(lb).hi.is_infinite())
            {
                wide = true;
                break;
            }
            if best.is_some_and(|(mu, _, _)| lb >= mu) {
                break;
            }
            let side = if heaps[0].is_empty() || heaps[1].is_empty() {
                usize::from(heaps[0].is_empty())
            } else {
                usize::from(heaps[0].len() > heaps[1].len())
            };
            let forward = side == 0;
            let other = 1 - side;
            let Reverse((d, l, u)) = heaps[side].pop().unwrap();
            let loc = local_bytes(&dist, &dist_vec_bytes, &best_len, &parent, &heaps);
            let _ = insert_hb_map(&mut best_len[side], u, l, &mut work, loc)?;
            self.visit_count(1)?;
            n += 1;
            if n.is_multiple_of(4096) {
                self.ctx.check()?;
            }
            let my_root = if forward { from } else { to };
            let other_root = if forward { to } else { from };
            if u == other_root {
                continue;
            }
            let scan = real(u);
            let loc = local_bytes(&dist, &dist_vec_bytes, &best_len, &parent, &heaps);
            let mut cached_steps: Option<Rc<[Step]>> = None;
            let mut out_steps: Vec<(u64, Step)> = Vec::new();
            if banned_nodes.is_some() {
                cached_steps = Some(self.neighbours_charged(scan, forward, &mut work, loc)?);
            } else {
                self.expand_one_charged(scan, forward, &mut out_steps, &mut work, loc)?;
            }
            let out_bytes = vec_bytes::<(u64, Step)>(out_steps.capacity());
            let step_count = cached_steps.as_ref().map_or(out_steps.len(), |s| s.len());
            let nl = l + 1;
            let at_boundary = nl == max_side[side];
            for idx in 0..step_count {
                if idx.is_multiple_of(4096) {
                    #[cfg(any(test, feature = "failpoints"))]
                    test_hooks::fire_traversal_poll();
                    self.ctx.check()?;
                }
                let st = match &cached_steps {
                    Some(s) => s[idx],
                    None => out_steps[idx].1,
                };
                let y = if forward && cycle && st.node == virt {
                    VIRT
                } else {
                    st.node
                };
                if y == my_root || banned_nodes.is_some_and(|b| b.contains(&y)) {
                    continue;
                }
                if max_side[other] == 0 && y != other_root {
                    continue;
                }
                if best_len[side].get(&y).is_some_and(|&bl| nl >= bl) {
                    continue;
                }
                let e = if forward {
                    Edge {
                        from: u,
                        to: y,
                        p: st.p,
                        rev: st.rev,
                    }
                } else {
                    Edge {
                        from: y,
                        to: u,
                        p: st.p,
                        rev: st.rev,
                    }
                };
                if banned_edges.is_some_and(|b| b.contains(&e)) {
                    continue;
                }
                let wv = self.weight(&e.map(real))?;
                if wv > 0.0 {
                    wmin = wmin.min(wv);
                    wmax = wmax.max(wv);
                    if bounded_weights_too_wide(prefix.total_max_len, wmin, wmax) {
                        wide = true;
                        break 'search;
                    }
                }
                let nd = d.add(Cost::of(wv));
                if prefix.total_max_len > 1
                    && (nd.hi.is_infinite() || prefix.cost.add(nd).hi.is_infinite())
                {
                    wide = true;
                    break 'search;
                }
                let cur_base = local_bytes(&dist, &dist_vec_bytes, &best_len, &parent, &heaps)
                    .saturating_add(out_bytes);
                let (kept, mut cur_base) = pareto_insert_charged(
                    &mut dist[side],
                    &mut dist_vec_bytes[side],
                    y,
                    nl,
                    nd,
                    &mut work,
                    cur_base,
                )?;
                if !kept {
                    continue;
                }
                (_, cur_base) = insert_hb_map(
                    &mut parent[side],
                    (y, nl),
                    Parent {
                        node: u,
                        p: st.p,
                        rev: st.rev,
                    },
                    &mut work,
                    cur_base,
                )?;
                if y != other_root {
                    if !at_boundary {
                        let _ =
                            push_heap(&mut heaps[side], Reverse((nd, nl, y)), &mut work, cur_base)?;
                    } else {
                        bounds[side] = opt_cost_min(bounds[side], Some(nd));
                    }
                }
                if let Some(other_v) = dist[other].get(&y)
                    && let Some((l_other, c_other)) = pareto_best_within(other_v, max_len - nl)
                {
                    let total = if forward {
                        nd.add(c_other)
                    } else {
                        c_other.add(nd)
                    };
                    if prefix.total_max_len > 1
                        && (total.hi.is_infinite() || prefix.cost.add(total).hi.is_infinite())
                    {
                        wide = true;
                        break 'search;
                    }
                    if best.is_none_or(|(mu, _, _)| total < mu)
                        && (y == from || y == to || !(self.sink(y, true) || self.blocked(y)?))
                    {
                        let lens = if forward {
                            (nl, l_other)
                        } else {
                            (l_other, nl)
                        };
                        best = Some((total, y, lens));
                    }
                }
            }
            drop(out_steps);
            drop(cached_steps);
            work.resize(local_bytes(
                &dist,
                &dist_vec_bytes,
                &best_len,
                &parent,
                &heaps,
            ))?;
        }
        drop(dist);
        drop(best_len);
        drop(heaps);
        if wide {
            drop(parent);
            drop(work);
            return self.dijkstra_pair_bounded_forward(
                from,
                to,
                virt,
                banned_nodes,
                banned_edges,
                max_len,
                prefix.cost,
                None,
            );
        }
        let Some((_, m, (lf, lb))) = best else {
            drop(parent);
            return Ok(None);
        };
        let combined_len = (lf as usize).saturating_add(lb as usize);
        let parent_bytes = map_bytes(&parent[0]).saturating_add(map_bytes(&parent[1]));
        work.resize(parent_bytes.saturating_add(vec_bytes::<Edge>(combined_len)))?;
        let mut a = Vec::with_capacity(combined_len);
        let mut cur = m;
        let mut l = lf;
        let mut steps = 0usize;
        while l > 0 {
            steps += 1;
            if steps.is_multiple_of(4096) {
                self.ctx.check()?;
            }
            let q = parent[0][&(cur, l)];
            a.push(Edge {
                from: q.node,
                to: cur,
                p: q.p,
                rev: q.rev,
            });
            cur = q.node;
            l -= 1;
        }
        a.reverse();
        let mut cur = m;
        let mut l = lb;
        while l > 0 {
            steps += 1;
            if steps.is_multiple_of(4096) {
                self.ctx.check()?;
            }
            let q = parent[1][&(cur, l)];
            a.push(Edge {
                from: cur,
                to: q.node,
                p: q.p,
                rev: q.rev,
            });
            cur = q.node;
            l -= 1;
        }
        drop(parent);
        let path = without_loops_charged(self.ctx, from, a, &mut work, 0)?;
        work.resize(vec_bytes::<Edge>(path.capacity()))?;
        Ok(Some((path, work)))
    }

    /// Source-order one-ended bounded Pareto search used as a fallback from
    /// [`dijkstra_pair_bounded`] and reverse [`dijkstra_bounded`] when wide weights or
    /// overflow could make split or reverse sums disagree with source-order accumulation.
    /// `init_cost` seeds `from` with an already-accumulated Yen prefix cost, and
    /// `bwd_dist`, when provided, prunes interior nodes that cannot reach `to` within the
    /// remaining edge budget.
    #[allow(clippy::too_many_arguments)]
    fn dijkstra_pair_bounded_forward(
        &self,
        from: u64,
        to: u64,
        virt: u64,
        banned_nodes: Option<&FxHashSet<u64>>,
        banned_edges: Option<&FxHashSet<Edge>>,
        max_len: u32,
        init_cost: Cost,
        bwd_dist: Option<&HbMap<u64, u32>>,
    ) -> Result<Option<(Vec<Edge>, Charge<'a>)>> {
        debug_assert!(from != to);
        let cycle = to == VIRT;
        let real = |x: u64| if x == VIRT { virt } else { x };
        self.set_roots(&[from, real(to)]);
        if max_len == 0 {
            return Ok(None);
        }
        let mut work = self.ctx.charge(0)?;
        let mut dist: HbMap<u64, Vec<(u32, Cost)>> = HbMap::default();
        let mut dist_vec_bytes = 0u64;
        let mut best_len: HbMap<u64, u32> = HbMap::default();
        let mut parent: HbMap<(u64, u32), Parent> = HbMap::default();
        let mut heap: BoundedHeap = BinaryHeap::new();
        let mut best_to: Option<(Cost, u32)> = None;
        let local_bytes = |dist: &HbMap<u64, Vec<(u32, Cost)>>,
                           dist_vec_bytes: u64,
                           best_len: &HbMap<u64, u32>,
                           parent: &HbMap<(u64, u32), Parent>,
                           heap: &BoundedHeap| {
            map_bytes(dist)
                .saturating_add(dist_vec_bytes)
                .saturating_add(map_bytes(best_len))
                .saturating_add(map_bytes(parent))
                .saturating_add(vec_bytes::<Reverse<(Cost, u32, u64)>>(heap.capacity()))
        };
        let mut base = 0u64;
        (_, base) = pareto_insert_charged(
            &mut dist,
            &mut dist_vec_bytes,
            from,
            0,
            init_cost,
            &mut work,
            base,
        )?;
        let _ = push_heap(&mut heap, Reverse((init_cost, 0, from)), &mut work, base)?;
        let mut n = 0u64;
        let mut found_len: Option<u32> = None;
        while let Some(Reverse((d, l, u))) = heap.pop() {
            n += 1;
            if n.is_multiple_of(4096) {
                self.ctx.check()?;
            }
            if best_len.get(&u).is_some_and(|&bl| u == to || l >= bl) {
                continue;
            }
            let loc = local_bytes(&dist, dist_vec_bytes, &best_len, &parent, &heap);
            let _ = insert_hb_map(&mut best_len, u, l, &mut work, loc)?;
            self.visit_count(1)?;
            if u == to {
                found_len = Some(l);
                break;
            }
            if l >= max_len {
                continue;
            }
            let scan = real(u);
            let loc = local_bytes(&dist, dist_vec_bytes, &best_len, &parent, &heap);
            let mut cached_steps: Option<Rc<[Step]>> = None;
            let mut out_steps: Vec<(u64, Step)> = Vec::new();
            if banned_nodes.is_some() {
                cached_steps = Some(self.neighbours_charged(scan, true, &mut work, loc)?);
            } else {
                self.expand_one_charged(scan, true, &mut out_steps, &mut work, loc)?;
            }
            let out_bytes = vec_bytes::<(u64, Step)>(out_steps.capacity());
            let step_count = cached_steps.as_ref().map_or(out_steps.len(), |s| s.len());
            let nl = l + 1;
            for idx in 0..step_count {
                if idx.is_multiple_of(4096) {
                    #[cfg(any(test, feature = "failpoints"))]
                    test_hooks::fire_traversal_poll();
                    self.ctx.check()?;
                }
                let st = match &cached_steps {
                    Some(s) => s[idx],
                    None => out_steps[idx].1,
                };
                let y = if cycle && st.node == virt {
                    VIRT
                } else {
                    st.node
                };
                if y == from || banned_nodes.is_some_and(|b| b.contains(&y)) {
                    continue;
                }
                if nl == max_len && y != to {
                    continue;
                }
                if y != to
                    && let Some(bd) = bwd_dist
                    && bd
                        .get(&y)
                        .is_none_or(|&rem| nl.saturating_add(rem) > max_len)
                {
                    continue;
                }
                if best_len.get(&y).is_some_and(|&bl| y == to || nl >= bl) {
                    continue;
                }
                let e = Edge {
                    from: u,
                    to: y,
                    p: st.p,
                    rev: st.rev,
                };
                if banned_edges.is_some_and(|b| b.contains(&e)) {
                    continue;
                }
                let nd = d.add(Cost::of(self.weight(&e.map(real))?));
                let mut cur_base = local_bytes(&dist, dist_vec_bytes, &best_len, &parent, &heap)
                    .saturating_add(out_bytes);
                if y == to {
                    if best_to.is_some_and(|(bc, bl)| (nd, nl) >= (bc, bl)) {
                        continue;
                    }
                    best_to = Some((nd, nl));
                } else {
                    let (kept, next_base) = pareto_insert_charged(
                        &mut dist,
                        &mut dist_vec_bytes,
                        y,
                        nl,
                        nd,
                        &mut work,
                        cur_base,
                    )?;
                    if !kept {
                        continue;
                    }
                    cur_base = next_base;
                }
                (_, cur_base) = insert_hb_map(
                    &mut parent,
                    (y, nl),
                    Parent {
                        node: u,
                        p: st.p,
                        rev: st.rev,
                    },
                    &mut work,
                    cur_base,
                )?;
                let _ = push_heap(&mut heap, Reverse((nd, nl, y)), &mut work, cur_base)?;
            }
            drop(out_steps);
            drop(cached_steps);
            work.resize(local_bytes(
                &dist,
                dist_vec_bytes,
                &best_len,
                &parent,
                &heap,
            ))?;
        }
        drop(dist);
        drop(best_len);
        drop(heap);
        let Some(len) = found_len else {
            drop(parent);
            return Ok(None);
        };
        let parent_bytes = map_bytes(&parent);
        work.resize(parent_bytes.saturating_add(vec_bytes::<Edge>(len as usize)))?;
        let mut a = Vec::with_capacity(len as usize);
        let mut cur = to;
        let mut l = len;
        let mut steps = 0usize;
        while l > 0 {
            steps += 1;
            if steps.is_multiple_of(4096) {
                self.ctx.check()?;
            }
            let q = parent[&(cur, l)];
            a.push(Edge {
                from: q.node,
                to: cur,
                p: q.p,
                rev: q.rev,
            });
            cur = q.node;
            l -= 1;
        }
        a.reverse();
        drop(parent);
        work.resize(vec_bytes::<Edge>(a.capacity()))?;
        Ok(Some((a, work)))
    }

    /// The shortest path from `from` to `to` (forward) that avoids the banned nodes and
    /// edges, of at most `budget` edges: Yen's spur search.
    #[allow(clippy::too_many_arguments)]
    fn spur(
        &self,
        from: u64,
        to: u64,
        virt: u64,
        banned_nodes: &FxHashSet<u64>,
        banned_edges: &FxHashSet<Edge>,
        budget: Option<u32>,
        prefix: PrefixCost,
    ) -> Result<Option<SpurPath<'a>>> {
        if self.spec.weighted() {
            if let Some(max) = budget {
                return Ok(self
                    .dijkstra_pair_bounded(
                        from,
                        to,
                        virt,
                        Some(banned_nodes),
                        Some(banned_edges),
                        max,
                        prefix,
                    )?
                    .map(|(p, work)| (p, 0.0, Some(work))));
            }
            if to != VIRT && from != to {
                return Ok(self
                    .dijkstra_pair(from, to, Some(banned_nodes), Some(banned_edges))?
                    .map(|(p, c)| (p, c, None)));
            }
            // a spur that closes a cycle searches from one end
            let close = to == VIRT;
            let targets: FxHashSet<u64> = [if close { virt } else { to }].into_iter().collect();
            let (tree, dist) = self.dijkstra(
                from,
                true,
                Some(&targets),
                false,
                close,
                Some(banned_nodes),
                Some(banned_edges),
                virt,
            )?;
            let Some(c) = dist.get(&to).map(|c| c.hi) else {
                return Ok(None);
            };
            return Ok(tree
                .paths(to, false, 1, None, &|| self.ctx.check())?
                .pop()
                .map(|p| (p, c, None)));
        }
        // bidirectional breadth-first search around the banned nodes and edges
        let bans = Bans {
            nodes: banned_nodes,
            edges: banned_edges,
        };
        Ok(self
            .bidi(from, to, virt, false, 1, Some(&bans), budget)?
            .pop()
            .map(|p| {
                let c = p.len() as f64;
                (p, c, None)
            }))
    }

    /// A path's cost as a double-double, added from its start, so that equal paths always
    /// get equal costs and costs that differ in the low bits rank apart. `real` maps the
    /// virtual end of a cycle back to its node for the weight lookup.
    fn exact_cost(&self, edges: &[Edge], real: impl Fn(u64) -> u64) -> Result<Cost> {
        if !self.spec.weighted() {
            return Ok(Cost::of(edges.len() as f64));
        }
        let mut c = Cost::ZERO;
        for e in edges {
            c = c.add(Cost::of(self.weight(&e.map(&real))?));
        }
        Ok(c)
    }

    /// Simple paths in nondecreasing cost by Yen's algorithm; `accept` sees each one
    /// and returns whether to go on.
    fn yen(
        &self,
        s: u64,
        t: u64,
        mut accept: impl FnMut(&[Edge], Cost) -> Result<bool>,
    ) -> Result<()> {
        let tt = if s == t { VIRT } else { t };
        let real = |x: u64| if x == VIRT { s } else { x };
        let max = self.spec.max_len;
        let none_n = FxHashSet::default();
        let none_e = FxHashSet::default();
        let budget = max;
        let init_prefix = PrefixCost::zero(budget.unwrap_or(0));
        let Some((first, _, first_charge)) =
            self.spur(s, tt, s, &none_n, &none_e, budget, init_prefix)?
        else {
            return Ok(());
        };
        let _first_charge = first_charge;
        // candidates are ranked by their whole cost in double-double, never by the
        // spur's rounded cost plus the root's, so that two equally cheap candidates
        // cannot rank apart by the order their weights were added in
        let first_cost = self.exact_cost(&first, real)?;
        let mut a: Vec<(Vec<Edge>, Cost)> = vec![(first, first_cost)];
        let mut seen: FxHashSet<Vec<Edge>> = FxHashSet::default();
        seen.insert(a[0].0.clone());
        // candidates by (cost, length, discovery)
        let mut heap: BinaryHeap<Reverse<(Cost, usize, u64)>> = BinaryHeap::new();
        let mut cands: FxHashMap<u64, (Vec<Edge>, Cost)> = FxHashMap::default();
        let mut seq = 0u64;
        loop {
            let (cur, cost) = a.last().cloned().unwrap();
            let real_edges: Vec<Edge> = cur.iter().map(|e| e.map(real)).collect();
            if !accept(&real_edges, cost)? {
                return Ok(());
            }
            let nodes: Vec<u64> = std::iter::once(s).chain(cur.iter().map(|e| e.to)).collect();
            let track_prefix = self.spec.weighted() && budget.is_some();
            let mut prefix_cost = Cost::ZERO;
            let (mut prefix_wmin, mut prefix_wmax) = (f64::INFINITY, 0.0f64);
            for i in 0..cur.len() {
                let step_budget = match budget {
                    Some(m) if m <= i as u32 => break,
                    Some(m) => Some(m - i as u32),
                    None => None,
                };
                if i > 0 && track_prefix {
                    let w = self.weight(&cur[i - 1].map(real))?;
                    if w > 0.0 {
                        prefix_wmin = prefix_wmin.min(w);
                        prefix_wmax = prefix_wmax.max(w);
                    }
                    prefix_cost = prefix_cost.add(Cost::of(w));
                }
                let root = &cur[..i];
                let mut banned_edges = FxHashSet::default();
                for (p, _) in &a {
                    if p.len() > i && p[..i] == *root {
                        banned_edges.insert(p[i]);
                    }
                }
                let banned_nodes: FxHashSet<u64> = nodes[..i].iter().copied().collect();
                let prefix = PrefixCost {
                    cost: prefix_cost,
                    wmin: prefix_wmin,
                    wmax: prefix_wmax,
                    total_max_len: budget.unwrap_or(0),
                };
                if let Some((sp, _, sp_charge)) = self.spur(
                    nodes[i],
                    tt,
                    s,
                    &banned_nodes,
                    &banned_edges,
                    step_budget,
                    prefix,
                )? {
                    let mut path = root.to_vec();
                    path.extend(sp);
                    if seen.insert(path.clone()) {
                        let len = path.len();
                        let c = self.exact_cost(&path, real)?;
                        cands.insert(seq, (path, c));
                        heap.push(Reverse((c, len, seq)));
                        seq += 1;
                        self.charge.add(len as u64 * 32)?;
                    }
                    drop(sp_charge);
                }
            }
            let Some(Reverse((_, _, id))) = heap.pop() else {
                return Ok(());
            };
            a.push(cands.remove(&id).unwrap());
        }
    }

    /// Every simple path from `root` (forward) or into `root` (backward) of at most
    /// `path:maxLength` edges, depth first. With targets only paths to them, pruned by
    /// each node's distance to the nearest target. `emit` gets each path in path order
    /// with its far end, and returns whether to go on.
    fn all_paths(
        &self,
        root: u64,
        forward: bool,
        targets: Option<&FxHashSet<u64>>,
        emit: &mut dyn FnMut(&[Edge], u64) -> Result<bool>,
    ) -> Result<()> {
        let max = self.spec.max_len.unwrap_or(0);
        let min = self.spec.min_len;
        let close = targets.is_none_or(|t| t.contains(&root));
        if min == 0 && close && !emit(&[], root)? {
            return Ok(());
        }
        if max == 0 {
            return Ok(());
        }
        // the distance of each node to the nearest target, within `max` (a lower bound
        // for simple paths)
        let dist: Option<FxHashMap<u64, u32>> = match targets {
            None => None,
            Some(ts) => {
                let mut d: FxHashMap<u64, u32> = ts.iter().map(|&t| (t, 0)).collect();
                let mut frontier: Vec<u64> = ts.iter().copied().collect();
                frontier.sort_unstable();
                // the targets end paths and are expanded backward even when they are
                // hubs; distances that are too small only weaken the pruning
                self.set_roots(&frontier);
                let mut level = 0;
                while !frontier.is_empty() && level < max {
                    let mut found = Vec::new();
                    self.expand(&frontier, !forward, &mut found)?;
                    level += 1;
                    let mut next = Vec::new();
                    for (_, st) in found {
                        if let std::collections::hash_map::Entry::Vacant(v) = d.entry(st.node) {
                            v.insert(level);
                            next.push(st.node);
                        }
                    }
                    self.visit(next.len() as u64)?;
                    next.sort_unstable();
                    frontier = next;
                }
                Some(d)
            }
        };
        self.set_roots(&[root]);
        let is_target = |y: u64| targets.is_none_or(|t| t.contains(&y));
        let mut on_path: FxHashSet<u64> = FxHashSet::default();
        on_path.insert(root);
        // (node, its neighbours, next index); edges in search order
        let mut stack: Vec<(u64, Rc<[Step]>, usize)> =
            vec![(root, self.neighbours(root, forward)?, 0)];
        let mut edges: Vec<Edge> = Vec::new();
        let mut ordered: Vec<Edge> = Vec::new();
        let mut steps = 0u64;
        while let Some(top) = stack.last_mut() {
            let (x, ns, i) = (top.0, top.1.clone(), top.2);
            if i >= ns.len() {
                stack.pop();
                on_path.remove(&x);
                edges.pop();
                continue;
            }
            top.2 += 1;
            let st = ns[i];
            let y = st.node;
            let len = edges.len() as u32 + 1;
            let closing = close && y == root;
            if !closing && on_path.contains(&y) {
                continue;
            }
            if !closing
                && let Some(d) = &dist
                && d.get(&y).is_none_or(|&k| k > max - len)
            {
                continue;
            }
            steps += 1;
            if steps.is_multiple_of(1024) {
                self.visit(1024)?;
                self.ctx.check()?;
            }
            let e = if forward {
                Edge {
                    from: x,
                    to: y,
                    p: st.p,
                    rev: st.rev,
                }
            } else {
                Edge {
                    from: y,
                    to: x,
                    p: st.p,
                    rev: st.rev,
                }
            };
            edges.push(e);
            if (closing || is_target(y)) && len >= min {
                ordered.clear();
                if forward {
                    ordered.extend_from_slice(&edges);
                } else {
                    ordered.extend(edges.iter().rev());
                }
                if !emit(&ordered, y)? {
                    return Ok(());
                }
            }
            if closing || len >= max {
                edges.pop();
                continue;
            }
            on_path.insert(y);
            stack.push((y, self.neighbours(y, forward)?, 0));
        }
        Ok(())
    }

    /// Take a path if the call's limit allows; false when the limit is reached.
    fn take(&self) -> bool {
        let l = self.left.get();
        if l == 0 {
            return false;
        }
        self.left.set(l - 1);
        true
    }

    fn full(&self) -> bool {
        self.left.get() == 0
    }
}

/// What a group of searches looks for.
#[derive(Default)]
struct Targets {
    any: bool,
    set: BTreeSet<u64>,
}

/// Run a path search: over the rows of `input` when the leaf reads its group.
pub fn run(
    ctx: &Ctx,
    spec: &PathSearchSpec,
    input: Option<Table>,
    edges: Option<Table>,
    vars: &[VarId],
) -> Result<(Table, serde_json::Map<String, serde_json::Value>)> {
    let input = input.unwrap_or_else(Table::unit);
    let col = |e: &PathEnd| match e {
        PathEnd::Var(v) => input.col_of(*v),
        PathEnd::Const(_) => None,
    };
    let (sc, tc) = (col(&spec.source), col(&spec.target));
    let end_of = |e: &PathEnd, c: Option<usize>, i: usize| -> Option<u64> {
        match (e, c) {
            (PathEnd::Const(id), _) => Some(id.0),
            (PathEnd::Var(_), Some(c)) => {
                let x = input.cols[c][i];
                (!x.is_undef()).then_some(x.0)
            }
            (PathEnd::Var(_), None) => None,
        }
    };
    let keys: Vec<(Option<u64>, Option<u64>)> = (0..input.len())
        .map(|i| (end_of(&spec.source, sc, i), end_of(&spec.target, tc, i)))
        .collect();
    let mut forward: BTreeMap<u64, Targets> = BTreeMap::new();
    let mut backward: BTreeSet<u64> = BTreeSet::new();
    for k in &keys {
        match *k {
            (Some(s), Some(t)) => {
                forward.entry(s).or_default().set.insert(t);
            }
            (Some(s), None) => forward.entry(s).or_default().any = true,
            (None, Some(t)) => {
                backward.insert(t);
            }
            (None, None) => {
                return Err(bad(
                    "a row binds neither the source nor the target; bind one of them",
                ));
            }
        }
    }
    let eng = Engine::new(ctx, spec, edges.as_ref())?;
    drop(edges);
    let mut found: Vec<Found> = Vec::new();
    let mut by_pair: FxHashMap<(u64, u64), Vec<u32>> = FxHashMap::default();
    let mut by_source: FxHashMap<u64, Vec<u32>> = FxHashMap::default();
    let mut by_target: FxHashMap<u64, Vec<u32>> = FxHashMap::default();
    let weighted = spec.weighted();
    let all = matches!(spec.algorithm, Algorithm::AllShortest);
    // add a path found from a source (`fwd`) or into a target
    let (want_pairs, want_sources) = (Cell::new(true), Cell::new(true));
    let need_edges = spec.per_edge() || weighted;
    // a path known only by its length (one shortest path, no edge outputs, no weights)
    let by_level = !need_edges && !all;
    let mut add = |found: &mut Vec<Found>,
                   start: u64,
                   end: u64,
                   edges: Vec<Edge>,
                   len: Option<usize>,
                   fwd: bool,
                   mut edge_charge: Option<Charge<'_>>|
     -> Result<bool> {
        if !eng.take() {
            return Ok(false);
        }
        let cost = eng.path_cost(&edges)?;
        let i = found.len() as u32;
        let len = len.unwrap_or(edges.len());
        let edges = if need_edges { edges } else { Vec::new() };
        let need = (edges.len() as u64).saturating_mul(32).saturating_add(64);
        if let Some(ref ec) = edge_charge {
            ec.resize(need)?;
            eng.charge.adopt_charge(&mut edge_charge)?;
        } else {
            eng.charge.add(need)?;
        }
        found.push(Found {
            start,
            end,
            edges,
            len,
            cost,
        });
        if fwd {
            // only the lookups some input row makes
            if want_pairs.get() {
                by_pair.entry((start, end)).or_default().push(i);
            }
            if want_sources.get() {
                by_source.entry(start).or_default().push(i);
            }
        } else {
            by_target.entry(end).or_default().push(i);
        }
        Ok(true)
    };
    let cap = |eng: &Engine| eng.left.get();
    let bounded_weighted_shortest =
        weighted && spec.max_len.is_some() && matches!(spec.algorithm, Algorithm::Shortest);
    // weighted `allShortest` with a length limit enumerates optimal short paths via Yen
    // whose spurs run the length-bounded Pareto-pruned search
    let yen_all_shortest =
        weighted && spec.max_len.is_some() && matches!(spec.algorithm, Algorithm::AllShortest);
    'sources: for (&s, targets) in &forward {
        if eng.full() {
            break;
        }
        let tset: FxHashSet<u64> = targets.set.iter().copied().collect();
        want_pairs.set(!targets.set.is_empty());
        want_sources.set(targets.any);
        match spec.algorithm {
            Algorithm::Shortest if bounded_weighted_shortest => {
                let max = spec.max_len.unwrap();
                if targets.any {
                    eng.begin();
                    if spec.min_len == 0 && !add(&mut found, s, s, Vec::new(), None, true, None)? {
                        break 'sources;
                    }
                    if eng.full() {
                        break 'sources;
                    }
                    let (outcome, tree_charge) = eng.dijkstra_bounded(s, true, max)?;
                    let BoundedTreeOutcome::Tree((order, parent)) = outcome else {
                        unreachable!("forward bounded Dijkstra never takes reverse wide fallback");
                    };
                    let _tree_charge = tree_charge;
                    for (m, l) in order {
                        if eng.full() {
                            break 'sources;
                        }
                        if spec.min_len == 0 && m == VIRT {
                            continue;
                        }
                        let end = if m == VIRT { s } else { m };
                        let edge_charge = eng.charge.part();
                        edge_charge.resize(vec_bytes::<Edge>(l as usize))?;
                        let p = reconstruct_bounded(eng.ctx, s, true, &parent, m, l)?;
                        if !add(&mut found, s, end, p, None, true, Some(edge_charge))? {
                            break 'sources;
                        }
                    }
                } else {
                    for &t in &targets.set {
                        if eng.full() {
                            break 'sources;
                        }
                        eng.begin();
                        if spec.min_len == 0 && s == t {
                            if !add(&mut found, s, t, Vec::new(), None, true, None)? {
                                break 'sources;
                            }
                            continue;
                        }
                        let tt = if s == t { VIRT } else { t };
                        let real = |x: u64| if x == VIRT { s } else { x };
                        if let Some((mut p, path_charge)) = eng.dijkstra_pair_bounded(
                            s,
                            tt,
                            s,
                            None,
                            None,
                            max,
                            PrefixCost::zero(max),
                        )? {
                            for e in &mut p {
                                *e = e.map(real);
                            }
                            if !add(&mut found, s, t, p, None, true, Some(path_charge))? {
                                break 'sources;
                            }
                        }
                    }
                }
            }
            Algorithm::AllShortest if yen_all_shortest => {
                if targets.any {
                    return Err(bad(
                        "path:maxLength with path:weight needs a bound target in path:allShortest",
                    ));
                }
                for &t in &targets.set {
                    if eng.full() {
                        break 'sources;
                    }
                    eng.begin();
                    if spec.min_len == 0 && s == t {
                        if !add(&mut found, s, t, Vec::new(), None, true, None)? {
                            break 'sources;
                        }
                        continue;
                    }
                    let mut best: Option<Cost> = None;
                    let mut out = Vec::new();
                    eng.yen(s, t, |p, c| {
                        if let Some(b) = best
                            && c > b
                        {
                            return Ok(false);
                        }
                        if p.len() as u32 > spec.max_len.unwrap() || (p.len() as u32) < spec.min_len
                        {
                            return Ok(true);
                        }
                        if best.is_none() {
                            best = Some(c);
                        }
                        out.push(p.to_vec());
                        Ok(out.len() < cap(&eng))
                    })?;
                    for p in out {
                        if !add(&mut found, s, t, p, None, true, None)? {
                            break 'sources;
                        }
                    }
                }
            }
            Algorithm::Shortest | Algorithm::AllShortest => {
                let single = !targets.any && targets.set.len() == 1;
                if single && !weighted {
                    let t = *targets.set.first().unwrap();
                    eng.begin();
                    if spec.min_len == 0 && s == t {
                        add(&mut found, s, t, Vec::new(), None, true, None)?;
                        continue;
                    }
                    for p in eng.bidirectional(s, t, all, if all { cap(&eng) } else { 1 })? {
                        if !add(&mut found, s, t, p, None, true, None)? {
                            break 'sources;
                        }
                    }
                    continue;
                }
                // every cheapest path of one pair, and the cheapest cycles through the
                // source: from both ends
                let t0 = targets.set.first().copied();
                let both_ends = single && weighted && (all || t0 == Some(s));
                if both_ends {
                    let t = t0.unwrap();
                    eng.begin();
                    if spec.min_len == 0 && s == t {
                        if !add(&mut found, s, t, Vec::new(), None, true, None)? {
                            break 'sources;
                        }
                        continue;
                    }
                    let mut stop = false;
                    let answered = eng.dijkstra_pair_all(s, t, all, &mut |p| {
                        let more = add(&mut found, s, t, p, None, true, None)?;
                        stop = !more;
                        // path:shortest keeps the first cheapest cycle
                        Ok(more && all)
                    })?;
                    if stop {
                        break 'sources;
                    }
                    if answered {
                        continue;
                    }
                    // with weights too wide to add exactly, or past the largest double,
                    // the search from the source alone answers, on the same visit budget
                }
                // one cheapest path between two different nodes: from both ends
                if single && weighted && !both_ends {
                    let t = t0.unwrap();
                    eng.begin();
                    if let Some((p, _)) = eng.dijkstra_pair(s, t, None, None)?
                        && !add(&mut found, s, t, p, None, true, None)?
                    {
                        break 'sources;
                    }
                    continue;
                }
                if !both_ends {
                    eng.begin();
                }
                let close = targets.any || tset.contains(&s);
                let (tree, cost) = if weighted {
                    let (tree, cost) = eng.dijkstra(
                        s,
                        true,
                        (!targets.any).then_some(&tset),
                        all,
                        close,
                        None,
                        None,
                        s,
                    )?;
                    (tree, Some(cost))
                } else {
                    let tree = eng.bfs(s, true, (!targets.any).then_some(&tset), all, close)?;
                    (tree, None)
                };
                let prune = eng.prune(&tree, cost.as_ref(), all)?;
                // every node reached, nearest first, or the targets in id order
                let mut ends: Vec<u64> = if targets.any {
                    tree.order.iter().map(|e| e.0).collect()
                } else {
                    targets
                        .set
                        .iter()
                        .map(|&t| if t == s { VIRT } else { t })
                        .collect()
                };
                // with path:minLength 0 the shortest path from the source to itself is
                // the empty one, not a cycle
                if spec.min_len == 0 && close {
                    ends.retain(|&m| m != VIRT);
                    if !add(&mut found, s, s, Vec::new(), None, true, None)? {
                        break 'sources;
                    }
                }
                for m in ends {
                    let end = if m == VIRT { s } else { m };
                    if by_level && !weighted {
                        if let Some(&(_, level)) = tree.first.get(&m)
                            && !add(
                                &mut found,
                                s,
                                end,
                                Vec::new(),
                                Some(level as usize),
                                true,
                                None,
                            )?
                        {
                            break 'sources;
                        }
                        continue;
                    }
                    let n = if all { cap(&eng) } else { 1 };
                    for p in tree.paths(m, all, n, prune.as_ref(), &|| eng.ctx.check())? {
                        let p = p
                            .into_iter()
                            .map(|e| e.map(|x| if x == VIRT { s } else { x }))
                            .collect();
                        if !add(&mut found, s, end, p, None, true, None)? {
                            break 'sources;
                        }
                    }
                }
            }
            Algorithm::KShortest => {
                if targets.any {
                    return Err(bad("path:kShortest needs a bound target"));
                }
                for &t in &targets.set {
                    if eng.full() {
                        break 'sources;
                    }
                    eng.begin();
                    let mut n = 0usize;
                    if spec.min_len == 0 && s == t {
                        if !add(&mut found, s, t, Vec::new(), None, true, None)? {
                            break 'sources;
                        }
                        n += 1;
                    }
                    if eng.full() {
                        break 'sources;
                    }
                    if n >= spec.k {
                        continue;
                    }
                    let mut out = Vec::new();
                    let max = spec.max_len.unwrap_or(u32::MAX);
                    eng.yen(s, t, |p, _| {
                        let len = p.len() as u32;
                        if len >= spec.min_len && len <= max {
                            out.push(p.to_vec());
                            n += 1;
                        }
                        Ok(n < spec.k && out.len() < cap(&eng))
                    })?;
                    for p in out {
                        if !add(&mut found, s, t, p, None, true, None)? {
                            break 'sources;
                        }
                    }
                }
            }
            Algorithm::All => {
                eng.begin();
                let mut stop = false;
                eng.all_paths(s, true, (!targets.any).then_some(&tset), &mut |p, end| {
                    if !add(&mut found, s, end, p.to_vec(), None, true, None)? {
                        stop = true;
                        return Ok(false);
                    }
                    Ok(true)
                })?;
                if stop {
                    break 'sources;
                }
            }
        }
    }
    'targets: for &t in &backward {
        if eng.full() {
            break;
        }
        eng.begin();
        match spec.algorithm {
            Algorithm::KShortest => return Err(bad("path:kShortest needs a bound source")),
            Algorithm::Shortest | Algorithm::AllShortest => {
                if yen_all_shortest {
                    return Err(bad(
                        "path:maxLength with path:weight needs a bound source in path:allShortest",
                    ));
                }
                if bounded_weighted_shortest {
                    if spec.min_len == 0 && !add(&mut found, t, t, Vec::new(), None, false, None)? {
                        break 'targets;
                    }
                    if eng.full() {
                        break 'targets;
                    }
                    let max = spec.max_len.unwrap();
                    match eng.dijkstra_bounded(t, false, max)? {
                        (BoundedTreeOutcome::Tree((order, parent)), tree_charge) => {
                            let _tree_charge = tree_charge;
                            for (m, l) in order {
                                if eng.full() {
                                    break 'targets;
                                }
                                if spec.min_len == 0 && m == VIRT {
                                    continue;
                                }
                                let start = if m == VIRT { t } else { m };
                                let edge_charge = eng.charge.part();
                                edge_charge.resize(vec_bytes::<Edge>(l as usize))?;
                                let p = reconstruct_bounded(eng.ctx, t, false, &parent, m, l)?;
                                if !add(&mut found, start, t, p, None, false, Some(edge_charge))? {
                                    break 'targets;
                                }
                            }
                        }
                        (
                            BoundedTreeOutcome::ReverseWide {
                                order,
                                parent,
                                bwd_dist,
                                direct_multi,
                            },
                            tree_charge,
                        ) => {
                            let _tree_charge = tree_charge;
                            for (m, l) in order {
                                if eng.full() {
                                    break 'targets;
                                }
                                if spec.min_len == 0 && m == VIRT {
                                    continue;
                                }
                                let start = if m == VIRT { t } else { m };
                                if !bounded_chain_has_multi(
                                    eng.ctx,
                                    &parent,
                                    &direct_multi,
                                    m,
                                    l,
                                    max,
                                )? {
                                    let edge_charge = eng.charge.part();
                                    edge_charge.resize(vec_bytes::<Edge>(l as usize))?;
                                    let p = reconstruct_bounded(eng.ctx, t, false, &parent, m, l)?;
                                    if !add(
                                        &mut found,
                                        start,
                                        t,
                                        p,
                                        None,
                                        false,
                                        Some(edge_charge),
                                    )? {
                                        break 'targets;
                                    }
                                    continue;
                                }
                                let to = if m == VIRT { VIRT } else { t };
                                let real = |x: u64| if x == VIRT { start } else { x };
                                if let Some((mut p, path_charge)) = eng
                                    .dijkstra_pair_bounded_forward(
                                        start,
                                        to,
                                        start,
                                        None,
                                        None,
                                        max,
                                        Cost::ZERO,
                                        Some(&bwd_dist),
                                    )?
                                {
                                    for e in &mut p {
                                        *e = e.map(real);
                                    }
                                    if !add(
                                        &mut found,
                                        start,
                                        t,
                                        p,
                                        None,
                                        false,
                                        Some(path_charge),
                                    )? {
                                        break 'targets;
                                    }
                                }
                            }
                        }
                    }
                    continue;
                }
                let (tree, cost) = if weighted {
                    let (tree, cost) = eng.dijkstra(t, false, None, all, true, None, None, t)?;
                    (tree, Some(cost))
                } else {
                    let tree = eng.bfs(t, false, None, all, true)?;
                    (tree, None)
                };
                let prune = eng.prune(&tree, cost.as_ref(), all)?;
                if spec.min_len == 0 && !add(&mut found, t, t, Vec::new(), None, false, None)? {
                    break 'targets;
                }
                let mut starts: Vec<u64> = tree.order.iter().map(|e| e.0).collect();
                if spec.min_len == 0 {
                    starts.retain(|&m| m != VIRT);
                }
                for m in starts {
                    let start = if m == VIRT { t } else { m };
                    if by_level && !weighted {
                        if let Some(&(_, level)) = tree.first.get(&m)
                            && !add(
                                &mut found,
                                start,
                                t,
                                Vec::new(),
                                Some(level as usize),
                                false,
                                None,
                            )?
                        {
                            break 'targets;
                        }
                        continue;
                    }
                    let n = if all { cap(&eng) } else { 1 };
                    for p in tree.paths(m, all, n, prune.as_ref(), &|| eng.ctx.check())? {
                        let p = p
                            .into_iter()
                            .map(|e| e.map(|x| if x == VIRT { t } else { x }))
                            .collect();
                        if !add(&mut found, start, t, p, None, false, None)? {
                            break 'targets;
                        }
                    }
                }
            }
            Algorithm::All => {
                let mut stop = false;
                eng.all_paths(t, false, None, &mut |p, start| {
                    if !add(&mut found, start, t, p.to_vec(), None, false, None)? {
                        stop = true;
                        return Ok(false);
                    }
                    Ok(true)
                })?;
                if stop {
                    break 'targets;
                }
            }
        }
    }

    // the output: each input row extended with the rows of its pair's paths
    let per_edge = spec.per_edge();
    let mut out = Table::new(vars.to_vec());
    let in_cols: Vec<Option<usize>> = vars.iter().map(|v| input.col_of(*v)).collect();
    let pos = |v: Option<VarId>| v.and_then(|v| vars.iter().position(|x| *x == v));
    let (src_c, tgt_c) = (
        match spec.source {
            PathEnd::Var(v) => pos(Some(v)),
            _ => None,
        },
        match spec.target {
            PathEnd::Var(v) => pos(Some(v)),
            _ => None,
        },
    );
    let (pi_c, ei_c, es_c, ep_c, eo_c, len_c, cost_c) = (
        pos(spec.path_index),
        pos(spec.edge_index),
        pos(spec.edge_s),
        pos(spec.edge_p),
        pos(spec.edge_o),
        pos(spec.length),
        pos(spec.cost),
    );
    let edge_c = pos(spec.edge);
    // the triple term of each edge, built once
    let mut terms: FxHashMap<(u64, u64, u64), Id> = FxHashMap::default();
    let mut triple_term = |(s, p, o): (u64, u64, u64)| -> Result<Id> {
        if let Some(&id) = terms.get(&(s, p, o)) {
            return Ok(id);
        }
        let decode = ctx.charge(0)?;
        let parts = (
            ctx.term_checked_into(Id(s), &decode)?,
            ctx.term_checked_into(Id(p), &decode)?,
            ctx.term_checked_into(Id(o), &decode)?,
        );
        let id =
            match parts {
                (Some(s), Some(Term::NamedNode(p)), Some(o)) => match s {
                    Term::NamedNode(n) => ctx.intern_term(&Term::Triple(Box::new(
                        oxrdf::Triple::new(oxrdf::NamedOrBlankNode::NamedNode(n), p, o),
                    ))),
                    Term::BlankNode(b) => ctx.intern_term(&Term::Triple(Box::new(
                        oxrdf::Triple::new(oxrdf::NamedOrBlankNode::BlankNode(b), p, o),
                    ))),
                    _ => Id::UNDEF,
                },
                _ => Id::UNDEF,
            };
        drop(decode);
        ctx.check()?;
        terms.insert((s, p, o), id);
        Ok(id)
    };
    let int = |n: usize| Id::from_i64(n as i64).unwrap_or(Id::UNDEF);
    let cost_id = |f: &Found| -> Id {
        if weighted {
            Id::from_f64(f.cost).unwrap_or_else(|| ctx.intern_value(&Value::Double(f.cost.into())))
        } else {
            int(f.len)
        }
    };
    let empty: Vec<u32> = Vec::new();
    let mut row = vec![Id::UNDEF; vars.len()];
    let mut path_row = vec![Id::UNDEF; vars.len()];
    let mut r = vec![Id::UNDEF; vars.len()];
    for (i, key) in keys.iter().enumerate() {
        if i % 1024 == 0 {
            ctx.check()?;
            ctx.check_output(out.len(), out.width())?;
        }
        let ids = match *key {
            (Some(s), Some(t)) => by_pair.get(&(s, t)).unwrap_or(&empty),
            (Some(s), None) => by_source.get(&s).unwrap_or(&empty),
            (None, Some(t)) => by_target.get(&t).unwrap_or(&empty),
            (None, None) => &empty,
        };
        for (j, c) in in_cols.iter().enumerate() {
            row[j] = c.map_or(Id::UNDEF, |c| input.cols[c][i]);
        }
        for &pid in ids {
            let f = &found[pid as usize];
            path_row.fill(Id::UNDEF);
            let set = |c: Option<usize>, id: Id, r: &mut Vec<Id>| {
                if let Some(c) = c {
                    r[c] = id;
                }
            };
            set(src_c, Id(f.start), &mut path_row);
            set(tgt_c, Id(f.end), &mut path_row);
            set(pi_c, int(pid as usize), &mut path_row);
            set(len_c, int(f.len), &mut path_row);
            set(cost_c, cost_id(f), &mut path_row);
            let n_rows = if per_edge { f.edges.len().max(1) } else { 1 };
            for k in 0..n_rows {
                r.copy_from_slice(&path_row);
                if per_edge && let Some(e) = f.edges.get(k) {
                    let (s, p, o) = e.triple();
                    set(ei_c, int(k), &mut r);
                    set(es_c, Id(s), &mut r);
                    set(ep_c, Id(p), &mut r);
                    set(eo_c, Id(o), &mut r);
                    if edge_c.is_some() {
                        set(edge_c, triple_term((s, p, o))?, &mut r);
                    }
                }
                // join with the input row: equal values or one side unbound
                let mut ok = true;
                for j in 0..vars.len() {
                    match (row[j].is_undef(), r[j].is_undef()) {
                        (_, true) => r[j] = row[j],
                        (true, false) => {}
                        (false, false) => ok &= row[j] == r[j],
                    }
                }
                if ok {
                    out.push_row(&r);
                }
            }
        }
    }
    let mut counters = serde_json::Map::new();
    counters.insert("searches".into(), eng.searches.get().into());
    counters.insert("visited".into(), eng.visited_total.get().into());
    counters.insert("paths".into(), (found.len() as u64).into());
    if eng.sweeps.get() > 0 {
        counters.insert("sweeps".into(), eng.sweeps.get().into());
    }
    if spec.max_degree.is_some() {
        counters.insert("skippedHubs".into(), eng.skipped_hubs.get().into());
    }
    Ok((out, counters))
}

/// Whether a path search runs as a resumable cursor reader (items 829, 1122 and 1123):
/// every path search without `path:limit` streams its matching paths under the memory
/// budget, with optional group input and optional nested edge patterns admitted up front.
pub(crate) fn stream_eligible(spec: &PathSearchSpec) -> bool {
    spec.limit.is_none()
}

type HbMap<K, V> = hashbrown::HashMap<K, V, FxBuildHasher>;
type HbSet<T> = hashbrown::HashSet<T, FxBuildHasher>;
/// Pre-admitted `(order, parent)` result of `SearchContext::run_dijkstra_bounded`.
type ChargedBoundedTree = (Vec<(u64, u32)>, HbMap<(u64, u32), Parent>);

#[inline]
fn vec_bytes<T>(cap: usize) -> u64 {
    (cap as u64).saturating_mul(size_of::<T>() as u64)
}

#[inline]
fn map_bytes<K: Eq + std::hash::Hash, V>(m: &HbMap<K, V>) -> u64 {
    m.allocation_size() as u64
}

#[inline]
fn set_bytes<T: Eq + std::hash::Hash>(s: &HbSet<T>) -> u64 {
    s.allocation_size() as u64
}

/// Upper bound on the new allocation when inserting one non-present entry into a hashbrown
/// table with `len` live items, `cap` reported capacity (`len + growth_left`), and
/// `alloc_bytes` physical allocation bytes.
#[inline]
fn hb_growth_for_one<E>(len: usize, cap: usize, alloc_bytes: u64) -> u64 {
    if len < cap {
        0
    } else if alloc_bytes == 0 {
        super::charged::table_bound(1, size_of::<E>())
    } else {
        // When `growth_left == 0` (`len == cap`), hashbrown either reuses a tombstone,
        // rehashes in place (0 bytes allocated), or doubles its bucket count. Doubling
        // buckets from `B` to `2*B` allocates strictly less than `2 * alloc_bytes` for
        // every `B >= 4` and entry layout, even when deletions dropped `cap` well below
        // the physical table's full capacity.
        alloc_bytes.saturating_mul(2)
    }
}

/// Sort `slice` by `key` with zero extra heap allocation and bounded `ctx.check()` polling.
fn sort_checked_by_key<T, K: Ord>(slice: &mut [T], ctx: &Ctx, key: impl Fn(&T) -> K) -> Result<()> {
    ctx.check()?;
    let n = slice.len();
    if n <= 4096 {
        slice.sort_unstable_by_key(key);
        return Ok(());
    }
    // Zero-allocation in-place heapsort (O(n log n) worst case) with periodic polling.
    let mut ticks = 0usize;
    let sift_down = |s: &mut [T], mut root: usize, end: usize, ticks: &mut usize| -> Result<()> {
        loop {
            let left = 2 * root + 1;
            if left >= end {
                return Ok(());
            }
            *ticks += 1;
            if (*ticks).is_multiple_of(4096) {
                ctx.check()?;
            }
            let right = left + 1;
            let mut swap = root;
            if key(&s[swap]) < key(&s[left]) {
                swap = left;
            }
            if right < end && key(&s[swap]) < key(&s[right]) {
                swap = right;
            }
            if swap == root {
                return Ok(());
            }
            s.swap(root, swap);
            root = swap;
        }
    };
    let mut start = n / 2;
    while start > 0 {
        start -= 1;
        sift_down(slice, start, n, &mut ticks)?;
    }
    let mut end = n;
    while end > 1 {
        end -= 1;
        slice.swap(0, end);
        sift_down(slice, 0, end, &mut ticks)?;
    }
    ctx.check()
}

/// Sort and deduplicate a `Vec<u64>` with zero extra heap allocation and bounded `ctx.check()`.
fn sort_dedup_u64_checked(v: &mut Vec<u64>, ctx: &Ctx) -> Result<()> {
    sort_checked_by_key(v.as_mut_slice(), ctx, |&x| x)?;
    if v.len() <= 1 {
        return Ok(());
    }
    let mut w = 1usize;
    for r in 1..v.len() {
        if r.is_multiple_of(4096) {
            ctx.check()?;
        }
        if v[r] != v[w - 1] {
            v[w] = v[r];
            w += 1;
        }
    }
    v.truncate(w);
    Ok(())
}

/// Resizable memory charge shared by streaming (`OwnedCharge`) and eager search-scoped
/// (`Charge<'_>`) pre-admission helpers.
trait ByteCharge {
    fn resize(&mut self, bytes: u64) -> Result<()>;
}

impl ByteCharge for super::ctx::OwnedCharge {
    #[inline]
    fn resize(&mut self, bytes: u64) -> Result<()> {
        super::ctx::OwnedCharge::resize(self, bytes)
    }
}

impl ByteCharge for Charge<'_> {
    #[inline]
    fn resize(&mut self, bytes: u64) -> Result<()> {
        Charge::resize(self, bytes)
    }
}

fn push_vec<T, C: ByteCharge>(v: &mut Vec<T>, item: T, charge: &mut C, base: u64) -> Result<u64> {
    if v.len() == v.capacity() {
        let old_bytes = vec_bytes::<T>(v.capacity());
        let new_cap = vec_grown(v.capacity(), v.len(), 1, size_of::<T>());
        let new_bytes = vec_bytes::<T>(new_cap);
        charge.resize(base.saturating_add(new_bytes))?;
        v.push(item);
        let actual = vec_bytes::<T>(v.capacity());
        let next = base.saturating_sub(old_bytes).saturating_add(actual);
        charge.resize(next)?;
        Ok(next)
    } else {
        v.push(item);
        Ok(base)
    }
}

fn push_heap<T: Ord, C: ByteCharge>(
    h: &mut BinaryHeap<T>,
    item: T,
    charge: &mut C,
    base: u64,
) -> Result<u64> {
    if h.len() == h.capacity() {
        let old_bytes = vec_bytes::<T>(h.capacity());
        let new_cap = vec_grown(h.capacity(), h.len(), 1, size_of::<T>());
        let new_bytes = vec_bytes::<T>(new_cap);
        charge.resize(base.saturating_add(new_bytes))?;
        h.push(item);
        let actual = vec_bytes::<T>(h.capacity());
        let next = base.saturating_sub(old_bytes).saturating_add(actual);
        charge.resize(next)?;
        Ok(next)
    } else {
        h.push(item);
        Ok(base)
    }
}

fn insert_hb_set<T: Eq + std::hash::Hash, C: ByteCharge>(
    s: &mut HbSet<T>,
    item: T,
    charge: &mut C,
    base: u64,
) -> Result<(bool, u64)> {
    if s.contains(&item) {
        return Ok((false, base));
    }
    let old_bytes = set_bytes(s);
    let growth = hb_growth_for_one::<T>(s.len(), s.capacity(), old_bytes);
    if growth != 0 {
        charge.resize(base.saturating_add(growth))?;
        let inserted = s.insert(item);
        let actual = set_bytes(s);
        debug_assert!(
            actual <= old_bytes.max(growth),
            "hashbrown set grew to {actual} bytes beyond predicted {growth}"
        );
        let next = base.saturating_sub(old_bytes).saturating_add(actual);
        charge.resize(next)?;
        Ok((inserted, next))
    } else {
        let inserted = s.insert(item);
        debug_assert_eq!(set_bytes(s), old_bytes);
        Ok((inserted, base))
    }
}

fn insert_hb_map<K: Eq + std::hash::Hash, V, C: ByteCharge>(
    m: &mut HbMap<K, V>,
    key: K,
    val: V,
    charge: &mut C,
    base: u64,
) -> Result<(Option<V>, u64)> {
    if let Some(slot) = m.get_mut(&key) {
        return Ok((Some(std::mem::replace(slot, val)), base));
    }
    let old_bytes = map_bytes(m);
    let growth = hb_growth_for_one::<(K, V)>(m.len(), m.capacity(), old_bytes);
    if growth != 0 {
        charge.resize(base.saturating_add(growth))?;
        let prev = m.insert(key, val);
        let actual = map_bytes(m);
        debug_assert!(
            actual <= old_bytes.max(growth),
            "hashbrown map grew to {actual} bytes beyond predicted {growth}"
        );
        let next = base.saturating_sub(old_bytes).saturating_add(actual);
        charge.resize(next)?;
        Ok((prev, next))
    } else {
        let prev = m.insert(key, val);
        debug_assert_eq!(map_bytes(m), old_bytes);
        Ok((prev, base))
    }
}

fn push_hb_map_vec<K: Eq + std::hash::Hash, V, C: ByteCharge>(
    m: &mut HbMap<K, Vec<V>>,
    vec_bytes_total: &mut u64,
    key: K,
    val: V,
    charge: &mut C,
    base: u64,
) -> Result<u64> {
    if let Some(v) = m.get_mut(&key) {
        if v.len() == v.capacity() {
            let old_v = vec_bytes::<V>(v.capacity());
            let new_cap = vec_grown(v.capacity(), v.len(), 1, size_of::<V>());
            let new_v = vec_bytes::<V>(new_cap);
            charge.resize(base.saturating_add(new_v))?;
            v.push(val);
            let actual_v = vec_bytes::<V>(v.capacity());
            *vec_bytes_total = vec_bytes_total
                .saturating_sub(old_v)
                .saturating_add(actual_v);
            let next = base.saturating_sub(old_v).saturating_add(actual_v);
            charge.resize(next)?;
            Ok(next)
        } else {
            v.push(val);
            Ok(base)
        }
    } else {
        let old_m = map_bytes(m);
        let m_growth = hb_growth_for_one::<(K, Vec<V>)>(m.len(), m.capacity(), old_m);
        let v_cap = vec_grown(0, 0, 1, size_of::<V>());
        let new_v = vec_bytes::<V>(v_cap);
        charge.resize(base.saturating_add(m_growth).saturating_add(new_v))?;
        let mut v = Vec::with_capacity(v_cap);
        v.push(val);
        let actual_v = vec_bytes::<V>(v.capacity());
        m.insert(key, v);
        let actual_m = map_bytes(m);
        debug_assert!(
            actual_m <= old_m.max(m_growth),
            "hashbrown map<vec> grew to {actual_m} bytes beyond predicted {m_growth}"
        );
        *vec_bytes_total = vec_bytes_total.saturating_add(actual_v);
        let next = base
            .saturating_sub(old_m)
            .saturating_add(actual_m)
            .saturating_add(actual_v);
        charge.resize(next)?;
        Ok(next)
    }
}

fn pareto_insert_charged<C: ByteCharge>(
    m: &mut HbMap<u64, Vec<(u32, Cost)>>,
    vec_bytes_total: &mut u64,
    key: u64,
    len: u32,
    cost: Cost,
    charge: &mut C,
    base: u64,
) -> Result<(bool, u64)> {
    if let Some(v) = m.get_mut(&key) {
        let Some((pos, replace)) = pareto_locate(v, len, cost) else {
            return Ok((false, base));
        };
        if replace {
            v[pos].1 = cost;
            let mut end = pos + 1;
            while end < v.len() && v[end].1 >= cost {
                end += 1;
            }
            if end > pos + 1 {
                v.drain(pos + 1..end);
            }
            return Ok((true, base));
        }
        let mut end = pos;
        while end < v.len() && v[end].1 >= cost {
            end += 1;
        }
        if end > pos {
            v[pos] = (len, cost);
            if end > pos + 1 {
                v.drain(pos + 1..end);
            }
            return Ok((true, base));
        }
        if v.len() == v.capacity() {
            let old_v = vec_bytes::<(u32, Cost)>(v.capacity());
            let new_cap = vec_grown(v.capacity(), v.len(), 1, size_of::<(u32, Cost)>());
            let new_v = vec_bytes::<(u32, Cost)>(new_cap);
            charge.resize(base.saturating_add(new_v))?;
            v.insert(pos, (len, cost));
            let actual_v = vec_bytes::<(u32, Cost)>(v.capacity());
            *vec_bytes_total = vec_bytes_total
                .saturating_sub(old_v)
                .saturating_add(actual_v);
            let next = base.saturating_sub(old_v).saturating_add(actual_v);
            charge.resize(next)?;
            Ok((true, next))
        } else {
            v.insert(pos, (len, cost));
            Ok((true, base))
        }
    } else {
        let old_m = map_bytes(m);
        let m_growth = hb_growth_for_one::<(u64, Vec<(u32, Cost)>)>(m.len(), m.capacity(), old_m);
        let v_cap = vec_grown(0, 0, 1, size_of::<(u32, Cost)>());
        let new_v = vec_bytes::<(u32, Cost)>(v_cap);
        charge.resize(base.saturating_add(m_growth).saturating_add(new_v))?;
        let mut v = Vec::with_capacity(v_cap);
        v.push((len, cost));
        let actual_v = vec_bytes::<(u32, Cost)>(v.capacity());
        m.insert(key, v);
        let actual_m = map_bytes(m);
        debug_assert!(
            actual_m <= old_m.max(m_growth),
            "hashbrown pareto map grew to {actual_m} bytes beyond predicted {m_growth}"
        );
        *vec_bytes_total = vec_bytes_total.saturating_add(actual_v);
        let next = base
            .saturating_sub(old_m)
            .saturating_add(actual_m)
            .saturating_add(actual_v);
        charge.resize(next)?;
        Ok((true, next))
    }
}

/// Pre-admitted edge maps built from a nested edge pattern child cursor (item 1123).
#[derive(Default)]
pub(crate) struct ChargedPatternEdges {
    by_start: HbMap<u64, Vec<u64>>,
    by_end: HbMap<u64, Vec<u64>>,
    vec_bytes: u64,
}

impl ChargedPatternEdges {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    pub(crate) fn retained_bytes(&self) -> u64 {
        map_bytes(&self.by_start)
            .saturating_add(map_bytes(&self.by_end))
            .saturating_add(self.vec_bytes)
    }

    pub(crate) fn push_batch(
        &mut self,
        ctx: &Ctx,
        (s, e): (VarId, VarId),
        batch: &Table,
        charge: &mut super::ctx::OwnedCharge,
    ) -> Result<()> {
        let (Some(sc), Some(ec)) = (batch.col_of(s), batch.col_of(e)) else {
            return Ok(());
        };
        let mut base = self.retained_bytes();
        for i in 0..batch.len() {
            if i.is_multiple_of(4096) {
                ctx.check()?;
            }
            let (a, b) = (batch.cols[sc][i], batch.cols[ec][i]);
            if a.is_undef() || b.is_undef() {
                continue;
            }
            base = push_hb_map_vec(
                &mut self.by_start,
                &mut self.vec_bytes,
                a.0,
                b.0,
                charge,
                base,
            )?;
            base = push_hb_map_vec(
                &mut self.by_end,
                &mut self.vec_bytes,
                b.0,
                a.0,
                charge,
                base,
            )?;
        }
        charge.resize(self.retained_bytes())
    }

    pub(crate) fn finish(&mut self, ctx: &Ctx) -> Result<()> {
        let mut ticks = 0usize;
        for v in self.by_start.values_mut().chain(self.by_end.values_mut()) {
            ticks += v.len().max(1);
            if ticks >= 4096 {
                ticks = 0;
                ctx.check()?;
            }
            sort_dedup_u64_checked(v, ctx)?;
        }
        ctx.check()
    }
}

/// Prune an `HbMap` in place without extra heap allocation, polling `ctx.check()` every
/// 4096 visited entries and stopping immediately on cancellation or timeout.
fn retain_hb_map_checked<K: Eq + std::hash::Hash, V>(
    map: &mut HbMap<K, V>,
    ctx: &Ctx,
    mut keep: impl FnMut(&K) -> bool,
) -> Result<()> {
    let mut visited = 0usize;
    let err: Cell<Option<Error>> = Cell::new(None);
    let mut iter = map.extract_if(|k, _| {
        let check_now = visited.is_multiple_of(4096);
        visited += 1;
        if check_now {
            #[cfg(test)]
            test_hooks::fire_prune_poll();
            if let Err(e) = ctx.check() {
                err.set(Some(e));
                return true;
            }
        }
        !keep(k)
    });
    for _ in iter.by_ref() {
        if let Some(e) = err.take() {
            return Err(e);
        }
    }
    if let Some(e) = err.into_inner() {
        return Err(e);
    }
    Ok(())
}

/// Pre-admitted search tree with exact physical byte accounting.
struct ChargedTree {
    root: u64,
    forward: bool,
    first: HbMap<u64, (Parent, u32)>,
    extra: HbMap<u64, Vec<Parent>>,
    extra_vec_bytes: u64,
    order: Vec<(u64, u32)>,
}

impl ChargedTree {
    fn new(root: u64, forward: bool) -> Self {
        Self {
            root,
            forward,
            first: HbMap::default(),
            extra: HbMap::default(),
            extra_vec_bytes: 0,
            order: Vec::new(),
        }
    }

    fn retained_bytes(&self) -> u64 {
        map_bytes(&self.first)
            .saturating_add(map_bytes(&self.extra))
            .saturating_add(self.extra_vec_bytes)
            .saturating_add(vec_bytes::<(u64, u32)>(self.order.capacity()))
    }

    fn contains(&self, x: u64) -> bool {
        x == self.root || self.first.contains_key(&x)
    }

    fn parent_at(&self, x: u64, i: usize, all: bool) -> Option<Parent> {
        match i {
            0 => self.first.get(&x).map(|e| e.0),
            _ if all => self.extra.get(&x).and_then(|v| v.get(i - 1)).copied(),
            _ => None,
        }
    }

    fn insert_first(
        &mut self,
        y: u64,
        parent: Parent,
        level: u32,
        charge: &mut super::ctx::OwnedCharge,
        base: u64,
    ) -> Result<u64> {
        let (_, next) = insert_hb_map(&mut self.first, y, (parent, level), charge, base)?;
        Ok(next)
    }

    fn push_extra(
        &mut self,
        y: u64,
        parent: Parent,
        charge: &mut super::ctx::OwnedCharge,
        base: u64,
    ) -> Result<u64> {
        push_hb_map_vec(
            &mut self.extra,
            &mut self.extra_vec_bytes,
            y,
            parent,
            charge,
            base,
        )
    }

    fn remove_extra(&mut self, y: u64) -> u64 {
        if let Some(old) = self.extra.remove(&y) {
            let freed = vec_bytes::<Parent>(old.capacity());
            self.extra_vec_bytes = self.extra_vec_bytes.saturating_sub(freed);
            freed
        } else {
            0
        }
    }

    fn push_order(
        &mut self,
        item: (u64, u32),
        charge: &mut super::ctx::OwnedCharge,
        base: u64,
    ) -> Result<u64> {
        push_vec(&mut self.order, item, charge, base)
    }

    fn retain_settled(&mut self, ctx: &Ctx, settled: &HbSet<u64>) -> Result<()> {
        retain_hb_map_checked(&mut self.first, ctx, |k| settled.contains(k))
    }

    fn build_ranks(
        &self,
        ctx: &Ctx,
        charge: &mut super::ctx::OwnedCharge,
        base: u64,
    ) -> Result<(HbMap<u64, u32>, u64)> {
        let bytes = super::charged::table_bound(self.order.len(), size_of::<(u64, u32)>());
        charge.resize(base.saturating_add(bytes))?;
        let mut rank = HbMap::with_capacity_and_hasher(self.order.len(), Default::default());
        for (i, &(n, _)) in self.order.iter().enumerate() {
            if i.is_multiple_of(4096) {
                ctx.check()?;
            }
            rank.insert(n, i as u32);
        }
        let actual = map_bytes(&rank);
        debug_assert!(actual <= bytes);
        let next = base.saturating_add(actual);
        charge.resize(next)?;
        Ok((rank, next))
    }
}

/// Resumable DFS path enumerator over a [`ChargedTree`], yielding one path at a time.
struct TreePathIter {
    all: bool,
    forward: bool,
    root: u64,
    stack: Vec<(u64, usize)>,
    low: Vec<u32>,
    links: Vec<(u64, Parent)>,
    on: HbSet<u64>,
    steps: u64,
}

impl TreePathIter {
    fn open(
        tree: &ChargedTree,
        m: u64,
        all: bool,
        rank: Option<&HbMap<u64, u32>>,
        charge: &mut super::ctx::OwnedCharge,
        base: u64,
    ) -> Result<Option<Self>> {
        if !tree.contains(m) {
            return Ok(None);
        }
        let init_rank = rank
            .filter(|_| all)
            .and_then(|r| r.get(&m).copied())
            .unwrap_or(0);
        let init_bytes = vec_bytes::<(u64, usize)>(4)
            .saturating_add(vec_bytes::<u32>(4))
            .saturating_add(super::charged::table_bound(1, size_of::<u64>()));
        charge.resize(base.saturating_add(init_bytes))?;
        let mut stack = Vec::with_capacity(4);
        stack.push((m, 0));
        let mut low = Vec::with_capacity(4);
        low.push(init_rank);
        let mut on = HbSet::with_capacity_and_hasher(1, Default::default());
        on.insert(m);
        let iter = Self {
            all,
            forward: tree.forward,
            root: tree.root,
            stack,
            low,
            links: Vec::new(),
            on,
            steps: 0,
        };
        charge.resize(base.saturating_add(iter.retained_bytes()))?;
        Ok(Some(iter))
    }

    fn retained_bytes(&self) -> u64 {
        vec_bytes::<(u64, usize)>(self.stack.capacity())
            .saturating_add(vec_bytes::<u32>(self.low.capacity()))
            .saturating_add(vec_bytes::<(u64, Parent)>(self.links.capacity()))
            .saturating_add(set_bytes(&self.on))
    }

    #[allow(clippy::too_many_arguments)]
    fn next_path(
        &mut self,
        ctx: &Ctx,
        tree: &ChargedTree,
        cost: Option<&HbMap<u64, Cost>>,
        rank: Option<&HbMap<u64, u32>>,
        forbid: &dyn Fn(u64) -> bool,
        charge: &mut super::ctx::OwnedCharge,
        outer_bytes: u64,
    ) -> Result<Option<Vec<Edge>>> {
        let prune_pair = if self.all { cost.zip(rank) } else { None };
        let rank_of = |n: u64| {
            prune_pair
                .and_then(|(_, r)| r.get(&n).copied())
                .unwrap_or(0)
        };
        while let Some(top) = self.stack.last_mut() {
            self.steps += 1;
            if self.steps.is_multiple_of(4096) {
                ctx.check()?;
            }
            let x = top.0;
            if x == self.root {
                let edge_bytes = vec_bytes::<Edge>(self.links.len());
                charge.resize(
                    outer_bytes
                        .saturating_add(self.retained_bytes())
                        .saturating_add(edge_bytes),
                )?;
                let mut edges = Vec::with_capacity(self.links.len());
                if self.forward {
                    for (idx, (node, q)) in self.links.iter().rev().enumerate() {
                        if idx.is_multiple_of(4096) {
                            ctx.check()?;
                        }
                        edges.push(Edge {
                            from: q.node,
                            to: *node,
                            p: q.p,
                            rev: q.rev,
                        });
                    }
                } else {
                    for (idx, (node, q)) in self.links.iter().enumerate() {
                        if idx.is_multiple_of(4096) {
                            ctx.check()?;
                        }
                        edges.push(Edge {
                            from: *node,
                            to: q.node,
                            p: q.p,
                            rev: q.rev,
                        });
                    }
                }
                self.stack.pop();
                self.low.pop();
                self.links.pop();
                self.on.remove(&x);
                return Ok(Some(edges));
            }
            let i = top.1;
            top.1 += 1;
            match tree.parent_at(x, i, self.all) {
                Some(q) if self.on.contains(&q.node) && q.node != self.root => {}
                Some(q) if forbid(q.node) => {}
                Some(q) => {
                    let low_top = self.low.last().copied().unwrap_or(0);
                    if let Some((c_map, r_map)) = prune_pair
                        && !self.escapes(
                            ctx,
                            tree,
                            x,
                            q.node,
                            low_top,
                            c_map,
                            r_map,
                            charge,
                            outer_bytes,
                        )?
                    {
                        continue;
                    }
                    let mut base = outer_bytes.saturating_add(self.retained_bytes());
                    base = push_vec(&mut self.links, (x, q), charge, base)?;
                    base = push_vec(&mut self.stack, (q.node, 0), charge, base)?;
                    base = push_vec(&mut self.low, low_top.min(rank_of(q.node)), charge, base)?;
                    let _ = insert_hb_set(&mut self.on, q.node, charge, base)?;
                }
                None => {
                    self.stack.pop();
                    self.low.pop();
                    self.links.pop();
                    self.on.remove(&x);
                }
            }
        }
        charge.resize(outer_bytes.saturating_add(self.retained_bytes()))?;
        Ok(None)
    }

    #[allow(clippy::too_many_arguments)]
    fn escapes(
        &mut self,
        ctx: &Ctx,
        tree: &ChargedTree,
        x: u64,
        q: u64,
        low: u32,
        cost: &HbMap<u64, Cost>,
        rank: &HbMap<u64, u32>,
        charge: &mut super::ctx::OwnedCharge,
        outer_bytes: u64,
    ) -> Result<bool> {
        if q == tree.root {
            return Ok(true);
        }
        if rank.get(&q).is_some_and(|&r| r < low) {
            return Ok(true);
        }
        let (Some(&c), Some(&cq)) = (cost.get(&x), cost.get(&q)) else {
            return Ok(true);
        };
        if cq < c {
            return Ok(true);
        }
        let base_fixed = outer_bytes.saturating_add(self.retained_bytes());
        let mut seen: HbSet<u64> = HbSet::default();
        let mut todo: Vec<u64> = Vec::new();
        let mut base = base_fixed;
        (_, base) = insert_hb_set(&mut seen, q, charge, base)?;
        base = push_vec(&mut todo, q, charge, base)?;
        let mut found = false;
        'search: while let Some(z) = todo.pop() {
            let mut i = 0;
            while let Some(p) = tree.parent_at(z, i, true) {
                i += 1;
                self.steps += 1;
                if self.steps.is_multiple_of(4096) {
                    ctx.check()?;
                }
                let y = p.node;
                if y == tree.root {
                    found = true;
                    break 'search;
                }
                if self.on.contains(&y) {
                    continue;
                }
                match cost.get(&y) {
                    Some(&cy) if cy < c => {
                        found = true;
                        break 'search;
                    }
                    None => {
                        found = true;
                        break 'search;
                    }
                    _ => {}
                }
                let (inserted, next_base) = insert_hb_set(&mut seen, y, charge, base)?;
                base = next_base;
                if inserted {
                    base = push_vec(&mut todo, y, charge, base)?;
                }
            }
        }
        drop(seen);
        drop(todo);
        charge.resize(base_fixed)?;
        Ok(found)
    }
}

/// Pre-admitted `without_loops` cutting out zero-weight cycles from joined halves.
fn without_loops_charged<C: ByteCharge>(
    ctx: &Ctx,
    s: u64,
    edges: Vec<Edge>,
    charge: &mut C,
    base_excluding_edges: u64,
) -> Result<Vec<Edge>> {
    let edges_bytes = vec_bytes::<Edge>(edges.capacity());
    let out_bytes = vec_bytes::<Edge>(edges.len());
    let at_bytes =
        super::charged::table_bound(edges.len().saturating_add(1), size_of::<(u64, usize)>());
    charge.resize(
        base_excluding_edges
            .saturating_add(edges_bytes)
            .saturating_add(out_bytes)
            .saturating_add(at_bytes),
    )?;
    let mut out: Vec<Edge> = Vec::with_capacity(edges.len());
    let mut at: HbMap<u64, usize> =
        HbMap::with_capacity_and_hasher(edges.len().saturating_add(1), Default::default());
    let actual_at = map_bytes(&at);
    debug_assert!(actual_at <= at_bytes);
    charge.resize(
        base_excluding_edges
            .saturating_add(edges_bytes)
            .saturating_add(vec_bytes::<Edge>(out.capacity()))
            .saturating_add(actual_at),
    )?;
    at.insert(s, 0);
    let mut ticks = 0usize;
    for e in edges {
        ticks += 1;
        if ticks.is_multiple_of(4096) {
            ctx.check()?;
        }
        if let Some(&i) = at.get(&e.to) {
            for d in out.drain(i..) {
                ticks += 1;
                if ticks.is_multiple_of(4096) {
                    ctx.check()?;
                }
                at.remove(&d.to);
            }
            at.insert(e.to, i);
            continue;
        }
        out.push(e);
        at.insert(e.to, out.len());
    }
    drop(at);
    let final_bytes = vec_bytes::<Edge>(out.capacity());
    charge.resize(base_excluding_edges.saturating_add(final_bytes))?;
    Ok(out)
}

struct StreamBans<'b> {
    nodes: &'b HbSet<u64>,
    edges: &'b HbSet<Edge>,
}

trait ChargedSink {
    const CHECKS: bool;
    fn put<C: ByteCharge>(
        &mut self,
        x: u64,
        step: Step,
        charge: Option<(&mut C, u64)>,
    ) -> Result<()>;
    fn len(&self) -> usize;
}

impl ChargedSink for Vec<(u64, Step)> {
    const CHECKS: bool = false;
    #[inline]
    fn put<C: ByteCharge>(
        &mut self,
        x: u64,
        step: Step,
        charge: Option<(&mut C, u64)>,
    ) -> Result<()> {
        if let Some((charge, base_excluding_out)) = charge {
            let base = base_excluding_out.saturating_add(vec_bytes::<(u64, Step)>(self.capacity()));
            let _ = push_vec(self, (x, step), charge, base)?;
        } else {
            self.push((x, step));
        }
        Ok(())
    }
    #[inline]
    fn len(&self) -> usize {
        Vec::len(self)
    }
}

impl ChargedSink for Count {
    const CHECKS: bool = true;
    #[inline]
    fn put<C: ByteCharge>(&mut self, _: u64, _: Step, _: Option<(&mut C, u64)>) -> Result<()> {
        self.0 += 1;
        Ok(())
    }
    #[inline]
    fn len(&self) -> usize {
        self.0
    }
}

#[allow(clippy::too_many_arguments)]
fn scan_lane_charged<O: ChargedSink, C: ByteCharge>(
    dedup: bool,
    sweeps: &mut u64,
    ctx: &Ctx,
    spec: &PathSearchSpec,
    lane: &Lane,
    frontier: &[u64],
    rev: bool,
    skip_self: bool,
    out: &mut O,
    cap: usize,
    mut charge: Option<(&mut C, u64)>,
) -> Result<()> {
    let snap = &ctx.snap;
    let (kc, nc, pc) = lane.cols();
    let gc = lane.perm.col_of(crate::index::G);
    let mask = (1 << kc) | (1 << nc) | (1 << gc) | pc.map_or(0, |c| 1 << c);
    let graph = &spec.graph;
    let fixed_p = lane.p.unwrap_or(0);
    let excl: &[u64] = if lane.p.is_none() { &spec.exclude } else { &[] };
    let mut last: Option<(u64, u64, u64)> = None;
    let mut emit = |x: u64, node: u64, p: u64, g: u64, out: &mut O| -> Result<()> {
        if !graph.accepts(g) || (skip_self && node == x) {
            return Ok(());
        }
        if !excl.is_empty() && excl.binary_search(&p).is_ok() {
            return Ok(());
        }
        if dedup && last == Some((x, node, p)) {
            return Ok(());
        }
        last = Some((x, node, p));
        let ch = charge.as_mut().map(|(c, b)| (&mut **c, *b));
        out.put(x, Step { node, p, rev }, ch)
    };
    let rows = match lane.p {
        Some(p) => snap.estimate(lane.perm, &[p]),
        None => snap.len(),
    };
    let sweep = ctx.opt.batched_paths
        && frontier.len() >= 64
        && (frontier.len() as u64).saturating_mul(SWEEP_RATIO) >= rows;
    if !sweep {
        let mut scanned = 0usize;
        for &x in frontier {
            let key_buf;
            let key: &[u64] = match lane.p {
                Some(p) => {
                    key_buf = [p, x];
                    &key_buf
                }
                None => std::slice::from_ref(&x),
            };
            snap.scan_between_cols_checked(
                lane.perm,
                pad(key, 0),
                pad(key, u64::MAX),
                mask,
                || ctx.check(),
                |c| {
                    match c {
                        Chunk::Block(b, s, e) => {
                            for i in s..e {
                                scanned += 1;
                                if scanned.is_multiple_of(4096) {
                                    ctx.check()?;
                                }
                                let p = pc.map_or(fixed_p, |c| b.cols[c][i]);
                                emit(x, b.cols[nc][i], p, b.cols[gc][i], out)?;
                                if out.len() >= cap {
                                    return Ok(false);
                                }
                            }
                        }
                        Chunk::Row(k) => {
                            scanned += 1;
                            if scanned.is_multiple_of(4096) {
                                ctx.check()?;
                            }
                            emit(x, k[nc], pc.map_or(fixed_p, |c| k[c]), k[gc], out)?;
                        }
                    }
                    if O::CHECKS {
                        ctx.check()?;
                    }
                    Ok(out.len() < cap)
                },
            )?;
        }
        return Ok(());
    }
    *sweeps += 1;
    let (first, last_node) = (frontier[0], frontier[frontier.len() - 1]);
    let (lo_buf, hi_buf);
    let (lo, hi): (&[u64], &[u64]) = match lane.p {
        Some(p) => {
            lo_buf = [p, first];
            hi_buf = [p, last_node];
            (&lo_buf, &hi_buf)
        }
        None => (
            std::slice::from_ref(&first),
            std::slice::from_ref(&last_node),
        ),
    };
    let mut j = 0;
    let mut scanned = 0usize;
    snap.scan_between_cols_checked(
        lane.perm,
        pad(lo, 0),
        pad(hi, u64::MAX),
        mask,
        || ctx.check(),
        |c| {
            ctx.check()?;
            match c {
                Chunk::Block(b, s, e) => {
                    let keys = &b.cols[kc][s..e];
                    let mut i = 0;
                    while i < keys.len() && j < frontier.len() {
                        scanned += 1;
                        if scanned.is_multiple_of(4096) {
                            ctx.check()?;
                        }
                        let (k, f) = (keys[i], frontier[j]);
                        if k < f {
                            i += keys[i..].partition_point(|&x| x < f);
                        } else if k > f {
                            j += frontier[j..].partition_point(|&x| x < k);
                        } else {
                            while i < keys.len() && keys[i] == f {
                                scanned += 1;
                                if scanned.is_multiple_of(4096) {
                                    ctx.check()?;
                                }
                                let r = s + i;
                                let p = pc.map_or(fixed_p, |c| b.cols[c][r]);
                                emit(f, b.cols[nc][r], p, b.cols[gc][r], out)?;
                                i += 1;
                            }
                        }
                    }
                }
                Chunk::Row(k) => {
                    scanned += 1;
                    if scanned.is_multiple_of(4096) {
                        ctx.check()?;
                    }
                    j += frontier[j..].partition_point(|&x| x < k[kc]);
                    if j < frontier.len() && frontier[j] == k[kc] {
                        emit(k[kc], k[nc], pc.map_or(fixed_p, |c| k[c]), k[gc], out)?;
                    }
                }
            }
            Ok(j < frontier.len())
        },
    )?;
    Ok(())
}

/// Owned, `Send` graph-reading engine shared across all streaming path-search jobs.
struct SearchContext {
    dedup: bool,
    succ: Vec<Side>,
    pred: Vec<Side>,
    empty_steps: std::sync::Arc<[Step]>,
    sinks: bool,
    degree: DegreeCount,
    pattern: ChargedPatternEdges,
    cache: HbMap<(u64, bool), std::sync::Arc<[Step]>>,
    cache_step_bytes: u64,
    weights: HbMap<(u64, u64, u64), f64>,
    hubs: HbMap<u64, bool>,
    roots: Vec<u64>,
    visited: u64,
    visited_total: u64,
    searches: u64,
    sweeps: u64,
    skipped_hubs: u64,
    paths_found: u64,
}

enum BidiWeightedOutcome {
    Empty,
    FallbackSingleSource,
    One(Found),
    Stream(Box<BidiWeightedAllJob>),
}

impl SearchContext {
    fn open(
        ctx: &Ctx,
        spec: &PathSearchSpec,
        pattern: ChargedPatternEdges,
        charge: &mut super::ctx::OwnedCharge,
    ) -> Result<Self> {
        let lane = |p: Option<u64>, subject_side: bool, is_pattern: bool| Lane {
            perm: match (p.is_some(), subject_side) {
                (true, true) => Perm::Pso,
                (true, false) => Perm::Pos,
                (false, true) => Perm::Spo,
                (false, false) => Perm::Osp,
            },
            p,
            subject_side,
            pattern: is_pattern,
        };
        let side = |lane: Lane, rev: bool, skip_self: bool| Side {
            lane,
            rev,
            skip_self,
        };
        let side_len: usize = match (&spec.edge_pattern, &spec.predicates) {
            (Some(_), _) | (None, None) => usize::from(spec.direction == Direction::Both) + 1,
            (None, Some(ps)) => ps
                .iter()
                .zip(&spec.directions)
                .map(|(_, &d)| usize::from(d == Direction::Both) + 1)
                .sum(),
        };
        let empty_arc_bytes = (2 * size_of::<usize>()) as u64;
        let side_bytes = vec_bytes::<Side>(side_len).saturating_mul(2);
        charge.resize(
            pattern
                .retained_bytes()
                .saturating_add(side_bytes)
                .saturating_add(empty_arc_bytes),
        )?;
        let mut succ = Vec::with_capacity(side_len);
        let mut pred = Vec::with_capacity(side_len);
        let empty_steps: std::sync::Arc<[Step]> = std::sync::Arc::from([]);
        let push_primary =
            |succ: &mut Vec<Side>, pred: &mut Vec<Side>, s: Lane, o: Lane, d: Direction| match d {
                Direction::Forward | Direction::Both => {
                    succ.push(side(s, false, false));
                    pred.push(side(o, false, false));
                }
                Direction::Backward => {
                    succ.push(side(o, true, false));
                    pred.push(side(s, true, false));
                }
            };
        let push_reverse =
            |succ: &mut Vec<Side>, pred: &mut Vec<Side>, s: Lane, o: Lane, d: Direction| {
                if d == Direction::Both {
                    succ.push(side(o, true, true));
                    pred.push(side(s, true, true));
                }
            };
        match (&spec.edge_pattern, &spec.predicates) {
            (Some(_), _) => {
                let (s, o, d) = (
                    lane(None, true, true),
                    lane(None, false, true),
                    spec.direction,
                );
                push_primary(&mut succ, &mut pred, s, o, d);
                push_reverse(&mut succ, &mut pred, s, o, d);
            }
            (None, Some(ps)) => {
                for (idx, (&p, &d)) in ps.iter().zip(&spec.directions).enumerate() {
                    if idx.is_multiple_of(4096) {
                        ctx.check()?;
                    }
                    push_primary(
                        &mut succ,
                        &mut pred,
                        lane(Some(p), true, false),
                        lane(Some(p), false, false),
                        d,
                    );
                }
                for (idx, (&p, &d)) in ps.iter().zip(&spec.directions).enumerate() {
                    if idx.is_multiple_of(4096) {
                        ctx.check()?;
                    }
                    push_reverse(
                        &mut succ,
                        &mut pred,
                        lane(Some(p), true, false),
                        lane(Some(p), false, false),
                        d,
                    );
                }
            }
            (None, None) => {
                let (s, o, d) = (
                    lane(None, true, false),
                    lane(None, false, false),
                    spec.direction,
                );
                push_primary(&mut succ, &mut pred, s, o, d);
                push_reverse(&mut succ, &mut pred, s, o, d);
            }
        }
        let sinks = succ
            .iter()
            .any(|s: &Side| s.lane.pattern || !s.lane.subject_side)
            || pred
                .iter()
                .any(|s: &Side| s.lane.pattern || s.lane.subject_side);
        let degree = match spec.max_degree {
            None => DegreeCount::Scan,
            Some(_) => degree_count(ctx, &spec.graph)?,
        };
        let search = Self {
            dedup: spec.graph.multi(),
            succ,
            pred,
            empty_steps,
            sinks,
            degree,
            pattern,
            cache: HbMap::default(),
            cache_step_bytes: 0,
            weights: HbMap::default(),
            hubs: HbMap::default(),
            roots: Vec::new(),
            visited: 0,
            visited_total: 0,
            searches: 0,
            sweeps: 0,
            skipped_hubs: 0,
            paths_found: 0,
        };
        charge.resize(search.retained_bytes())?;
        Ok(search)
    }

    fn retained_bytes(&self) -> u64 {
        let lanes = vec_bytes::<Side>(self.succ.capacity())
            .saturating_add(vec_bytes::<Side>(self.pred.capacity()))
            .saturating_add((2 * size_of::<usize>()) as u64);
        let cache = self.cache_step_bytes.saturating_add(map_bytes(&self.cache));
        let hubs = map_bytes(&self.hubs);
        let weights = map_bytes(&self.weights);
        self.pattern
            .retained_bytes()
            .saturating_add(lanes)
            .saturating_add(cache)
            .saturating_add(hubs)
            .saturating_add(weights)
            .saturating_add(vec_bytes::<u64>(self.roots.capacity()))
    }

    fn begin(&mut self) {
        self.visited = 0;
        self.searches += 1;
    }

    fn visit(&mut self, spec: &PathSearchSpec, n: u64) -> Result<()> {
        let v = self.visited + n;
        self.visited = v;
        self.visited_total += n;
        if v > spec.max_visited {
            return Err(bad(format!(
                "a search visited more than {} nodes; raise path:maxVisited, lower path:maxLength or add predicates",
                spec.max_visited
            )));
        }
        Ok(())
    }

    fn set_roots(
        &mut self,
        spec: &PathSearchSpec,
        roots: &[u64],
        charge: &mut super::ctx::OwnedCharge,
        outer_bytes: u64,
    ) -> Result<()> {
        if spec.max_degree.is_some() || self.sinks {
            if roots.len() > self.roots.capacity() {
                let add = vec_bytes::<u64>(roots.len());
                charge.resize(
                    outer_bytes
                        .saturating_add(self.retained_bytes())
                        .saturating_add(add),
                )?;
            }
            self.roots.clear();
            self.roots.reserve_exact(roots.len());
            self.roots.extend_from_slice(roots);
            charge.resize(outer_bytes.saturating_add(self.retained_bytes()))?;
        }
        Ok(())
    }

    fn sink(&self, ctx: &Ctx, x: u64, succ: bool) -> bool {
        self.sinks
            && ctx.snap.term_kind(Id(x)) == crate::store::TermKind::Literal
            && (succ || !self.roots.contains(&x))
    }

    fn blocked(
        &mut self,
        ctx: &Ctx,
        spec: &PathSearchSpec,
        x: u64,
        charge: &mut super::ctx::OwnedCharge,
        outer_bytes: u64,
    ) -> Result<bool> {
        if spec.max_degree.is_none() || self.roots.contains(&x) {
            return Ok(false);
        }
        let Some(limit) = spec.max_degree else {
            return Ok(false);
        };
        if let Some(&h) = self.hubs.get(&x) {
            return Ok(h);
        }
        let h = self.degree_above(ctx, spec, x, limit)?;
        let base = outer_bytes.saturating_add(self.retained_bytes());
        let _ = insert_hb_map(&mut self.hubs, x, h, charge, base)?;
        Ok(h)
    }

    fn pattern_degree(&self, s: &Side, x: u64) -> u64 {
        let map = if s.lane.subject_side {
            &self.pattern.by_start
        } else {
            &self.pattern.by_end
        };
        let ns = map.get(&x).map_or(&[][..], Vec::as_slice);
        let n = ns.len() as u64;
        if s.skip_self && ns.binary_search(&x).is_ok() {
            n - 1
        } else {
            n
        }
    }

    fn degree_above(
        &mut self,
        ctx: &Ctx,
        spec: &PathSearchSpec,
        x: u64,
        limit: u64,
    ) -> Result<bool> {
        if self.degree == DegreeCount::Zero {
            return Ok(false);
        }
        let snap = &ctx.snap;
        let mut upper = 0u64;
        for s in &self.succ {
            if s.lane.pattern {
                upper += self.pattern_degree(s, x);
            } else {
                let key_buf;
                let key: &[u64] = match s.lane.p {
                    Some(p) => {
                        key_buf = [p, x];
                        &key_buf
                    }
                    None => std::slice::from_ref(&x),
                };
                upper += snap.count(s.lane.perm, key)?;
            }
        }
        if upper <= limit {
            return Ok(false);
        }
        if self.succ.iter().all(|s| s.lane.pattern) {
            return Ok(true);
        }
        if self.degree == DegreeCount::Index {
            let excl = &spec.exclude;
            let mut less = 0u64;
            for s in &self.succ {
                let lane = &s.lane;
                if lane.p.is_none() {
                    let by = if lane.subject_side {
                        Perm::Pso
                    } else {
                        Perm::Pos
                    };
                    for &pe in excl {
                        less += snap.count(by, &[pe, x])?;
                    }
                }
                if s.skip_self {
                    less += match lane.p {
                        Some(p) => snap.count(Perm::Pso, &[p, x, x])?,
                        None => {
                            let mut loops = snap.count(Perm::Sop, &[x, x])?;
                            for &pe in excl {
                                loops = loops.saturating_sub(snap.count(Perm::Pso, &[pe, x, x])?);
                            }
                            loops
                        }
                    };
                }
            }
            return Ok(upper.saturating_sub(less) > limit);
        }
        let cap = usize::try_from(limit)
            .unwrap_or(usize::MAX)
            .saturating_add(1);
        let mut out = Count(0);
        let dedup = self.dedup;
        for s in &self.succ {
            if s.lane.pattern {
                let map = if s.lane.subject_side {
                    &self.pattern.by_start
                } else {
                    &self.pattern.by_end
                };
                let ns = map.get(&x).map_or(&[][..], Vec::as_slice);
                let n = ns.len();
                let deg = if s.skip_self && ns.binary_search(&x).is_ok() {
                    n - 1
                } else {
                    n
                };
                out.0 = out.0.saturating_add(deg);
            } else {
                scan_lane_charged(
                    dedup,
                    &mut self.sweeps,
                    ctx,
                    spec,
                    &s.lane,
                    &[x],
                    s.rev,
                    s.skip_self,
                    &mut out,
                    cap,
                    None::<(&mut super::ctx::OwnedCharge, u64)>,
                )?;
            }
            if out.0 >= cap {
                return Ok(true);
            }
        }
        Ok(false)
    }

    #[allow(clippy::too_many_arguments)]
    fn expand(
        &mut self,
        ctx: &Ctx,
        spec: &PathSearchSpec,
        frontier: &[u64],
        succ: bool,
        out: &mut Vec<(u64, Step)>,
        charge: &mut super::ctx::OwnedCharge,
        outer_bytes: u64,
    ) -> Result<()> {
        if spec.max_degree.is_none() && !self.sinks {
            return self.expand_all(ctx, spec, frontier, succ, out, charge, outer_bytes);
        }
        let mut kept: Option<Vec<u64>> = None;
        let mut filtered_bytes = 0u64;
        for (i, &x) in frontier.iter().enumerate() {
            if i.is_multiple_of(4096) {
                ctx.check()?;
            }
            let skip = if self.sink(ctx, x, succ) {
                true
            } else if self.blocked(
                ctx,
                spec,
                x,
                charge,
                outer_bytes.saturating_add(filtered_bytes),
            )? {
                self.skipped_hubs += 1;
                true
            } else {
                false
            };
            if skip && kept.is_none() {
                filtered_bytes = vec_bytes::<u64>(i);
                charge.resize(
                    self.retained_bytes()
                        .saturating_add(outer_bytes)
                        .saturating_add(filtered_bytes),
                )?;
                let mut filtered = Vec::with_capacity(i);
                filtered.extend_from_slice(&frontier[..i]);
                kept = Some(filtered);
            } else if !skip && let Some(filtered) = &mut kept {
                if filtered.len() == filtered.capacity() {
                    let next = vec_grown(filtered.capacity(), filtered.len(), 1, size_of::<u64>());
                    let next_bytes = vec_bytes::<u64>(next);
                    charge.resize(
                        self.retained_bytes()
                            .saturating_add(outer_bytes)
                            .saturating_add(filtered_bytes)
                            .saturating_add(next_bytes),
                    )?;
                    filtered.push(x);
                    filtered_bytes = vec_bytes::<u64>(filtered.capacity());
                    charge.resize(
                        self.retained_bytes()
                            .saturating_add(outer_bytes)
                            .saturating_add(filtered_bytes),
                    )?;
                } else {
                    filtered.push(x);
                }
            }
        }
        let res = match &kept {
            None => self.expand_all(ctx, spec, frontier, succ, out, charge, outer_bytes),
            Some(k) => {
                let k_bytes = vec_bytes::<u64>(k.capacity());
                self.expand_all(
                    ctx,
                    spec,
                    k,
                    succ,
                    out,
                    charge,
                    outer_bytes.saturating_add(k_bytes),
                )
            }
        };
        drop(kept);
        if res.is_ok() {
            charge.resize(
                self.retained_bytes()
                    .saturating_add(outer_bytes)
                    .saturating_add(vec_bytes::<(u64, Step)>(out.capacity())),
            )?;
        }
        res
    }

    #[allow(clippy::too_many_arguments)]
    fn expand_all(
        &mut self,
        ctx: &Ctx,
        spec: &PathSearchSpec,
        frontier: &[u64],
        succ: bool,
        out: &mut Vec<(u64, Step)>,
        charge: &mut super::ctx::OwnedCharge,
        outer_bytes: u64,
    ) -> Result<()> {
        if frontier.is_empty() {
            return Ok(());
        }
        let base = outer_bytes.saturating_add(self.retained_bytes());
        let dedup = self.dedup;
        let sides = if succ { &self.succ } else { &self.pred };
        for s in sides {
            let start = out.len();
            if s.lane.pattern {
                let map = if s.lane.subject_side {
                    &self.pattern.by_start
                } else {
                    &self.pattern.by_end
                };
                let mut scanned = 0usize;
                let mut cur_base = base.saturating_add(vec_bytes::<(u64, Step)>(out.capacity()));
                for &x in frontier {
                    for &node in map.get(&x).map_or(&[][..], Vec::as_slice) {
                        scanned += 1;
                        if scanned.is_multiple_of(4096) {
                            ctx.check()?;
                        }
                        if !(s.skip_self && node == x) {
                            cur_base = push_vec(
                                out,
                                (
                                    x,
                                    Step {
                                        node,
                                        p: 0,
                                        rev: s.rev,
                                    },
                                ),
                                charge,
                                cur_base,
                            )?;
                        }
                    }
                }
            } else {
                scan_lane_charged(
                    dedup,
                    &mut self.sweeps,
                    ctx,
                    spec,
                    &s.lane,
                    frontier,
                    s.rev,
                    s.skip_self,
                    out,
                    usize::MAX,
                    Some((charge, base)),
                )?;
            }
            if !succ && (s.lane.pattern || s.lane.subject_side) {
                let snap = &ctx.snap;
                let mut w = start;
                for r in start..out.len() {
                    if r.is_multiple_of(4096) {
                        ctx.check()?;
                    }
                    if snap.term_kind(Id(out[r].1.node)) != crate::store::TermKind::Literal {
                        out[w] = out[r];
                        w += 1;
                    }
                }
                out.truncate(w);
            }
        }
        ctx.check()
    }

    fn neighbours(
        &mut self,
        ctx: &Ctx,
        spec: &PathSearchSpec,
        x: u64,
        succ: bool,
        charge: &mut super::ctx::OwnedCharge,
        outer_bytes: u64,
    ) -> Result<std::sync::Arc<[Step]>> {
        if self.sink(ctx, x, succ) {
            return Ok(self.empty_steps.clone());
        }
        if self.blocked(ctx, spec, x, charge, outer_bytes)? {
            self.skipped_hubs += 1;
            return Ok(self.empty_steps.clone());
        }
        if let Some(n) = self.cache.get(&(x, succ)) {
            return Ok(n.clone());
        }
        let mut out = Vec::new();
        self.expand_all(ctx, spec, &[x], succ, &mut out, charge, outer_bytes)?;
        let out_bytes = vec_bytes::<(u64, Step)>(out.capacity());
        if out.is_empty() {
            let steps = self.empty_steps.clone();
            let base = outer_bytes
                .saturating_add(self.retained_bytes())
                .saturating_add(out_bytes);
            let _ = insert_hb_map(&mut self.cache, (x, succ), steps.clone(), charge, base)?;
            drop(out);
            charge.resize(outer_bytes.saturating_add(self.retained_bytes()))?;
            return Ok(steps);
        }
        let added = vec_bytes::<Step>(out.len()).saturating_add((2 * size_of::<usize>()) as u64);
        let growth = hb_growth_for_one::<((u64, bool), std::sync::Arc<[Step]>)>(
            self.cache.len(),
            self.cache.capacity(),
            map_bytes(&self.cache),
        );
        charge.resize(
            outer_bytes
                .saturating_add(self.retained_bytes())
                .saturating_add(out_bytes)
                .saturating_add(added)
                .saturating_add(growth),
        )?;
        let steps: std::sync::Arc<[Step]> = out.into_iter().map(|(_, s)| s).collect();
        self.cache_step_bytes = self.cache_step_bytes.saturating_add(added);
        self.cache.insert((x, succ), steps.clone());
        charge.resize(outer_bytes.saturating_add(self.retained_bytes()))?;
        Ok(steps)
    }

    fn weight(
        &mut self,
        ctx: &Ctx,
        spec: &PathSearchSpec,
        e: &Edge,
        charge: &mut super::ctx::OwnedCharge,
        outer_bytes: u64,
    ) -> Result<f64> {
        let Some(w) = spec.weight else {
            return Ok(1.0);
        };
        if w.is_none() || spec.reifies.is_none() {
            return Ok(spec.default_weight);
        }
        let key = e.triple();
        if let Some(&x) = self.weights.get(&key) {
            return Ok(x);
        }
        let x = lookup_weight_charged(ctx, spec, w, key)?;
        let w_base = outer_bytes.saturating_add(self.retained_bytes());
        let _ = insert_hb_map(&mut self.weights, key, x, charge, w_base)?;
        Ok(x)
    }

    fn exact_cost(
        &mut self,
        ctx: &Ctx,
        spec: &PathSearchSpec,
        edges: &[Edge],
        real: impl Fn(u64) -> u64,
        charge: &mut super::ctx::OwnedCharge,
        outer_bytes: u64,
    ) -> Result<Cost> {
        if !spec.weighted() {
            return Ok(Cost::of(edges.len() as f64));
        }
        let mut sum = Cost::ZERO;
        for (idx, e) in edges.iter().enumerate() {
            if idx.is_multiple_of(4096) {
                ctx.check()?;
            }
            let w = self.weight(ctx, spec, &e.map(&real), charge, outer_bytes)?;
            sum = sum.add(Cost::of(w));
        }
        Ok(sum)
    }

    fn path_cost(
        &mut self,
        ctx: &Ctx,
        spec: &PathSearchSpec,
        edges: &[Edge],
        charge: &mut super::ctx::OwnedCharge,
        outer_bytes: u64,
    ) -> Result<f64> {
        Ok(self
            .exact_cost(ctx, spec, edges, |x| x, charge, outer_bytes)?
            .hi)
    }

    #[allow(clippy::too_many_arguments)]
    fn grow_tree(
        &mut self,
        ctx: &Ctx,
        spec: &PathSearchSpec,
        tree: &mut ChargedTree,
        frontier: &[u64],
        depth: u32,
        all: bool,
        virt: u64,
        close: bool,
        bans: Option<&StreamBans>,
        charge: &mut super::ctx::OwnedCharge,
        outer_excluding_tree: u64,
    ) -> Result<Vec<u64>> {
        let mut scan: Vec<u64> = Vec::new();
        let mut virt_root = false;
        let base_start = self
            .retained_bytes()
            .saturating_add(outer_excluding_tree)
            .saturating_add(tree.retained_bytes());
        if !frontier.is_empty() {
            let scan_bytes = vec_bytes::<u64>(frontier.len());
            charge.resize(base_start.saturating_add(scan_bytes))?;
            scan.reserve_exact(frontier.len());
        }
        for (i, &x) in frontier.iter().enumerate() {
            if i.is_multiple_of(4096) {
                ctx.check()?;
            }
            if x == VIRT {
                if tree.root == VIRT {
                    virt_root = true;
                }
            } else if !(tree.root == VIRT && x == virt) {
                scan.push(x);
            }
        }
        let scan_bytes = vec_bytes::<u64>(scan.capacity());
        let mut found: Vec<(u64, Step)> = Vec::new();
        self.expand(
            ctx,
            spec,
            &scan,
            tree.forward,
            &mut found,
            charge,
            outer_excluding_tree
                .saturating_add(tree.retained_bytes())
                .saturating_add(scan_bytes),
        )?;
        drop(scan);
        if virt_root {
            let mut more: Vec<(u64, Step)> = Vec::new();
            let found_bytes = vec_bytes::<(u64, Step)>(found.capacity());
            self.expand(
                ctx,
                spec,
                &[virt],
                tree.forward,
                &mut more,
                charge,
                outer_excluding_tree
                    .saturating_add(tree.retained_bytes())
                    .saturating_add(found_bytes),
            )?;
            let mut base = self
                .retained_bytes()
                .saturating_add(outer_excluding_tree)
                .saturating_add(tree.retained_bytes())
                .saturating_add(found_bytes)
                .saturating_add(vec_bytes::<(u64, Step)>(more.capacity()));
            for (idx, (_, s)) in more.into_iter().enumerate() {
                if idx.is_multiple_of(4096) {
                    ctx.check()?;
                }
                base = push_vec(&mut found, (VIRT, s), charge, base)?;
            }
        }
        let found_bytes = vec_bytes::<(u64, Step)>(found.capacity());
        let mut next: Vec<u64> = Vec::new();
        let mut base = self
            .retained_bytes()
            .saturating_add(outer_excluding_tree)
            .saturating_add(tree.retained_bytes())
            .saturating_add(found_bytes);
        charge.resize(base)?;
        for (edge_idx, (x, st)) in found.into_iter().enumerate() {
            if edge_idx.is_multiple_of(4096) {
                ctx.check()?;
            }
            let y = if close && st.node == virt {
                VIRT
            } else {
                st.node
            };
            if y == tree.root {
                continue;
            }
            if let Some(b) = bans {
                let e = if tree.forward {
                    Edge {
                        from: x,
                        to: y,
                        p: st.p,
                        rev: st.rev,
                    }
                } else {
                    Edge {
                        from: y,
                        to: x,
                        p: st.p,
                        rev: st.rev,
                    }
                };
                if b.nodes.contains(&y) || b.edges.contains(&e) {
                    continue;
                }
            }
            let parent = Parent {
                node: x,
                p: st.p,
                rev: st.rev,
            };
            if let Some(&(_, lvl)) = tree.first.get(&y) {
                if all && lvl == depth + 1 {
                    base = tree.push_extra(y, parent, charge, base)?;
                }
            } else {
                base = tree.insert_first(y, parent, depth + 1, charge, base)?;
                base = push_vec(&mut next, y, charge, base)?;
            }
        }
        // `found` has been consumed and dropped.
        let mut base = self
            .retained_bytes()
            .saturating_add(outer_excluding_tree)
            .saturating_add(tree.retained_bytes())
            .saturating_add(vec_bytes::<u64>(next.capacity()));
        charge.resize(base)?;
        self.visit(spec, next.len() as u64)?;
        sort_checked_by_key(&mut next, ctx, |&y| y)?;
        for (i, &y) in next.iter().enumerate() {
            if i.is_multiple_of(4096) {
                ctx.check()?;
            }
            base = tree.push_order((y, depth + 1), charge, base)?;
        }
        charge.resize(
            self.retained_bytes()
                .saturating_add(outer_excluding_tree)
                .saturating_add(tree.retained_bytes())
                .saturating_add(vec_bytes::<u64>(next.capacity())),
        )?;
        Ok(next)
    }

    #[allow(clippy::too_many_arguments)]
    fn run_bfs(
        &mut self,
        ctx: &Ctx,
        spec: &PathSearchSpec,
        root: u64,
        forward: bool,
        targets: Option<&[u64]>,
        all: bool,
        close: bool,
        charge: &mut super::ctx::OwnedCharge,
        outer_bytes: u64,
    ) -> Result<ChargedTree> {
        self.set_roots(spec, &[root], charge, outer_bytes)?;
        let mut tree = ChargedTree::new(root, forward);
        let mut frontier: Vec<u64> = Vec::new();
        let mut missing: HbSet<u64> = HbSet::default();
        let mut base = self.retained_bytes().saturating_add(outer_bytes);
        base = push_vec(&mut frontier, root, charge, base)?;
        if let Some(ts) = targets {
            for (i, &x) in ts.iter().enumerate() {
                if i.is_multiple_of(4096) {
                    ctx.check()?;
                }
                let key = if close && x == root { VIRT } else { x };
                (_, base) = insert_hb_set(&mut missing, key, charge, base)?;
            }
        }
        let mut depth = 0u32;
        let max = spec.max_len.unwrap_or(u32::MAX);
        self.visit(spec, 1)?;
        while !frontier.is_empty() && depth < max {
            let f_bytes = vec_bytes::<u64>(frontier.capacity());
            let m_bytes = set_bytes(&missing);
            let next = self.grow_tree(
                ctx,
                spec,
                &mut tree,
                &frontier,
                depth,
                all,
                root,
                close,
                None,
                charge,
                outer_bytes.saturating_add(f_bytes).saturating_add(m_bytes),
            )?;
            frontier = next;
            depth += 1;
            if targets.is_some() {
                for (idx, y) in frontier.iter().enumerate() {
                    if idx.is_multiple_of(4096) {
                        ctx.check()?;
                    }
                    missing.remove(y);
                }
                if missing.is_empty() {
                    break;
                }
            }
            charge.resize(
                self.retained_bytes()
                    .saturating_add(outer_bytes)
                    .saturating_add(tree.retained_bytes())
                    .saturating_add(vec_bytes::<u64>(frontier.capacity()))
                    .saturating_add(set_bytes(&missing)),
            )?;
        }
        drop(frontier);
        drop(missing);
        charge.resize(
            self.retained_bytes()
                .saturating_add(outer_bytes)
                .saturating_add(tree.retained_bytes()),
        )?;
        Ok(tree)
    }

    #[allow(clippy::too_many_arguments)]
    fn run_bidi(
        &mut self,
        ctx: &Ctx,
        spec: &PathSearchSpec,
        s: u64,
        tt: u64,
        virt: u64,
        all: bool,
        bans: Option<&StreamBans>,
        max: Option<u32>,
        charge: &mut super::ctx::OwnedCharge,
        outer_bytes: u64,
    ) -> Result<Option<(ChargedTree, ChargedTree, Vec<u64>)>> {
        let cycle = tt == VIRT;
        self.set_roots(
            spec,
            &[s, if cycle { virt } else { tt }],
            charge,
            outer_bytes,
        )?;
        let mut f = ChargedTree::new(s, true);
        let mut b = ChargedTree::new(tt, false);
        let mut ff: Vec<u64> = Vec::new();
        let mut bf: Vec<u64> = Vec::new();
        let mut base = self.retained_bytes().saturating_add(outer_bytes);
        base = push_vec(&mut ff, s, charge, base)?;
        let _ = push_vec(&mut bf, tt, charge, base)?;
        let (mut fd, mut bd) = (0u32, 0u32);
        let max = max.unwrap_or(u32::MAX);
        self.visit(spec, 2)?;
        loop {
            if fd.saturating_add(bd) >= max || ff.is_empty() || bf.is_empty() {
                drop(f);
                drop(b);
                drop(ff);
                drop(bf);
                charge.resize(self.retained_bytes().saturating_add(outer_bytes))?;
                return Ok(None);
            }
            let forward = ff.len() <= bf.len();
            if forward {
                let extra = outer_bytes
                    .saturating_add(b.retained_bytes())
                    .saturating_add(vec_bytes::<u64>(ff.capacity()))
                    .saturating_add(vec_bytes::<u64>(bf.capacity()));
                ff = self.grow_tree(
                    ctx, spec, &mut f, &ff, fd, all, virt, cycle, bans, charge, extra,
                )?;
                fd += 1;
            } else {
                let extra = outer_bytes
                    .saturating_add(f.retained_bytes())
                    .saturating_add(vec_bytes::<u64>(ff.capacity()))
                    .saturating_add(vec_bytes::<u64>(bf.capacity()));
                bf = self.grow_tree(
                    ctx, spec, &mut b, &bf, bd, all, virt, false, bans, charge, extra,
                )?;
                bd += 1;
            }
            let mut meet: Vec<u64> = Vec::new();
            let trees_and_fronts = outer_bytes
                .saturating_add(f.retained_bytes())
                .saturating_add(b.retained_bytes())
                .saturating_add(vec_bytes::<u64>(ff.capacity()))
                .saturating_add(vec_bytes::<u64>(bf.capacity()));
            let mut mbase = self.retained_bytes().saturating_add(trees_and_fronts);
            charge.resize(mbase)?;
            if forward {
                for (i, &y) in ff.iter().enumerate() {
                    if i.is_multiple_of(4096) {
                        ctx.check()?;
                    }
                    if b.contains(y) {
                        mbase = push_vec(&mut meet, y, charge, mbase)?;
                    }
                }
            } else {
                for (i, &y) in bf.iter().enumerate() {
                    if i.is_multiple_of(4096) {
                        ctx.check()?;
                    }
                    if f.contains(y) {
                        mbase = push_vec(&mut meet, y, charge, mbase)?;
                    }
                }
            }
            if spec.max_degree.is_some() || self.sinks {
                let mut w = 0usize;
                for r in 0..meet.len() {
                    if r.is_multiple_of(4096) {
                        ctx.check()?;
                    }
                    let m = meet[r];
                    let extra_for_block =
                        trees_and_fronts.saturating_add(vec_bytes::<u64>(meet.capacity()));
                    let pass = m == s
                        || m == tt
                        || !(self.sink(ctx, m, true)
                            || self.blocked(ctx, spec, m, charge, extra_for_block)?);
                    if pass {
                        meet[w] = m;
                        w += 1;
                    }
                }
                meet.truncate(w);
            }
            if meet.is_empty() {
                drop(meet);
                charge.resize(self.retained_bytes().saturating_add(trees_and_fronts))?;
                continue;
            }
            drop(ff);
            drop(bf);
            charge.resize(
                self.retained_bytes()
                    .saturating_add(outer_bytes)
                    .saturating_add(f.retained_bytes())
                    .saturating_add(b.retained_bytes())
                    .saturating_add(vec_bytes::<u64>(meet.capacity())),
            )?;
            return Ok(Some((f, b, meet)));
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn run_dijkstra(
        &mut self,
        ctx: &Ctx,
        spec: &PathSearchSpec,
        root: u64,
        forward: bool,
        targets: Option<&[u64]>,
        all: bool,
        close: bool,
        banned_nodes: Option<&HbSet<u64>>,
        banned_edges: Option<&HbSet<Edge>>,
        virt: u64,
        charge: &mut super::ctx::OwnedCharge,
        outer_bytes: u64,
    ) -> Result<(ChargedTree, HbMap<u64, Cost>)> {
        self.set_roots(
            spec,
            &[if root == VIRT { virt } else { root }],
            charge,
            outer_bytes,
        )?;
        let mut tree = ChargedTree::new(root, forward);
        let mut dist: HbMap<u64, Cost> = HbMap::default();
        let mut settled: HbSet<u64> = HbSet::default();
        let mut heap: BinaryHeap<Reverse<(Cost, u64)>> = BinaryHeap::new();
        let mut missing: HbSet<u64> = HbSet::default();
        let mut base = self.retained_bytes().saturating_add(outer_bytes);
        if let Some(ts) = targets {
            for (i, &x) in ts.iter().enumerate() {
                if i.is_multiple_of(4096) {
                    ctx.check()?;
                }
                let key = if close && x == virt { VIRT } else { x };
                (_, base) = insert_hb_set(&mut missing, key, charge, base)?;
            }
        }
        (_, base) = insert_hb_map(&mut dist, root, Cost::ZERO, charge, base)?;
        base = push_heap(&mut heap, Reverse((Cost::ZERO, root)), charge, base)?;
        let mut n = 0u64;
        let mut last_cost: Option<Cost> = None;
        while let Some(Reverse((d, u))) = heap.pop() {
            n += 1;
            if n.is_multiple_of(4096) {
                ctx.check()?;
            }
            if last_cost.is_some_and(|c| d > c) {
                break;
            }
            let (inserted, next_base) = insert_hb_set(&mut settled, u, charge, base)?;
            base = next_base;
            if !inserted {
                continue;
            }
            if u != root {
                base = tree.push_order((u, 0), charge, base)?;
            }
            self.visit(spec, 1)?;
            if targets.is_some() {
                missing.remove(&u);
                if missing.is_empty() {
                    if !all {
                        break;
                    }
                    last_cost = Some(d);
                }
            }
            if u == VIRT && root != VIRT {
                continue;
            }
            let scan = if u == VIRT { virt } else { u };
            let local_outer = outer_bytes
                .saturating_add(tree.retained_bytes())
                .saturating_add(map_bytes(&dist))
                .saturating_add(set_bytes(&settled))
                .saturating_add(vec_bytes::<Reverse<(Cost, u64)>>(heap.capacity()))
                .saturating_add(set_bytes(&missing));
            let mut cached_steps: Option<std::sync::Arc<[Step]>> = None;
            let mut out_steps: Vec<(u64, Step)> = Vec::new();
            if banned_nodes.is_some() {
                cached_steps =
                    Some(self.neighbours(ctx, spec, scan, forward, charge, local_outer)?);
            } else {
                self.expand(
                    ctx,
                    spec,
                    &[scan],
                    forward,
                    &mut out_steps,
                    charge,
                    local_outer,
                )?;
            }
            let out_bytes = vec_bytes::<(u64, Step)>(out_steps.capacity());
            let step_count = cached_steps.as_ref().map_or(out_steps.len(), |s| s.len());
            for idx in 0..step_count {
                if idx.is_multiple_of(4096) {
                    ctx.check()?;
                }
                let st = match &cached_steps {
                    Some(s) => s[idx],
                    None => out_steps[idx].1,
                };
                let y = if close && st.node == virt {
                    VIRT
                } else {
                    st.node
                };
                if banned_nodes.is_some_and(|b| b.contains(&y)) {
                    continue;
                }
                let tied = settled.contains(&y);
                if tied && !all {
                    continue;
                }
                let e = if forward {
                    Edge {
                        from: u,
                        to: y,
                        p: st.p,
                        rev: st.rev,
                    }
                } else {
                    Edge {
                        from: y,
                        to: u,
                        p: st.p,
                        rev: st.rev,
                    }
                };
                if banned_edges.is_some_and(|b| b.contains(&e)) {
                    continue;
                }
                let real = |x: u64| if x == VIRT { virt } else { x };
                let step_outer = outer_bytes
                    .saturating_add(tree.retained_bytes())
                    .saturating_add(map_bytes(&dist))
                    .saturating_add(set_bytes(&settled))
                    .saturating_add(vec_bytes::<Reverse<(Cost, u64)>>(heap.capacity()))
                    .saturating_add(set_bytes(&missing))
                    .saturating_add(out_bytes);
                let w = self.weight(ctx, spec, &e.map(real), charge, step_outer)?;
                let nd = d.add(Cost::of(w));
                let parent = Parent {
                    node: u,
                    p: st.p,
                    rev: st.rev,
                };
                let mut cur_base = self.retained_bytes().saturating_add(step_outer);
                match dist.get(&y) {
                    Some(&cur) if nd > cur => {}
                    Some(&cur) if nd == cur => {
                        if all && y != root {
                            let _ = tree.push_extra(y, parent, charge, cur_base)?;
                        }
                    }
                    _ if tied => {}
                    _ => {
                        (_, cur_base) = insert_hb_map(&mut dist, y, nd, charge, cur_base)?;
                        cur_base = tree.insert_first(y, parent, 0, charge, cur_base)?;
                        let freed = tree.remove_extra(y);
                        if freed > 0 {
                            cur_base = cur_base.saturating_sub(freed);
                            charge.resize(cur_base)?;
                        }
                        let _ = push_heap(&mut heap, Reverse((nd, y)), charge, cur_base)?;
                    }
                }
            }
            drop(out_steps);
            drop(cached_steps);
            base = self
                .retained_bytes()
                .saturating_add(outer_bytes)
                .saturating_add(tree.retained_bytes())
                .saturating_add(map_bytes(&dist))
                .saturating_add(set_bytes(&settled))
                .saturating_add(vec_bytes::<Reverse<(Cost, u64)>>(heap.capacity()))
                .saturating_add(set_bytes(&missing));
            charge.resize(base)?;
        }
        if settled.len() < dist.len() {
            tree.retain_settled(ctx, &settled)?;
            retain_hb_map_checked(&mut dist, ctx, |k| settled.contains(k))?;
        }
        drop(settled);
        drop(heap);
        drop(missing);
        charge.resize(
            self.retained_bytes()
                .saturating_add(outer_bytes)
                .saturating_add(tree.retained_bytes())
                .saturating_add(map_bytes(&dist)),
        )?;
        Ok((tree, dist))
    }

    #[allow(clippy::too_many_arguments)]
    fn run_dijkstra_pair(
        &mut self,
        ctx: &Ctx,
        spec: &PathSearchSpec,
        s: u64,
        t: u64,
        banned_nodes: Option<&HbSet<u64>>,
        banned_edges: Option<&HbSet<Edge>>,
        charge: &mut super::ctx::OwnedCharge,
        outer_bytes: u64,
    ) -> Result<Option<(Vec<Edge>, f64)>> {
        debug_assert!(s != t && s != VIRT && t != VIRT);
        self.set_roots(spec, &[s, t], charge, outer_bytes)?;
        let mut trees = [ChargedTree::new(s, true), ChargedTree::new(t, false)];
        let mut dist: [HbMap<u64, Cost>; 2] = Default::default();
        let mut settled: [HbSet<u64>; 2] = Default::default();
        let mut heaps: [BinaryHeap<Reverse<(Cost, u64)>>; 2] = Default::default();
        let mut base = self.retained_bytes().saturating_add(outer_bytes);
        for (i, r) in [s, t].into_iter().enumerate() {
            (_, base) = insert_hb_map(&mut dist[i], r, Cost::ZERO, charge, base)?;
            base = push_heap(&mut heaps[i], Reverse((Cost::ZERO, r)), charge, base)?;
        }
        let local_bytes =
            |trees: &[ChargedTree; 2],
             dist: &[HbMap<u64, Cost>; 2],
             settled: &[HbSet<u64>; 2],
             heaps: &[BinaryHeap<Reverse<(Cost, u64)>>; 2]| {
                trees[0]
                    .retained_bytes()
                    .saturating_add(trees[1].retained_bytes())
                    .saturating_add(map_bytes(&dist[0]))
                    .saturating_add(map_bytes(&dist[1]))
                    .saturating_add(set_bytes(&settled[0]))
                    .saturating_add(set_bytes(&settled[1]))
                    .saturating_add(vec_bytes::<Reverse<(Cost, u64)>>(heaps[0].capacity()))
                    .saturating_add(vec_bytes::<Reverse<(Cost, u64)>>(heaps[1].capacity()))
            };
        let mut best: Option<(Cost, u64)> = None;
        let mut n = 0u64;
        loop {
            for i in 0..2 {
                while let Some(&Reverse((_, u))) = heaps[i].peek() {
                    if !settled[i].contains(&u) {
                        break;
                    }
                    heaps[i].pop();
                    n += 1;
                    if n.is_multiple_of(4096) {
                        ctx.check()?;
                    }
                }
            }
            let (Some(&Reverse((df, _))), Some(&Reverse((db, _)))) =
                (heaps[0].peek(), heaps[1].peek())
            else {
                break;
            };
            if best.is_some_and(|(mu, _)| df.add(db) >= mu) {
                break;
            }
            let side = usize::from(heaps[0].len() > heaps[1].len());
            let forward = side == 0;
            let Reverse((d, u)) = heaps[side].pop().unwrap();
            base = self
                .retained_bytes()
                .saturating_add(outer_bytes)
                .saturating_add(local_bytes(&trees, &dist, &settled, &heaps));
            (_, base) = insert_hb_set(&mut settled[side], u, charge, base)?;
            if u != trees[side].root {
                let _ = trees[side].push_order((u, 0), charge, base)?;
            }
            self.visit(spec, 1)?;
            n += 1;
            if n.is_multiple_of(4096) {
                ctx.check()?;
            }
            let loc = outer_bytes.saturating_add(local_bytes(&trees, &dist, &settled, &heaps));
            let mut cached_steps: Option<std::sync::Arc<[Step]>> = None;
            let mut out_steps: Vec<(u64, Step)> = Vec::new();
            if banned_nodes.is_some() {
                cached_steps = Some(self.neighbours(ctx, spec, u, forward, charge, loc)?);
            } else {
                self.expand(ctx, spec, &[u], forward, &mut out_steps, charge, loc)?;
            }
            let out_bytes = vec_bytes::<(u64, Step)>(out_steps.capacity());
            let step_count = cached_steps.as_ref().map_or(out_steps.len(), |s| s.len());
            for idx in 0..step_count {
                if idx.is_multiple_of(4096) {
                    ctx.check()?;
                }
                let st = match &cached_steps {
                    Some(s) => s[idx],
                    None => out_steps[idx].1,
                };
                let y = st.node;
                if settled[side].contains(&y) || banned_nodes.is_some_and(|b| b.contains(&y)) {
                    continue;
                }
                let e = if forward {
                    Edge {
                        from: u,
                        to: y,
                        p: st.p,
                        rev: st.rev,
                    }
                } else {
                    Edge {
                        from: y,
                        to: u,
                        p: st.p,
                        rev: st.rev,
                    }
                };
                if banned_edges.is_some_and(|b| b.contains(&e)) {
                    continue;
                }
                let step_outer = outer_bytes
                    .saturating_add(local_bytes(&trees, &dist, &settled, &heaps))
                    .saturating_add(out_bytes);
                let w = self.weight(ctx, spec, &e, charge, step_outer)?;
                let nd = d.add(Cost::of(w));
                if dist[side].get(&y).is_some_and(|&c| nd >= c) {
                    continue;
                }
                let mut cur_base = self.retained_bytes().saturating_add(step_outer);
                (_, cur_base) = insert_hb_map(&mut dist[side], y, nd, charge, cur_base)?;
                cur_base = trees[side].insert_first(
                    y,
                    Parent {
                        node: u,
                        p: st.p,
                        rev: st.rev,
                    },
                    0,
                    charge,
                    cur_base,
                )?;
                let _ = push_heap(&mut heaps[side], Reverse((nd, y)), charge, cur_base)?;
                if let Some(&o) = dist[1 - side].get(&y)
                    && best.is_none_or(|(mu, _)| nd.add(o) < mu)
                {
                    let block_outer = outer_bytes
                        .saturating_add(local_bytes(&trees, &dist, &settled, &heaps))
                        .saturating_add(out_bytes);
                    let pass = y == s
                        || y == t
                        || !(self.sink(ctx, y, true)
                            || self.blocked(ctx, spec, y, charge, block_outer)?);
                    if pass {
                        best = Some((nd.add(o), y));
                    }
                }
            }
            drop(out_steps);
            drop(cached_steps);
            charge.resize(
                self.retained_bytes()
                    .saturating_add(outer_bytes)
                    .saturating_add(local_bytes(&trees, &dist, &settled, &heaps)),
            )?;
        }
        drop(dist);
        drop(settled);
        drop(heaps);
        let Some((_, m)) = best else {
            drop(trees);
            charge.resize(self.retained_bytes().saturating_add(outer_bytes))?;
            return Ok(None);
        };
        let t_bytes = trees[0]
            .retained_bytes()
            .saturating_add(trees[1].retained_bytes());
        let base_trees = self
            .retained_bytes()
            .saturating_add(outer_bytes)
            .saturating_add(t_bytes);
        charge.resize(base_trees)?;
        let a = match TreePathIter::open(&trees[0], m, false, None, charge, base_trees)? {
            Some(mut it) => {
                let p = it.next_path(ctx, &trees[0], None, None, &|_| false, charge, base_trees)?;
                drop(it);
                p
            }
            None => None,
        };
        let Some(a) = a else {
            drop(trees);
            charge.resize(self.retained_bytes().saturating_add(outer_bytes))?;
            return Ok(None);
        };
        let a_bytes = vec_bytes::<Edge>(a.capacity());
        let base_a = base_trees.saturating_add(a_bytes);
        charge.resize(base_a)?;
        let z = match TreePathIter::open(&trees[1], m, false, None, charge, base_a)? {
            Some(mut it) => {
                let p = it.next_path(ctx, &trees[1], None, None, &|_| false, charge, base_a)?;
                drop(it);
                p
            }
            None => None,
        };
        drop(trees);
        let Some(z) = z else {
            drop(a);
            charge.resize(self.retained_bytes().saturating_add(outer_bytes))?;
            return Ok(None);
        };
        let combined_len = a.len().saturating_add(z.len());
        let z_bytes = vec_bytes::<Edge>(z.capacity());
        let base_no_trees = self.retained_bytes().saturating_add(outer_bytes);
        charge.resize(
            base_no_trees
                .saturating_add(a_bytes)
                .saturating_add(z_bytes)
                .saturating_add(vec_bytes::<Edge>(combined_len)),
        )?;
        let mut combined = Vec::with_capacity(combined_len);
        for (idx, edge) in a.into_iter().chain(z).enumerate() {
            if idx.is_multiple_of(4096) {
                ctx.check()?;
            }
            combined.push(edge);
        }
        let path = without_loops_charged(ctx, s, combined, charge, base_no_trees)?;
        let path_outer = outer_bytes.saturating_add(vec_bytes::<Edge>(path.capacity()));
        let cost = self.path_cost(ctx, spec, &path, charge, path_outer)?;
        Ok(Some((path, cost)))
    }

    #[allow(clippy::too_many_arguments)]
    fn run_dijkstra_pair_all(
        &mut self,
        ctx: &Ctx,
        spec: &PathSearchSpec,
        s: u64,
        t: u64,
        all: bool,
        need_edges: bool,
        charge: &mut super::ctx::OwnedCharge,
        outer_bytes: u64,
    ) -> Result<BidiWeightedOutcome> {
        let cycle = s == t;
        let tt = if cycle { VIRT } else { t };
        let real = |x: u64| if x == VIRT { s } else { x };
        self.set_roots(spec, &[s, t], charge, outer_bytes)?;
        let mut trees = [ChargedTree::new(s, true), ChargedTree::new(tt, false)];
        let mut dist: [HbMap<u64, Cost>; 2] = Default::default();
        let mut settled: [HbSet<u64>; 2] = Default::default();
        let mut heaps: [BinaryHeap<Reverse<(Cost, u64)>>; 2] = Default::default();
        let mut meets: HbSet<Edge> = HbSet::default();
        let mut base = self.retained_bytes().saturating_add(outer_bytes);
        for (i, r) in [s, tt].into_iter().enumerate() {
            (_, base) = insert_hb_map(&mut dist[i], r, Cost::ZERO, charge, base)?;
            base = push_heap(&mut heaps[i], Reverse((Cost::ZERO, r)), charge, base)?;
        }
        let local_bytes = |trees: &[ChargedTree; 2],
                           dist: &[HbMap<u64, Cost>; 2],
                           settled: &[HbSet<u64>; 2],
                           heaps: &[BinaryHeap<Reverse<(Cost, u64)>>; 2],
                           meets: &HbSet<Edge>| {
            trees[0]
                .retained_bytes()
                .saturating_add(trees[1].retained_bytes())
                .saturating_add(map_bytes(&dist[0]))
                .saturating_add(map_bytes(&dist[1]))
                .saturating_add(set_bytes(&settled[0]))
                .saturating_add(set_bytes(&settled[1]))
                .saturating_add(vec_bytes::<Reverse<(Cost, u64)>>(heaps[0].capacity()))
                .saturating_add(vec_bytes::<Reverse<(Cost, u64)>>(heaps[1].capacity()))
                .saturating_add(set_bytes(meets))
        };
        let mut best: Option<Cost> = None;
        let mut best_edge: Option<Edge> = None;
        let mut n = 0u64;
        let mut tops = [Cost::INFINITE; 2];
        let (mut wmin, mut wmax) = (f64::INFINITY, 0.0f64);
        loop {
            for i in 0..2 {
                while let Some(&Reverse((_, u))) = heaps[i].peek() {
                    if !settled[i].contains(&u) {
                        break;
                    }
                    heaps[i].pop();
                    n += 1;
                    if n.is_multiple_of(4096) {
                        ctx.check()?;
                    }
                }
                tops[i] = heaps[i].peek().map_or(Cost::INFINITE, |r| r.0.0);
            }
            let empty = [heaps[0].is_empty(), heaps[1].is_empty()];
            if (empty[0] && empty[1]) || (best.is_none() && (empty[0] || empty[1])) {
                break;
            }
            if !all && best.is_some_and(|mu| tops[0].add(tops[1]) >= mu) {
                break;
            }
            if best.is_some_and(|mu| tops[0].add(tops[1]) > mu)
                && tops[0] > Cost::ZERO
                && tops[1] > Cost::ZERO
            {
                break;
            }
            let side = if empty[0] || empty[1] {
                usize::from(empty[0])
            } else {
                usize::from(heaps[0].len() > heaps[1].len())
            };
            let forward = side == 0;
            let Reverse((d, u)) = heaps[side].pop().unwrap();
            base = self
                .retained_bytes()
                .saturating_add(outer_bytes)
                .saturating_add(local_bytes(&trees, &dist, &settled, &heaps, &meets));
            (_, base) = insert_hb_set(&mut settled[side], u, charge, base)?;
            if u != trees[side].root {
                let _ = trees[side].push_order((u, 0), charge, base)?;
            }
            self.visit(spec, 1)?;
            n += 1;
            if n.is_multiple_of(4096) {
                ctx.check()?;
            }
            if u == if forward { tt } else { s } {
                continue;
            }
            let scan = real(u);
            let loc =
                outer_bytes.saturating_add(local_bytes(&trees, &dist, &settled, &heaps, &meets));
            let mut out_steps = Vec::new();
            self.expand(ctx, spec, &[scan], forward, &mut out_steps, charge, loc)?;
            let out_bytes = vec_bytes::<(u64, Step)>(out_steps.capacity());
            for (idx, (_, st)) in out_steps.iter().copied().enumerate() {
                if idx.is_multiple_of(4096) {
                    ctx.check()?;
                }
                let y = if forward && cycle && st.node == s {
                    VIRT
                } else {
                    st.node
                };
                if y == trees[side].root {
                    continue;
                }
                let e = if forward {
                    Edge {
                        from: u,
                        to: y,
                        p: st.p,
                        rev: st.rev,
                    }
                } else {
                    Edge {
                        from: y,
                        to: u,
                        p: st.p,
                        rev: st.rev,
                    }
                };
                let step_outer = outer_bytes
                    .saturating_add(local_bytes(&trees, &dist, &settled, &heaps, &meets))
                    .saturating_add(out_bytes);
                let wv = self.weight(ctx, spec, &e.map(real), charge, step_outer)?;
                if wv > 0.0 {
                    wmin = wmin.min(wv);
                    wmax = wmax.max(wv);
                }
                let w = Cost::of(wv);
                let nd = d.add(w);
                let parent = Parent {
                    node: u,
                    p: st.p,
                    rev: st.rev,
                };
                let mut cur_base = self.retained_bytes().saturating_add(step_outer);
                match dist[side].get(&y) {
                    Some(&cur) if nd > cur => {}
                    Some(&cur) if nd == cur => {
                        if all {
                            let _ = trees[side].push_extra(y, parent, charge, cur_base)?;
                        }
                    }
                    _ if settled[side].contains(&y) => {}
                    _ => {
                        (_, cur_base) = insert_hb_map(&mut dist[side], y, nd, charge, cur_base)?;
                        cur_base = trees[side].insert_first(y, parent, 0, charge, cur_base)?;
                        let freed = trees[side].remove_extra(y);
                        if freed > 0 {
                            cur_base = cur_base.saturating_sub(freed);
                            charge.resize(cur_base)?;
                        }
                        let _ = push_heap(&mut heaps[side], Reverse((nd, y)), charge, cur_base)?;
                    }
                }
                if let Some(&o) = dist[1 - side].get(&y) {
                    let check_outer = outer_bytes
                        .saturating_add(local_bytes(&trees, &dist, &settled, &heaps, &meets))
                        .saturating_add(out_bytes);
                    let pass_from = e.from == s
                        || e.from == tt
                        || !(self.sink(ctx, e.from, true)
                            || self.blocked(ctx, spec, e.from, charge, check_outer)?);
                    let pass_to = pass_from
                        && (e.to == s
                            || e.to == tt
                            || !(self.sink(ctx, e.to, true)
                                || self.blocked(ctx, spec, e.to, charge, check_outer)?));
                    if pass_to {
                        let c = if forward { nd.add(o) } else { o.add(w).add(d) };
                        if best.is_none_or(|mu| c < mu) {
                            best = Some(c);
                            best_edge = Some(e);
                        }
                        if all {
                            let m_base = self.retained_bytes().saturating_add(check_outer);
                            let _ = insert_hb_set(&mut meets, e, charge, m_base)?;
                        }
                    }
                }
            }
            drop(out_steps);
            charge.resize(
                self.retained_bytes()
                    .saturating_add(outer_bytes)
                    .saturating_add(local_bytes(&trees, &dist, &settled, &heaps, &meets)),
            )?;
        }
        if best.is_none() {
            drop(trees);
            drop(dist);
            drop(settled);
            drop(heaps);
            drop(meets);
            charge.resize(self.retained_bytes().saturating_add(outer_bytes))?;
            return Ok(BidiWeightedOutcome::Empty);
        }
        if wmax > wmin * WIDE_WEIGHTS {
            drop(trees);
            drop(dist);
            drop(settled);
            drop(heaps);
            drop(meets);
            charge.resize(self.retained_bytes().saturating_add(outer_bytes))?;
            return Ok(BidiWeightedOutcome::FallbackSingleSource);
        }
        if !all {
            drop(dist);
            drop(settled);
            drop(heaps);
            drop(meets);
            let (Some(mu), Some(e)) = (best, best_edge) else {
                drop(trees);
                charge.resize(self.retained_bytes().saturating_add(outer_bytes))?;
                return Ok(BidiWeightedOutcome::FallbackSingleSource);
            };
            if mu.hi.is_infinite() {
                drop(trees);
                charge.resize(self.retained_bytes().saturating_add(outer_bytes))?;
                return Ok(BidiWeightedOutcome::FallbackSingleSource);
            }
            let base_trees = self
                .retained_bytes()
                .saturating_add(outer_bytes)
                .saturating_add(trees[0].retained_bytes())
                .saturating_add(trees[1].retained_bytes());
            charge.resize(base_trees)?;
            let a = match TreePathIter::open(&trees[0], e.from, false, None, charge, base_trees)? {
                Some(mut it) => {
                    let p =
                        it.next_path(ctx, &trees[0], None, None, &|_| false, charge, base_trees)?;
                    drop(it);
                    p
                }
                None => None,
            };
            let Some(a) = a else {
                drop(trees);
                charge.resize(self.retained_bytes().saturating_add(outer_bytes))?;
                return Ok(BidiWeightedOutcome::FallbackSingleSource);
            };
            let a_bytes = vec_bytes::<Edge>(a.capacity());
            let base_a = base_trees.saturating_add(a_bytes);
            charge.resize(base_a)?;
            let z = match TreePathIter::open(&trees[1], e.to, false, None, charge, base_a)? {
                Some(mut it) => {
                    let p = it.next_path(ctx, &trees[1], None, None, &|_| false, charge, base_a)?;
                    drop(it);
                    p
                }
                None => None,
            };
            drop(trees);
            let Some(z) = z else {
                drop(a);
                charge.resize(self.retained_bytes().saturating_add(outer_bytes))?;
                return Ok(BidiWeightedOutcome::FallbackSingleSource);
            };
            let combined_len = a.len().saturating_add(1).saturating_add(z.len());
            let z_bytes = vec_bytes::<Edge>(z.capacity());
            let base_no_trees = self.retained_bytes().saturating_add(outer_bytes);
            charge.resize(
                base_no_trees
                    .saturating_add(a_bytes)
                    .saturating_add(z_bytes)
                    .saturating_add(vec_bytes::<Edge>(combined_len)),
            )?;
            let mut p = Vec::with_capacity(combined_len);
            for (idx, edge) in a.into_iter().chain(std::iter::once(e)).chain(z).enumerate() {
                if idx.is_multiple_of(4096) {
                    ctx.check()?;
                }
                p.push(edge);
            }
            let mut p = without_loops_charged(ctx, s, p, charge, base_no_trees)?;
            for (idx, edge) in p.iter_mut().enumerate() {
                if idx.is_multiple_of(4096) {
                    ctx.check()?;
                }
                *edge = edge.map(real);
            }
            let cost = self.path_cost(
                ctx,
                spec,
                &p,
                charge,
                outer_bytes.saturating_add(vec_bytes::<Edge>(p.capacity())),
            )?;
            let len = p.len();
            let edges = if need_edges { p } else { Vec::new() };
            charge.resize(
                self.retained_bytes()
                    .saturating_add(outer_bytes)
                    .saturating_add(vec_bytes::<Edge>(edges.capacity())),
            )?;
            self.paths_found += 1;
            return Ok(BidiWeightedOutcome::One(Found {
                start: s,
                end: t,
                edges,
                len,
                cost,
            }));
        }
        // `all == true`: retain settled nodes and compute optimal join edges.
        for i in 0..2 {
            if settled[i].len() < dist[i].len() {
                trees[i].retain_settled(ctx, &settled[i])?;
                retain_hb_map_checked(&mut dist[i], ctx, |k| settled[i].contains(k))?;
            }
        }
        drop(settled);
        drop(heaps);
        let at_end = dist[0].get(&tt).copied().filter(|&c| c < tops[0]);
        let mut joins: Vec<(Edge, Cost)> = Vec::new();
        let mut min_cost: Option<Cost> = at_end;
        let jbase = self
            .retained_bytes()
            .saturating_add(outer_bytes)
            .saturating_add(trees[0].retained_bytes())
            .saturating_add(trees[1].retained_bytes())
            .saturating_add(map_bytes(&dist[0]))
            .saturating_add(map_bytes(&dist[1]))
            .saturating_add(set_bytes(&meets));
        charge.resize(jbase)?;
        for (idx, &e) in meets.iter().enumerate() {
            if idx.is_multiple_of(4096) {
                ctx.check()?;
            }
            let in_r0_from = dist[0].get(&e.from).is_some_and(|&c| c < tops[0]);
            let in_r0_to = dist[0].get(&e.to).is_some_and(|&c| c < tops[0]);
            let in_r1_to = dist[1].get(&e.to).is_some_and(|&c| c < tops[1]);
            if !in_r0_from || in_r0_to || !in_r1_to {
                continue;
            }
            let w_outer = outer_bytes
                .saturating_add(trees[0].retained_bytes())
                .saturating_add(trees[1].retained_bytes())
                .saturating_add(map_bytes(&dist[0]))
                .saturating_add(map_bytes(&dist[1]))
                .saturating_add(set_bytes(&meets))
                .saturating_add(vec_bytes::<(Edge, Cost)>(joins.capacity()));
            let w = Cost::of(self.weight(ctx, spec, &e.map(real), charge, w_outer)?);
            let total_c = dist[0][&e.from].add(w).add(dist[1][&e.to]);
            let cur_jbase = self.retained_bytes().saturating_add(w_outer);
            let _ = push_vec(&mut joins, (e, total_c), charge, cur_jbase)?;
            min_cost = Some(min_cost.map_or(total_c, |m| m.min(total_c)));
        }
        drop(meets);
        let Some(mu) = min_cost else {
            drop(trees);
            drop(dist);
            drop(joins);
            charge.resize(self.retained_bytes().saturating_add(outer_bytes))?;
            return Ok(BidiWeightedOutcome::FallbackSingleSource);
        };
        if mu.hi.is_infinite() {
            drop(trees);
            drop(dist);
            drop(joins);
            charge.resize(self.retained_bytes().saturating_add(outer_bytes))?;
            return Ok(BidiWeightedOutcome::FallbackSingleSource);
        }
        let mut keep = 0usize;
        for idx in 0..joins.len() {
            if idx.is_multiple_of(4096) {
                ctx.check()?;
            }
            if joins[idx].1 == mu {
                if keep != idx {
                    joins[keep] = joins[idx];
                }
                keep += 1;
            }
        }
        joins.truncate(keep);
        sort_checked_by_key(&mut joins, ctx, |(e, _)| (e.from, e.to, e.p, e.rev))?;
        let mut rbase = self
            .retained_bytes()
            .saturating_add(outer_bytes)
            .saturating_add(trees[0].retained_bytes())
            .saturating_add(trees[1].retained_bytes())
            .saturating_add(map_bytes(&dist[0]))
            .saturating_add(map_bytes(&dist[1]))
            .saturating_add(vec_bytes::<(Edge, Cost)>(joins.capacity()));
        charge.resize(rbase)?;
        let (rank0, next_rbase) = trees[0].build_ranks(ctx, charge, rbase)?;
        rbase = next_rbase;
        let (rank1, _) = trees[1].build_ranks(ctx, charge, rbase)?;
        let mut job = BidiWeightedAllJob {
            s,
            t,
            tt,
            trees,
            dist,
            ranks: [rank0, rank1],
            top0: tops[0],
            joins,
            join_idx: 0,
            need_edges,
            at_end_iter: None,
            front_iter: None,
            cur_front: None,
            back_iter: None,
            skip_joins: false,
        };
        let job_base = self
            .retained_bytes()
            .saturating_add(outer_bytes)
            .saturating_add(job.retained_bytes());
        if at_end == Some(mu) {
            job.at_end_iter = TreePathIter::open(
                &job.trees[0],
                tt,
                true,
                Some(&job.ranks[0]),
                charge,
                job_base,
            )?;
            // If `tt` lies in `region(0)`, every non-empty backward half ends at `tt`
            // (in `region(0)`), so no join edge can produce a valid backward half.
            job.skip_joins = true;
        }
        charge.resize(
            self.retained_bytes()
                .saturating_add(outer_bytes)
                .saturating_add(job.retained_bytes())
                .saturating_add(size_of::<BidiWeightedAllJob>() as u64),
        )?;
        Ok(BidiWeightedOutcome::Stream(Box::new(job)))
    }

    #[allow(clippy::too_many_arguments)]
    fn run_dijkstra_bounded(
        &mut self,
        ctx: &Ctx,
        spec: &PathSearchSpec,
        root: u64,
        forward: bool,
        max_len: u32,
        charge: &mut super::ctx::OwnedCharge,
        outer_bytes: u64,
    ) -> Result<BoundedTreeOutcome> {
        self.set_roots(spec, &[root], charge, outer_bytes)?;
        if max_len == 0 {
            return Ok(BoundedTreeOutcome::Tree((Vec::new(), HbMap::default())));
        }
        let real = |x: u64| if x == VIRT { root } else { x };
        let mut best_len: HbMap<u64, u32> = HbMap::default();
        let mut dist: HbMap<u64, Vec<(u32, Cost)>> = HbMap::default();
        let mut dist_vec_bytes = 0u64;
        let mut parent: HbMap<(u64, u32), Parent> = HbMap::default();
        let mut order: Vec<(u64, u32)> = Vec::new();
        let mut heap: BoundedHeap = BinaryHeap::new();
        let mut direct_multi: HbMap<u64, u32> = HbMap::default();
        let mut backtrack_parent: HbMap<u64, u64> = HbMap::default();
        let mut virt_first_len: Option<u32> = None;
        let mut best_virt: Option<(Cost, u32)> = None;
        let (mut wmin, mut wmax) = (f64::INFINITY, 0.0f64);
        let mut wide = false;
        let track_reverse_wide = !forward && max_len > 1;
        #[allow(clippy::too_many_arguments)]
        let local_bytes = |best_len: &HbMap<u64, u32>,
                           dist: &HbMap<u64, Vec<(u32, Cost)>>,
                           dist_vec_bytes: u64,
                           parent: &HbMap<(u64, u32), Parent>,
                           order: &Vec<(u64, u32)>,
                           heap: &BoundedHeap,
                           direct_multi: &HbMap<u64, u32>,
                           backtrack_parent: &HbMap<u64, u64>| {
            map_bytes(best_len)
                .saturating_add(map_bytes(dist))
                .saturating_add(dist_vec_bytes)
                .saturating_add(map_bytes(parent))
                .saturating_add(vec_bytes::<(u64, u32)>(order.capacity()))
                .saturating_add(vec_bytes::<Reverse<(Cost, u32, u64)>>(heap.capacity()))
                .saturating_add(map_bytes(direct_multi))
                .saturating_add(map_bytes(backtrack_parent))
        };
        let mut base = self.retained_bytes().saturating_add(outer_bytes);
        (_, base) = pareto_insert_charged(
            &mut dist,
            &mut dist_vec_bytes,
            root,
            0,
            Cost::ZERO,
            charge,
            base,
        )?;
        let _ = push_heap(&mut heap, Reverse((Cost::ZERO, 0, root)), charge, base)?;
        let mut n = 0u64;
        while let Some(Reverse((d, l, u))) = heap.pop() {
            n += 1;
            if n.is_multiple_of(4096) {
                ctx.check()?;
            }
            if best_len.get(&u).is_some_and(|&bl| u == VIRT || l >= bl) {
                continue;
            }
            let cur_base = self
                .retained_bytes()
                .saturating_add(outer_bytes)
                .saturating_add(local_bytes(
                    &best_len,
                    &dist,
                    dist_vec_bytes,
                    &parent,
                    &order,
                    &heap,
                    &direct_multi,
                    &backtrack_parent,
                ));
            let (prev, next_base) = insert_hb_map(&mut best_len, u, l, charge, cur_base)?;
            if prev.is_none() && u != root {
                let _ = push_vec(&mut order, (u, l), charge, next_base)?;
            }
            self.visit(spec, 1)?;
            if u == VIRT || l >= max_len {
                continue;
            }
            let loc = outer_bytes.saturating_add(local_bytes(
                &best_len,
                &dist,
                dist_vec_bytes,
                &parent,
                &order,
                &heap,
                &direct_multi,
                &backtrack_parent,
            ));
            let mut out_steps: Vec<(u64, Step)> = Vec::new();
            self.expand(ctx, spec, &[u], forward, &mut out_steps, charge, loc)?;
            let out_bytes = vec_bytes::<(u64, Step)>(out_steps.capacity());
            let nl = l + 1;
            for (idx, (_, st)) in out_steps.iter().copied().enumerate() {
                if idx.is_multiple_of(4096) {
                    #[cfg(any(test, feature = "failpoints"))]
                    test_hooks::fire_traversal_poll();
                    ctx.check()?;
                }
                let y = if st.node == root { VIRT } else { st.node };
                if y == u {
                    continue;
                }
                if track_reverse_wide {
                    let cur_base = self
                        .retained_bytes()
                        .saturating_add(outer_bytes)
                        .saturating_add(local_bytes(
                            &best_len,
                            &dist,
                            dist_vec_bytes,
                            &parent,
                            &order,
                            &heap,
                            &direct_multi,
                            &backtrack_parent,
                        ))
                        .saturating_add(out_bytes);
                    let _ = record_reverse_multi_relax(
                        u,
                        l,
                        y,
                        nl,
                        &parent,
                        &dist,
                        &mut virt_first_len,
                        &mut direct_multi,
                        &mut backtrack_parent,
                        charge,
                        cur_base,
                    )?;
                }
                if best_len.get(&y).is_some_and(|&bl| y == VIRT || nl >= bl) {
                    continue;
                }
                let e = if forward {
                    Edge {
                        from: u,
                        to: y,
                        p: st.p,
                        rev: st.rev,
                    }
                } else {
                    Edge {
                        from: y,
                        to: u,
                        p: st.p,
                        rev: st.rev,
                    }
                };
                let step_outer = outer_bytes
                    .saturating_add(local_bytes(
                        &best_len,
                        &dist,
                        dist_vec_bytes,
                        &parent,
                        &order,
                        &heap,
                        &direct_multi,
                        &backtrack_parent,
                    ))
                    .saturating_add(out_bytes);
                let w = self.weight(ctx, spec, &e.map(real), charge, step_outer)?;
                if track_reverse_wide && !wide && w > 0.0 {
                    wmin = wmin.min(w);
                    wmax = wmax.max(w);
                    if bounded_weights_too_wide(max_len, wmin, wmax) {
                        wide = true;
                    }
                }
                let nd = d.add(Cost::of(w));
                if track_reverse_wide && !wide && nd.hi.is_infinite() {
                    wide = true;
                }
                let mut cur_base = self.retained_bytes().saturating_add(step_outer);
                if y == VIRT {
                    if best_virt.is_some_and(|(bc, bl)| (nd, nl) >= (bc, bl)) {
                        continue;
                    }
                    best_virt = Some((nd, nl));
                } else {
                    let (kept, next_base) = pareto_insert_charged(
                        &mut dist,
                        &mut dist_vec_bytes,
                        y,
                        nl,
                        nd,
                        charge,
                        cur_base,
                    )?;
                    if !kept {
                        continue;
                    }
                    cur_base = next_base;
                }
                (_, cur_base) = insert_hb_map(
                    &mut parent,
                    (y, nl),
                    Parent {
                        node: u,
                        p: st.p,
                        rev: st.rev,
                    },
                    charge,
                    cur_base,
                )?;
                let _ = push_heap(&mut heap, Reverse((nd, nl, y)), charge, cur_base)?;
            }
            drop(out_steps);
            charge.resize(
                self.retained_bytes()
                    .saturating_add(outer_bytes)
                    .saturating_add(local_bytes(
                        &best_len,
                        &dist,
                        dist_vec_bytes,
                        &parent,
                        &order,
                        &heap,
                        &direct_multi,
                        &backtrack_parent,
                    )),
            )?;
        }
        drop(dist);
        drop(heap);
        if wide && !direct_multi.is_empty() {
            let prop_base = self
                .retained_bytes()
                .saturating_add(outer_bytes)
                .saturating_add(map_bytes(&parent))
                .saturating_add(vec_bytes::<(u64, u32)>(order.capacity()))
                .saturating_add(map_bytes(&best_len))
                .saturating_add(map_bytes(&direct_multi))
                .saturating_add(map_bytes(&backtrack_parent));
            let _ = propagate_reverse_backtracks(
                ctx,
                max_len,
                &best_len,
                &backtrack_parent,
                &mut direct_multi,
                charge,
                prop_base,
            )?;
            drop(backtrack_parent);
            charge.resize(
                self.retained_bytes()
                    .saturating_add(outer_bytes)
                    .saturating_add(map_bytes(&parent))
                    .saturating_add(vec_bytes::<(u64, u32)>(order.capacity()))
                    .saturating_add(map_bytes(&best_len))
                    .saturating_add(map_bytes(&direct_multi)),
            )?;
            return Ok(BoundedTreeOutcome::ReverseWide {
                order,
                parent,
                bwd_dist: best_len,
                direct_multi,
            });
        }
        drop(best_len);
        drop(direct_multi);
        drop(backtrack_parent);
        charge.resize(
            self.retained_bytes()
                .saturating_add(outer_bytes)
                .saturating_add(map_bytes(&parent))
                .saturating_add(vec_bytes::<(u64, u32)>(order.capacity())),
        )?;
        Ok(BoundedTreeOutcome::Tree((order, parent)))
    }

    #[allow(clippy::too_many_arguments)]
    fn run_dijkstra_pair_bounded(
        &mut self,
        ctx: &Ctx,
        spec: &PathSearchSpec,
        from: u64,
        to: u64,
        virt: u64,
        banned_nodes: Option<&HbSet<u64>>,
        banned_edges: Option<&HbSet<Edge>>,
        max_len: u32,
        prefix: PrefixCost,
        charge: &mut super::ctx::OwnedCharge,
        outer_bytes: u64,
    ) -> Result<Option<Vec<Edge>>> {
        debug_assert!(from != to);
        let cycle = to == VIRT;
        let real = |x: u64| if x == VIRT { virt } else { x };
        self.set_roots(spec, &[from, real(to)], charge, outer_bytes)?;
        if max_len == 0 {
            return Ok(None);
        }
        if prefix.total_max_len > 1
            && (prefix.cost.hi.is_infinite()
                || bounded_weights_too_wide(prefix.total_max_len, prefix.wmin, prefix.wmax))
        {
            return self.run_dijkstra_pair_bounded_forward(
                ctx,
                spec,
                from,
                to,
                virt,
                banned_nodes,
                banned_edges,
                max_len,
                prefix.cost,
                None,
                charge,
                outer_bytes,
            );
        }
        let max_side = [max_len.div_ceil(2), max_len / 2];
        let mut dist: [HbMap<u64, Vec<(u32, Cost)>>; 2] = Default::default();
        let mut dist_vec_bytes = [0u64; 2];
        let mut best_len: [HbMap<u64, u32>; 2] = Default::default();
        let mut parent: [HbMap<(u64, u32), Parent>; 2] = Default::default();
        let mut heaps: [BoundedHeap; 2] = Default::default();
        let mut bounds: [Option<Cost>; 2] = [None, None];
        let local_bytes = |dist: &[HbMap<u64, Vec<(u32, Cost)>>; 2],
                           dist_vec_bytes: &[u64; 2],
                           best_len: &[HbMap<u64, u32>; 2],
                           parent: &[HbMap<(u64, u32), Parent>; 2],
                           heaps: &[BoundedHeap; 2]| {
            map_bytes(&dist[0])
                .saturating_add(map_bytes(&dist[1]))
                .saturating_add(dist_vec_bytes[0])
                .saturating_add(dist_vec_bytes[1])
                .saturating_add(map_bytes(&best_len[0]))
                .saturating_add(map_bytes(&best_len[1]))
                .saturating_add(map_bytes(&parent[0]))
                .saturating_add(map_bytes(&parent[1]))
                .saturating_add(vec_bytes::<Reverse<(Cost, u32, u64)>>(heaps[0].capacity()))
                .saturating_add(vec_bytes::<Reverse<(Cost, u32, u64)>>(heaps[1].capacity()))
        };
        let mut base = self.retained_bytes().saturating_add(outer_bytes);
        for (i, r) in [from, to].into_iter().enumerate() {
            (_, base) = pareto_insert_charged(
                &mut dist[i],
                &mut dist_vec_bytes[i],
                r,
                0,
                Cost::ZERO,
                charge,
                base,
            )?;
            if max_side[i] > 0 {
                base = push_heap(&mut heaps[i], Reverse((Cost::ZERO, 0, r)), charge, base)?;
            } else {
                (_, base) = insert_hb_map(&mut best_len[i], r, 0, charge, base)?;
                bounds[i] = Some(Cost::ZERO);
            }
        }
        let mut best: Option<(Cost, u64, (u32, u32))> = None;
        let mut n = 0u64;
        let (mut wmin, mut wmax) = (prefix.wmin, prefix.wmax);
        let mut wide = false;
        'search: loop {
            let mut tops: [Option<Cost>; 2] = [None, None];
            for i in 0..2 {
                while let Some(&Reverse((_, l, u))) = heaps[i].peek() {
                    if best_len[i].get(&u).is_none_or(|&bl| l < bl) {
                        break;
                    }
                    heaps[i].pop();
                    n += 1;
                    if n.is_multiple_of(4096) {
                        ctx.check()?;
                    }
                }
                tops[i] = heaps[i].peek().map(|r| r.0.0);
            }
            let lb = opt_cost_min(
                opt_cost_add(tops[0], opt_cost_min(tops[1], bounds[1])),
                opt_cost_add(tops[1], opt_cost_min(tops[0], bounds[0])),
            );
            let Some(lb) = lb else {
                break;
            };
            if prefix.total_max_len > 1
                && (lb.hi.is_infinite() || prefix.cost.add(lb).hi.is_infinite())
            {
                wide = true;
                break;
            }
            if best.is_some_and(|(mu, _, _)| lb >= mu) {
                break;
            }
            let side = if heaps[0].is_empty() || heaps[1].is_empty() {
                usize::from(heaps[0].is_empty())
            } else {
                usize::from(heaps[0].len() > heaps[1].len())
            };
            let forward = side == 0;
            let other = 1 - side;
            let Reverse((d, l, u)) = heaps[side].pop().unwrap();
            base = self
                .retained_bytes()
                .saturating_add(outer_bytes)
                .saturating_add(local_bytes(
                    &dist,
                    &dist_vec_bytes,
                    &best_len,
                    &parent,
                    &heaps,
                ));
            let _ = insert_hb_map(&mut best_len[side], u, l, charge, base)?;
            self.visit(spec, 1)?;
            n += 1;
            if n.is_multiple_of(4096) {
                ctx.check()?;
            }
            let my_root = if forward { from } else { to };
            let other_root = if forward { to } else { from };
            if u == other_root {
                continue;
            }
            let scan = real(u);
            let loc = outer_bytes.saturating_add(local_bytes(
                &dist,
                &dist_vec_bytes,
                &best_len,
                &parent,
                &heaps,
            ));
            let mut cached_steps: Option<std::sync::Arc<[Step]>> = None;
            let mut out_steps: Vec<(u64, Step)> = Vec::new();
            if banned_nodes.is_some() {
                cached_steps = Some(self.neighbours(ctx, spec, scan, forward, charge, loc)?);
            } else {
                self.expand(ctx, spec, &[scan], forward, &mut out_steps, charge, loc)?;
            }
            let out_bytes = vec_bytes::<(u64, Step)>(out_steps.capacity());
            let step_count = cached_steps.as_ref().map_or(out_steps.len(), |s| s.len());
            let nl = l + 1;
            let at_boundary = nl == max_side[side];
            for idx in 0..step_count {
                if idx.is_multiple_of(4096) {
                    #[cfg(any(test, feature = "failpoints"))]
                    test_hooks::fire_traversal_poll();
                    ctx.check()?;
                }
                let st = match &cached_steps {
                    Some(s) => s[idx],
                    None => out_steps[idx].1,
                };
                let y = if forward && cycle && st.node == virt {
                    VIRT
                } else {
                    st.node
                };
                if y == my_root || banned_nodes.is_some_and(|b| b.contains(&y)) {
                    continue;
                }
                if max_side[other] == 0 && y != other_root {
                    continue;
                }
                if best_len[side].get(&y).is_some_and(|&bl| nl >= bl) {
                    continue;
                }
                let e = if forward {
                    Edge {
                        from: u,
                        to: y,
                        p: st.p,
                        rev: st.rev,
                    }
                } else {
                    Edge {
                        from: y,
                        to: u,
                        p: st.p,
                        rev: st.rev,
                    }
                };
                if banned_edges.is_some_and(|b| b.contains(&e)) {
                    continue;
                }
                let step_outer = outer_bytes
                    .saturating_add(local_bytes(
                        &dist,
                        &dist_vec_bytes,
                        &best_len,
                        &parent,
                        &heaps,
                    ))
                    .saturating_add(out_bytes);
                let w = self.weight(ctx, spec, &e.map(real), charge, step_outer)?;
                if w > 0.0 {
                    wmin = wmin.min(w);
                    wmax = wmax.max(w);
                    if bounded_weights_too_wide(prefix.total_max_len, wmin, wmax) {
                        wide = true;
                        break 'search;
                    }
                }
                let nd = d.add(Cost::of(w));
                if prefix.total_max_len > 1
                    && (nd.hi.is_infinite() || prefix.cost.add(nd).hi.is_infinite())
                {
                    wide = true;
                    break 'search;
                }
                let mut cur_base = self.retained_bytes().saturating_add(step_outer);
                let (kept, next_base) = pareto_insert_charged(
                    &mut dist[side],
                    &mut dist_vec_bytes[side],
                    y,
                    nl,
                    nd,
                    charge,
                    cur_base,
                )?;
                if !kept {
                    continue;
                }
                cur_base = next_base;
                (_, cur_base) = insert_hb_map(
                    &mut parent[side],
                    (y, nl),
                    Parent {
                        node: u,
                        p: st.p,
                        rev: st.rev,
                    },
                    charge,
                    cur_base,
                )?;
                if y != other_root {
                    if !at_boundary {
                        let _ =
                            push_heap(&mut heaps[side], Reverse((nd, nl, y)), charge, cur_base)?;
                    } else {
                        bounds[side] = opt_cost_min(bounds[side], Some(nd));
                    }
                }
                if let Some(other_v) = dist[other].get(&y)
                    && let Some((l_other, c_other)) = pareto_best_within(other_v, max_len - nl)
                {
                    let total = if forward {
                        nd.add(c_other)
                    } else {
                        c_other.add(nd)
                    };
                    if prefix.total_max_len > 1
                        && (total.hi.is_infinite() || prefix.cost.add(total).hi.is_infinite())
                    {
                        wide = true;
                        break 'search;
                    }
                    if best.is_none_or(|(mu, _, _)| total < mu) {
                        let block_outer = outer_bytes
                            .saturating_add(local_bytes(
                                &dist,
                                &dist_vec_bytes,
                                &best_len,
                                &parent,
                                &heaps,
                            ))
                            .saturating_add(out_bytes);
                        let pass = y == from
                            || y == to
                            || !(self.sink(ctx, y, true)
                                || self.blocked(ctx, spec, y, charge, block_outer)?);
                        if pass {
                            let lens = if forward {
                                (nl, l_other)
                            } else {
                                (l_other, nl)
                            };
                            best = Some((total, y, lens));
                        }
                    }
                }
            }
            drop(out_steps);
            drop(cached_steps);
            charge.resize(
                self.retained_bytes()
                    .saturating_add(outer_bytes)
                    .saturating_add(local_bytes(
                        &dist,
                        &dist_vec_bytes,
                        &best_len,
                        &parent,
                        &heaps,
                    )),
            )?;
        }
        drop(dist);
        drop(best_len);
        drop(heaps);
        if wide {
            drop(parent);
            charge.resize(self.retained_bytes().saturating_add(outer_bytes))?;
            return self.run_dijkstra_pair_bounded_forward(
                ctx,
                spec,
                from,
                to,
                virt,
                banned_nodes,
                banned_edges,
                max_len,
                prefix.cost,
                None,
                charge,
                outer_bytes,
            );
        }
        let Some((_, m, (lf, lb))) = best else {
            drop(parent);
            charge.resize(self.retained_bytes().saturating_add(outer_bytes))?;
            return Ok(None);
        };
        let combined_len = (lf as usize).saturating_add(lb as usize);
        let parent_bytes = map_bytes(&parent[0]).saturating_add(map_bytes(&parent[1]));
        let base_no_parent = self.retained_bytes().saturating_add(outer_bytes);
        charge.resize(
            base_no_parent
                .saturating_add(parent_bytes)
                .saturating_add(vec_bytes::<Edge>(combined_len)),
        )?;
        let mut a = Vec::with_capacity(combined_len);
        let mut cur = m;
        let mut l = lf;
        let mut steps = 0usize;
        while l > 0 {
            steps += 1;
            if steps.is_multiple_of(4096) {
                ctx.check()?;
            }
            let q = parent[0][&(cur, l)];
            a.push(Edge {
                from: q.node,
                to: cur,
                p: q.p,
                rev: q.rev,
            });
            cur = q.node;
            l -= 1;
        }
        a.reverse();
        let mut cur = m;
        let mut l = lb;
        while l > 0 {
            steps += 1;
            if steps.is_multiple_of(4096) {
                ctx.check()?;
            }
            let q = parent[1][&(cur, l)];
            a.push(Edge {
                from: cur,
                to: q.node,
                p: q.p,
                rev: q.rev,
            });
            cur = q.node;
            l -= 1;
        }
        drop(parent);
        let path = without_loops_charged(ctx, from, a, charge, base_no_parent)?;
        charge.resize(base_no_parent.saturating_add(vec_bytes::<Edge>(path.capacity())))?;
        Ok(Some(path))
    }

    /// Source-order one-ended bounded Pareto search used as a fallback from
    /// [`run_dijkstra_pair_bounded`] and reverse [`run_dijkstra_bounded`] when wide weights
    /// or overflow could make split or reverse sums disagree with source-order accumulation.
    #[allow(clippy::too_many_arguments)]
    fn run_dijkstra_pair_bounded_forward(
        &mut self,
        ctx: &Ctx,
        spec: &PathSearchSpec,
        from: u64,
        to: u64,
        virt: u64,
        banned_nodes: Option<&HbSet<u64>>,
        banned_edges: Option<&HbSet<Edge>>,
        max_len: u32,
        init_cost: Cost,
        bwd_dist: Option<&HbMap<u64, u32>>,
        charge: &mut super::ctx::OwnedCharge,
        outer_bytes: u64,
    ) -> Result<Option<Vec<Edge>>> {
        debug_assert!(from != to);
        let cycle = to == VIRT;
        let real = |x: u64| if x == VIRT { virt } else { x };
        self.set_roots(spec, &[from, real(to)], charge, outer_bytes)?;
        if max_len == 0 {
            return Ok(None);
        }
        let mut dist: HbMap<u64, Vec<(u32, Cost)>> = HbMap::default();
        let mut dist_vec_bytes = 0u64;
        let mut best_len: HbMap<u64, u32> = HbMap::default();
        let mut parent: HbMap<(u64, u32), Parent> = HbMap::default();
        let mut heap: BoundedHeap = BinaryHeap::new();
        let mut best_to: Option<(Cost, u32)> = None;
        let local_bytes = |dist: &HbMap<u64, Vec<(u32, Cost)>>,
                           dist_vec_bytes: u64,
                           best_len: &HbMap<u64, u32>,
                           parent: &HbMap<(u64, u32), Parent>,
                           heap: &BoundedHeap| {
            map_bytes(dist)
                .saturating_add(dist_vec_bytes)
                .saturating_add(map_bytes(best_len))
                .saturating_add(map_bytes(parent))
                .saturating_add(vec_bytes::<Reverse<(Cost, u32, u64)>>(heap.capacity()))
        };
        let mut base = self.retained_bytes().saturating_add(outer_bytes);
        (_, base) = pareto_insert_charged(
            &mut dist,
            &mut dist_vec_bytes,
            from,
            0,
            init_cost,
            charge,
            base,
        )?;
        let _ = push_heap(&mut heap, Reverse((init_cost, 0, from)), charge, base)?;
        let mut n = 0u64;
        let mut found_len: Option<u32> = None;
        while let Some(Reverse((d, l, u))) = heap.pop() {
            n += 1;
            if n.is_multiple_of(4096) {
                ctx.check()?;
            }
            if best_len.get(&u).is_some_and(|&bl| u == to || l >= bl) {
                continue;
            }
            let cur_base = self
                .retained_bytes()
                .saturating_add(outer_bytes)
                .saturating_add(local_bytes(
                    &dist,
                    dist_vec_bytes,
                    &best_len,
                    &parent,
                    &heap,
                ));
            let _ = insert_hb_map(&mut best_len, u, l, charge, cur_base)?;
            self.visit(spec, 1)?;
            if u == to {
                found_len = Some(l);
                break;
            }
            if l >= max_len {
                continue;
            }
            let scan = real(u);
            let loc = outer_bytes.saturating_add(local_bytes(
                &dist,
                dist_vec_bytes,
                &best_len,
                &parent,
                &heap,
            ));
            let mut cached_steps: Option<std::sync::Arc<[Step]>> = None;
            let mut out_steps: Vec<(u64, Step)> = Vec::new();
            if banned_nodes.is_some() {
                cached_steps = Some(self.neighbours(ctx, spec, scan, true, charge, loc)?);
            } else {
                self.expand(ctx, spec, &[scan], true, &mut out_steps, charge, loc)?;
            }
            let out_bytes = vec_bytes::<(u64, Step)>(out_steps.capacity());
            let step_count = cached_steps.as_ref().map_or(out_steps.len(), |s| s.len());
            let nl = l + 1;
            for idx in 0..step_count {
                if idx.is_multiple_of(4096) {
                    #[cfg(any(test, feature = "failpoints"))]
                    test_hooks::fire_traversal_poll();
                    ctx.check()?;
                }
                let st = match &cached_steps {
                    Some(s) => s[idx],
                    None => out_steps[idx].1,
                };
                let y = if cycle && st.node == virt {
                    VIRT
                } else {
                    st.node
                };
                if y == from || banned_nodes.is_some_and(|b| b.contains(&y)) {
                    continue;
                }
                if nl == max_len && y != to {
                    continue;
                }
                if y != to
                    && let Some(bd) = bwd_dist
                    && bd
                        .get(&y)
                        .is_none_or(|&rem| nl.saturating_add(rem) > max_len)
                {
                    continue;
                }
                if best_len.get(&y).is_some_and(|&bl| y == to || nl >= bl) {
                    continue;
                }
                let e = Edge {
                    from: u,
                    to: y,
                    p: st.p,
                    rev: st.rev,
                };
                if banned_edges.is_some_and(|b| b.contains(&e)) {
                    continue;
                }
                let step_outer = outer_bytes
                    .saturating_add(local_bytes(
                        &dist,
                        dist_vec_bytes,
                        &best_len,
                        &parent,
                        &heap,
                    ))
                    .saturating_add(out_bytes);
                let w = self.weight(ctx, spec, &e.map(real), charge, step_outer)?;
                let nd = d.add(Cost::of(w));
                let mut cur_base = self.retained_bytes().saturating_add(step_outer);
                if y == to {
                    if best_to.is_some_and(|(bc, bl)| (nd, nl) >= (bc, bl)) {
                        continue;
                    }
                    best_to = Some((nd, nl));
                } else {
                    let (kept, next_base) = pareto_insert_charged(
                        &mut dist,
                        &mut dist_vec_bytes,
                        y,
                        nl,
                        nd,
                        charge,
                        cur_base,
                    )?;
                    if !kept {
                        continue;
                    }
                    cur_base = next_base;
                }
                (_, cur_base) = insert_hb_map(
                    &mut parent,
                    (y, nl),
                    Parent {
                        node: u,
                        p: st.p,
                        rev: st.rev,
                    },
                    charge,
                    cur_base,
                )?;
                let _ = push_heap(&mut heap, Reverse((nd, nl, y)), charge, cur_base)?;
            }
            drop(out_steps);
            drop(cached_steps);
            charge.resize(
                self.retained_bytes()
                    .saturating_add(outer_bytes)
                    .saturating_add(local_bytes(
                        &dist,
                        dist_vec_bytes,
                        &best_len,
                        &parent,
                        &heap,
                    )),
            )?;
        }
        drop(dist);
        drop(best_len);
        drop(heap);
        let base_no_parent = self.retained_bytes().saturating_add(outer_bytes);
        let Some(len) = found_len else {
            drop(parent);
            charge.resize(base_no_parent)?;
            return Ok(None);
        };
        let parent_bytes = map_bytes(&parent);
        charge.resize(
            base_no_parent
                .saturating_add(parent_bytes)
                .saturating_add(vec_bytes::<Edge>(len as usize)),
        )?;
        let mut a = Vec::with_capacity(len as usize);
        let mut cur = to;
        let mut l = len;
        let mut steps = 0usize;
        while l > 0 {
            steps += 1;
            if steps.is_multiple_of(4096) {
                ctx.check()?;
            }
            let q = parent[&(cur, l)];
            a.push(Edge {
                from: q.node,
                to: cur,
                p: q.p,
                rev: q.rev,
            });
            cur = q.node;
            l -= 1;
        }
        a.reverse();
        drop(parent);
        charge.resize(base_no_parent.saturating_add(vec_bytes::<Edge>(a.capacity())))?;
        Ok(Some(a))
    }

    #[allow(clippy::too_many_arguments)]
    fn run_spur(
        &mut self,
        ctx: &Ctx,
        spec: &PathSearchSpec,
        from: u64,
        to: u64,
        virt: u64,
        banned_nodes: &HbSet<u64>,
        banned_edges: &HbSet<Edge>,
        budget: Option<u32>,
        prefix: PrefixCost,
        charge: &mut super::ctx::OwnedCharge,
        outer_bytes: u64,
    ) -> Result<Option<(Vec<Edge>, f64)>> {
        if spec.weighted() {
            if let Some(max) = budget {
                return Ok(self
                    .run_dijkstra_pair_bounded(
                        ctx,
                        spec,
                        from,
                        to,
                        virt,
                        Some(banned_nodes),
                        Some(banned_edges),
                        max,
                        prefix,
                        charge,
                        outer_bytes,
                    )?
                    .map(|p| (p, 0.0)));
            }
            if to != VIRT && from != to {
                return self.run_dijkstra_pair(
                    ctx,
                    spec,
                    from,
                    to,
                    Some(banned_nodes),
                    Some(banned_edges),
                    charge,
                    outer_bytes,
                );
            }
            let close = to == VIRT;
            let target_id = if close { virt } else { to };
            let (tree, dist) = self.run_dijkstra(
                ctx,
                spec,
                from,
                true,
                Some(&[target_id]),
                false,
                close,
                Some(banned_nodes),
                Some(banned_edges),
                virt,
                charge,
                outer_bytes,
            )?;
            let Some(c) = dist.get(&to).map(|c| c.hi) else {
                drop(tree);
                drop(dist);
                charge.resize(self.retained_bytes().saturating_add(outer_bytes))?;
                return Ok(None);
            };
            drop(dist);
            let base_t = self
                .retained_bytes()
                .saturating_add(outer_bytes)
                .saturating_add(tree.retained_bytes());
            charge.resize(base_t)?;
            let p = match TreePathIter::open(&tree, to, false, None, charge, base_t)? {
                Some(mut it) => {
                    let res = it.next_path(ctx, &tree, None, None, &|_| false, charge, base_t)?;
                    drop(it);
                    res
                }
                None => None,
            };
            drop(tree);
            charge.resize(
                self.retained_bytes()
                    .saturating_add(outer_bytes)
                    .saturating_add(p.as_ref().map_or(0, |e| vec_bytes::<Edge>(e.capacity()))),
            )?;
            return Ok(p.map(|e| (e, c)));
        }
        let bans = StreamBans {
            nodes: banned_nodes,
            edges: banned_edges,
        };
        let Some((f, b, meet)) = self.run_bidi(
            ctx,
            spec,
            from,
            to,
            virt,
            false,
            Some(&bans),
            budget,
            charge,
            outer_bytes,
        )?
        else {
            return Ok(None);
        };
        let m = meet[0];
        drop(meet);
        let base_t = self
            .retained_bytes()
            .saturating_add(outer_bytes)
            .saturating_add(f.retained_bytes())
            .saturating_add(b.retained_bytes());
        charge.resize(base_t)?;
        let a = match TreePathIter::open(&f, m, false, None, charge, base_t)? {
            Some(mut it) => {
                let res = it.next_path(ctx, &f, None, None, &|_| false, charge, base_t)?;
                drop(it);
                res
            }
            None => None,
        };
        let Some(a) = a else {
            drop(f);
            drop(b);
            charge.resize(self.retained_bytes().saturating_add(outer_bytes))?;
            return Ok(None);
        };
        let a_bytes = vec_bytes::<Edge>(a.capacity());
        let base_a = base_t.saturating_add(a_bytes);
        charge.resize(base_a)?;
        let z = match TreePathIter::open(&b, m, false, None, charge, base_a)? {
            Some(mut it) => {
                let res = it.next_path(ctx, &b, None, None, &|_| false, charge, base_a)?;
                drop(it);
                res
            }
            None => None,
        };
        drop(f);
        drop(b);
        let Some(z) = z else {
            drop(a);
            charge.resize(self.retained_bytes().saturating_add(outer_bytes))?;
            return Ok(None);
        };
        let combined_len = a.len().saturating_add(z.len());
        let base_plain = self.retained_bytes().saturating_add(outer_bytes);
        charge.resize(
            base_plain
                .saturating_add(a_bytes)
                .saturating_add(vec_bytes::<Edge>(z.capacity()))
                .saturating_add(vec_bytes::<Edge>(combined_len)),
        )?;
        let mut p = Vec::with_capacity(combined_len);
        for (idx, edge) in a.into_iter().chain(z).enumerate() {
            if idx.is_multiple_of(4096) {
                ctx.check()?;
            }
            p.push(edge);
        }
        let c = p.len() as f64;
        charge.resize(base_plain.saturating_add(vec_bytes::<Edge>(p.capacity())))?;
        Ok(Some((p, c)))
    }

    fn counters(&self, spec: &PathSearchSpec) -> serde_json::Map<String, serde_json::Value> {
        let mut c = serde_json::Map::new();
        c.insert("searches".into(), self.searches.into());
        c.insert("visited".into(), self.visited_total.into());
        c.insert("paths".into(), self.paths_found.into());
        if self.sweeps > 0 {
            c.insert("sweeps".into(), self.sweeps.into());
        }
        if spec.max_degree.is_some() {
            c.insert("skippedHubs".into(), self.skipped_hubs.into());
        }
        c
    }
}

fn lookup_weight_charged(
    ctx: &Ctx,
    spec: &PathSearchSpec,
    w: Option<u64>,
    (s, p, o): (u64, u64, u64),
) -> Result<f64> {
    let default = spec.default_weight;
    let (Some(w), Some(reifies)) = (w, spec.reifies) else {
        return Ok(default);
    };
    let terms = (ctx.term(Id(s)), ctx.term(Id(p)), ctx.term(Id(o)));
    let (Some(s), Some(Term::NamedNode(p)), Some(o)) = terms else {
        return Ok(default);
    };
    let s = match s {
        Term::NamedNode(n) => oxrdf::NamedOrBlankNode::NamedNode(n),
        Term::BlankNode(b) => oxrdf::NamedOrBlankNode::BlankNode(b),
        _ => return Ok(default),
    };
    let tt = Term::Triple(Box::new(oxrdf::Triple::new(s, p, o)));
    let Some(tt) = ctx.snap.lookup_term(&tt) else {
        return Ok(default);
    };
    let graph = &spec.graph;
    let mut best: Option<f64> = None;
    let mut last_reifier: Option<u64> = None;
    let mut scanned = 0usize;
    let scan_weights = |reifier: u64, best: &mut Option<f64>| -> Result<()> {
        let mut inner_scanned = 0usize;
        let mut handle_weight = |val_id: u64, g: u64| -> Result<()> {
            if !graph.accepts(g) {
                return Ok(());
            }
            let v = ctx
                .value(Id(val_id))
                .as_ref()
                .and_then(super::value::approx_f64);
            match v {
                Some(v) if v >= 0.0 && v.is_finite() => {
                    *best = Some(best.map_or(v, |b: f64| b.min(v)));
                    Ok(())
                }
                _ => {
                    let t = ctx
                        .term(Id(val_id))
                        .map_or_else(|| "?".to_string(), |t| t.to_string());
                    Err(bad(format!("the weight {t} is not a non-negative number")))
                }
            }
        };
        ctx.snap.scan_checked(
            Perm::Pso,
            &[w, reifier],
            || ctx.check(),
            |c| {
                match c {
                    Chunk::Block(b, s_idx, e_idx) => {
                        for i in s_idx..e_idx {
                            inner_scanned += 1;
                            if inner_scanned.is_multiple_of(4096) {
                                ctx.check()?;
                            }
                            handle_weight(b.cols[2][i], b.cols[3][i])?;
                        }
                    }
                    Chunk::Row(k) => {
                        inner_scanned += 1;
                        if inner_scanned.is_multiple_of(4096) {
                            ctx.check()?;
                        }
                        handle_weight(k[2], k[3])?;
                    }
                }
                Ok(true)
            },
        )
    };
    ctx.snap.scan_checked(
        Perm::Pos,
        &[reifies, tt.0],
        || ctx.check(),
        |c| {
            match c {
                Chunk::Block(b, s_idx, e_idx) => {
                    for i in s_idx..e_idx {
                        scanned += 1;
                        if scanned.is_multiple_of(4096) {
                            ctx.check()?;
                        }
                        if graph.accepts(b.cols[3][i]) {
                            let r = b.cols[2][i];
                            if last_reifier != Some(r) {
                                last_reifier = Some(r);
                                scan_weights(r, &mut best)?;
                            }
                        }
                    }
                }
                Chunk::Row(k) => {
                    scanned += 1;
                    if scanned.is_multiple_of(4096) {
                        ctx.check()?;
                    }
                    if graph.accepts(k[3]) {
                        let r = k[2];
                        if last_reifier != Some(r) {
                            last_reifier = Some(r);
                            scan_weights(r, &mut best)?;
                        }
                    }
                }
            }
            Ok(true)
        },
    )?;
    Ok(best.unwrap_or(default))
}

/// Resumable stream over a single-source/single-target BFS or Dijkstra [`ChargedTree`].
struct TreeStreamJob {
    tree: ChargedTree,
    cost: Option<HbMap<u64, Cost>>,
    rank: Option<HbMap<u64, u32>>,
    root: u64,
    forward: bool,
    all: bool,
    by_level: bool,
    weighted: bool,
    need_edges: bool,
    pending_zero: bool,
    ends: Vec<u64>,
    end_idx: usize,
    cur_end: u64,
    iter: Option<TreePathIter>,
}

impl TreeStreamJob {
    #[allow(clippy::too_many_arguments)]
    fn open(
        ctx: &Ctx,
        spec: &PathSearchSpec,
        search: &mut SearchContext,
        root: u64,
        forward: bool,
        targets: Option<&[u64]>,
        all: bool,
        close: bool,
        call_begin: bool,
        charge: &mut super::ctx::OwnedCharge,
        outer_bytes: u64,
    ) -> Result<Self> {
        if call_begin {
            search.begin();
        }
        let weighted = spec.weighted();
        let need_edges = spec.per_edge() || weighted;
        let by_level = !need_edges && !all;
        let (tree, cost) = if weighted {
            let (tree, dist) = search.run_dijkstra(
                ctx,
                spec,
                root,
                forward,
                targets,
                all,
                close,
                None,
                None,
                root,
                charge,
                outer_bytes,
            )?;
            let cost = if all { Some(dist) } else { None };
            (tree, cost)
        } else {
            let tree = search.run_bfs(
                ctx,
                spec,
                root,
                forward,
                targets,
                all,
                close,
                charge,
                outer_bytes,
            )?;
            (tree, None)
        };
        let mut base = search
            .retained_bytes()
            .saturating_add(outer_bytes)
            .saturating_add(tree.retained_bytes())
            .saturating_add(cost.as_ref().map_or(0, map_bytes));
        charge.resize(base)?;
        let rank = if all && cost.is_some() {
            let (r, next_base) = tree.build_ranks(ctx, charge, base)?;
            base = next_base;
            Some(r)
        } else {
            None
        };
        let end_count = targets.map_or(tree.order.len(), <[u64]>::len);
        charge.resize(base.saturating_add(vec_bytes::<u64>(end_count)))?;
        let mut ends: Vec<u64> = Vec::with_capacity(end_count);
        if let Some(ts) = targets {
            for (idx, &t) in ts.iter().enumerate() {
                if idx.is_multiple_of(4096) {
                    ctx.check()?;
                }
                ends.push(if t == root { VIRT } else { t });
            }
        } else {
            for (idx, &(n, _)) in tree.order.iter().enumerate() {
                if idx.is_multiple_of(4096) {
                    ctx.check()?;
                }
                ends.push(n);
            }
        }
        let pending_zero = spec.min_len == 0 && close;
        if pending_zero {
            let mut keep = 0usize;
            for idx in 0..ends.len() {
                if idx.is_multiple_of(4096) {
                    ctx.check()?;
                }
                if ends[idx] != VIRT {
                    if keep != idx {
                        ends[keep] = ends[idx];
                    }
                    keep += 1;
                }
            }
            ends.truncate(keep);
        }
        let job = Self {
            tree,
            cost,
            rank,
            root,
            forward,
            all,
            by_level,
            weighted,
            need_edges,
            pending_zero,
            ends,
            end_idx: 0,
            cur_end: 0,
            iter: None,
        };
        charge.resize(
            search
                .retained_bytes()
                .saturating_add(outer_bytes)
                .saturating_add(job.retained_bytes()),
        )?;
        Ok(job)
    }

    fn retained_bytes(&self) -> u64 {
        self.tree
            .retained_bytes()
            .saturating_add(self.cost.as_ref().map_or(0, map_bytes))
            .saturating_add(self.rank.as_ref().map_or(0, map_bytes))
            .saturating_add(vec_bytes::<u64>(self.ends.capacity()))
            .saturating_add(self.iter.as_ref().map_or(0, TreePathIter::retained_bytes))
    }

    fn next_path(
        &mut self,
        ctx: &Ctx,
        spec: &PathSearchSpec,
        search: &mut SearchContext,
        charge: &mut super::ctx::OwnedCharge,
        outer_bytes: u64,
    ) -> Result<Option<Found>> {
        if self.pending_zero {
            self.pending_zero = false;
            search.paths_found += 1;
            return Ok(Some(Found {
                start: self.root,
                end: self.root,
                edges: Vec::new(),
                len: 0,
                cost: 0.0,
            }));
        }
        loop {
            ctx.check()?;
            if let Some(it) = &mut self.iter {
                let outer_for_iter = search
                    .retained_bytes()
                    .saturating_add(outer_bytes)
                    .saturating_add(self.tree.retained_bytes())
                    .saturating_add(self.cost.as_ref().map_or(0, map_bytes))
                    .saturating_add(self.rank.as_ref().map_or(0, map_bytes))
                    .saturating_add(vec_bytes::<u64>(self.ends.capacity()));
                if let Some(mut edges) = it.next_path(
                    ctx,
                    &self.tree,
                    self.cost.as_ref(),
                    self.rank.as_ref(),
                    &|_| false,
                    charge,
                    outer_for_iter,
                )? {
                    let root = self.root;
                    for (idx, e) in edges.iter_mut().enumerate() {
                        if idx.is_multiple_of(4096) {
                            ctx.check()?;
                        }
                        *e = e.map(|x| if x == VIRT { root } else { x });
                    }
                    let len = edges.len();
                    let cost = if self.weighted {
                        let w_outer = outer_bytes
                            .saturating_add(self.retained_bytes())
                            .saturating_add(vec_bytes::<Edge>(edges.capacity()));
                        search.path_cost(ctx, spec, &edges, charge, w_outer)?
                    } else {
                        len as f64
                    };
                    if !self.all {
                        self.iter = None;
                    }
                    let edges = if self.need_edges { edges } else { Vec::new() };
                    let (start, end) = if self.forward {
                        (self.root, self.cur_end)
                    } else {
                        (self.cur_end, self.root)
                    };
                    search.paths_found += 1;
                    return Ok(Some(Found {
                        start,
                        end,
                        edges,
                        len,
                        cost,
                    }));
                }
                self.iter = None;
                charge.resize(
                    search
                        .retained_bytes()
                        .saturating_add(outer_bytes)
                        .saturating_add(self.retained_bytes()),
                )?;
            }
            if self.end_idx >= self.ends.len() {
                return Ok(None);
            }
            let m = self.ends[self.end_idx];
            self.end_idx += 1;
            let other_end = if m == VIRT { self.root } else { m };
            if self.by_level && !self.weighted {
                if let Some(&(_, level)) = self.tree.first.get(&m) {
                    let (start, end) = if self.forward {
                        (self.root, other_end)
                    } else {
                        (other_end, self.root)
                    };
                    search.paths_found += 1;
                    return Ok(Some(Found {
                        start,
                        end,
                        edges: Vec::new(),
                        len: level as usize,
                        cost: level as f64,
                    }));
                }
                continue;
            }
            self.cur_end = other_end;
            let base = search
                .retained_bytes()
                .saturating_add(outer_bytes)
                .saturating_add(self.retained_bytes());
            self.iter =
                TreePathIter::open(&self.tree, m, self.all, self.rank.as_ref(), charge, base)?;
        }
    }
}

/// Resumable stream for unweighted bidirectional `path:allShortest` between `(s, t)`.
struct BidiUnweightedJob {
    s: u64,
    t: u64,
    f: ChargedTree,
    b: ChargedTree,
    meet: Vec<u64>,
    meet_idx: usize,
    need_edges: bool,
    front_iter: Option<TreePathIter>,
    cur_front: Option<Vec<Edge>>,
    back_iter: Option<TreePathIter>,
}

impl BidiUnweightedJob {
    fn retained_bytes(&self) -> u64 {
        self.f
            .retained_bytes()
            .saturating_add(self.b.retained_bytes())
            .saturating_add(vec_bytes::<u64>(self.meet.capacity()))
            .saturating_add(
                self.front_iter
                    .as_ref()
                    .map_or(0, TreePathIter::retained_bytes),
            )
            .saturating_add(
                self.cur_front
                    .as_ref()
                    .map_or(0, |v| vec_bytes::<Edge>(v.capacity())),
            )
            .saturating_add(
                self.back_iter
                    .as_ref()
                    .map_or(0, TreePathIter::retained_bytes),
            )
    }

    fn next_path(
        &mut self,
        ctx: &Ctx,
        search: &mut SearchContext,
        charge: &mut super::ctx::OwnedCharge,
        outer_bytes: u64,
    ) -> Result<Option<Found>> {
        let s = self.s;
        let real = |x: u64| if x == VIRT { s } else { x };
        loop {
            ctx.check()?;
            if let (Some(front), Some(bit)) = (&self.cur_front, &mut self.back_iter) {
                let outer_bit = search
                    .retained_bytes()
                    .saturating_add(outer_bytes)
                    .saturating_add(self.f.retained_bytes())
                    .saturating_add(self.b.retained_bytes())
                    .saturating_add(vec_bytes::<u64>(self.meet.capacity()))
                    .saturating_add(
                        self.front_iter
                            .as_ref()
                            .map_or(0, TreePathIter::retained_bytes),
                    )
                    .saturating_add(vec_bytes::<Edge>(front.capacity()));
                if let Some(back) =
                    bit.next_path(ctx, &self.b, None, None, &|_| false, charge, outer_bit)?
                {
                    let len = front.len().saturating_add(back.len());
                    let edges = if self.need_edges {
                        let base_all = search
                            .retained_bytes()
                            .saturating_add(outer_bytes)
                            .saturating_add(self.retained_bytes())
                            .saturating_add(vec_bytes::<Edge>(back.capacity()));
                        charge.resize(base_all.saturating_add(vec_bytes::<Edge>(len)))?;
                        let mut e = Vec::with_capacity(len);
                        for (idx, edge) in front.iter().chain(back.iter()).enumerate() {
                            if idx.is_multiple_of(4096) {
                                ctx.check()?;
                            }
                            e.push(edge.map(real));
                        }
                        e
                    } else {
                        Vec::new()
                    };
                    drop(back);
                    search.paths_found += 1;
                    return Ok(Some(Found {
                        start: self.s,
                        end: self.t,
                        edges,
                        len,
                        cost: len as f64,
                    }));
                }
                self.back_iter = None;
                self.cur_front = None;
                charge.resize(
                    search
                        .retained_bytes()
                        .saturating_add(outer_bytes)
                        .saturating_add(self.retained_bytes()),
                )?;
            }
            if let Some(fit) = &mut self.front_iter {
                let outer_fit = search
                    .retained_bytes()
                    .saturating_add(outer_bytes)
                    .saturating_add(self.f.retained_bytes())
                    .saturating_add(self.b.retained_bytes())
                    .saturating_add(vec_bytes::<u64>(self.meet.capacity()));
                if let Some(front) =
                    fit.next_path(ctx, &self.f, None, None, &|_| false, charge, outer_fit)?
                {
                    let m = self.meet[self.meet_idx - 1];
                    self.cur_front = Some(front);
                    let base_b = search
                        .retained_bytes()
                        .saturating_add(outer_bytes)
                        .saturating_add(self.retained_bytes());
                    self.back_iter = TreePathIter::open(&self.b, m, true, None, charge, base_b)?;
                    continue;
                }
                self.front_iter = None;
                charge.resize(
                    search
                        .retained_bytes()
                        .saturating_add(outer_bytes)
                        .saturating_add(self.retained_bytes()),
                )?;
            }
            if self.meet_idx >= self.meet.len() {
                return Ok(None);
            }
            let m = self.meet[self.meet_idx];
            self.meet_idx += 1;
            let base = search
                .retained_bytes()
                .saturating_add(outer_bytes)
                .saturating_add(self.retained_bytes());
            self.front_iter = TreePathIter::open(&self.f, m, true, None, charge, base)?;
        }
    }
}

/// Resumable stream for weighted bidirectional `path:allShortest` between `(s, t)`.
struct BidiWeightedAllJob {
    s: u64,
    t: u64,
    tt: u64,
    trees: [ChargedTree; 2],
    dist: [HbMap<u64, Cost>; 2],
    ranks: [HbMap<u64, u32>; 2],
    top0: Cost,
    joins: Vec<(Edge, Cost)>,
    join_idx: usize,
    need_edges: bool,
    at_end_iter: Option<TreePathIter>,
    front_iter: Option<TreePathIter>,
    cur_front: Option<Vec<Edge>>,
    back_iter: Option<TreePathIter>,
    skip_joins: bool,
}

impl BidiWeightedAllJob {
    fn base_fixed_bytes(&self) -> u64 {
        self.trees[0]
            .retained_bytes()
            .saturating_add(self.trees[1].retained_bytes())
            .saturating_add(map_bytes(&self.dist[0]))
            .saturating_add(map_bytes(&self.dist[1]))
            .saturating_add(map_bytes(&self.ranks[0]))
            .saturating_add(map_bytes(&self.ranks[1]))
            .saturating_add(vec_bytes::<(Edge, Cost)>(self.joins.capacity()))
    }

    fn retained_bytes(&self) -> u64 {
        self.base_fixed_bytes()
            .saturating_add(
                self.at_end_iter
                    .as_ref()
                    .map_or(0, TreePathIter::retained_bytes),
            )
            .saturating_add(
                self.front_iter
                    .as_ref()
                    .map_or(0, TreePathIter::retained_bytes),
            )
            .saturating_add(
                self.cur_front
                    .as_ref()
                    .map_or(0, |v| vec_bytes::<Edge>(v.capacity())),
            )
            .saturating_add(
                self.back_iter
                    .as_ref()
                    .map_or(0, TreePathIter::retained_bytes),
            )
    }

    fn next_path(
        &mut self,
        ctx: &Ctx,
        spec: &PathSearchSpec,
        search: &mut SearchContext,
        charge: &mut super::ctx::OwnedCharge,
        outer_bytes: u64,
    ) -> Result<Option<Found>> {
        let s = self.s;
        let real = |x: u64| if x == VIRT { s } else { x };
        loop {
            ctx.check()?;
            let fixed = self.base_fixed_bytes();
            if let Some(it) = &mut self.at_end_iter {
                let outer_it = search
                    .retained_bytes()
                    .saturating_add(outer_bytes)
                    .saturating_add(fixed);
                if let Some(mut edges) = it.next_path(
                    ctx,
                    &self.trees[0],
                    Some(&self.dist[0]),
                    Some(&self.ranks[0]),
                    &|_| false,
                    charge,
                    outer_it,
                )? {
                    for (idx, edge) in edges.iter_mut().enumerate() {
                        if idx.is_multiple_of(4096) {
                            ctx.check()?;
                        }
                        *edge = edge.map(real);
                    }
                    let len = edges.len();
                    let w_outer = outer_bytes
                        .saturating_add(self.retained_bytes())
                        .saturating_add(vec_bytes::<Edge>(edges.capacity()));
                    let cost = search.path_cost(ctx, spec, &edges, charge, w_outer)?;
                    let edges = if self.need_edges { edges } else { Vec::new() };
                    search.paths_found += 1;
                    return Ok(Some(Found {
                        start: self.s,
                        end: self.t,
                        edges,
                        len,
                        cost,
                    }));
                }
                self.at_end_iter = None;
                charge.resize(
                    search
                        .retained_bytes()
                        .saturating_add(outer_bytes)
                        .saturating_add(self.retained_bytes()),
                )?;
            }
            if self.skip_joins {
                return Ok(None);
            }
            if let (Some(front), Some(bit)) = (&self.cur_front, &mut self.back_iter) {
                let outer_bit = search
                    .retained_bytes()
                    .saturating_add(outer_bytes)
                    .saturating_add(fixed)
                    .saturating_add(
                        self.front_iter
                            .as_ref()
                            .map_or(0, TreePathIter::retained_bytes),
                    )
                    .saturating_add(vec_bytes::<Edge>(front.capacity()));
                let dist0 = &self.dist[0];
                let top0 = self.top0;
                let forbid_region0 = |node: u64| dist0.get(&node).is_some_and(|&c| c < top0);
                if let Some(back) = bit.next_path(
                    ctx,
                    &self.trees[1],
                    Some(&self.dist[1]),
                    Some(&self.ranks[1]),
                    &forbid_region0,
                    charge,
                    outer_bit,
                )? {
                    let e = self.joins[self.join_idx - 1].0;
                    let len = front.len().saturating_add(1).saturating_add(back.len());
                    let base_all = search
                        .retained_bytes()
                        .saturating_add(outer_bytes)
                        .saturating_add(self.retained_bytes())
                        .saturating_add(vec_bytes::<Edge>(back.capacity()));
                    charge.resize(base_all.saturating_add(vec_bytes::<Edge>(len)))?;
                    let mut path = Vec::with_capacity(len);
                    for (idx, edge) in front
                        .iter()
                        .copied()
                        .chain(std::iter::once(e))
                        .chain(back.iter().copied())
                        .enumerate()
                    {
                        if idx.is_multiple_of(4096) {
                            ctx.check()?;
                        }
                        path.push(edge.map(real));
                    }
                    drop(back);
                    let w_outer = outer_bytes
                        .saturating_add(self.retained_bytes())
                        .saturating_add(vec_bytes::<Edge>(path.capacity()));
                    let cost = search.path_cost(ctx, spec, &path, charge, w_outer)?;
                    let edges = if self.need_edges { path } else { Vec::new() };
                    search.paths_found += 1;
                    return Ok(Some(Found {
                        start: self.s,
                        end: self.t,
                        edges,
                        len,
                        cost,
                    }));
                }
                self.back_iter = None;
                self.cur_front = None;
                charge.resize(
                    search
                        .retained_bytes()
                        .saturating_add(outer_bytes)
                        .saturating_add(self.retained_bytes()),
                )?;
            }
            if let Some(fit) = &mut self.front_iter {
                let outer_fit = search
                    .retained_bytes()
                    .saturating_add(outer_bytes)
                    .saturating_add(fixed);
                if let Some(front) = fit.next_path(
                    ctx,
                    &self.trees[0],
                    Some(&self.dist[0]),
                    Some(&self.ranks[0]),
                    &|_| false,
                    charge,
                    outer_fit,
                )? {
                    let v = self.joins[self.join_idx - 1].0.to;
                    self.cur_front = Some(front);
                    let base_b = search
                        .retained_bytes()
                        .saturating_add(outer_bytes)
                        .saturating_add(self.retained_bytes());
                    self.back_iter = TreePathIter::open(
                        &self.trees[1],
                        v,
                        true,
                        Some(&self.ranks[1]),
                        charge,
                        base_b,
                    )?;
                    continue;
                }
                self.front_iter = None;
                charge.resize(
                    search
                        .retained_bytes()
                        .saturating_add(outer_bytes)
                        .saturating_add(self.retained_bytes()),
                )?;
            }
            if self.join_idx >= self.joins.len() {
                let _ = self.tt;
                return Ok(None);
            }
            let u = self.joins[self.join_idx].0.from;
            self.join_idx += 1;
            let base = search
                .retained_bytes()
                .saturating_add(outer_bytes)
                .saturating_add(self.retained_bytes());
            self.front_iter =
                TreePathIter::open(&self.trees[0], u, true, Some(&self.ranks[0]), charge, base)?;
        }
    }
}

/// Resumable Yen's algorithm for `path:kShortest` and weighted `path:allShortest` with
/// `path:maxLength`.
enum YenMode {
    AllShortest {
        max_len: u32,
        best: Option<Cost>,
    },
    KShortest {
        max_len: u32,
        emitted: usize,
        k: usize,
    },
}

fn ban_yen_prefix_edges(
    ctx: &Ctx,
    paths: &[(Vec<Edge>, Cost)],
    i: usize,
    banned_edges: &mut HbSet<Edge>,
    charge: &mut super::ctx::OwnedCharge,
    mut loop_base: u64,
) -> Result<u64> {
    let cur_edges = &paths.last().unwrap().0;
    let root_slice = &cur_edges[..i];
    let mut cmp_work = 0usize;
    for (p, _) in paths {
        if p.len() <= i {
            cmp_work += 1;
            if cmp_work >= 4096 {
                cmp_work = 0;
                ctx.check()?;
            }
            continue;
        }
        let mut same = true;
        if i == 0 {
            cmp_work += 1;
            if cmp_work >= 4096 {
                cmp_work = 0;
                ctx.check()?;
            }
        } else {
            let mut pos = 0usize;
            while pos < i {
                let chunk = (i - pos).min(4096 - cmp_work);
                let eq = p[pos..pos + chunk] == root_slice[pos..pos + chunk];
                cmp_work += chunk;
                if cmp_work >= 4096 {
                    cmp_work = 0;
                    ctx.check()?;
                }
                if !eq {
                    same = false;
                    break;
                }
                pos += chunk;
            }
        }
        if same {
            (_, loop_base) = insert_hb_set(banned_edges, p[i], charge, loop_base)?;
        }
    }
    Ok(loop_base)
}

struct YenJob {
    s: u64,
    t: u64,
    tt: u64,
    mode: YenMode,
    min_len: u32,
    need_edges: bool,
    pending_zero: bool,
    done: bool,
    initialized: bool,
    need_expand: bool,
    a: Vec<(Vec<Edge>, Cost)>,
    a_edge_bytes: u64,
    seen: HbSet<Vec<Edge>>,
    seen_edge_bytes: u64,
    heap: BinaryHeap<Reverse<(Cost, usize, u64)>>,
    cands: HbMap<u64, (Vec<Edge>, Cost)>,
    cands_edge_bytes: u64,
    seq: u64,
}

impl YenJob {
    fn open(spec: &PathSearchSpec, search: &mut SearchContext, s: u64, t: u64) -> Self {
        search.begin();
        let mode = match spec.algorithm {
            Algorithm::AllShortest => YenMode::AllShortest {
                max_len: spec.max_len.unwrap_or(u32::MAX),
                best: None,
            },
            Algorithm::KShortest => YenMode::KShortest {
                max_len: spec.max_len.unwrap_or(u32::MAX),
                emitted: 0,
                k: spec.k,
            },
            Algorithm::Shortest | Algorithm::All => {
                unreachable!("YenJob handles AllShortest and KShortest")
            }
        };
        let pending_zero = spec.min_len == 0 && s == t;
        Self {
            s,
            t,
            tt: if s == t { VIRT } else { t },
            mode,
            min_len: spec.min_len,
            need_edges: spec.per_edge() || spec.weighted(),
            pending_zero,
            done: false,
            initialized: false,
            need_expand: false,
            a: Vec::new(),
            a_edge_bytes: 0,
            seen: HbSet::default(),
            seen_edge_bytes: 0,
            heap: BinaryHeap::new(),
            cands: HbMap::default(),
            cands_edge_bytes: 0,
            seq: 0,
        }
    }

    fn retained_bytes(&self) -> u64 {
        vec_bytes::<(Vec<Edge>, Cost)>(self.a.capacity())
            .saturating_add(self.a_edge_bytes)
            .saturating_add(set_bytes(&self.seen))
            .saturating_add(self.seen_edge_bytes)
            .saturating_add(vec_bytes::<Reverse<(Cost, usize, u64)>>(
                self.heap.capacity(),
            ))
            .saturating_add(map_bytes(&self.cands))
            .saturating_add(self.cands_edge_bytes)
    }

    fn next_path(
        &mut self,
        ctx: &Ctx,
        spec: &PathSearchSpec,
        search: &mut SearchContext,
        charge: &mut super::ctx::OwnedCharge,
        outer_bytes: u64,
    ) -> Result<Option<Found>> {
        if self.done {
            return Ok(None);
        }
        if self.pending_zero {
            self.pending_zero = false;
            match &mut self.mode {
                YenMode::AllShortest { .. } => {
                    self.done = true;
                }
                YenMode::KShortest { emitted, k, .. } => {
                    *emitted += 1;
                    if *emitted >= *k {
                        self.done = true;
                    }
                }
            }
            search.paths_found += 1;
            return Ok(Some(Found {
                start: self.s,
                end: self.t,
                edges: Vec::new(),
                len: 0,
                cost: 0.0,
            }));
        }
        let s = self.s;
        let tt = self.tt;
        let real = |x: u64| if x == VIRT { s } else { x };
        let budget = spec.max_len;
        if !self.initialized {
            self.initialized = true;
            let none_n = HbSet::default();
            let none_e = HbSet::default();
            let spur_outer = outer_bytes.saturating_add(self.retained_bytes());
            let Some((first, _)) = search.run_spur(
                ctx,
                spec,
                s,
                tt,
                s,
                &none_n,
                &none_e,
                budget,
                PrefixCost::zero(budget.unwrap_or(0)),
                charge,
                spur_outer,
            )?
            else {
                self.done = true;
                return Ok(None);
            };
            let first_bytes = vec_bytes::<Edge>(first.capacity());
            let first_cost = search.exact_cost(
                ctx,
                spec,
                &first,
                real,
                charge,
                outer_bytes
                    .saturating_add(self.retained_bytes())
                    .saturating_add(first_bytes),
            )?;
            let clone_bytes = vec_bytes::<Edge>(first.len());
            let mut base = search
                .retained_bytes()
                .saturating_add(outer_bytes)
                .saturating_add(self.retained_bytes())
                .saturating_add(first_bytes);
            charge.resize(base.saturating_add(clone_bytes))?;
            let first_clone = first.clone();
            let clone_cap = vec_bytes::<Edge>(first_clone.capacity());
            base = base.saturating_add(clone_cap);
            self.seen_edge_bytes = self.seen_edge_bytes.saturating_add(clone_cap);
            (_, base) = insert_hb_set(&mut self.seen, first_clone, charge, base)?;
            self.a_edge_bytes = self.a_edge_bytes.saturating_add(first_bytes);
            let _ = push_vec(&mut self.a, (first, first_cost), charge, base)?;
            self.need_expand = false;
        }
        loop {
            ctx.check()?;
            if self.need_expand {
                self.need_expand = false;
                let cur_len = self.a.last().unwrap().0.len();
                let nodes_bytes = vec_bytes::<u64>(cur_len.saturating_add(1));
                let base_before = search
                    .retained_bytes()
                    .saturating_add(outer_bytes)
                    .saturating_add(self.retained_bytes());
                charge.resize(base_before.saturating_add(nodes_bytes))?;
                let mut nodes: Vec<u64> = Vec::with_capacity(cur_len.saturating_add(1));
                nodes.push(s);
                for (idx, e) in self.a.last().unwrap().0.iter().enumerate() {
                    if idx.is_multiple_of(4096) {
                        ctx.check()?;
                    }
                    nodes.push(e.to);
                }
                let nodes_cap_bytes = vec_bytes::<u64>(nodes.capacity());
                let track_prefix = spec.weighted() && budget.is_some();
                let mut prefix_cost = Cost::ZERO;
                let (mut prefix_wmin, mut prefix_wmax) = (f64::INFINITY, 0.0f64);
                for i in 0..cur_len {
                    ctx.check()?;
                    let step_budget = match budget {
                        Some(m) if m <= i as u32 => break,
                        Some(m) => Some(m - i as u32),
                        None => None,
                    };
                    if i > 0 && track_prefix {
                        let edge = self.a.last().unwrap().0[i - 1].map(real);
                        let w = search.weight(
                            ctx,
                            spec,
                            &edge,
                            charge,
                            outer_bytes
                                .saturating_add(self.retained_bytes())
                                .saturating_add(nodes_cap_bytes),
                        )?;
                        if w > 0.0 {
                            prefix_wmin = prefix_wmin.min(w);
                            prefix_wmax = prefix_wmax.max(w);
                        }
                        prefix_cost = prefix_cost.add(Cost::of(w));
                    }
                    let mut banned_edges: HbSet<Edge> = HbSet::default();
                    let mut banned_nodes: HbSet<u64> = HbSet::default();
                    let mut loop_base = search
                        .retained_bytes()
                        .saturating_add(outer_bytes)
                        .saturating_add(self.retained_bytes())
                        .saturating_add(nodes_cap_bytes);
                    loop_base = ban_yen_prefix_edges(
                        ctx,
                        &self.a,
                        i,
                        &mut banned_edges,
                        charge,
                        loop_base,
                    )?;
                    for (nd_idx, &nd) in nodes[..i].iter().enumerate() {
                        if nd_idx.is_multiple_of(4096) {
                            ctx.check()?;
                        }
                        (_, loop_base) = insert_hb_set(&mut banned_nodes, nd, charge, loop_base)?;
                    }
                    let spur_outer = outer_bytes
                        .saturating_add(self.retained_bytes())
                        .saturating_add(nodes_cap_bytes)
                        .saturating_add(set_bytes(&banned_edges))
                        .saturating_add(set_bytes(&banned_nodes));
                    let prefix = PrefixCost {
                        cost: prefix_cost,
                        wmin: prefix_wmin,
                        wmax: prefix_wmax,
                        total_max_len: budget.unwrap_or(0),
                    };
                    let sp = search.run_spur(
                        ctx,
                        spec,
                        nodes[i],
                        tt,
                        s,
                        &banned_nodes,
                        &banned_edges,
                        step_budget,
                        prefix,
                        charge,
                        spur_outer,
                    )?;
                    drop(banned_edges);
                    drop(banned_nodes);
                    let Some((sp, _)) = sp else {
                        charge.resize(
                            search
                                .retained_bytes()
                                .saturating_add(outer_bytes)
                                .saturating_add(self.retained_bytes())
                                .saturating_add(nodes_cap_bytes),
                        )?;
                        continue;
                    };
                    let total_len = i.saturating_add(sp.len());
                    let sp_bytes = vec_bytes::<Edge>(sp.capacity());
                    let path_base = search
                        .retained_bytes()
                        .saturating_add(outer_bytes)
                        .saturating_add(self.retained_bytes())
                        .saturating_add(nodes_cap_bytes)
                        .saturating_add(sp_bytes);
                    charge.resize(path_base.saturating_add(vec_bytes::<Edge>(total_len)))?;
                    let mut path = Vec::with_capacity(total_len);
                    for (idx, edge) in self.a.last().unwrap().0[..i]
                        .iter()
                        .copied()
                        .chain(sp)
                        .enumerate()
                    {
                        if idx.is_multiple_of(4096) {
                            ctx.check()?;
                        }
                        path.push(edge);
                    }
                    let path_bytes = vec_bytes::<Edge>(path.capacity());
                    if !self.seen.contains(&path) {
                        let c = search.exact_cost(
                            ctx,
                            spec,
                            &path,
                            real,
                            charge,
                            outer_bytes
                                .saturating_add(self.retained_bytes())
                                .saturating_add(nodes_cap_bytes)
                                .saturating_add(path_bytes),
                        )?;
                        let len = path.len();
                        let clone_bytes = vec_bytes::<Edge>(len);
                        let mut cbase = search
                            .retained_bytes()
                            .saturating_add(outer_bytes)
                            .saturating_add(self.retained_bytes())
                            .saturating_add(nodes_cap_bytes)
                            .saturating_add(path_bytes);
                        charge.resize(cbase.saturating_add(clone_bytes))?;
                        let seen_copy = path.clone();
                        let seen_cap = vec_bytes::<Edge>(seen_copy.capacity());
                        cbase = cbase.saturating_add(seen_cap);
                        self.seen_edge_bytes = self.seen_edge_bytes.saturating_add(seen_cap);
                        (_, cbase) = insert_hb_set(&mut self.seen, seen_copy, charge, cbase)?;
                        self.cands_edge_bytes = self.cands_edge_bytes.saturating_add(path_bytes);
                        (_, cbase) =
                            insert_hb_map(&mut self.cands, self.seq, (path, c), charge, cbase)?;
                        let _ =
                            push_heap(&mut self.heap, Reverse((c, len, self.seq)), charge, cbase)?;
                        self.seq += 1;
                    } else {
                        drop(path);
                    }
                    charge.resize(
                        search
                            .retained_bytes()
                            .saturating_add(outer_bytes)
                            .saturating_add(self.retained_bytes())
                            .saturating_add(nodes_cap_bytes),
                    )?;
                }
                drop(nodes);
                let Some(Reverse((_, _, id))) = self.heap.pop() else {
                    self.done = true;
                    charge.resize(
                        search
                            .retained_bytes()
                            .saturating_add(outer_bytes)
                            .saturating_add(self.retained_bytes()),
                    )?;
                    return Ok(None);
                };
                let (next_edges, next_cost) = self.cands.remove(&id).unwrap();
                let moved = vec_bytes::<Edge>(next_edges.capacity());
                self.cands_edge_bytes = self.cands_edge_bytes.saturating_sub(moved);
                self.a_edge_bytes = self.a_edge_bytes.saturating_add(moved);
                let cur_base = search
                    .retained_bytes()
                    .saturating_add(outer_bytes)
                    .saturating_add(self.retained_bytes());
                let settled = push_vec(&mut self.a, (next_edges, next_cost), charge, cur_base)?;
                charge.resize(settled)?;
            }
            let (cur_len, cur_cost) = {
                let (ref e, c) = *self.a.last().unwrap();
                (e.len(), c)
            };
            self.need_expand = true;
            let len_u32 = cur_len as u32;
            match &mut self.mode {
                YenMode::AllShortest { max_len, best } => {
                    if let Some(b) = *best
                        && cur_cost > b
                    {
                        self.done = true;
                        return Ok(None);
                    }
                    if len_u32 < self.min_len || len_u32 > *max_len {
                        continue;
                    }
                    if best.is_none() {
                        *best = Some(cur_cost);
                    }
                }
                YenMode::KShortest {
                    max_len,
                    emitted,
                    k,
                } => {
                    if len_u32 < self.min_len || len_u32 > *max_len {
                        continue;
                    }
                    *emitted += 1;
                    if *emitted >= *k {
                        self.done = true;
                    }
                }
            }
            let edges = if self.need_edges {
                let base_e = search
                    .retained_bytes()
                    .saturating_add(outer_bytes)
                    .saturating_add(self.retained_bytes());
                charge.resize(base_e.saturating_add(vec_bytes::<Edge>(cur_len)))?;
                let mut out = Vec::with_capacity(cur_len);
                for (idx, edge) in self.a.last().unwrap().0.iter().enumerate() {
                    if idx.is_multiple_of(4096) {
                        ctx.check()?;
                    }
                    out.push(edge.map(real));
                }
                out
            } else {
                Vec::new()
            };
            search.paths_found += 1;
            return Ok(Some(Found {
                start: self.s,
                end: self.t,
                edges,
                len: cur_len,
                cost: cur_cost.hi,
            }));
        }
    }
}

/// Resumable DFS for `Algorithm::All`, supporting single, multiple or unbound endpoints
/// and optional nested edge patterns.
struct AllPathsJob {
    forward: bool,
    root: u64,
    targets: Option<HbSet<u64>>,
    close: bool,
    max_len: u32,
    min_len: u32,
    pending_zero: bool,
    pending_push: Option<u64>,
    dist: Option<HbMap<u64, u32>>,
    on_path: HbSet<u64>,
    stack: Vec<(u64, std::sync::Arc<[Step]>, usize)>,
    edges: Vec<Edge>,
    steps: u64,
    poll_ticks: u64,
    done: bool,
}

impl AllPathsJob {
    #[allow(clippy::too_many_arguments)]
    fn open(
        ctx: &Ctx,
        spec: &PathSearchSpec,
        search: &mut SearchContext,
        root: u64,
        forward: bool,
        targets: Option<&[u64]>,
        charge: &mut super::ctx::OwnedCharge,
        outer_bytes: u64,
    ) -> Result<Self> {
        search.begin();
        let max_len = spec.max_len.unwrap_or(0);
        let min_len = spec.min_len;
        let mut target_set: Option<HbSet<u64>> = None;
        let mut base = search.retained_bytes().saturating_add(outer_bytes);
        if let Some(ts) = targets {
            let mut set = HbSet::default();
            for (i, &t) in ts.iter().enumerate() {
                if i.is_multiple_of(4096) {
                    ctx.check()?;
                }
                (_, base) = insert_hb_set(&mut set, t, charge, base)?;
            }
            target_set = Some(set);
        }
        let close = target_set.as_ref().is_none_or(|ts| ts.contains(&root));
        let mut st = Self {
            forward,
            root,
            targets: target_set,
            close,
            max_len,
            min_len,
            pending_zero: min_len == 0 && close,
            pending_push: None,
            dist: None,
            on_path: HbSet::default(),
            stack: Vec::new(),
            edges: Vec::new(),
            steps: 0,
            poll_ticks: 0,
            done: false,
        };
        if max_len == 0 {
            st.done = !st.pending_zero;
            charge.resize(
                search
                    .retained_bytes()
                    .saturating_add(outer_bytes)
                    .saturating_add(st.retained_bytes()),
            )?;
            return Ok(st);
        }
        if let Some(ts) = targets {
            let mut d: HbMap<u64, u32> = HbMap::default();
            let mut frontier: Vec<u64> = Vec::new();
            let mut bfs_base = search
                .retained_bytes()
                .saturating_add(outer_bytes)
                .saturating_add(st.retained_bytes());
            for (i, &t) in ts.iter().enumerate() {
                if i.is_multiple_of(4096) {
                    ctx.check()?;
                }
                (_, bfs_base) = insert_hb_map(&mut d, t, 0, charge, bfs_base)?;
                bfs_base = push_vec(&mut frontier, t, charge, bfs_base)?;
            }
            sort_dedup_u64_checked(&mut frontier, ctx)?;
            search.set_roots(
                spec,
                &frontier,
                charge,
                outer_bytes
                    .saturating_add(st.retained_bytes())
                    .saturating_add(map_bytes(&d))
                    .saturating_add(vec_bytes::<u64>(frontier.capacity())),
            )?;
            let mut level = 0u32;
            while !frontier.is_empty() && level < max_len {
                let d_bytes = map_bytes(&d);
                let frontier_bytes = vec_bytes::<u64>(frontier.capacity());
                let mut found = Vec::new();
                search.expand(
                    ctx,
                    spec,
                    &frontier,
                    !forward,
                    &mut found,
                    charge,
                    outer_bytes
                        .saturating_add(st.retained_bytes())
                        .saturating_add(d_bytes)
                        .saturating_add(frontier_bytes),
                )?;
                level += 1;
                let found_bytes = vec_bytes::<(u64, Step)>(found.capacity());
                let mut next = Vec::new();
                let mut added_nodes = 0u64;
                let mut loop_base = search
                    .retained_bytes()
                    .saturating_add(outer_bytes)
                    .saturating_add(st.retained_bytes())
                    .saturating_add(d_bytes)
                    .saturating_add(frontier_bytes)
                    .saturating_add(found_bytes);
                for (edge_idx, (_, step)) in found.into_iter().enumerate() {
                    if edge_idx.is_multiple_of(4096) {
                        ctx.check()?;
                    }
                    if !d.contains_key(&step.node) {
                        (_, loop_base) =
                            insert_hb_map(&mut d, step.node, level, charge, loop_base)?;
                        loop_base = push_vec(&mut next, step.node, charge, loop_base)?;
                        added_nodes += 1;
                        if added_nodes.is_multiple_of(1024) {
                            search.visit(spec, 1024)?;
                        }
                    }
                }
                let rem = added_nodes % 1024;
                if rem > 0 {
                    search.visit(spec, rem)?;
                }
                sort_checked_by_key(&mut next, ctx, |&y| y)?;
                frontier = next;
                charge.resize(
                    search
                        .retained_bytes()
                        .saturating_add(outer_bytes)
                        .saturating_add(st.retained_bytes())
                        .saturating_add(map_bytes(&d))
                        .saturating_add(vec_bytes::<u64>(frontier.capacity())),
                )?;
            }
            drop(frontier);
            st.dist = Some(d);
            charge.resize(
                search
                    .retained_bytes()
                    .saturating_add(outer_bytes)
                    .saturating_add(st.retained_bytes()),
            )?;
        }
        search.set_roots(
            spec,
            &[root],
            charge,
            outer_bytes.saturating_add(st.retained_bytes()),
        )?;
        let cur_base = search
            .retained_bytes()
            .saturating_add(outer_bytes)
            .saturating_add(st.retained_bytes());
        let _ = insert_hb_set(&mut st.on_path, root, charge, cur_base)?;
        let root_ns = search.neighbours(
            ctx,
            spec,
            root,
            forward,
            charge,
            outer_bytes.saturating_add(st.retained_bytes()),
        )?;
        let stack_base = search
            .retained_bytes()
            .saturating_add(outer_bytes)
            .saturating_add(st.retained_bytes());
        let _ = push_vec(&mut st.stack, (root, root_ns, 0), charge, stack_base)?;
        Ok(st)
    }

    fn retained_bytes(&self) -> u64 {
        let targets = self.targets.as_ref().map_or(0, set_bytes);
        let dist = self.dist.as_ref().map_or(0, map_bytes);
        let stack = vec_bytes::<(u64, std::sync::Arc<[Step]>, usize)>(self.stack.capacity());
        let on_path = set_bytes(&self.on_path);
        let edges = vec_bytes::<Edge>(self.edges.capacity());
        targets
            .saturating_add(dist)
            .saturating_add(stack)
            .saturating_add(on_path)
            .saturating_add(edges)
    }

    fn next_path(
        &mut self,
        ctx: &Ctx,
        spec: &PathSearchSpec,
        search: &mut SearchContext,
        charge: &mut super::ctx::OwnedCharge,
        outer_bytes: u64,
    ) -> Result<Option<Found>> {
        if self.done {
            return Ok(None);
        }
        if self.pending_zero {
            self.pending_zero = false;
            if self.max_len == 0 {
                self.done = true;
            }
            search.paths_found += 1;
            return Ok(Some(Found {
                start: self.root,
                end: self.root,
                edges: Vec::new(),
                len: 0,
                cost: 0.0,
            }));
        }
        if let Some(y) = self.pending_push.take() {
            let cur_base = search
                .retained_bytes()
                .saturating_add(outer_bytes)
                .saturating_add(self.retained_bytes());
            let _ = insert_hb_set(&mut self.on_path, y, charge, cur_base)?;
            let ns = search.neighbours(
                ctx,
                spec,
                y,
                self.forward,
                charge,
                outer_bytes.saturating_add(self.retained_bytes()),
            )?;
            let stack_base = search
                .retained_bytes()
                .saturating_add(outer_bytes)
                .saturating_add(self.retained_bytes());
            let _ = push_vec(&mut self.stack, (y, ns, 0), charge, stack_base)?;
        }
        let need_edges = spec.per_edge() || spec.weighted();
        while let Some(top) = self.stack.last_mut() {
            self.poll_ticks = self.poll_ticks.wrapping_add(1);
            if self.poll_ticks.is_multiple_of(1024) {
                #[cfg(any(test, feature = "failpoints"))]
                test_hooks::fire_traversal_poll();
                ctx.check()?;
            }
            let (x, ns, i) = (top.0, top.1.clone(), top.2);
            if i >= ns.len() {
                self.stack.pop();
                self.on_path.remove(&x);
                self.edges.pop();
                continue;
            }
            top.2 += 1;
            let st = ns[i];
            let y = st.node;
            let len = self.edges.len() as u32 + 1;
            let closing = self.close && y == self.root;
            if !closing && self.on_path.contains(&y) {
                continue;
            }
            if !closing
                && let Some(d) = &self.dist
                && d.get(&y).is_none_or(|&k| k > self.max_len - len)
            {
                continue;
            }
            self.steps += 1;
            if self.steps.is_multiple_of(1024) {
                search.visit(spec, 1024)?;
                ctx.check()?;
            }
            let e = if self.forward {
                Edge {
                    from: x,
                    to: y,
                    p: st.p,
                    rev: st.rev,
                }
            } else {
                Edge {
                    from: y,
                    to: x,
                    p: st.p,
                    rev: st.rev,
                }
            };
            let edge_base = search
                .retained_bytes()
                .saturating_add(outer_bytes)
                .saturating_add(self.retained_bytes());
            let _ = push_vec(&mut self.edges, e, charge, edge_base)?;
            let is_target = self.targets.as_ref().is_none_or(|ts| ts.contains(&y));
            let matched = (closing || is_target) && len >= self.min_len;
            let stop_branch = closing || len >= self.max_len;
            if matched {
                let plen = self.edges.len();
                let (ordered, cost) = if need_edges {
                    let ordered_bytes = vec_bytes::<Edge>(plen);
                    charge.resize(
                        search
                            .retained_bytes()
                            .saturating_add(outer_bytes)
                            .saturating_add(self.retained_bytes())
                            .saturating_add(ordered_bytes),
                    )?;
                    let mut ordered = Vec::with_capacity(plen);
                    if self.forward {
                        for (idx, &edge) in self.edges.iter().enumerate() {
                            if idx.is_multiple_of(4096) {
                                ctx.check()?;
                            }
                            ordered.push(edge);
                        }
                    } else {
                        for (idx, &edge) in self.edges.iter().rev().enumerate() {
                            if idx.is_multiple_of(4096) {
                                ctx.check()?;
                            }
                            ordered.push(edge);
                        }
                    }
                    let cost = search.path_cost(
                        ctx,
                        spec,
                        &ordered,
                        charge,
                        outer_bytes
                            .saturating_add(self.retained_bytes())
                            .saturating_add(vec_bytes::<Edge>(ordered.capacity())),
                    )?;
                    (ordered, cost)
                } else {
                    (Vec::new(), plen as f64)
                };
                if stop_branch {
                    self.edges.pop();
                } else {
                    self.pending_push = Some(y);
                }
                let (start, end) = if self.forward {
                    (self.root, y)
                } else {
                    (y, self.root)
                };
                search.paths_found += 1;
                return Ok(Some(Found {
                    start,
                    end,
                    edges: ordered,
                    len: plen,
                    cost,
                }));
            }
            if stop_branch {
                self.edges.pop();
                continue;
            }
            let cur_base = search
                .retained_bytes()
                .saturating_add(outer_bytes)
                .saturating_add(self.retained_bytes());
            let _ = insert_hb_set(&mut self.on_path, y, charge, cur_base)?;
            let child_ns = search.neighbours(
                ctx,
                spec,
                y,
                self.forward,
                charge,
                outer_bytes.saturating_add(self.retained_bytes()),
            )?;
            let stack_base = search
                .retained_bytes()
                .saturating_add(outer_bytes)
                .saturating_add(self.retained_bytes());
            let _ = push_vec(&mut self.stack, (y, child_ns, 0), charge, stack_base)?;
        }
        self.done = true;
        Ok(None)
    }
}

/// Active resumable search stream for one scheduled job.
enum BoundedShortestState {
    Tree {
        parent: HbMap<(u64, u32), Parent>,
        order: Vec<(u64, u32)>,
        idx: usize,
    },
    ReverseWide {
        parent: HbMap<(u64, u32), Parent>,
        order: Vec<(u64, u32)>,
        bwd_dist: HbMap<u64, u32>,
        direct_multi: HbMap<u64, u32>,
        idx: usize,
        max_len: u32,
    },
}

/// Resumable job for length-bounded weighted `path:shortest` with an unbound end
/// (ROADMAP item 275): runs one Pareto-pruned search (and, for reverse wide weights with
/// multi-route nodes, re-solves only multi-route sources and sources whose reverse tree
/// path passes through one within the remaining length budget in source order pruned by
/// the reverse search's minimum edge distances) and emits each reached node's optimal path
/// within `max_len` on demand.
struct BoundedShortestJob {
    root: u64,
    forward: bool,
    state: BoundedShortestState,
    pending_zero: bool,
    skip_virt: bool,
    need_edges: bool,
}

impl BoundedShortestJob {
    #[allow(clippy::too_many_arguments)]
    fn open(
        ctx: &Ctx,
        spec: &PathSearchSpec,
        search: &mut SearchContext,
        root: u64,
        forward: bool,
        max_len: u32,
        charge: &mut super::ctx::OwnedCharge,
        outer_bytes: u64,
    ) -> Result<Self> {
        search.begin();
        let state = match search.run_dijkstra_bounded(
            ctx,
            spec,
            root,
            forward,
            max_len,
            charge,
            outer_bytes,
        )? {
            BoundedTreeOutcome::Tree((order, parent)) => BoundedShortestState::Tree {
                parent,
                order,
                idx: 0,
            },
            BoundedTreeOutcome::ReverseWide {
                order,
                parent,
                bwd_dist,
                direct_multi,
            } => BoundedShortestState::ReverseWide {
                parent,
                order,
                bwd_dist,
                direct_multi,
                idx: 0,
                max_len,
            },
        };
        let pending_zero = spec.min_len == 0;
        Ok(Self {
            root,
            forward,
            state,
            pending_zero,
            skip_virt: pending_zero,
            need_edges: spec.per_edge() || spec.weighted(),
        })
    }

    fn retained_bytes(&self) -> u64 {
        match &self.state {
            BoundedShortestState::Tree { parent, order, .. } => {
                map_bytes(parent).saturating_add(vec_bytes::<(u64, u32)>(order.capacity()))
            }
            BoundedShortestState::ReverseWide {
                parent,
                order,
                bwd_dist,
                direct_multi,
                ..
            } => map_bytes(parent)
                .saturating_add(vec_bytes::<(u64, u32)>(order.capacity()))
                .saturating_add(map_bytes(bwd_dist))
                .saturating_add(map_bytes(direct_multi)),
        }
    }

    fn next_path(
        &mut self,
        ctx: &Ctx,
        spec: &PathSearchSpec,
        search: &mut SearchContext,
        charge: &mut super::ctx::OwnedCharge,
        outer_bytes: u64,
    ) -> Result<Option<Found>> {
        if self.pending_zero {
            self.pending_zero = false;
            search.paths_found += 1;
            return Ok(Some(Found {
                start: self.root,
                end: self.root,
                edges: Vec::new(),
                len: 0,
                cost: 0.0,
            }));
        }
        let job_outer = outer_bytes.saturating_add(self.retained_bytes());
        match &mut self.state {
            BoundedShortestState::Tree { parent, order, idx } => {
                while *idx < order.len() {
                    let (m, len) = order[*idx];
                    *idx += 1;
                    if self.skip_virt && m == VIRT {
                        continue;
                    }
                    let real = |x: u64| if x == VIRT { self.root } else { x };
                    let base = search.retained_bytes().saturating_add(job_outer);
                    charge.resize(base.saturating_add(vec_bytes::<Edge>(len as usize)))?;
                    let mut edges = Vec::with_capacity(len as usize);
                    let mut cur = m;
                    let mut l = len;
                    let mut steps = 0usize;
                    while l > 0 {
                        steps += 1;
                        if steps.is_multiple_of(4096) {
                            ctx.check()?;
                        }
                        let q = parent[&(cur, l)];
                        let e = if self.forward {
                            Edge {
                                from: q.node,
                                to: cur,
                                p: q.p,
                                rev: q.rev,
                            }
                        } else {
                            Edge {
                                from: cur,
                                to: q.node,
                                p: q.p,
                                rev: q.rev,
                            }
                        };
                        edges.push(e.map(real));
                        cur = q.node;
                        l -= 1;
                    }
                    if self.forward {
                        edges.reverse();
                    }
                    let cost = search.path_cost(
                        ctx,
                        spec,
                        &edges,
                        charge,
                        job_outer.saturating_add(vec_bytes::<Edge>(edges.capacity())),
                    )?;
                    let edges = if self.need_edges { edges } else { Vec::new() };
                    charge.resize(
                        search
                            .retained_bytes()
                            .saturating_add(job_outer)
                            .saturating_add(vec_bytes::<Edge>(edges.capacity())),
                    )?;
                    let other = real(m);
                    let (start, end) = if self.forward {
                        (self.root, other)
                    } else {
                        (other, self.root)
                    };
                    search.paths_found += 1;
                    return Ok(Some(Found {
                        start,
                        end,
                        edges,
                        len: len as usize,
                        cost,
                    }));
                }
                Ok(None)
            }
            BoundedShortestState::ReverseWide {
                parent,
                order,
                bwd_dist,
                direct_multi,
                idx,
                max_len,
            } => {
                while *idx < order.len() {
                    let (m, len) = order[*idx];
                    *idx += 1;
                    if self.skip_virt && m == VIRT {
                        continue;
                    }
                    let start = if m == VIRT { self.root } else { m };
                    let (edges, len) =
                        if !bounded_chain_has_multi(ctx, parent, direct_multi, m, len, *max_len)? {
                            let base = search.retained_bytes().saturating_add(job_outer);
                            charge.resize(base.saturating_add(vec_bytes::<Edge>(len as usize)))?;
                            let edges = reconstruct_bounded(ctx, self.root, false, parent, m, len)?;
                            (edges, len as usize)
                        } else {
                            let to = if m == VIRT { VIRT } else { self.root };
                            let real = |x: u64| if x == VIRT { start } else { x };
                            let Some(mut edges) = search.run_dijkstra_pair_bounded_forward(
                                ctx,
                                spec,
                                start,
                                to,
                                start,
                                None,
                                None,
                                *max_len,
                                Cost::ZERO,
                                Some(bwd_dist),
                                charge,
                                job_outer,
                            )?
                            else {
                                continue;
                            };
                            for (i, edge) in edges.iter_mut().enumerate() {
                                if i.is_multiple_of(4096) {
                                    ctx.check()?;
                                }
                                *edge = edge.map(real);
                            }
                            let len = edges.len();
                            (edges, len)
                        };
                    let cost = search.path_cost(
                        ctx,
                        spec,
                        &edges,
                        charge,
                        job_outer.saturating_add(vec_bytes::<Edge>(edges.capacity())),
                    )?;
                    let edges = if self.need_edges { edges } else { Vec::new() };
                    charge.resize(
                        search
                            .retained_bytes()
                            .saturating_add(job_outer)
                            .saturating_add(vec_bytes::<Edge>(edges.capacity())),
                    )?;
                    search.paths_found += 1;
                    return Ok(Some(Found {
                        start,
                        end: self.root,
                        edges,
                        len,
                        cost,
                    }));
                }
                Ok(None)
            }
        }
    }
}

enum ActiveJob {
    None,
    One(Option<Found>),
    All(Box<AllPathsJob>),
    Tree(Box<TreeStreamJob>),
    BoundedShortest(Box<BoundedShortestJob>),
    BidiUnweighted(Box<BidiUnweightedJob>),
    BidiWeightedAll(Box<BidiWeightedAllJob>),
    Yen(Box<YenJob>),
}

impl ActiveJob {
    fn retained_bytes(&self) -> u64 {
        match self {
            ActiveJob::None => 0,
            ActiveJob::One(f) => f
                .as_ref()
                .map_or(0, |p| vec_bytes::<Edge>(p.edges.capacity())),
            ActiveJob::All(st) => {
                (size_of::<AllPathsJob>() as u64).saturating_add(st.retained_bytes())
            }
            ActiveJob::Tree(st) => {
                (size_of::<TreeStreamJob>() as u64).saturating_add(st.retained_bytes())
            }
            ActiveJob::BoundedShortest(st) => {
                (size_of::<BoundedShortestJob>() as u64).saturating_add(st.retained_bytes())
            }
            ActiveJob::BidiUnweighted(st) => {
                (size_of::<BidiUnweightedJob>() as u64).saturating_add(st.retained_bytes())
            }
            ActiveJob::BidiWeightedAll(st) => {
                (size_of::<BidiWeightedAllJob>() as u64).saturating_add(st.retained_bytes())
            }
            ActiveJob::Yen(st) => (size_of::<YenJob>() as u64).saturating_add(st.retained_bytes()),
        }
    }

    fn next_path(
        &mut self,
        ctx: &Ctx,
        spec: &PathSearchSpec,
        search: &mut SearchContext,
        charge: &mut super::ctx::OwnedCharge,
        outer_bytes: u64,
    ) -> Result<Option<Found>> {
        match self {
            ActiveJob::None => Ok(None),
            ActiveJob::One(f) => Ok(f.take()),
            ActiveJob::All(st) => st.next_path(
                ctx,
                spec,
                search,
                charge,
                outer_bytes.saturating_add(size_of::<AllPathsJob>() as u64),
            ),
            ActiveJob::Tree(st) => st.next_path(
                ctx,
                spec,
                search,
                charge,
                outer_bytes.saturating_add(size_of::<TreeStreamJob>() as u64),
            ),
            ActiveJob::BoundedShortest(st) => st.next_path(
                ctx,
                spec,
                search,
                charge,
                outer_bytes.saturating_add(size_of::<BoundedShortestJob>() as u64),
            ),
            ActiveJob::BidiUnweighted(st) => st.next_path(
                ctx,
                search,
                charge,
                outer_bytes.saturating_add(size_of::<BidiUnweightedJob>() as u64),
            ),
            ActiveJob::BidiWeightedAll(st) => st.next_path(
                ctx,
                spec,
                search,
                charge,
                outer_bytes.saturating_add(size_of::<BidiWeightedAllJob>() as u64),
            ),
            ActiveJob::Yen(st) => st.next_path(
                ctx,
                spec,
                search,
                charge,
                outer_bytes.saturating_add(size_of::<YenJob>() as u64),
            ),
        }
    }
}

struct ForwardJob {
    source: u64,
    any: bool,
    targets: Vec<u64>,
}

/// Charged group input rows and their lookup indices (item 1123).
struct InputState {
    table: Table,
    _table_charge: super::ctx::OwnedCharge,
    in_cols: Vec<Option<usize>>,
    rows_by_pair: HbMap<(u64, u64), Vec<u32>>,
    rows_by_source: HbMap<u64, Vec<u32>>,
    rows_by_target: HbMap<u64, Vec<u32>>,
    row_vec_bytes: u64,
}

impl InputState {
    fn index_bytes(&self) -> u64 {
        map_bytes(&self.rows_by_pair)
            .saturating_add(map_bytes(&self.rows_by_source))
            .saturating_add(map_bytes(&self.rows_by_target))
            .saturating_add(self.row_vec_bytes)
            .saturating_add(vec_bytes::<Option<usize>>(self.in_cols.capacity()))
    }
}

struct RowCols {
    src_c: Option<usize>,
    tgt_c: Option<usize>,
    pi_c: Option<usize>,
    ei_c: Option<usize>,
    es_c: Option<usize>,
    ep_c: Option<usize>,
    eo_c: Option<usize>,
    edge_c: Option<usize>,
    len_c: Option<usize>,
    cost_c: Option<usize>,
}

impl RowCols {
    fn new(spec: &PathSearchSpec, vars: &[VarId]) -> Self {
        let pos = |v: Option<VarId>| v.and_then(|v| vars.iter().position(|x| *x == v));
        Self {
            src_c: match spec.source {
                PathEnd::Var(v) => pos(Some(v)),
                _ => None,
            },
            tgt_c: match spec.target {
                PathEnd::Var(v) => pos(Some(v)),
                _ => None,
            },
            pi_c: pos(spec.path_index),
            ei_c: pos(spec.edge_index),
            es_c: pos(spec.edge_s),
            ep_c: pos(spec.edge_p),
            eo_c: pos(spec.edge_o),
            edge_c: pos(spec.edge),
            len_c: pos(spec.length),
            cost_c: pos(spec.cost),
        }
    }
}

struct CurrentEmission {
    pid: u32,
    found: Found,
    is_forward: bool,
    /// For group inputs: 0 = `rows_by_pair`, 1 = `rows_by_source` (or `rows_by_target` when backward).
    phase: u8,
    row_pos: usize,
    edge_at: usize,
}

impl CurrentEmission {
    fn retained_bytes(&self) -> u64 {
        vec_bytes::<Edge>(self.found.edges.capacity())
    }
}

/// Owned, `Send` resumable reader for every `SERVICE path:search` without `path:limit`
/// (items 829, 1122 and 1123).
pub(crate) struct Reader {
    cols: RowCols,
    search: SearchContext,
    input: Option<InputState>,
    forward: Vec<ForwardJob>,
    forward_target_bytes: u64,
    backward: Vec<u64>,
    fwd_idx: usize,
    fwd_target_idx: usize,
    bwd_idx: usize,
    active: ActiveJob,
    active_is_forward: bool,
    current: Option<CurrentEmission>,
    next_path_id: u32,
    terms: HbMap<(u64, u64, u64), Id>,
    path_row: Vec<Id>,
    in_row: Vec<Id>,
    row_buf: Vec<Id>,
    charge: super::ctx::OwnedCharge,
}

fn fill_undef_checked(buf: &mut [Id], ctx: &Ctx, ticks: &mut usize) -> Result<()> {
    let len = buf.len();
    if *ticks + len < 4096 {
        buf.fill(Id::UNDEF);
        *ticks += len;
        return Ok(());
    }
    let mut pos = 0usize;
    while pos < len {
        let chunk = (len - pos).min(4096 - *ticks);
        buf[pos..pos + chunk].fill(Id::UNDEF);
        *ticks += chunk;
        if *ticks >= 4096 {
            *ticks = 0;
            ctx.check()?;
        }
        pos += chunk;
    }
    Ok(())
}

fn copy_slice_checked(dst: &mut [Id], src: &[Id], ctx: &Ctx, ticks: &mut usize) -> Result<()> {
    debug_assert_eq!(dst.len(), src.len());
    let len = dst.len();
    if *ticks + len < 4096 {
        dst.copy_from_slice(src);
        *ticks += len;
        return Ok(());
    }
    let mut pos = 0usize;
    while pos < len {
        let chunk = (len - pos).min(4096 - *ticks);
        dst[pos..pos + chunk].copy_from_slice(&src[pos..pos + chunk]);
        *ticks += chunk;
        if *ticks >= 4096 {
            *ticks = 0;
            ctx.check()?;
        }
        pos += chunk;
    }
    Ok(())
}

fn push_row_checked(out: &mut Table, row: &[Id], ctx: &Ctx, ticks: &mut usize) -> Result<()> {
    debug_assert_eq!(out.cols.len(), row.len());
    let prev_len = out.len;
    if *ticks + row.len() < 4096 {
        out.push_row(row);
        *ticks += row.len();
        return Ok(());
    }
    for (c, &v) in row.iter().enumerate() {
        *ticks += 1;
        if *ticks >= 4096 {
            *ticks = 0;
            if let Err(e) = ctx.check() {
                for col in &mut out.cols[..c] {
                    col.truncate(prev_len);
                }
                return Err(e);
            }
        }
        out.cols[c].push(v);
    }
    out.len = prev_len + 1;
    Ok(())
}

impl Reader {
    pub(crate) fn open(
        ctx: &std::sync::Arc<Ctx>,
        spec: &PathSearchSpec,
        vars: &[VarId],
        input: Option<(Table, super::ctx::OwnedCharge)>,
        pattern: ChargedPatternEdges,
        mut charge: super::ctx::OwnedCharge,
    ) -> Result<Self> {
        let cols = RowCols::new(spec, vars);
        let search = SearchContext::open(ctx, spec, pattern, &mut charge)?;
        let mut forward: Vec<ForwardJob> = Vec::new();
        let mut forward_target_bytes = 0u64;
        let mut backward: Vec<u64> = Vec::new();
        let input_state = if let Some((table, table_charge)) = input {
            let col = |e: &PathEnd| match e {
                PathEnd::Var(v) => table.col_of(*v),
                PathEnd::Const(_) => None,
            };
            let (sc, tc) = (col(&spec.source), col(&spec.target));
            let end_of = |t: &Table, e: &PathEnd, c: Option<usize>, i: usize| -> Option<u64> {
                match (e, c) {
                    (PathEnd::Const(id), _) => Some(id.0),
                    (PathEnd::Var(_), Some(c)) => {
                        let x = t.cols[c][i];
                        (!x.is_undef()).then_some(x.0)
                    }
                    (PathEnd::Var(_), None) => None,
                }
            };
            let in_cols_bytes = vec_bytes::<Option<usize>>(vars.len());
            charge.resize(search.retained_bytes().saturating_add(in_cols_bytes))?;
            let mut in_cols = Vec::with_capacity(vars.len());
            for (j, v) in vars.iter().enumerate() {
                if j.is_multiple_of(4096) {
                    ctx.check()?;
                }
                in_cols.push(table.col_of(*v));
            }
            let mut state = InputState {
                table,
                _table_charge: table_charge,
                in_cols,
                rows_by_pair: HbMap::default(),
                rows_by_source: HbMap::default(),
                rows_by_target: HbMap::default(),
                row_vec_bytes: 0,
            };
            let mut fwd_any: HbSet<u64> = HbSet::default();
            let mut fwd_targets: HbMap<u64, Vec<u64>> = HbMap::default();
            let mut fwd_t_vec_bytes = 0u64;
            let mut bwd_set: HbSet<u64> = HbSet::default();
            let mut base = search.retained_bytes().saturating_add(state.index_bytes());
            charge.resize(base)?;
            for i in 0..state.table.len() {
                if i.is_multiple_of(4096) {
                    ctx.check()?;
                }
                let src = end_of(&state.table, &spec.source, sc, i);
                let tgt = end_of(&state.table, &spec.target, tc, i);
                let row_id = i as u32;
                match (src, tgt) {
                    (Some(s), Some(t)) => {
                        base = push_hb_map_vec(
                            &mut state.rows_by_pair,
                            &mut state.row_vec_bytes,
                            (s, t),
                            row_id,
                            &mut charge,
                            base,
                        )?;
                        base = push_hb_map_vec(
                            &mut fwd_targets,
                            &mut fwd_t_vec_bytes,
                            s,
                            t,
                            &mut charge,
                            base,
                        )?;
                    }
                    (Some(s), None) => {
                        base = push_hb_map_vec(
                            &mut state.rows_by_source,
                            &mut state.row_vec_bytes,
                            s,
                            row_id,
                            &mut charge,
                            base,
                        )?;
                        (_, base) = insert_hb_set(&mut fwd_any, s, &mut charge, base)?;
                    }
                    (None, Some(t)) => {
                        base = push_hb_map_vec(
                            &mut state.rows_by_target,
                            &mut state.row_vec_bytes,
                            t,
                            row_id,
                            &mut charge,
                            base,
                        )?;
                        (_, base) = insert_hb_set(&mut bwd_set, t, &mut charge, base)?;
                    }
                    (None, None) => {
                        return Err(bad(
                            "a row binds neither the source nor the target; bind one of them",
                        ));
                    }
                }
            }
            // Collect all distinct forward sources (union of `fwd_any` and `fwd_targets`).
            for (idx, &s) in fwd_any.iter().enumerate() {
                if idx.is_multiple_of(4096) {
                    ctx.check()?;
                }
                if !fwd_targets.contains_key(&s) {
                    (_, base) = insert_hb_map(&mut fwd_targets, s, Vec::new(), &mut charge, base)?;
                }
            }
            let fwd_len = fwd_targets.len();
            charge.resize(base.saturating_add(vec_bytes::<ForwardJob>(fwd_len)))?;
            forward.reserve_exact(fwd_len);
            let mut ticks = 0usize;
            for (s, mut ts) in fwd_targets {
                ticks += ts.len().max(1);
                if ticks >= 4096 {
                    ticks = 0;
                    ctx.check()?;
                }
                sort_dedup_u64_checked(&mut ts, ctx)?;
                forward_target_bytes =
                    forward_target_bytes.saturating_add(vec_bytes::<u64>(ts.capacity()));
                forward.push(ForwardJob {
                    source: s,
                    any: fwd_any.contains(&s),
                    targets: ts,
                });
            }
            drop(fwd_any);
            let bwd_base = search
                .retained_bytes()
                .saturating_add(state.index_bytes())
                .saturating_add(vec_bytes::<ForwardJob>(forward.capacity()))
                .saturating_add(forward_target_bytes)
                .saturating_add(set_bytes(&bwd_set));
            charge.resize(bwd_base)?;
            sort_checked_by_key(&mut forward, ctx, |j| j.source)?;
            let bwd_len = bwd_set.len();
            charge.resize(bwd_base.saturating_add(vec_bytes::<u64>(bwd_len)))?;
            backward.reserve_exact(bwd_len);
            for (idx, t) in bwd_set.into_iter().enumerate() {
                if idx.is_multiple_of(4096) {
                    ctx.check()?;
                }
                backward.push(t);
            }
            let post_bwd = search
                .retained_bytes()
                .saturating_add(state.index_bytes())
                .saturating_add(vec_bytes::<ForwardJob>(forward.capacity()))
                .saturating_add(forward_target_bytes)
                .saturating_add(vec_bytes::<u64>(backward.capacity()));
            charge.resize(post_bwd)?;
            sort_checked_by_key(&mut backward, ctx, |&t| t)?;
            Some(state)
        } else {
            let base = search.retained_bytes();
            match (&spec.source, &spec.target) {
                (PathEnd::Const(s), PathEnd::Const(t)) => {
                    let add = vec_bytes::<ForwardJob>(1).saturating_add(vec_bytes::<u64>(1));
                    charge.resize(base.saturating_add(add))?;
                    let ts = vec![t.0];
                    forward_target_bytes = vec_bytes::<u64>(ts.capacity());
                    forward.reserve_exact(1);
                    forward.push(ForwardJob {
                        source: s.0,
                        any: false,
                        targets: ts,
                    });
                }
                (PathEnd::Const(s), PathEnd::Var(_)) => {
                    charge.resize(base.saturating_add(vec_bytes::<ForwardJob>(1)))?;
                    forward.reserve_exact(1);
                    forward.push(ForwardJob {
                        source: s.0,
                        any: true,
                        targets: Vec::new(),
                    });
                }
                (PathEnd::Var(_), PathEnd::Const(t)) => {
                    charge.resize(base.saturating_add(vec_bytes::<u64>(1)))?;
                    backward.reserve_exact(1);
                    backward.push(t.0);
                }
                (PathEnd::Var(_), PathEnd::Var(_)) => {
                    return Err(bad(
                        "a row binds neither the source nor the target; bind one of them",
                    ));
                }
            }
            None
        };
        let width = vars.len();
        let pre_scratch = search
            .retained_bytes()
            .saturating_add(input_state.as_ref().map_or(0, InputState::index_bytes))
            .saturating_add(vec_bytes::<ForwardJob>(forward.capacity()))
            .saturating_add(forward_target_bytes)
            .saturating_add(vec_bytes::<u64>(backward.capacity()));
        let scratch_bytes = vec_bytes::<Id>(width).saturating_mul(3);
        charge.resize(pre_scratch.saturating_add(scratch_bytes))?;
        let mut path_row = Vec::with_capacity(width);
        path_row.resize(width, Id::UNDEF);
        let mut in_row = Vec::with_capacity(width);
        in_row.resize(width, Id::UNDEF);
        let mut row_buf = Vec::with_capacity(width);
        row_buf.resize(width, Id::UNDEF);
        let mut reader = Self {
            cols,
            search,
            input: input_state,
            forward,
            forward_target_bytes,
            backward,
            fwd_idx: 0,
            fwd_target_idx: 0,
            bwd_idx: 0,
            active: ActiveJob::None,
            active_is_forward: true,
            current: None,
            next_path_id: 0,
            terms: HbMap::default(),
            path_row,
            in_row,
            row_buf,
            charge,
        };
        reader.update_charge()?;
        Ok(reader)
    }

    pub(crate) fn counters(
        &self,
        spec: &PathSearchSpec,
    ) -> serde_json::Map<String, serde_json::Value> {
        self.search.counters(spec)
    }

    fn terms_bytes(&self) -> u64 {
        map_bytes(&self.terms)
    }

    fn current_bytes(&self) -> u64 {
        self.current
            .as_ref()
            .map_or(0, CurrentEmission::retained_bytes)
    }

    fn jobs_bytes(&self) -> u64 {
        vec_bytes::<ForwardJob>(self.forward.capacity())
            .saturating_add(self.forward_target_bytes)
            .saturating_add(vec_bytes::<u64>(self.backward.capacity()))
    }

    fn row_scratch_bytes(&self) -> u64 {
        vec_bytes::<Id>(self.path_row.capacity())
            .saturating_add(vec_bytes::<Id>(self.in_row.capacity()))
            .saturating_add(vec_bytes::<Id>(self.row_buf.capacity()))
    }

    /// Bytes on `self.charge` excluding `self.search` and `self.active`.
    fn fixed_bytes_excluding_active(&self) -> u64 {
        self.input
            .as_ref()
            .map_or(0, InputState::index_bytes)
            .saturating_add(self.jobs_bytes())
            .saturating_add(self.current_bytes())
            .saturating_add(self.terms_bytes())
            .saturating_add(self.row_scratch_bytes())
    }

    fn total_bytes(&self) -> u64 {
        self.search
            .retained_bytes()
            .saturating_add(self.fixed_bytes_excluding_active())
            .saturating_add(self.active.retained_bytes())
    }

    fn update_charge(&mut self) -> Result<()> {
        let total = self.total_bytes();
        self.charge.resize(total)
    }

    /// Open the next scheduled job into `self.active`, returning `false` when all jobs
    /// are exhausted.
    fn open_next_job(&mut self, ctx: &Ctx, spec: &PathSearchSpec) -> Result<bool> {
        self.active = ActiveJob::None;
        let outer = self.fixed_bytes_excluding_active();
        self.charge
            .resize(self.search.retained_bytes().saturating_add(outer))?;
        let weighted = spec.weighted();
        let all = matches!(spec.algorithm, Algorithm::AllShortest);
        let need_edges = spec.per_edge() || weighted;
        let bounded_weighted_shortest =
            weighted && spec.max_len.is_some() && matches!(spec.algorithm, Algorithm::Shortest);
        let yen_all_shortest =
            weighted && spec.max_len.is_some() && matches!(spec.algorithm, Algorithm::AllShortest);
        if self.fwd_idx < self.forward.len() {
            self.active_is_forward = true;
            let s = self.forward[self.fwd_idx].source;
            let any = self.forward[self.fwd_idx].any;
            match spec.algorithm {
                Algorithm::Shortest if bounded_weighted_shortest => {
                    let max = spec.max_len.unwrap();
                    if any {
                        let old = std::mem::take(&mut self.forward[self.fwd_idx].targets);
                        self.forward_target_bytes = self
                            .forward_target_bytes
                            .saturating_sub(vec_bytes::<u64>(old.capacity()));
                        drop(old);
                        self.fwd_idx += 1;
                        self.fwd_target_idx = 0;
                        let outer = self.fixed_bytes_excluding_active();
                        self.charge
                            .resize(self.search.retained_bytes().saturating_add(outer))?;
                        let job = BoundedShortestJob::open(
                            ctx,
                            spec,
                            &mut self.search,
                            s,
                            true,
                            max,
                            &mut self.charge,
                            outer,
                        )?;
                        self.charge.resize(
                            self.search
                                .retained_bytes()
                                .saturating_add(outer)
                                .saturating_add(job.retained_bytes())
                                .saturating_add(size_of::<BoundedShortestJob>() as u64),
                        )?;
                        self.active = ActiveJob::BoundedShortest(Box::new(job));
                    } else {
                        let t = self.forward[self.fwd_idx].targets[self.fwd_target_idx];
                        self.fwd_target_idx += 1;
                        if self.fwd_target_idx >= self.forward[self.fwd_idx].targets.len() {
                            let old = std::mem::take(&mut self.forward[self.fwd_idx].targets);
                            self.forward_target_bytes = self
                                .forward_target_bytes
                                .saturating_sub(vec_bytes::<u64>(old.capacity()));
                            drop(old);
                            self.fwd_idx += 1;
                            self.fwd_target_idx = 0;
                        }
                        let outer = self.fixed_bytes_excluding_active();
                        self.charge
                            .resize(self.search.retained_bytes().saturating_add(outer))?;
                        self.search.begin();
                        if spec.min_len == 0 && s == t {
                            self.search.paths_found += 1;
                            self.active = ActiveJob::One(Some(Found {
                                start: s,
                                end: t,
                                edges: Vec::new(),
                                len: 0,
                                cost: 0.0,
                            }));
                        } else {
                            let tt = if s == t { VIRT } else { t };
                            let real = |x: u64| if x == VIRT { s } else { x };
                            if let Some(mut edges) = self.search.run_dijkstra_pair_bounded(
                                ctx,
                                spec,
                                s,
                                tt,
                                s,
                                None,
                                None,
                                max,
                                PrefixCost::zero(max),
                                &mut self.charge,
                                outer,
                            )? {
                                for (idx, edge) in edges.iter_mut().enumerate() {
                                    if idx.is_multiple_of(4096) {
                                        ctx.check()?;
                                    }
                                    *edge = edge.map(real);
                                }
                                let cost = self.search.path_cost(
                                    ctx,
                                    spec,
                                    &edges,
                                    &mut self.charge,
                                    outer.saturating_add(vec_bytes::<Edge>(edges.capacity())),
                                )?;
                                let len = edges.len();
                                let edges = if need_edges { edges } else { Vec::new() };
                                self.search.paths_found += 1;
                                self.active = ActiveJob::One(Some(Found {
                                    start: s,
                                    end: t,
                                    edges,
                                    len,
                                    cost,
                                }));
                            }
                        }
                    }
                }
                Algorithm::AllShortest if yen_all_shortest => {
                    if any {
                        return Err(bad(
                            "path:maxLength with path:weight needs a bound target in path:allShortest",
                        ));
                    }
                    let t = self.forward[self.fwd_idx].targets[self.fwd_target_idx];
                    self.fwd_target_idx += 1;
                    if self.fwd_target_idx >= self.forward[self.fwd_idx].targets.len() {
                        let old = std::mem::take(&mut self.forward[self.fwd_idx].targets);
                        self.forward_target_bytes = self
                            .forward_target_bytes
                            .saturating_sub(vec_bytes::<u64>(old.capacity()));
                        drop(old);
                        self.fwd_idx += 1;
                        self.fwd_target_idx = 0;
                    }
                    let outer = self.fixed_bytes_excluding_active();
                    self.charge.resize(
                        self.search
                            .retained_bytes()
                            .saturating_add(outer)
                            .saturating_add(size_of::<YenJob>() as u64),
                    )?;
                    self.active =
                        ActiveJob::Yen(Box::new(YenJob::open(spec, &mut self.search, s, t)));
                }
                Algorithm::Shortest | Algorithm::AllShortest => {
                    let cur_idx = self.fwd_idx;
                    let single = !any && self.forward[cur_idx].targets.len() == 1;
                    let t0 = self.forward[cur_idx].targets.first().copied();
                    let moved_targets = std::mem::take(&mut self.forward[cur_idx].targets);
                    let moved_bytes = vec_bytes::<u64>(moved_targets.capacity());
                    let targets_opt: Option<Vec<u64>> = if any {
                        drop(moved_targets);
                        self.forward_target_bytes =
                            self.forward_target_bytes.saturating_sub(moved_bytes);
                        None
                    } else {
                        Some(moved_targets)
                    };
                    self.fwd_idx += 1;
                    self.fwd_target_idx = 0;
                    let mut outer = self.fixed_bytes_excluding_active();
                    if any {
                        self.charge
                            .resize(self.search.retained_bytes().saturating_add(outer))?;
                    }
                    if single && !weighted {
                        drop(targets_opt);
                        self.forward_target_bytes =
                            self.forward_target_bytes.saturating_sub(moved_bytes);
                        outer = self.fixed_bytes_excluding_active();
                        self.charge
                            .resize(self.search.retained_bytes().saturating_add(outer))?;
                        let t = t0.unwrap();
                        self.search.begin();
                        if spec.min_len == 0 && s == t {
                            self.search.paths_found += 1;
                            self.active = ActiveJob::One(Some(Found {
                                start: s,
                                end: t,
                                edges: Vec::new(),
                                len: 0,
                                cost: 0.0,
                            }));
                        } else {
                            let tt = if s == t { VIRT } else { t };
                            let Some((f, b, meet)) = self.search.run_bidi(
                                ctx,
                                spec,
                                s,
                                tt,
                                s,
                                all,
                                None,
                                spec.max_len,
                                &mut self.charge,
                                outer,
                            )?
                            else {
                                self.active = ActiveJob::None;
                                return Ok(true);
                            };
                            if !all {
                                let m = meet[0];
                                drop(meet);
                                let base_t = self
                                    .search
                                    .retained_bytes()
                                    .saturating_add(outer)
                                    .saturating_add(f.retained_bytes())
                                    .saturating_add(b.retained_bytes());
                                self.charge.resize(base_t)?;
                                let a = match TreePathIter::open(
                                    &f,
                                    m,
                                    false,
                                    None,
                                    &mut self.charge,
                                    base_t,
                                )? {
                                    Some(mut it) => {
                                        let p = it.next_path(
                                            ctx,
                                            &f,
                                            None,
                                            None,
                                            &|_| false,
                                            &mut self.charge,
                                            base_t,
                                        )?;
                                        drop(it);
                                        p
                                    }
                                    None => None,
                                };
                                let a_bytes =
                                    a.as_ref().map_or(0, |v| vec_bytes::<Edge>(v.capacity()));
                                let base_a = base_t.saturating_add(a_bytes);
                                self.charge.resize(base_a)?;
                                let z = match TreePathIter::open(
                                    &b,
                                    m,
                                    false,
                                    None,
                                    &mut self.charge,
                                    base_a,
                                )? {
                                    Some(mut it) => {
                                        let p = it.next_path(
                                            ctx,
                                            &b,
                                            None,
                                            None,
                                            &|_| false,
                                            &mut self.charge,
                                            base_a,
                                        )?;
                                        drop(it);
                                        p
                                    }
                                    None => None,
                                };
                                drop(f);
                                drop(b);
                                if let (Some(a), Some(z)) = (a, z) {
                                    let len = a.len().saturating_add(z.len());
                                    let real = |x: u64| if x == VIRT { s } else { x };
                                    let edges = if need_edges {
                                        let base_p =
                                            self.search.retained_bytes().saturating_add(outer);
                                        self.charge.resize(
                                            base_p
                                                .saturating_add(a_bytes)
                                                .saturating_add(vec_bytes::<Edge>(z.capacity()))
                                                .saturating_add(vec_bytes::<Edge>(len)),
                                        )?;
                                        let mut p = Vec::with_capacity(len);
                                        for (idx, edge) in a.into_iter().chain(z).enumerate() {
                                            if idx.is_multiple_of(4096) {
                                                ctx.check()?;
                                            }
                                            p.push(edge.map(real));
                                        }
                                        p
                                    } else {
                                        Vec::new()
                                    };
                                    self.search.paths_found += 1;
                                    self.active = ActiveJob::One(Some(Found {
                                        start: s,
                                        end: t,
                                        edges,
                                        len,
                                        cost: len as f64,
                                    }));
                                }
                            } else {
                                let job = BidiUnweightedJob {
                                    s,
                                    t,
                                    f,
                                    b,
                                    meet,
                                    meet_idx: 0,
                                    need_edges,
                                    front_iter: None,
                                    cur_front: None,
                                    back_iter: None,
                                };
                                self.charge.resize(
                                    self.search
                                        .retained_bytes()
                                        .saturating_add(outer)
                                        .saturating_add(job.retained_bytes())
                                        .saturating_add(size_of::<BidiUnweightedJob>() as u64),
                                )?;
                                self.active = ActiveJob::BidiUnweighted(Box::new(job));
                            }
                        }
                    } else {
                        let both_ends = single && weighted && (all || t0 == Some(s));
                        let mut fallback_from_both = false;
                        if both_ends {
                            let t = t0.unwrap();
                            self.search.begin();
                            if spec.min_len == 0 && s == t {
                                self.search.paths_found += 1;
                                self.active = ActiveJob::One(Some(Found {
                                    start: s,
                                    end: t,
                                    edges: Vec::new(),
                                    len: 0,
                                    cost: 0.0,
                                }));
                            } else {
                                match self.search.run_dijkstra_pair_all(
                                    ctx,
                                    spec,
                                    s,
                                    t,
                                    all,
                                    need_edges,
                                    &mut self.charge,
                                    outer,
                                )? {
                                    BidiWeightedOutcome::Empty => {
                                        self.active = ActiveJob::None;
                                    }
                                    BidiWeightedOutcome::One(f) => {
                                        self.active = ActiveJob::One(Some(f));
                                    }
                                    BidiWeightedOutcome::Stream(job) => {
                                        self.active = ActiveJob::BidiWeightedAll(job);
                                    }
                                    BidiWeightedOutcome::FallbackSingleSource => {
                                        fallback_from_both = true;
                                    }
                                }
                            }
                        }
                        if single && weighted && !both_ends {
                            drop(targets_opt);
                            self.forward_target_bytes =
                                self.forward_target_bytes.saturating_sub(moved_bytes);
                            outer = self.fixed_bytes_excluding_active();
                            self.charge
                                .resize(self.search.retained_bytes().saturating_add(outer))?;
                            let t = t0.unwrap();
                            self.search.begin();
                            if let Some((edges, cost)) = self.search.run_dijkstra_pair(
                                ctx,
                                spec,
                                s,
                                t,
                                None,
                                None,
                                &mut self.charge,
                                outer,
                            )? {
                                let len = edges.len();
                                let edges = if need_edges { edges } else { Vec::new() };
                                self.search.paths_found += 1;
                                self.active = ActiveJob::One(Some(Found {
                                    start: s,
                                    end: t,
                                    edges,
                                    len,
                                    cost,
                                }));
                            }
                        } else if !both_ends || fallback_from_both {
                            let close = any
                                || targets_opt
                                    .as_ref()
                                    .is_some_and(|ts| ts.binary_search(&s).is_ok());
                            let job = TreeStreamJob::open(
                                ctx,
                                spec,
                                &mut self.search,
                                s,
                                true,
                                targets_opt.as_deref(),
                                all,
                                close,
                                !both_ends,
                                &mut self.charge,
                                outer,
                            )?;
                            if targets_opt.is_some() {
                                drop(targets_opt);
                                self.forward_target_bytes =
                                    self.forward_target_bytes.saturating_sub(moved_bytes);
                            }
                            let post_outer = self.fixed_bytes_excluding_active();
                            self.charge.resize(
                                self.search
                                    .retained_bytes()
                                    .saturating_add(post_outer)
                                    .saturating_add(job.retained_bytes())
                                    .saturating_add(size_of::<TreeStreamJob>() as u64),
                            )?;
                            self.active = ActiveJob::Tree(Box::new(job));
                        } else if targets_opt.is_some() {
                            drop(targets_opt);
                            self.forward_target_bytes =
                                self.forward_target_bytes.saturating_sub(moved_bytes);
                        }
                    }
                }
                Algorithm::KShortest => {
                    if any {
                        return Err(bad("path:kShortest needs a bound target"));
                    }
                    let t = self.forward[self.fwd_idx].targets[self.fwd_target_idx];
                    self.fwd_target_idx += 1;
                    if self.fwd_target_idx >= self.forward[self.fwd_idx].targets.len() {
                        let old = std::mem::take(&mut self.forward[self.fwd_idx].targets);
                        self.forward_target_bytes = self
                            .forward_target_bytes
                            .saturating_sub(vec_bytes::<u64>(old.capacity()));
                        drop(old);
                        self.fwd_idx += 1;
                        self.fwd_target_idx = 0;
                    }
                    let outer = self.fixed_bytes_excluding_active();
                    self.charge.resize(
                        self.search
                            .retained_bytes()
                            .saturating_add(outer)
                            .saturating_add(size_of::<YenJob>() as u64),
                    )?;
                    self.active =
                        ActiveJob::Yen(Box::new(YenJob::open(spec, &mut self.search, s, t)));
                }
                Algorithm::All => {
                    let cur_idx = self.fwd_idx;
                    let moved_targets = std::mem::take(&mut self.forward[cur_idx].targets);
                    let moved_bytes = vec_bytes::<u64>(moved_targets.capacity());
                    let targets_opt: Option<Vec<u64>> = if any {
                        drop(moved_targets);
                        self.forward_target_bytes =
                            self.forward_target_bytes.saturating_sub(moved_bytes);
                        None
                    } else {
                        Some(moved_targets)
                    };
                    self.fwd_idx += 1;
                    self.fwd_target_idx = 0;
                    let outer = self.fixed_bytes_excluding_active();
                    if any {
                        self.charge
                            .resize(self.search.retained_bytes().saturating_add(outer))?;
                    }
                    let job = AllPathsJob::open(
                        ctx,
                        spec,
                        &mut self.search,
                        s,
                        true,
                        targets_opt.as_deref(),
                        &mut self.charge,
                        outer,
                    )?;
                    if targets_opt.is_some() {
                        drop(targets_opt);
                        self.forward_target_bytes =
                            self.forward_target_bytes.saturating_sub(moved_bytes);
                    }
                    let post_outer = self.fixed_bytes_excluding_active();
                    self.charge.resize(
                        self.search
                            .retained_bytes()
                            .saturating_add(post_outer)
                            .saturating_add(job.retained_bytes())
                            .saturating_add(size_of::<AllPathsJob>() as u64),
                    )?;
                    self.active = ActiveJob::All(Box::new(job));
                }
            }
            self.update_charge()?;
            return Ok(true);
        }
        if self.bwd_idx < self.backward.len() {
            self.active_is_forward = false;
            let t = self.backward[self.bwd_idx];
            self.bwd_idx += 1;
            match spec.algorithm {
                Algorithm::KShortest => {
                    return Err(bad("path:kShortest needs a bound source"));
                }
                Algorithm::Shortest | Algorithm::AllShortest => {
                    if yen_all_shortest {
                        return Err(bad(
                            "path:maxLength with path:weight needs a bound source in path:allShortest",
                        ));
                    }
                    if bounded_weighted_shortest {
                        let job = BoundedShortestJob::open(
                            ctx,
                            spec,
                            &mut self.search,
                            t,
                            false,
                            spec.max_len.unwrap(),
                            &mut self.charge,
                            outer,
                        )?;
                        self.charge.resize(
                            self.search
                                .retained_bytes()
                                .saturating_add(outer)
                                .saturating_add(job.retained_bytes())
                                .saturating_add(size_of::<BoundedShortestJob>() as u64),
                        )?;
                        self.active = ActiveJob::BoundedShortest(Box::new(job));
                        self.update_charge()?;
                        return Ok(true);
                    }
                    let job = TreeStreamJob::open(
                        ctx,
                        spec,
                        &mut self.search,
                        t,
                        false,
                        None,
                        all,
                        true,
                        true,
                        &mut self.charge,
                        outer,
                    )?;
                    self.charge.resize(
                        self.search
                            .retained_bytes()
                            .saturating_add(outer)
                            .saturating_add(job.retained_bytes())
                            .saturating_add(size_of::<TreeStreamJob>() as u64),
                    )?;
                    self.active = ActiveJob::Tree(Box::new(job));
                }
                Algorithm::All => {
                    let job = AllPathsJob::open(
                        ctx,
                        spec,
                        &mut self.search,
                        t,
                        false,
                        None,
                        &mut self.charge,
                        outer,
                    )?;
                    self.charge.resize(
                        self.search
                            .retained_bytes()
                            .saturating_add(outer)
                            .saturating_add(job.retained_bytes())
                            .saturating_add(size_of::<AllPathsJob>() as u64),
                    )?;
                    self.active = ActiveJob::All(Box::new(job));
                }
            }
            self.update_charge()?;
            return Ok(true);
        }
        Ok(false)
    }

    fn pull_next_path(
        &mut self,
        ctx: &Ctx,
        spec: &PathSearchSpec,
    ) -> Result<Option<(Found, bool)>> {
        loop {
            ctx.check()?;
            let outer = self.fixed_bytes_excluding_active();
            if let Some(found) =
                self.active
                    .next_path(ctx, spec, &mut self.search, &mut self.charge, outer)?
            {
                return Ok(Some((found, self.active_is_forward)));
            }
            if !self.open_next_job(ctx, spec)? {
                return Ok(None);
            }
        }
    }

    /// Fill `out` with up to `cap` rows. Returns `true` when the search is exhausted.
    pub(crate) fn next(
        &mut self,
        ctx: &Ctx,
        spec: &PathSearchSpec,
        vars: &[VarId],
        out: &mut Table,
        cap: usize,
    ) -> Result<bool> {
        let per_edge = spec.per_edge();
        let weighted = spec.weighted();
        let width = vars.len();
        let int = |n: usize| Id::from_i64(n as i64).unwrap_or(Id::UNDEF);
        let set = |c: Option<usize>, id: Id, row: &mut [Id]| {
            if let Some(c) = c {
                row[c] = id;
            }
        };
        let mut ticks = 0usize;
        while out.len() < cap {
            ctx.check()?;
            if self.current.is_none() {
                let Some((found, is_forward)) = self.pull_next_path(ctx, spec)? else {
                    self.active = ActiveJob::None;
                    self.input = None;
                    self.forward.clear();
                    self.forward.shrink_to_fit();
                    self.forward_target_bytes = 0;
                    self.backward.clear();
                    self.backward.shrink_to_fit();
                    self.terms = HbMap::default();
                    self.update_charge()?;
                    return Ok(true);
                };
                let pid = self.next_path_id;
                self.next_path_id = self.next_path_id.saturating_add(1);
                self.current = Some(CurrentEmission {
                    pid,
                    found,
                    is_forward,
                    phase: 0,
                    row_pos: 0,
                    edge_at: 0,
                });
                self.update_charge()?;
            }
            let cur = self.current.as_ref().expect("current emission");
            let pid = cur.pid;
            let start = cur.found.start;
            let end = cur.found.end;
            let plen = cur.found.len;
            let cost = cur.found.cost;
            let is_forward = cur.is_forward;
            let n_rows = if per_edge {
                cur.found.edges.len().max(1)
            } else {
                1
            };
            let cost_id = if weighted {
                Id::from_f64(cost).unwrap_or_else(|| ctx.intern_value(&Value::Double(cost.into())))
            } else {
                int(plen)
            };
            fill_undef_checked(&mut self.path_row, ctx, &mut ticks)?;
            set(self.cols.src_c, Id(start), &mut self.path_row);
            set(self.cols.tgt_c, Id(end), &mut self.path_row);
            set(self.cols.pi_c, int(pid as usize), &mut self.path_row);
            set(self.cols.len_c, int(plen), &mut self.path_row);
            set(self.cols.cost_c, cost_id, &mut self.path_row);
            let mut finished_current = false;
            'emit: while out.len() < cap {
                ticks += 1;
                if ticks >= 4096 {
                    ticks = 0;
                    ctx.check()?;
                }
                let input_row_idx: Option<usize> = if let Some(inp) = &self.input {
                    let cur = self.current.as_mut().expect("current emission");
                    let next_row = loop {
                        let slice: Option<&[u32]> = if is_forward {
                            match cur.phase {
                                0 => inp.rows_by_pair.get(&(start, end)).map(Vec::as_slice),
                                1 => inp.rows_by_source.get(&start).map(Vec::as_slice),
                                _ => break None,
                            }
                        } else {
                            match cur.phase {
                                0 => inp.rows_by_target.get(&end).map(Vec::as_slice),
                                _ => break None,
                            }
                        };
                        if let Some(s) = slice
                            && cur.row_pos < s.len()
                        {
                            break Some(s[cur.row_pos] as usize);
                        }
                        cur.phase += 1;
                        cur.row_pos = 0;
                        cur.edge_at = 0;
                    };
                    match next_row {
                        Some(idx) => Some(idx),
                        None => {
                            finished_current = true;
                            break 'emit;
                        }
                    }
                } else {
                    None
                };
                if let (Some(inp), Some(idx)) = (&self.input, input_row_idx) {
                    for (j, c) in inp.in_cols.iter().enumerate() {
                        ticks += 1;
                        if ticks >= 4096 {
                            ticks = 0;
                            ctx.check()?;
                        }
                        self.in_row[j] = c.map_or(Id::UNDEF, |c| inp.table.cols[c][idx]);
                    }
                }
                while out.len() < cap {
                    ticks += 1;
                    if ticks >= 4096 {
                        ticks = 0;
                        ctx.check()?;
                    }
                    let edge_k = {
                        let cur = self.current.as_mut().expect("current emission");
                        if cur.edge_at >= n_rows {
                            break;
                        }
                        let k = cur.edge_at;
                        cur.edge_at += 1;
                        k
                    };
                    copy_slice_checked(&mut self.row_buf, &self.path_row, ctx, &mut ticks)?;
                    if per_edge {
                        let edge_opt = self
                            .current
                            .as_ref()
                            .and_then(|c| c.found.edges.get(edge_k).copied());
                        if let Some(e) = edge_opt {
                            let (s, p, o) = e.triple();
                            set(self.cols.ei_c, int(edge_k), &mut self.row_buf);
                            set(self.cols.es_c, Id(s), &mut self.row_buf);
                            set(self.cols.ep_c, Id(p), &mut self.row_buf);
                            set(self.cols.eo_c, Id(o), &mut self.row_buf);
                            if self.cols.edge_c.is_some() {
                                let tid = if let Some(&existing) = self.terms.get(&(s, p, o)) {
                                    existing
                                } else {
                                    let computed = {
                                        let parts =
                                            (ctx.term(Id(s)), ctx.term(Id(p)), ctx.term(Id(o)));
                                        if let (Some(s), Some(Term::NamedNode(p)), Some(o)) = parts
                                        {
                                            match s {
                                                Term::NamedNode(n) => ctx.intern_term(
                                                    &Term::Triple(Box::new(oxrdf::Triple::new(
                                                        oxrdf::NamedOrBlankNode::NamedNode(n),
                                                        p,
                                                        o,
                                                    ))),
                                                ),
                                                Term::BlankNode(b) => ctx.intern_term(
                                                    &Term::Triple(Box::new(oxrdf::Triple::new(
                                                        oxrdf::NamedOrBlankNode::BlankNode(b),
                                                        p,
                                                        o,
                                                    ))),
                                                ),
                                                _ => Id::UNDEF,
                                            }
                                        } else {
                                            Id::UNDEF
                                        }
                                    };
                                    let total = self.total_bytes();
                                    let _ = insert_hb_map(
                                        &mut self.terms,
                                        (s, p, o),
                                        computed,
                                        &mut self.charge,
                                        total,
                                    )?;
                                    computed
                                };
                                ctx.check()?;
                                set(self.cols.edge_c, tid, &mut self.row_buf);
                            }
                        }
                    }
                    if input_row_idx.is_some() {
                        let mut ok = true;
                        for j in 0..width {
                            ticks += 1;
                            if ticks >= 4096 {
                                ticks = 0;
                                ctx.check()?;
                            }
                            match (self.in_row[j].is_undef(), self.row_buf[j].is_undef()) {
                                (_, true) => self.row_buf[j] = self.in_row[j],
                                (true, false) => {}
                                (false, false) => ok &= self.in_row[j] == self.row_buf[j],
                            }
                        }
                        if ok {
                            push_row_checked(out, &self.row_buf, ctx, &mut ticks)?;
                        }
                    } else {
                        push_row_checked(out, &self.row_buf, ctx, &mut ticks)?;
                    }
                }
                let done_edges = self.current.as_ref().is_none_or(|c| c.edge_at >= n_rows);
                if done_edges {
                    if self.input.is_some() {
                        let cur = self.current.as_mut().expect("current emission");
                        cur.row_pos += 1;
                        cur.edge_at = 0;
                    } else {
                        finished_current = true;
                        break 'emit;
                    }
                }
            }
            if finished_current {
                self.current = None;
                self.update_charge()?;
            }
        }
        Ok(false)
    }
}

#[doc(hidden)]
#[cfg(any(test, feature = "failpoints"))]
pub mod test_hooks {
    use std::cell::RefCell;

    thread_local! {
        static ON_TRAVERSAL_POLL: RefCell<Option<Box<dyn FnMut()>>> = const { RefCell::new(None) };
        #[cfg(test)]
        static ON_PRUNE_POLL: RefCell<Option<Box<dyn FnMut()>>> = const { RefCell::new(None) };
    }

    /// Restores the thread-local traversal poll hook when dropped.
    pub struct TraversalPollGuard;

    impl Drop for TraversalPollGuard {
        fn drop(&mut self) {
            ON_TRAVERSAL_POLL.with(|slot| {
                *slot.borrow_mut() = None;
            });
        }
    }

    /// Register a callback invoked right before `ctx.check()` at traversal checkpoints.
    #[must_use]
    pub fn on_traversal_poll(f: impl FnMut() + 'static) -> TraversalPollGuard {
        ON_TRAVERSAL_POLL.with(|slot| {
            *slot.borrow_mut() = Some(Box::new(f));
        });
        TraversalPollGuard
    }

    pub(super) fn fire_traversal_poll() {
        ON_TRAVERSAL_POLL.with(|slot| {
            if let Some(f) = slot.borrow_mut().as_mut() {
                f();
            }
        });
    }

    #[cfg(test)]
    pub(super) struct PrunePollGuard;

    #[cfg(test)]
    impl Drop for PrunePollGuard {
        fn drop(&mut self) {
            ON_PRUNE_POLL.with(|slot| {
                *slot.borrow_mut() = None;
            });
        }
    }

    #[cfg(test)]
    #[must_use]
    pub(super) fn on_prune_poll(f: impl FnMut() + 'static) -> PrunePollGuard {
        ON_PRUNE_POLL.with(|slot| {
            *slot.borrow_mut() = Some(Box::new(f));
        });
        PrunePollGuard
    }

    #[cfg(test)]
    pub(super) fn fire_prune_poll() {
        ON_PRUNE_POLL.with(|slot| {
            if let Some(f) = slot.borrow_mut().as_mut() {
                f();
            }
        });
    }
}

#[cfg(test)]
mod tree_tests {
    use super::*;

    #[test]
    fn r2_backward_frontier_admission_and_pruned_dfs_poll() {
        use crate::io::Source;
        use crate::store::{Store, StoreOptions};
        use oxrdfio::RdfFormat;
        use std::sync::Arc;
        use std::sync::atomic::Ordering;

        let store = Store::in_memory(StoreOptions::default());
        let mut data = String::from("<urn:other> <urn:p> <urn:target>, \"a literal\" .\n");
        for i in 0..2048 {
            data.push_str(&format!("<urn:root> <urn:p> <urn:dead:{i}> .\n"));
        }
        store
            .load(&[Source::from_bytes(
                data.into_bytes(),
                RdfFormat::Turtle,
                None,
            )])
            .unwrap();
        let snap = store.snapshot();
        let find = |iri| snap.lookup_iri(iri).unwrap();
        let (root, target, p) = (find("urn:root"), find("urn:target"), find("urn:p"));
        let spec = PathSearchSpec {
            algorithm: Algorithm::All,
            source: PathEnd::Const(root),
            target: PathEnd::Const(target),
            predicates: Some(vec![p.0]),
            directions: vec![Direction::Forward],
            direction: Direction::Forward,
            exclude: Vec::new(),
            max_degree: None,
            edge_pattern: None,
            min_len: 1,
            max_len: Some(2),
            k: 1,
            limit: None,
            max_visited: 100_000,
            weight: None,
            default_weight: 1.0,
            reifies: None,
            path_index: Some(0),
            edge_index: None,
            edge_s: None,
            edge_p: None,
            edge_o: None,
            edge: None,
            length: None,
            cost: None,
            graph: GraphFilter::Default,
        };
        let ctx = Arc::new(Ctx::new(snap.clone()));
        let mut held = super::super::ctx::OwnedCharge::new(&ctx, 0).unwrap();
        let mut search =
            SearchContext::open(&ctx, &spec, ChargedPatternEdges::new(), &mut held).unwrap();
        let mut state = AllPathsJob::open(
            &ctx,
            &spec,
            &mut search,
            root.0,
            true,
            Some(&[target.0]),
            &mut held,
            0,
        )
        .unwrap();
        ctx.cancel.store(true, Ordering::Relaxed);
        assert!(matches!(
            state.next_path(&ctx, &spec, &mut search, &mut held, 0),
            Err(Error::Cancelled)
        ));
        assert_eq!(
            state.steps, 0,
            "the distance map must reject all visited neighbors"
        );
        assert_eq!(state.poll_ticks, 1024);

        let literal = snap
            .lookup_term(&oxrdf::Literal::new_simple_literal("a literal").into())
            .unwrap();
        let mut limited = Ctx::new(snap);
        let retained = search
            .retained_bytes()
            .saturating_add(state.retained_bytes());
        limited.mem_limit = retained + 128;
        let limited = Arc::new(limited);
        let mut held = super::super::ctx::OwnedCharge::new(&limited, retained).unwrap();
        search.sinks = true;
        let mut frontier = vec![target.0; 1024];
        frontier.push(literal.0);
        let mut edges = Vec::new();
        let result = search.expand(
            &limited,
            &spec,
            &frontier,
            false,
            &mut edges,
            &mut held,
            state.retained_bytes(),
        );
        assert!(matches!(
            result,
            Err(Error::BudgetExceeded(b))
                if b.kind == crate::error::BudgetKind::Memory && b.requested == retained + 8192
        ));
        assert_eq!(edges.capacity(), 0);
        assert_eq!(held.bytes(), retained);
    }

    fn parent(node: u64) -> Parent {
        Parent {
            node,
            p: 0,
            rev: false,
        }
    }

    #[test]
    fn joining_halves_is_checked_against_the_deadline() {
        let edge = |from, to| Edge {
            from,
            to,
            p: 0,
            rev: false,
        };
        let front: Vec<Vec<Edge>> = (0..100).map(|i| vec![edge(0, 10 + i)]).collect();
        let back: Vec<Vec<Edge>> = (0..100).map(|i| vec![edge(1000 + i, 1)]).collect();
        let stop = || Err(Error::Cancelled);
        let mut budget = Budget {
            steps: 0,
            check: &stop,
        };
        let mut n = 0;
        let r = join_halves(&front, edge(5, 6), &back, &mut budget, &mut |_| {
            n += 1;
            Ok(true)
        });
        assert!(r.is_err());
        assert!(n < 4096, "{n} paths joined before the check");
        // without a failing check every pair is joined once
        let go = || Ok(());
        let mut budget = Budget {
            steps: 0,
            check: &go,
        };
        let mut n = 0;
        assert!(
            join_halves(&front, edge(5, 6), &back, &mut budget, &mut |_| {
                n += 1;
                Ok(true)
            })
            .unwrap()
        );
        assert_eq!(n, 10_000);
    }

    #[test]
    fn pruning_work_is_checked_against_the_deadline() {
        // node 1 has the root as its first parent and K + 1 as an extra one, and K + 1
        // reaches 1 only through a chain of first parents, so the search for a way out
        // of K + 1 visits K parents and finds none
        const K: u64 = 3 * 4096;
        let mut t = Tree::new(0, true);
        t.first.insert(1, (parent(0), 0));
        for i in 2..=K + 1 {
            t.first.insert(i, (parent(i - 1), 0));
        }
        t.extra.insert(1, vec![parent(K + 1)]);
        t.order = (1..=K + 1).map(|i| (i, 0)).collect();
        let cost: FxHashMap<u64, Cost> = (1..=K + 1).map(|i| (i, Cost::ZERO)).collect();
        let check = || Err(Error::Timeout);
        let prune = Prune {
            cost: &cost,
            rank: t.ranks(),
        };
        let r = t.paths(1, true, 10, Some(&prune), &check);
        assert!(matches!(r, Err(Error::Timeout)), "{:?}", r.map(|p| p.len()));
    }

    /// A chain `0 -> 1 -> ... -> len` of first parents with every cost zero.
    fn chain(len: u64) -> Tree {
        let mut t = Tree::new(0, true);
        for i in 1..=len {
            t.first.insert(i, (parent(i - 1), 0));
        }
        t.order = (1..=len).map(|i| (i, 0)).collect();
        t
    }

    #[test]
    fn costs_add_up_alike_from_either_end() {
        let w = [1.0, 1.0, 1.0, 1e16];
        let fwd = w.iter().fold(Cost::ZERO, |c, &x| c.add(Cost::of(x)));
        let bwd = w.iter().rev().fold(Cost::ZERO, |c, &x| c.add(Cost::of(x)));
        assert_eq!(fwd, bwd);
        // the unit weights are kept, so this path is dearer than 1e16 + 2
        assert!(fwd > Cost::of(1e16).add(Cost::of(2.0)));
        // halves met in the middle add up to the whole
        let half = Cost::of(1.0).add(Cost::of(1.0));
        assert_eq!(half.add(Cost::of(1.0).add(Cost::of(1e16))), fwd);
        assert_eq!(Cost::of(f64::MAX).add(Cost::of(f64::MAX)), Cost::INFINITE);
        // the rounded parts overflow, the whole does not
        let u = 2f64.powi(969);
        let near = Cost {
            hi: f64::MAX,
            lo: -u,
        };
        assert_eq!(
            near.add(Cost::of(2.0 * u)),
            Cost {
                hi: f64::MAX,
                lo: u
            }
        );
        assert_eq!(Cost::INFINITE.add(Cost::of(1.0)), Cost::INFINITE);
    }

    #[test]
    fn halves_that_share_a_node_are_cut_to_a_simple_path() {
        let e = |from, to| Edge {
            from,
            to,
            p: 9,
            rev: false,
        };
        // 1 → 2 → 3 → 2 → 4, where the halves met at 3 over zero-weight edges
        let cut = without_loops(1, vec![e(1, 2), e(2, 3), e(3, 2), e(2, 4)]);
        assert_eq!(cut, vec![e(1, 2), e(2, 4)]);
        // a loop back through the source
        let cut = without_loops(1, vec![e(1, 2), e(2, 1), e(1, 5)]);
        assert_eq!(cut, vec![e(1, 5)]);
        let plain = vec![e(1, 2), e(2, 3)];
        assert_eq!(without_loops(1, plain.clone()), plain);
    }

    #[test]
    fn single_path_reconstruction_is_linear() {
        const L: u64 = 5000;
        let t = chain(L);
        let cost: FxHashMap<u64, Cost> = (1..=L).map(|i| (i, Cost::ZERO)).collect();
        let calls = Cell::new(0u32);
        let check = || {
            calls.set(calls.get() + 1);
            Ok(())
        };
        // an empty rank map disables the settling-order test, so only the `all` test
        // keeps this reconstruction linear
        let prune = Prune {
            cost: &cost,
            rank: FxHashMap::default(),
        };
        let r = t.paths(L, false, 1, Some(&prune), &check).unwrap();
        assert_eq!(r.len(), 1);
        assert_eq!(r[0].len(), L as usize);
        assert!(calls.get() <= 2, "{} checks", calls.get());
    }

    #[test]
    fn all_shortest_reconstruction_of_a_two_way_zero_weight_chain_is_linear() {
        const L: u64 = 5000;
        let mut t = chain(L);
        for i in 1..L {
            t.extra.insert(i, vec![parent(i + 1)]);
        }
        let cost: FxHashMap<u64, Cost> = (1..=L).map(|i| (i, Cost::ZERO)).collect();
        let calls = Cell::new(0u32);
        let check = || {
            calls.set(calls.get() + 1);
            Ok(())
        };
        let prune = Prune {
            cost: &cost,
            rank: t.ranks(),
        };
        // the walk takes three steps per node, which are a push of the first parent, a
        // skip of the extra parent on the path and a pop, so 3L + 1 steps make three checks
        let r = t.paths(L, true, 10, Some(&prune), &check).unwrap();
        assert_eq!(r.len(), 1);
        assert_eq!(r[0].len(), L as usize);
        assert!(calls.get() <= 3, "{} checks", calls.get());
    }

    #[test]
    fn all_shortest_reconstruction_of_a_one_way_zero_weight_chain_is_linear() {
        const L: u64 = 5000;
        let t = chain(L);
        let cost: FxHashMap<u64, Cost> = (1..=L).map(|i| (i, Cost::ZERO)).collect();
        let calls = Cell::new(0u32);
        let check = || {
            calls.set(calls.get() + 1);
            Ok(())
        };
        let prune = Prune {
            cost: &cost,
            rank: t.ranks(),
        };
        let r = t.paths(L, true, 10, Some(&prune), &check).unwrap();
        assert_eq!(r.len(), 1);
        assert_eq!(r[0].len(), L as usize);
        assert!(calls.get() <= 2, "{} checks", calls.get());
    }

    #[test]
    fn hb_table_growth_after_deletions_admits_old_plus_new_and_tombstone_reuse() {
        use crate::store::{Store, StoreOptions};
        use std::collections::VecDeque;
        use std::sync::Arc;

        let store = Store::in_memory(StoreOptions::default());
        let snap = store.snapshot();
        let ctx_unbounded = Arc::new(Ctx::new(snap.clone()));

        // Build a 256-bucket HbSet (`full_cap = 224`), then churn inserts and deletions
        // without growing until tombstones pull `capacity()` below `full_cap / 2` (under
        // half the buckets). Topping up to `len == capacity` leaves a full table whose
        // next insert must pre-admit old + 2*old yet rehash in place (reusing tombstones)
        // before later inserts grow to 512 buckets.
        let mut charge = super::super::ctx::OwnedCharge::new(&ctx_unbounded, 0).unwrap();
        let mut set: HbSet<u64> = HbSet::default();
        let mut live: VecDeque<u64> = VecDeque::new();
        let mut base = 0u64;
        let mut next_key = 0u64;
        while set.capacity() < 224 {
            let (_, next) = insert_hb_set(&mut set, next_key, &mut charge, base).unwrap();
            base = next;
            live.push_back(next_key);
            next_key += 1;
        }
        let full_cap = set.capacity();
        let alloc_256 = set_bytes(&set);
        assert!(alloc_256 > 0);
        assert_eq!(base, alloc_256);
        let target_len = full_cap / 2;
        let drain_to = full_cap / 5;
        let mut rounds = 0usize;
        loop {
            while set.len() < set.capacity() {
                let (_, next) = insert_hb_set(&mut set, next_key, &mut charge, base).unwrap();
                base = next;
                live.push_back(next_key);
                next_key += 1;
            }
            assert_eq!(set_bytes(&set), alloc_256);
            if set.len() < target_len {
                break;
            }
            rounds += 1;
            assert!(
                rounds < 64,
                "HbSet tombstones did not pull full length below {target_len} (len={}, cap={})",
                set.len(),
                set.capacity()
            );
            while set.len() > drain_to {
                let k = live.pop_front().unwrap();
                assert!(set.remove(&k));
            }
        }
        let full_tombstone_len = set.len();
        assert_eq!(
            set.len(),
            set.capacity(),
            "tombstones must exhaust hashbrown growth_left for HbSet"
        );
        assert!(
            set.capacity() < target_len,
            "tombstone-laden HbSet capacity ({}) must be below half full capacity ({target_len})",
            set.capacity()
        );
        assert_eq!(
            set_bytes(&set),
            alloc_256,
            "physical allocation stays at 256 buckets after deletions"
        );

        // Under a tight budget that covers old + 256-bucket growth (`2 * alloc_256`) but
        // NOT old + 512-bucket growth (`3 * alloc_256`), inserting when `len == cap` must
        // refuse unconditionally BEFORE allocating or modifying the table.
        {
            let mut tight = Ctx::new(snap.clone());
            tight.mem_limit = alloc_256 + alloc_256;
            let tight = Arc::new(tight);
            let mut tight_charge = super::super::ctx::OwnedCharge::new(&tight, alloc_256).unwrap();
            let err = insert_hb_set(&mut set, 10_000_000, &mut tight_charge, alloc_256);
            assert!(
                matches!(err, Err(Error::BudgetExceeded(b)) if b.kind == crate::error::BudgetKind::Memory),
                "expected pre-allocation memory refusal for HbSet: {err:?}"
            );
            assert_eq!(set.len(), full_tombstone_len);
            assert_eq!(set_bytes(&set), alloc_256);
            assert_eq!(tight_charge.bytes(), alloc_256);
        }

        // Now allow old + 2*old and insert keys until allocating growth occurs. The first
        // insert at `len == cap` must reuse tombstones (in-place rehash: physical
        // allocation stays at `alloc_256` while `capacity()` recovers above `len()`)
        // BEFORE later inserts grow to 512 buckets.
        let mut saw_tombstone_reuse = false;
        let mut saw_growth = false;
        let mut steps = 0usize;
        while !saw_growth && steps < 1000 {
            let before_bytes = set_bytes(&set);
            let was_full = set.len() == set.capacity();
            let (_, next) = insert_hb_set(&mut set, next_key, &mut charge, base).unwrap();
            base = next;
            next_key += 1;
            steps += 1;
            let after_bytes = set_bytes(&set);
            assert_eq!(base, after_bytes);
            assert_eq!(charge.bytes(), after_bytes);
            if was_full && after_bytes == before_bytes && set.capacity() > set.len() {
                assert!(
                    !saw_growth,
                    "tombstone reuse should happen before allocating growth"
                );
                saw_tombstone_reuse = true;
            }
            if after_bytes > before_bytes {
                assert!(
                    saw_tombstone_reuse,
                    "expected tombstone reuse before 512-bucket growth for HbSet"
                );
                saw_growth = true;
            }
        }
        assert!(saw_tombstone_reuse, "expected HbSet tombstone reuse");
        assert!(saw_growth, "expected HbSet to grow past 256 buckets");

        // Same shape, unconditional refusal, tombstone reuse, and growth checks for HbMap.
        let mut map: HbMap<u64, u64> = HbMap::default();
        let mut mlive: VecDeque<u64> = VecDeque::new();
        let mut mcharge = super::super::ctx::OwnedCharge::new(&ctx_unbounded, 0).unwrap();
        let mut mbase = 0u64;
        let mut mkey = 0u64;
        while map.capacity() < 224 {
            let (_, next) = insert_hb_map(&mut map, mkey, mkey * 2, &mut mcharge, mbase).unwrap();
            mbase = next;
            mlive.push_back(mkey);
            mkey += 1;
        }
        let m_full_cap = map.capacity();
        let m_alloc_256 = map_bytes(&map);
        assert!(m_alloc_256 > 0);
        assert_eq!(mbase, m_alloc_256);
        let m_target_len = m_full_cap / 2;
        let m_drain_to = m_full_cap / 5;
        let mut mrounds = 0usize;
        loop {
            while map.len() < map.capacity() {
                let (_, next) =
                    insert_hb_map(&mut map, mkey, mkey * 2, &mut mcharge, mbase).unwrap();
                mbase = next;
                mlive.push_back(mkey);
                mkey += 1;
            }
            assert_eq!(map_bytes(&map), m_alloc_256);
            if map.len() < m_target_len {
                break;
            }
            mrounds += 1;
            assert!(
                mrounds < 64,
                "HbMap tombstones did not pull full length below {m_target_len} (len={}, cap={})",
                map.len(),
                map.capacity()
            );
            while map.len() > m_drain_to {
                let k = mlive.pop_front().unwrap();
                assert!(map.remove(&k).is_some());
            }
        }
        let m_full_tombstone_len = map.len();
        assert_eq!(
            map.len(),
            map.capacity(),
            "tombstones must exhaust hashbrown growth_left for HbMap"
        );
        assert!(
            map.capacity() < m_target_len,
            "tombstone-laden HbMap capacity ({}) must be below half full capacity ({m_target_len})",
            map.capacity()
        );
        assert_eq!(map_bytes(&map), m_alloc_256);
        {
            let mut tight = Ctx::new(snap);
            tight.mem_limit = m_alloc_256 + m_alloc_256;
            let tight = Arc::new(tight);
            let mut tight_charge =
                super::super::ctx::OwnedCharge::new(&tight, m_alloc_256).unwrap();
            let err = insert_hb_map(
                &mut map,
                10_000_000,
                20_000_000,
                &mut tight_charge,
                m_alloc_256,
            );
            assert!(
                matches!(err, Err(Error::BudgetExceeded(b)) if b.kind == crate::error::BudgetKind::Memory),
                "expected pre-allocation memory refusal for HbMap: {err:?}"
            );
            assert_eq!(map.len(), m_full_tombstone_len);
            assert_eq!(map_bytes(&map), m_alloc_256);
            assert_eq!(tight_charge.bytes(), m_alloc_256);
        }
        let mut m_saw_tombstone_reuse = false;
        let mut m_saw_growth = false;
        let mut msteps = 0usize;
        while !m_saw_growth && msteps < 1000 {
            let before_bytes = map_bytes(&map);
            let was_full = map.len() == map.capacity();
            let (_, next) = insert_hb_map(&mut map, mkey, mkey * 2, &mut mcharge, mbase).unwrap();
            mbase = next;
            mkey += 1;
            msteps += 1;
            let after_bytes = map_bytes(&map);
            assert_eq!(mbase, after_bytes);
            assert_eq!(mcharge.bytes(), after_bytes);
            if was_full && after_bytes == before_bytes && map.capacity() > map.len() {
                assert!(
                    !m_saw_growth,
                    "tombstone reuse should happen before allocating growth"
                );
                m_saw_tombstone_reuse = true;
            }
            if after_bytes > before_bytes {
                assert!(
                    m_saw_tombstone_reuse,
                    "expected tombstone reuse before 512-bucket growth for HbMap"
                );
                m_saw_growth = true;
            }
        }
        assert!(m_saw_tombstone_reuse, "expected HbMap tombstone reuse");
        assert!(m_saw_growth, "expected HbMap to grow past 256 buckets");
    }

    #[test]
    fn streaming_weight_lookup_handles_many_reifiers_and_many_weights_with_cancellation() {
        use crate::io::Source;
        use crate::store::{Store, StoreOptions};
        use oxrdfio::RdfFormat;
        use std::sync::Arc;
        use std::sync::atomic::Ordering;

        let store = Store::in_memory(StoreOptions::default());
        let mut data = String::from("@prefix ex: <http://example.org/> .\n");
        data.push_str("ex:s ex:p ex:t1 .\nex:s ex:p ex:t2 .\nex:s ex:p ex:t3 .\n");
        // 5,000 distinct reifiers on (ex:s, ex:p, ex:t1) with minimum late in the scan.
        for i in 0..5000 {
            let w = if i == 4200 { "2.5" } else { "10.0" };
            data.push_str(&format!("<< ex:s ex:p ex:t1 ~ ex:r{i} >> ex:w {w} .\n"));
        }
        // Single reifier on (ex:s, ex:p, ex:t2) with 20,000 distinct xsd:integer weights
        // (Tag::Int = 3) plus 0.5 as xsd:decimal (Tag::Decimal = 9 > Tag::Int), placing
        // the minimum weight strictly LAST in Perm::Pso key order.
        data.push_str("<< ex:s ex:p ex:t2 ~ ex:singleR >> ex:w 0.5 .\n");
        for i in 10..20_010 {
            data.push_str(&format!("ex:singleR ex:w {i} .\n"));
        }
        // Small baseline edge (1 reifier, 1 weight) for poll-count comparison.
        data.push_str("<< ex:s ex:p ex:t3 ~ ex:smallR >> ex:w 3.0 .\n");
        store
            .load(&[Source::from_bytes(
                data.into_bytes(),
                RdfFormat::Turtle,
                None,
            )])
            .unwrap();
        let snap = store.snapshot();
        let find = |iri| snap.lookup_iri(iri).unwrap();
        let s = find("http://example.org/s");
        let p = find("http://example.org/p");
        let t1 = find("http://example.org/t1");
        let t2 = find("http://example.org/t2");
        let t3 = find("http://example.org/t3");
        let w = find("http://example.org/w");
        let single_r = find("http://example.org/singleR");
        let reifies = find(RDF_REIFIES);
        assert_eq!(
            snap.count(Perm::Pso, &[w.0, single_r.0]).unwrap(),
            20_001,
            "ex:singleR must hold 20,001 distinct weight quads"
        );
        // Confirm the minimum 0.5 is genuinely the last key in Perm::Pso order.
        let mut last_weight_id = 0u64;
        let mut weight_count = 0usize;
        snap.scan_between_cols(
            Perm::Pso,
            pad(&[w.0, single_r.0], 0),
            pad(&[w.0, single_r.0], u64::MAX),
            1 << 2,
            |c| {
                match c {
                    Chunk::Block(b, s_i, e_i) => {
                        weight_count += e_i - s_i;
                        last_weight_id = b.cols[2][e_i - 1];
                    }
                    Chunk::Row(k) => {
                        weight_count += 1;
                        last_weight_id = k[2];
                    }
                }
                Ok(true)
            },
        )
        .unwrap();
        assert_eq!(weight_count, 20_001);
        let ctx = Arc::new(Ctx::new(snap));
        assert_eq!(
            ctx.value(Id(last_weight_id))
                .as_ref()
                .and_then(super::super::value::approx_f64),
            Some(0.5),
            "minimum weight must be last in PSO key order"
        );
        let spec = PathSearchSpec {
            algorithm: Algorithm::Shortest,
            source: PathEnd::Const(s),
            target: PathEnd::Const(t1),
            predicates: Some(vec![p.0]),
            directions: vec![Direction::Forward],
            direction: Direction::Forward,
            exclude: Vec::new(),
            max_degree: None,
            edge_pattern: None,
            min_len: 1,
            max_len: None,
            k: 1,
            limit: None,
            max_visited: 100_000,
            weight: Some(Some(w.0)),
            default_weight: 1.0,
            reifies: Some(reifies.0),
            path_index: None,
            edge_index: None,
            edge_s: None,
            edge_p: None,
            edge_o: None,
            edge: None,
            length: None,
            cost: None,
            graph: GraphFilter::Default,
        };
        // Warm ctx term cache for the endpoints so CHECKS measures scan polling only.
        let _ = (
            ctx.term(s),
            ctx.term(p),
            ctx.term(t1),
            ctx.term(t2),
            ctx.term(t3),
        );
        let c0 = super::super::ctx::CHECKS.with(Cell::get);
        assert_eq!(
            lookup_weight_charged(&ctx, &spec, Some(w.0), (s.0, p.0, t3.0)).unwrap(),
            3.0
        );
        let checks_small = super::super::ctx::CHECKS.with(Cell::get) - c0;

        let c1 = super::super::ctx::CHECKS.with(Cell::get);
        assert_eq!(
            lookup_weight_charged(&ctx, &spec, Some(w.0), (s.0, p.0, t1.0)).unwrap(),
            2.5
        );
        let checks_reifiers = super::super::ctx::CHECKS.with(Cell::get) - c1;
        assert!(
            checks_reifiers >= 5000,
            "outer reifier scan must poll during work, got only {checks_reifiers} checks"
        );

        let c2 = super::super::ctx::CHECKS.with(Cell::get);
        assert_eq!(
            lookup_weight_charged(&ctx, &spec, Some(w.0), (s.0, p.0, t2.0)).unwrap(),
            0.5
        );
        let checks_weights = super::super::ctx::CHECKS.with(Cell::get) - c2;
        assert!(
            checks_weights >= checks_small + 3,
            "inner 20,001-weight scan must poll mid-scan (got {checks_weights} vs baseline {checks_small})"
        );

        ctx.cancel.store(true, Ordering::Relaxed);
        assert!(matches!(
            lookup_weight_charged(&ctx, &spec, Some(w.0), (s.0, p.0, t1.0)),
            Err(Error::Cancelled)
        ));
        assert!(matches!(
            lookup_weight_charged(&ctx, &spec, Some(w.0), (s.0, p.0, t2.0)),
            Err(Error::Cancelled)
        ));
    }

    #[test]
    fn r2_deleted_adjacency_scan_and_inner_loops_poll_after_start() {
        use crate::io::Source;
        use crate::store::{Store, StoreOptions};
        use oxrdfio::RdfFormat;
        use std::sync::Arc;
        use std::sync::atomic::Ordering;

        // 1. Deleted base edges in a delta: both non-sweep and sweep branches of
        //    `scan_lane_charged` must poll via `scan_between_cols_checked` even when
        //    the visible-row callback is never invoked.
        let store = Store::in_memory(StoreOptions::default());
        let mut data = String::new();
        for i in 0..5000 {
            data.push_str(&format!("<urn:del:s> <urn:del:p> <urn:del:o:{i}> .\n"));
        }
        // Extra distinct IRIs (without <urn:del:p> edges) to build a >=64 sweep frontier.
        for i in 0..512 {
            data.push_str(&format!("<urn:del:f:{i}> <urn:del:tag> 1 .\n"));
        }
        store
            .load(&[Source::from_bytes(
                data.into_bytes(),
                RdfFormat::Turtle,
                None,
            )])
            .unwrap();
        let snap0 = store.snapshot();
        let s = snap0.lookup_iri("urn:del:s").unwrap();
        let p = snap0.lookup_iri("urn:del:p").unwrap();
        let mut tx = store.write();
        for i in 0..5000 {
            let o = snap0.lookup_iri(&format!("urn:del:o:{i}")).unwrap();
            assert!(tx.delete([s, p, o, Id::DEFAULT_GRAPH]).unwrap());
        }
        tx.commit().unwrap();
        let snap = store.snapshot();
        let ctx = Arc::new(Ctx::new(snap.clone()));
        let spec = PathSearchSpec {
            algorithm: Algorithm::Shortest,
            source: PathEnd::Const(s),
            target: PathEnd::Var(0),
            predicates: Some(vec![p.0]),
            directions: vec![Direction::Forward],
            direction: Direction::Forward,
            exclude: Vec::new(),
            max_degree: None,
            edge_pattern: None,
            min_len: 1,
            max_len: None,
            k: 1,
            limit: None,
            max_visited: 100_000,
            weight: None,
            default_weight: 1.0,
            reifies: None,
            path_index: None,
            edge_index: None,
            edge_s: None,
            edge_p: None,
            edge_o: None,
            edge: None,
            length: None,
            cost: None,
            graph: GraphFilter::Default,
        };
        let lane = Lane {
            perm: Perm::Pso,
            p: Some(p.0),
            subject_side: true,
            pattern: false,
        };

        // Non-sweep branch: 0 visible rows, >= 5,000 delta deletion checks.
        let mut sweeps = 0u64;
        let mut out: Vec<(u64, Step)> = Vec::new();
        let before = super::super::ctx::CHECKS.with(Cell::get);
        scan_lane_charged(
            false,
            &mut sweeps,
            &ctx,
            &spec,
            &lane,
            &[s.0],
            false,
            false,
            &mut out,
            usize::MAX,
            None::<(&mut super::super::ctx::OwnedCharge, u64)>,
        )
        .unwrap();
        let non_sweep_checks = super::super::ctx::CHECKS.with(Cell::get) - before;
        assert!(out.is_empty());
        assert_eq!(sweeps, 0);
        assert!(
            non_sweep_checks >= 5000,
            "non-sweep scan must poll across 5,000 deleted delta rows, got {non_sweep_checks}"
        );

        // Sweep branch: 513-node sorted frontier forces sweep; 0 visible rows, >= 5,000 checks.
        let mut frontier: Vec<u64> = (0..512)
            .map(|i| snap.lookup_iri(&format!("urn:del:f:{i}")).unwrap().0)
            .collect();
        frontier.push(s.0);
        frontier.sort_unstable();
        frontier.dedup();
        let before_sweep = super::super::ctx::CHECKS.with(Cell::get);
        scan_lane_charged(
            false,
            &mut sweeps,
            &ctx,
            &spec,
            &lane,
            &frontier,
            false,
            false,
            &mut out,
            usize::MAX,
            None::<(&mut super::super::ctx::OwnedCharge, u64)>,
        )
        .unwrap();
        let sweep_checks = super::super::ctx::CHECKS.with(Cell::get) - before_sweep;
        assert!(out.is_empty());
        assert_eq!(sweeps, 1);
        assert!(
            sweep_checks >= 5000,
            "sweep scan must poll across 5,000 deleted delta rows, got {sweep_checks}"
        );

        ctx.cancel.store(true, Ordering::Relaxed);
        assert!(matches!(
            scan_lane_charged(
                false,
                &mut sweeps,
                &ctx,
                &spec,
                &lane,
                &[s.0],
                false,
                false,
                &mut out,
                usize::MAX,
                None::<(&mut super::super::ctx::OwnedCharge, u64)>,
            ),
            Err(Error::Cancelled)
        ));
        assert!(matches!(
            scan_lane_charged(
                false,
                &mut sweeps,
                &ctx,
                &spec,
                &lane,
                &frontier,
                false,
                false,
                &mut out,
                usize::MAX,
                None::<(&mut super::super::ctx::OwnedCharge, u64)>,
            ),
            Err(Error::Cancelled)
        ));

        // 2. Wide grouped emission column copy/join/reset/append loops poll inside `Reader::next`.
        let gstore = Store::in_memory(StoreOptions::default());
        gstore
            .load(&[Source::from_bytes(
                b"<urn:g:s> <urn:g:p> <urn:g:t> .\n".to_vec(),
                RdfFormat::Turtle,
                None,
            )])
            .unwrap();
        let gsnap = gstore.snapshot();
        let gs = gsnap.lookup_iri("urn:g:s").unwrap();
        let gp = gsnap.lookup_iri("urn:g:p").unwrap();
        let gt = gsnap.lookup_iri("urn:g:t").unwrap();
        let gspec = PathSearchSpec {
            algorithm: Algorithm::Shortest,
            source: PathEnd::Var(0),
            target: PathEnd::Var(2),
            predicates: Some(vec![gp.0]),
            directions: vec![Direction::Forward],
            direction: Direction::Forward,
            exclude: Vec::new(),
            max_degree: None,
            edge_pattern: None,
            min_len: 1,
            max_len: None,
            k: 1,
            limit: None,
            max_visited: 100_000,
            weight: None,
            default_weight: 1.0,
            reifies: None,
            path_index: None,
            edge_index: None,
            edge_s: None,
            edge_p: None,
            edge_o: None,
            edge: None,
            length: Some(1),
            cost: None,
            graph: GraphFilter::Default,
        };
        let run_grouped = |width: usize, n_rows: usize, length_val: i64| -> (usize, Table, u64) {
            let gctx = Arc::new(Ctx::new(gsnap.clone()));
            let vars: Vec<VarId> = (0..width as u32).collect();
            let mut in_table = Table::new(vars.clone());
            let mut row = vec![Id::from_i64(7).unwrap(); width];
            row[0] = gs;
            row[1] = Id::from_i64(length_val).unwrap();
            row[2] = gt;
            for _ in 0..n_rows {
                in_table.push_row(&row);
            }
            let in_charge = super::super::ctx::OwnedCharge::new(&gctx, 0).unwrap();
            let charge = super::super::ctx::OwnedCharge::new(&gctx, 0).unwrap();
            let mut reader = Reader::open(
                &gctx,
                &gspec,
                &vars,
                Some((in_table, in_charge)),
                ChargedPatternEdges::new(),
                charge,
            )
            .unwrap();
            let mut out_table = Table::new(vars.clone());
            let c_before = super::super::ctx::CHECKS.with(Cell::get);
            let done = reader
                .next(&gctx, &gspec, &vars, &mut out_table, 64)
                .unwrap();
            let checks = super::super::ctx::CHECKS.with(Cell::get) - c_before;
            assert!(done);
            let paths = reader
                .counters(&gspec)
                .get("paths")
                .and_then(serde_json::Value::as_u64)
                .unwrap_or(0);
            (checks, out_table, paths)
        };

        // 2a. Incompatible length join: verify the path reached emission (`paths == 1`)
        //     and wide column loops add polls over the narrow baseline.
        let (narrow_incompat, narrow_out, narrow_paths) = run_grouped(4, 50, 99);
        let (wide_incompat, wide_out, wide_paths) = run_grouped(200, 50, 99);
        assert_eq!(narrow_paths, 1);
        assert_eq!(wide_paths, 1);
        assert!(narrow_out.is_empty());
        assert!(wide_out.is_empty());
        // 50 rows * (200 input copy + 200 row_buf copy + 200 join) = 30,000 ops -> >= 7 extra polls.
        assert!(
            wide_incompat >= narrow_incompat + 6,
            "wide grouped incompatible join must poll more than narrow baseline: wide={wide_incompat}, narrow={narrow_incompat}"
        );

        // 2b. Wide compatible emission: exercises path_row reset, input copy, row_buf copy,
        //     join, and checked row append across 5,000 columns.
        let (narrow_compat, narrow_compat_out, narrow_compat_paths) = run_grouped(4, 4, 1);
        let (wide_compat, wide_compat_out, wide_compat_paths) = run_grouped(5000, 4, 1);
        assert_eq!(narrow_compat_paths, 1);
        assert_eq!(wide_compat_paths, 1);
        assert_eq!(narrow_compat_out.len(), 4);
        assert_eq!(wide_compat_out.len(), 4);
        for r in 0..4 {
            assert_eq!(wide_compat_out.get(r, 0), gs);
            assert_eq!(wide_compat_out.get(r, 1), Id::from_i64(1).unwrap());
            assert_eq!(wide_compat_out.get(r, 2), gt);
            assert_eq!(wide_compat_out.get(r, 4999), Id::from_i64(7).unwrap());
        }
        // 1 * 5,000 reset + 4 * (5,000 copy + 5,000 row_buf + 5,000 join + 5,000 append) = 85,000 ops -> >= 20 polls.
        assert!(
            wide_compat >= narrow_compat + 18,
            "wide grouped compatible emission must poll across reset/copy/join/append: wide={wide_compat}, narrow={narrow_compat}"
        );
    }

    #[test]
    fn checked_sort_pattern_finish_and_cached_exact_cost_poll_cancellation() {
        use crate::store::{Store, StoreOptions};
        use std::sync::Arc;
        use std::sync::atomic::Ordering;

        let store = Store::in_memory(StoreOptions::default());
        let snap = store.snapshot();
        let ctx = Arc::new(Ctx::new(snap));

        // 1. `sort_dedup_u64_checked` on >4096 elements produces sorted unique output.
        let mut big: Vec<u64> = (0..10_000u64).rev().map(|i| i / 2).collect();
        sort_dedup_u64_checked(&mut big, &ctx).unwrap();
        assert_eq!(big.len(), 5000);
        assert!(big.windows(2).all(|w| w[0] < w[1]));

        // 2. `ChargedPatternEdges::finish` on a single 10,000-entry adjacency list cancels.
        let mut pat = ChargedPatternEdges::new();
        pat.by_start
            .insert(1, (0..10_000u64).rev().collect::<Vec<u64>>());
        ctx.cancel.store(true, Ordering::Relaxed);
        assert!(matches!(pat.finish(&ctx), Err(Error::Cancelled)));

        // 3. `SearchContext::exact_cost` on 5,000 cached edges polls cancellation even
        //    when every weight lookup hits the cache.
        let ctx_cost = Arc::new(Ctx::new(store.snapshot()));
        let mut held = super::super::ctx::OwnedCharge::new(&ctx_cost, 0).unwrap();
        let spec = PathSearchSpec {
            algorithm: Algorithm::Shortest,
            source: PathEnd::Const(Id(1)),
            target: PathEnd::Const(Id(2)),
            predicates: None,
            directions: Vec::new(),
            direction: Direction::Forward,
            exclude: Vec::new(),
            max_degree: None,
            edge_pattern: None,
            min_len: 1,
            max_len: None,
            k: 1,
            limit: None,
            max_visited: 100_000,
            weight: Some(Some(10)),
            default_weight: 1.0,
            reifies: Some(11),
            path_index: None,
            edge_index: None,
            edge_s: None,
            edge_p: None,
            edge_o: None,
            edge: None,
            length: None,
            cost: None,
            graph: GraphFilter::Default,
        };
        let mut search =
            SearchContext::open(&ctx_cost, &spec, ChargedPatternEdges::new(), &mut held).unwrap();
        let edge = Edge {
            from: 1,
            to: 2,
            p: 3,
            rev: false,
        };
        let base = search.retained_bytes();
        let _ = insert_hb_map(&mut search.weights, edge.triple(), 2.0, &mut held, base).unwrap();
        let edges = vec![edge; 5000];
        ctx_cost.cancel.store(true, Ordering::Relaxed);
        assert!(matches!(
            search.exact_cost(&ctx_cost, &spec, &edges, |x| x, &mut held, 0),
            Err(Error::Cancelled)
        ));
    }

    #[test]
    fn r3_weighted_finalization_pruning_polls_and_cancels() {
        use crate::io::Source;
        use crate::store::{Store, StoreOptions};
        use oxrdfio::RdfFormat;
        use std::sync::Arc;
        use std::sync::atomic::Ordering;

        let store = Store::in_memory(StoreOptions::default());
        let ctx = Arc::new(Ctx::new(store.snapshot()));

        // 1. Direct `retain_hb_map_checked`: polls across both mostly-removed and mostly-kept
        //    maps and stops immediately on cancellation without scanning the remaining table.
        let mut mostly_removed: HbMap<u64, u64> = HbMap::default();
        for k in 0..10_000u64 {
            mostly_removed.insert(k, k);
        }
        let c0 = super::super::ctx::CHECKS.with(Cell::get);
        retain_hb_map_checked(&mut mostly_removed, &ctx, |&k| k < 2).unwrap();
        let removed_checks = super::super::ctx::CHECKS.with(Cell::get) - c0;
        assert_eq!(mostly_removed.len(), 2);
        assert!(mostly_removed.contains_key(&0) && mostly_removed.contains_key(&1));
        assert!(
            removed_checks >= 3,
            "pruning 10,000 entries must poll every 4096 entries, got {removed_checks}"
        );

        let mut mostly_kept: HbMap<u64, u64> = HbMap::default();
        for k in 0..10_000u64 {
            mostly_kept.insert(k, k);
        }
        let mut pred_calls = 0usize;
        let cancel_ctx = Arc::new(Ctx::new(store.snapshot()));
        let err = retain_hb_map_checked(&mut mostly_kept, &cancel_ctx, |&k| {
            pred_calls += 1;
            if pred_calls == 100 {
                cancel_ctx.cancel.store(true, Ordering::Relaxed);
            }
            k < 9999
        });
        assert!(matches!(err, Err(Error::Cancelled)));
        assert!(
            pred_calls <= 4096,
            "mid-scan cancellation must stop at the 4096 boundary instead of scanning all 10,000 entries (visited {pred_calls})"
        );

        // 2. High-degree weighted source with cheap target: `run_dijkstra` and
        //    `run_dijkstra_pair_all` (`all = true`) both prune large tentative tables with
        //    polling and leave only settled entries.
        let gstore = Store::in_memory(StoreOptions::default());
        let mut nt = String::new();
        nt.push_str("<urn:p:s> <urn:p:e> <urn:p:t> .\n");
        for i in 0..8_192 {
            nt.push_str(&format!("<urn:p:s> <urn:p:e> <urn:p:d:{i}> .\n"));
        }
        gstore
            .load(&[Source::from_bytes(
                nt.into_bytes(),
                RdfFormat::NTriples,
                None,
            )])
            .unwrap();
        let snap = gstore.snapshot();
        let s = snap.lookup_iri("urn:p:s").unwrap();
        let p = snap.lookup_iri("urn:p:e").unwrap();
        let t = snap.lookup_iri("urn:p:t").unwrap();
        let spec = PathSearchSpec {
            algorithm: Algorithm::AllShortest,
            source: PathEnd::Const(s),
            target: PathEnd::Const(t),
            predicates: Some(vec![p.0]),
            directions: vec![Direction::Forward],
            direction: Direction::Forward,
            exclude: Vec::new(),
            max_degree: None,
            edge_pattern: None,
            min_len: 1,
            max_len: None,
            k: 1,
            limit: None,
            max_visited: 100_000,
            weight: Some(Some(10)),
            default_weight: 10.0,
            reifies: Some(11),
            path_index: None,
            edge_index: None,
            edge_s: None,
            edge_p: None,
            edge_o: None,
            edge: None,
            length: None,
            cost: None,
            graph: GraphFilter::Default,
        };

        // Single-source `run_dijkstra` with cheap target `t` (weight 1.0 vs default 10.0 for 8,192 decoys).
        let run_ss = |cancel_on_poll: Option<usize>| -> (Result<(usize, usize)>, usize) {
            let gctx = Arc::new(Ctx::new(snap.clone()));
            let mut charge = super::super::ctx::OwnedCharge::new(&gctx, 0).unwrap();
            let mut search =
                SearchContext::open(&gctx, &spec, ChargedPatternEdges::new(), &mut charge).unwrap();
            let base = search.retained_bytes();
            let _ = insert_hb_map(&mut search.weights, (s.0, p.0, t.0), 1.0, &mut charge, base)
                .unwrap();
            for i in 0..8_192 {
                let d = snap.lookup_iri(&format!("urn:p:d:{i}")).unwrap();
                let b = search.retained_bytes();
                let _ = insert_hb_map(&mut search.weights, (s.0, p.0, d.0), 10.0, &mut charge, b)
                    .unwrap();
            }
            let targets = [t.0];
            let polls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
            let polls_hook = polls.clone();
            let cancel_hook = gctx.cancel.clone();
            let _guard = test_hooks::on_prune_poll(move || {
                let n = polls_hook.fetch_add(1, Ordering::Relaxed) + 1;
                if cancel_on_poll == Some(n) {
                    cancel_hook.store(true, Ordering::Release);
                }
            });
            let res = search
                .run_dijkstra(
                    &gctx,
                    &spec,
                    s.0,
                    true,
                    Some(&targets),
                    true,
                    false,
                    None,
                    None,
                    s.0,
                    &mut charge,
                    0,
                )
                .map(|(tree, dist)| (tree.first.len(), dist.len()));
            drop(_guard);
            (res, polls.load(Ordering::Relaxed))
        };
        let (ss_res, ss_prune_polls) = run_ss(None);
        let (first_len, dist_len) = ss_res.unwrap();
        assert_eq!(
            first_len, 1,
            "only settled target `t` remains in tree.first"
        );
        assert_eq!(dist_len, 2, "only settled `s` and `t` remain in dist");
        // 8,193 entries in `tree.first` (3 polls at 0, 4096, 8192) + 8,194 entries in `dist`
        // (3 polls at 0, 4096, 8192) = 6 finalization pruning checkpoints.
        assert_eq!(
            ss_prune_polls, 6,
            "run_dijkstra must poll 3 times in tree.first and 3 times in dist during finalization pruning"
        );
        for (cancel_at, label) in [(1, "tree.first"), (4, "dist")] {
            let (res, observed) = run_ss(Some(cancel_at));
            assert!(
                matches!(res, Err(Error::Cancelled)),
                "run_dijkstra must cancel at finalization pruning checkpoint #{cancel_at} ({label}): {res:?}"
            );
            assert_eq!(
                observed, cancel_at,
                "run_dijkstra must abort immediately at finalization pruning checkpoint #{cancel_at} ({label})"
            );
        }

        // Bidirectional `run_dijkstra_pair_all` (`all = true`) on the same high-degree source.
        let run_bidi = |cancel_on_poll: Option<usize>| -> (Result<(usize, usize)>, usize) {
            let gctx = Arc::new(Ctx::new(snap.clone()));
            let mut charge = super::super::ctx::OwnedCharge::new(&gctx, 0).unwrap();
            let mut search =
                SearchContext::open(&gctx, &spec, ChargedPatternEdges::new(), &mut charge).unwrap();
            let base = search.retained_bytes();
            let _ = insert_hb_map(&mut search.weights, (s.0, p.0, t.0), 1.0, &mut charge, base)
                .unwrap();
            for i in 0..8_192 {
                let d = snap.lookup_iri(&format!("urn:p:d:{i}")).unwrap();
                let b = search.retained_bytes();
                let _ = insert_hb_map(&mut search.weights, (s.0, p.0, d.0), 10.0, &mut charge, b)
                    .unwrap();
            }
            let polls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
            let polls_hook = polls.clone();
            let cancel_hook = gctx.cancel.clone();
            let _guard = test_hooks::on_prune_poll(move || {
                let n = polls_hook.fetch_add(1, Ordering::Relaxed) + 1;
                if cancel_on_poll == Some(n) {
                    cancel_hook.store(true, Ordering::Release);
                }
            });
            let res = search
                .run_dijkstra_pair_all(&gctx, &spec, s.0, t.0, true, false, &mut charge, 0)
                .map(|outcome| match outcome {
                    BidiWeightedOutcome::Stream(job) => {
                        (job.trees[0].first.len(), job.dist[0].len())
                    }
                    _ => panic!("expected BidiWeightedOutcome::Stream for tight weight ratio"),
                });
            drop(_guard);
            (res, polls.load(Ordering::Relaxed))
        };
        let (bidi_res, bidi_prune_polls) = run_bidi(None);
        let (bidi_first_len, bidi_dist_len) = bidi_res.unwrap();
        // Only `s` settled on the forward side before `heaps[1]` emptied and
        // `tops[0] + tops[1] > best` stopped the search, so all 8,193 tentative
        // forward entries (`t` plus 8,192 decoys) were pruned away.
        assert_eq!(bidi_first_len, 0);
        assert_eq!(bidi_dist_len, 1);
        // 8,193 entries in `trees[0].first` (3 polls) + 8,194 entries in `dist[0]` (3 polls),
        // plus any backward tentative entries pruned on side 1.
        assert!(
            bidi_prune_polls >= 6,
            "run_dijkstra_pair_all must poll at least 6 times across trees[0].first and dist[0] during finalization pruning, got {bidi_prune_polls}"
        );
        for (cancel_at, label) in [(1, "trees[0].first"), (4, "dist[0]")] {
            let (res, observed) = run_bidi(Some(cancel_at));
            assert!(
                matches!(res, Err(Error::Cancelled)),
                "run_dijkstra_pair_all must cancel at finalization pruning checkpoint #{cancel_at} ({label}): {res:?}"
            );
            assert_eq!(
                observed, cancel_at,
                "run_dijkstra_pair_all must abort immediately at finalization pruning checkpoint #{cancel_at} ({label})"
            );
        }
    }

    #[test]
    fn r3_wide_row_reset_and_append_poll_and_rollback() {
        use crate::store::{Store, StoreOptions};
        use std::sync::Arc;
        use std::sync::atomic::Ordering;

        let store = Store::in_memory(StoreOptions::default());
        let ctx = Arc::new(Ctx::new(store.snapshot()));
        let width = 9_000usize;

        // 1. `fill_undef_checked` polls across wide buffers and cancels.
        let mut buf = vec![Id::from_i64(42).unwrap(); width];
        let mut ticks = 0usize;
        let c0 = super::super::ctx::CHECKS.with(Cell::get);
        fill_undef_checked(&mut buf, &ctx, &mut ticks).unwrap();
        let fill_checks = super::super::ctx::CHECKS.with(Cell::get) - c0;
        assert!(
            fill_checks >= 2,
            "fill_undef_checked must poll on width={width}, got {fill_checks}"
        );
        assert!(buf.iter().all(|id| id.is_undef()));

        // 2. `copy_slice_checked` polls across wide buffers.
        let src = vec![Id::from_i64(7).unwrap(); width];
        let mut dst = vec![Id::UNDEF; width];
        ticks = 0;
        let c1 = super::super::ctx::CHECKS.with(Cell::get);
        copy_slice_checked(&mut dst, &src, &ctx, &mut ticks).unwrap();
        let copy_checks = super::super::ctx::CHECKS.with(Cell::get) - c1;
        assert!(
            copy_checks >= 2,
            "copy_slice_checked must poll on width={width}, got {copy_checks}"
        );
        assert_eq!(dst, src);

        // 3. `push_row_checked` polls on wide rows and rolls back partial columns on error.
        let vars: Vec<VarId> = (0..width as u32).collect();
        let mut table = Table::new(vars);
        let row0 = vec![Id::from_i64(11).unwrap(); width];
        ticks = 0;
        let c2 = super::super::ctx::CHECKS.with(Cell::get);
        push_row_checked(&mut table, &row0, &ctx, &mut ticks).unwrap();
        let push_checks = super::super::ctx::CHECKS.with(Cell::get) - c2;
        assert!(
            push_checks >= 2,
            "push_row_checked must poll on width={width}, got {push_checks}"
        );
        assert_eq!(table.len(), 1);

        // Start 6 ticks before the 4096 boundary with cancel=true so columns 0..5 are appended
        // before column 5 triggers `ctx.check()?` and must roll back cleanly.
        let row1 = vec![Id::from_i64(99).unwrap(); width];
        ctx.cancel.store(true, Ordering::Relaxed);
        let mut near_boundary = 4090usize;
        assert!(matches!(
            push_row_checked(&mut table, &row1, &ctx, &mut near_boundary),
            Err(Error::Cancelled)
        ));
        assert_eq!(table.len(), 1);
        assert!(
            table
                .cols
                .iter()
                .all(|col| col.len() == 1 && col[0] == Id::from_i64(11).unwrap()),
            "mid-row cancellation must roll back every partially appended column"
        );
        ticks = 0;
        assert!(matches!(
            fill_undef_checked(&mut buf, &ctx, &mut ticks),
            Err(Error::Cancelled)
        ));
    }

    #[test]
    fn r3_yen_prefix_comparison_and_stale_dijkstra_pops_poll() {
        use crate::io::Source;
        use crate::store::{Store, StoreOptions};
        use oxrdfio::RdfFormat;
        use std::sync::Arc;
        use std::sync::atomic::Ordering;

        // 1. Long Yen prefix comparison polls across chunks and cancels immediately.
        let store = Store::in_memory(StoreOptions::default());
        let ctx = Arc::new(Ctx::new(store.snapshot()));
        let prefix_len = 10_000usize;
        let mut p0: Vec<Edge> = (0..prefix_len as u64)
            .map(|k| Edge {
                from: k + 1,
                to: k + 2,
                p: 1,
                rev: false,
            })
            .collect();
        let mut p1 = p0.clone();
        let tail0 = Edge {
            from: prefix_len as u64 + 1,
            to: 900_001,
            p: 1,
            rev: false,
        };
        let tail1 = Edge {
            from: prefix_len as u64 + 1,
            to: 900_002,
            p: 1,
            rev: false,
        };
        p0.push(tail0);
        p1.push(tail1);
        let paths = vec![(p0, Cost::of(1.0)), (p1, Cost::of(2.0))];
        let mut banned: HbSet<Edge> = HbSet::default();
        let mut charge = super::super::ctx::OwnedCharge::new(&ctx, 0).unwrap();
        let c0 = super::super::ctx::CHECKS.with(Cell::get);
        let _ =
            ban_yen_prefix_edges(&ctx, &paths, prefix_len, &mut banned, &mut charge, 0).unwrap();
        let yen_checks = super::super::ctx::CHECKS.with(Cell::get) - c0;
        assert_eq!(banned.len(), 2);
        assert!(banned.contains(&tail0) && banned.contains(&tail1));
        assert!(
            yen_checks >= 4,
            "comparing two 10,000-edge Yen prefixes must poll >= 4 times, got {yen_checks}"
        );

        ctx.cancel.store(true, Ordering::Relaxed);
        let mut banned2: HbSet<Edge> = HbSet::default();
        let c1 = super::super::ctx::CHECKS.with(Cell::get);
        assert!(matches!(
            ban_yen_prefix_edges(&ctx, &paths, prefix_len, &mut banned2, &mut charge, 0),
            Err(Error::Cancelled)
        ));
        let cancel_checks = super::super::ctx::CHECKS.with(Cell::get) - c1;
        assert_eq!(
            cancel_checks, 1,
            "cancelled Yen prefix comparison must abort on the first 4096-edge chunk"
        );

        // 2. Stale Dijkstra heap pops across `run_dijkstra`, `run_dijkstra_pair`, and
        //    `run_dijkstra_pair_all`: compare decreasing-weight relaxations (~8,850 stale
        //    pops on the forward side, 212 settled forward nodes) against an
        //    identical-topology baseline with 0 stale pops and identical shortest cost.
        //    9,500 heavy dead-end incoming edges on `t` keep `heaps[1].len() > heaps[0].len()`
        //    throughout both runs so bidirectional search expands the exact same nodes in
        //    both `base` and `stale`.
        let n_u = 60usize;
        let n_v = 150usize;
        let n_dead = 9_500usize;
        let gstore = Store::in_memory(StoreOptions::default());
        let mut nt = String::new();
        for i in 0..n_u {
            nt.push_str(&format!("<urn:st:s> <urn:st:p> <urn:st:u:{i}> .\n"));
            for j in 0..n_v {
                nt.push_str(&format!("<urn:st:u:{i}> <urn:st:p> <urn:st:v:{j}> .\n"));
            }
        }
        for j in 0..n_v {
            nt.push_str(&format!("<urn:st:v:{j}> <urn:st:p> <urn:st:m> .\n"));
        }
        nt.push_str("<urn:st:m> <urn:st:p> <urn:st:t> .\n");
        for k in 0..n_dead {
            nt.push_str(&format!("<urn:st:dead:{k}> <urn:st:p> <urn:st:t> .\n"));
        }
        gstore
            .load(&[Source::from_bytes(
                nt.into_bytes(),
                RdfFormat::NTriples,
                None,
            )])
            .unwrap();
        let snap = gstore.snapshot();
        let s = snap.lookup_iri("urn:st:s").unwrap();
        let p = snap.lookup_iri("urn:st:p").unwrap();
        let m = snap.lookup_iri("urn:st:m").unwrap();
        let t = snap.lookup_iri("urn:st:t").unwrap();
        let us: Vec<u64> = (0..n_u)
            .map(|i| snap.lookup_iri(&format!("urn:st:u:{i}")).unwrap().0)
            .collect();
        let vs: Vec<u64> = (0..n_v)
            .map(|j| snap.lookup_iri(&format!("urn:st:v:{j}")).unwrap().0)
            .collect();
        let deads: Vec<u64> = (0..n_dead)
            .map(|k| snap.lookup_iri(&format!("urn:st:dead:{k}")).unwrap().0)
            .collect();

        let spec = PathSearchSpec {
            algorithm: Algorithm::Shortest,
            source: PathEnd::Const(s),
            target: PathEnd::Const(t),
            predicates: Some(vec![p.0]),
            directions: vec![Direction::Forward],
            direction: Direction::Forward,
            exclude: Vec::new(),
            max_degree: None,
            edge_pattern: None,
            min_len: 1,
            max_len: None,
            k: 1,
            limit: None,
            max_visited: 100_000,
            weight: Some(Some(10)),
            default_weight: 1.0,
            reifies: Some(11),
            path_index: None,
            edge_index: None,
            edge_s: None,
            edge_p: None,
            edge_o: None,
            edge: None,
            length: None,
            cost: None,
            graph: GraphFilter::Default,
        };

        let build_search =
            |stale: bool| -> (Arc<Ctx>, SearchContext, super::super::ctx::OwnedCharge) {
                let gctx = Arc::new(Ctx::new(snap.clone()));
                let mut charge = super::super::ctx::OwnedCharge::new(&gctx, 0).unwrap();
                let mut search =
                    SearchContext::open(&gctx, &spec, ChargedPatternEdges::new(), &mut charge)
                        .unwrap();
                let mut put_w = |from: u64, to: u64, w: f64| {
                    let b = search.retained_bytes();
                    let _ = insert_hb_map(&mut search.weights, (from, p.0, to), w, &mut charge, b)
                        .unwrap();
                };
                // For `stale == true`: `s->u_i` has weight `i+1`, `u_i->v_j` has weight `500 - 2*i`
                // so total distance to `v_j` strictly decreases from 501 down to 442 (`59*150 = 8,850`
                // stale entries). For `stale == false`: `u_i->v_j` has weight `441 + 2*i`, giving
                // the exact same optimal distance `442` via `u_0` with 0 stale heap entries.
                for (i, &u) in us.iter().enumerate() {
                    let w_end = (i + 1) as f64;
                    let w_mid = if stale {
                        (500 - 2 * i) as f64
                    } else {
                        (441 + 2 * i) as f64
                    };
                    put_w(s.0, u, w_end);
                    for &v in &vs {
                        put_w(u, v, w_mid);
                    }
                }
                for &v in &vs {
                    put_w(v, m.0, 200.0);
                }
                put_w(m.0, t.0, 1_000.0);
                for &d in &deads {
                    put_w(d, t.0, 2_000.0);
                }
                (gctx, search, charge)
            };

        // Warm the snapshot block cache on both directions before comparing `CHECKS`
        // so cold block decodes do not inflate the baseline measurement.
        {
            let (ctx_w, mut search_w, mut ch_w) = build_search(false);
            let _ = search_w
                .run_dijkstra_pair(&ctx_w, &spec, s.0, t.0, None, None, &mut ch_w, 0)
                .unwrap();
        }

        // 2a. Single-source `run_dijkstra`
        let (ctx_b, mut search_b, mut ch_b) = build_search(false);
        let cb = super::super::ctx::CHECKS.with(Cell::get);
        let (_, dist_b) = search_b
            .run_dijkstra(
                &ctx_b,
                &spec,
                s.0,
                true,
                Some(&[m.0]),
                false,
                false,
                None,
                None,
                s.0,
                &mut ch_b,
                0,
            )
            .unwrap();
        let ss_base_checks = super::super::ctx::CHECKS.with(Cell::get) - cb;

        let (ctx_s, mut search_s, mut ch_s) = build_search(true);
        let cs = super::super::ctx::CHECKS.with(Cell::get);
        let (_, dist_s) = search_s
            .run_dijkstra(
                &ctx_s,
                &spec,
                s.0,
                true,
                Some(&[m.0]),
                false,
                false,
                None,
                None,
                s.0,
                &mut ch_s,
                0,
            )
            .unwrap();
        let ss_stale_checks = super::super::ctx::CHECKS.with(Cell::get) - cs;
        assert_eq!(dist_b.get(&m.0).unwrap().hi, 642.0);
        assert_eq!(dist_s.get(&m.0).unwrap().hi, 642.0);
        assert_eq!(search_b.visited, search_s.visited);
        assert!(
            ss_stale_checks >= ss_base_checks + 2,
            "run_dijkstra stale heap pops must add polls: stale={ss_stale_checks}, base={ss_base_checks}"
        );

        // 2b. Bidirectional `run_dijkstra_pair`
        let (ctx_b, mut search_b, mut ch_b) = build_search(false);
        let cb = super::super::ctx::CHECKS.with(Cell::get);
        let (_, cost_b) = search_b
            .run_dijkstra_pair(&ctx_b, &spec, s.0, t.0, None, None, &mut ch_b, 0)
            .unwrap()
            .unwrap();
        let pair_base_checks = super::super::ctx::CHECKS.with(Cell::get) - cb;

        let (ctx_s, mut search_s, mut ch_s) = build_search(true);
        let cs = super::super::ctx::CHECKS.with(Cell::get);
        let (_, cost_s) = search_s
            .run_dijkstra_pair(&ctx_s, &spec, s.0, t.0, None, None, &mut ch_s, 0)
            .unwrap()
            .unwrap();
        let pair_stale_checks = super::super::ctx::CHECKS.with(Cell::get) - cs;
        assert_eq!(cost_b, 1_642.0);
        assert_eq!(cost_s, 1_642.0);
        assert_eq!(search_b.visited, search_s.visited);
        assert!(
            pair_stale_checks >= pair_base_checks + 2,
            "run_dijkstra_pair stale heap pops must add polls: stale={pair_stale_checks}, base={pair_base_checks}"
        );

        // 2c. Bidirectional `run_dijkstra_pair_all` (`all = false` and `all = true`)
        for all in [false, true] {
            let (ctx_b, mut search_b, mut ch_b) = build_search(false);
            let cb = super::super::ctx::CHECKS.with(Cell::get);
            let out_b = search_b
                .run_dijkstra_pair_all(&ctx_b, &spec, s.0, t.0, all, false, &mut ch_b, 0)
                .unwrap();
            let all_base_checks = super::super::ctx::CHECKS.with(Cell::get) - cb;

            let (ctx_s, mut search_s, mut ch_s) = build_search(true);
            let cs = super::super::ctx::CHECKS.with(Cell::get);
            let out_s = search_s
                .run_dijkstra_pair_all(&ctx_s, &spec, s.0, t.0, all, false, &mut ch_s, 0)
                .unwrap();
            let all_stale_checks = super::super::ctx::CHECKS.with(Cell::get) - cs;
            assert!(!matches!(
                out_b,
                BidiWeightedOutcome::Empty | BidiWeightedOutcome::FallbackSingleSource
            ));
            assert!(!matches!(
                out_s,
                BidiWeightedOutcome::Empty | BidiWeightedOutcome::FallbackSingleSource
            ));
            assert_eq!(search_b.visited, search_s.visited);
            assert!(
                all_stale_checks >= all_base_checks + 2,
                "run_dijkstra_pair_all(all={all}) stale heap pops must add polls: stale={all_stale_checks}, base={all_base_checks}"
            );
        }
    }

    #[test]
    fn pareto_frontier_pruning_and_lookup() {
        let mut v = Vec::new();
        assert!(pareto_insert(&mut v, 3, Cost::of(10.0)));
        assert_eq!(v, vec![(3, Cost::of(10.0))]);

        // Dominated: longer and equal/more expensive.
        assert!(!pareto_insert(&mut v, 3, Cost::of(10.0)));
        assert!(!pareto_insert(&mut v, 3, Cost::of(12.0)));
        assert!(!pareto_insert(&mut v, 5, Cost::of(10.0)));
        assert!(!pareto_insert(&mut v, 5, Cost::of(15.0)));

        // Shorter and more expensive: kept on frontier.
        assert!(pareto_insert(&mut v, 1, Cost::of(20.0)));
        assert_eq!(v, vec![(1, Cost::of(20.0)), (3, Cost::of(10.0))]);

        // Longer and strictly cheaper: kept on frontier.
        assert!(pareto_insert(&mut v, 5, Cost::of(4.0)));
        assert_eq!(
            v,
            vec![(1, Cost::of(20.0)), (3, Cost::of(10.0)), (5, Cost::of(4.0))]
        );

        // Lookup cheapest within length budget.
        assert_eq!(pareto_best_within(&v, 0), None);
        assert_eq!(pareto_best_within(&v, 1), Some((1, Cost::of(20.0))));
        assert_eq!(pareto_best_within(&v, 2), Some((1, Cost::of(20.0))));
        assert_eq!(pareto_best_within(&v, 3), Some((3, Cost::of(10.0))));
        assert_eq!(pareto_best_within(&v, 4), Some((3, Cost::of(10.0))));
        assert_eq!(pareto_best_within(&v, 5), Some((5, Cost::of(4.0))));
        assert_eq!(pareto_best_within(&v, 99), Some((5, Cost::of(4.0))));

        // Shorter and cheaper than several longer entries prunes them all.
        assert!(pareto_insert(&mut v, 2, Cost::of(4.0)));
        assert_eq!(v, vec![(1, Cost::of(20.0)), (2, Cost::of(4.0))]);

        // Same-length improvement replaces in place and prunes dominated tail.
        assert!(pareto_insert(&mut v, 4, Cost::of(2.0)));
        assert!(pareto_insert(&mut v, 2, Cost::of(1.5)));
        assert_eq!(v, vec![(1, Cost::of(20.0)), (2, Cost::of(1.5))]);
    }

    #[test]
    fn opt_cost_lower_bound_handles_empty_and_infinite() {
        assert_eq!(opt_cost_min(None, None), None);
        assert_eq!(opt_cost_min(Some(Cost::of(3.0)), None), Some(Cost::of(3.0)));
        assert_eq!(opt_cost_min(None, Some(Cost::of(2.0))), Some(Cost::of(2.0)));
        assert_eq!(
            opt_cost_min(Some(Cost::of(3.0)), Some(Cost::of(2.0))),
            Some(Cost::of(2.0))
        );
        assert_eq!(opt_cost_add(None, Some(Cost::of(1.0))), None);
        assert_eq!(
            opt_cost_add(Some(Cost::of(1.0)), Some(Cost::INFINITE)),
            Some(Cost::INFINITE)
        );
        assert_eq!(
            opt_cost_min(Some(Cost::INFINITE), Some(Cost::of(5.0))),
            Some(Cost::of(5.0))
        );
    }

    /// Regression for Sol r4 P3: eager direct reconstruction in `ReverseWide` must transfer
    /// the edge buffer credit into `eng.charge` instead of charging `len * size_of::<Edge>()`
    /// on both the tree charge and `eng.charge` at the same time.
    #[test]
    fn eager_reverse_wide_direct_reconstruction_does_not_double_charge_edge_buffer() {
        use crate::io::Source;
        use crate::sparql::pool::QueryPool;
        use crate::sparql::{QueryOptions, query};
        use crate::store::{Store, StoreOptions};
        use oxrdfio::RdfFormat;

        let chain_len = 400usize;
        let mut ttl = String::from(
            "@prefix ex: <http://example.org/> .\n\
             ex:c1 ex:p ex:t {| ex:w 1.0e-12 |} .\n\
             ex:b ex:p ex:t {| ex:w 1.0e0 |} .\n\
             ex:b ex:p ex:x {| ex:w 5.0e-1 |} .\n\
             ex:x ex:p ex:t {| ex:w 5.0e-1 |} .\n",
        );
        for i in 2..=chain_len {
            ttl += &format!("ex:c{i} ex:p ex:c{} {{| ex:w 0.0e0 |}} .\n", i - 1);
        }
        let store = Store::in_memory(StoreOptions::default());
        store
            .load(&[Source::from_bytes(
                ttl.into_bytes(),
                RdfFormat::Turtle,
                None,
            )])
            .unwrap();
        let snap = store.snapshot();

        let p = snap.lookup_iri("http://example.org/p").unwrap();
        let w = snap.lookup_iri("http://example.org/w").unwrap();
        let t = snap.lookup_iri("http://example.org/t").unwrap();
        let reifies = snap
            .lookup_iri("http://www.w3.org/1999/02/22-rdf-syntax-ns#reifies")
            .map(|id| id.0);
        let spec = PathSearchSpec {
            algorithm: Algorithm::Shortest,
            source: PathEnd::Var(0),
            target: PathEnd::Const(t),
            predicates: Some(vec![p.0]),
            directions: vec![Direction::Forward],
            direction: Direction::Forward,
            exclude: Vec::new(),
            max_degree: None,
            edge_pattern: None,
            min_len: 1,
            max_len: Some(chain_len as u32),
            k: 1,
            limit: Some(chain_len),
            max_visited: 100_000,
            weight: Some(Some(w.0)),
            default_weight: 1.0,
            reifies,
            path_index: None,
            edge_index: None,
            edge_s: None,
            edge_p: None,
            edge_o: None,
            edge: None,
            length: None,
            cost: None,
            graph: GraphFilter::Default,
        };

        // Measure the exact retained tree charge and weight cache charge directly so the
        // tight budget below depends on no magic constants.
        let measure_ctx = Ctx::new(snap.clone());
        let eng = Engine::new(&measure_ctx, &spec, None).unwrap();
        let (outcome, tree_charge) = eng.dijkstra_bounded(t.0, false, chain_len as u32).unwrap();
        assert!(
            matches!(outcome, BoundedTreeOutcome::ReverseWide { .. }),
            "fixture must exercise the ReverseWide direct-reconstruction arm"
        );
        let retained_tree = tree_charge.bytes();
        let weight_cache = eng.charge.bytes();
        let retained_results: u64 = (1..=chain_len as u64)
            .map(|k| k * (size_of::<Edge>() as u64) + 64)
            .sum();
        drop(tree_charge);
        drop(eng);
        drop(measure_ctx);

        // Allow retained tree + weight cache + all 400 retained results + 4 KiB headroom.
        // The 400th path's edge buffer is `400 * 32 = 12_800` bytes, so double-charging it
        // overflows this budget while single-charging via credit transfer fits cleanly.
        let tight_limit = retained_tree
            .saturating_add(weight_cache)
            .saturating_add(retained_results)
            .saturating_add(4 * 1024);

        let mut fast_ctx = Ctx::new(snap.clone());
        fast_ctx.mem_limit = tight_limit;
        let (tbl, _) = super::run(&fast_ctx, &spec, None, None, &[0])
            .expect("eager ReverseWide direct reconstruction must fit without double-charging");
        assert_eq!(tbl.len(), chain_len);
        assert!(
            fast_ctx.mem_peak() <= tight_limit,
            "fast peak {} exceeded tight limit {tight_limit}",
            fast_ctx.mem_peak()
        );
        drop(tbl);
        drop(fast_ctx);

        // Also exercise the pooled ledger path end-to-end through `query`.
        let q = format!(
            "PREFIX ex: <http://example.org/>\n\
             PREFIX path: <urn:x-sparkles:path#>\n\
             SELECT ?src WHERE {{ SERVICE path:search {{ [] path:source ?src ; \
              path:target ex:t ; path:predicate ex:p ; path:weight ex:w ; \
              path:maxLength {chain_len} ; path:limit {chain_len} }} }}"
        );
        let pool = QueryPool::new(tight_limit.saturating_add(256 * 1024));
        let pooled_opts = QueryOptions {
            max_memory_bytes: Some(tight_limit.saturating_add(64 * 1024)),
            pool: Some(pool.clone()),
            no_cache: true,
            ..Default::default()
        };
        let pooled_res = query(snap, &q, &pooled_opts)
            .expect("pooled eager ReverseWide direct reconstruction must succeed");
        drop(pooled_res);
        assert_eq!(pool.granted(), 0);
    }
}
