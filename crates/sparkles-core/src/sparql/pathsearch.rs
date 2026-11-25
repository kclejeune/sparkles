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
//! Dijkstra's algorithm with weights, from both ends for the cheapest path of one pair and
//! for the spur searches of Yen's algorithm for the k shortest paths, and a
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
use rustc_hash::{FxHashMap, FxHashSet};
use spargebra::algebra::GraphPattern;
use spargebra::term::{NamedNodePattern, TermPattern, TriplePattern};
use sparkles_budget::sizes::{table_bytes as physical_table_bytes, table_grown, vec_grown};
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
            .term(*id)
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
        let v = self.visited.get() + n;
        self.visited.set(v);
        self.visited_total.set(self.visited_total.get() + n);
        if v > self.spec.max_visited {
            return Err(bad(format!(
                "a search visited more than {} nodes; raise path:maxVisited, lower path:maxLength or add predicates",
                self.spec.max_visited
            )));
        }
        self.charge.add(n * NODE_BYTES)
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
        self.weights.borrow_mut().insert(key, x);
        Ok(x)
    }

    fn lookup_weight(&self, w: Option<u64>, (s, p, o): (u64, u64, u64)) -> Result<f64> {
        let default = self.spec.default_weight;
        let (Some(w), Some(reifies)) = (w, self.spec.reifies) else {
            return Ok(default);
        };
        let ctx = self.ctx;
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
        let graph = &self.spec.graph;
        let mut reifiers = Vec::new();
        for k in ctx.snap.scan_keys(Perm::Pos, &[reifies, tt.0])? {
            if graph.accepts(k[3]) {
                reifiers.push(k[2]);
            }
        }
        let mut best: Option<f64> = None;
        for r in reifiers {
            for k in ctx.snap.scan_keys(Perm::Pso, &[w, r])? {
                if !graph.accepts(k[3]) {
                    continue;
                }
                let v = ctx
                    .value(Id(k[2]))
                    .as_ref()
                    .and_then(super::value::approx_f64);
                match v {
                    Some(v) if v >= 0.0 && v.is_finite() => {
                        best = Some(best.map_or(v, |b: f64| b.min(v)));
                    }
                    _ => {
                        let t = ctx
                            .term(Id(k[2]))
                            .map_or_else(|| "?".to_string(), |t| t.to_string());
                        return Err(bad(format!("the weight {t} is not a non-negative number")));
                    }
                }
            }
        }
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

    /// The shortest path from `from` to `to` (forward) that avoids the banned nodes and
    /// edges, of at most `budget` edges without weights: Yen's spur search.
    fn spur(
        &self,
        from: u64,
        to: u64,
        virt: u64,
        banned_nodes: &FxHashSet<u64>,
        banned_edges: &FxHashSet<Edge>,
        budget: Option<u32>,
    ) -> Result<Option<(Vec<Edge>, f64)>> {
        if self.spec.weighted() && to != VIRT && from != to {
            return self.dijkstra_pair(from, to, Some(banned_nodes), Some(banned_edges));
        }
        if self.spec.weighted() {
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
                .map(|p| (p, c)));
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
                (p, c)
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
        let budget = if self.spec.weighted() { None } else { max };
        let Some((first, _)) = self.spur(s, tt, s, &none_n, &none_e, budget)? else {
            return Ok(());
        };
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
            for i in 0..cur.len() {
                let root = &cur[..i];
                let mut banned_edges = FxHashSet::default();
                for (p, _) in &a {
                    if p.len() > i && p[..i] == *root {
                        banned_edges.insert(p[i]);
                    }
                }
                let banned_nodes: FxHashSet<u64> = nodes[..i].iter().copied().collect();
                let budget = match budget {
                    Some(m) if m <= i as u32 => continue,
                    Some(m) => Some(m - i as u32),
                    None => None,
                };
                if let Some((sp, _)) =
                    self.spur(nodes[i], tt, s, &banned_nodes, &banned_edges, budget)?
                {
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
                   fwd: bool|
     -> Result<bool> {
        if !eng.take() {
            return Ok(false);
        }
        let cost = eng.path_cost(&edges)?;
        let i = found.len() as u32;
        let len = len.unwrap_or(edges.len());
        let edges = if need_edges { edges } else { Vec::new() };
        eng.charge.add(edges.len() as u64 * 32 + 64)?;
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
    // the shortest modes with weights and a length limit enumerate in cost order
    let yen_shortest = weighted
        && spec.max_len.is_some()
        && matches!(spec.algorithm, Algorithm::Shortest | Algorithm::AllShortest);
    'sources: for (&s, targets) in &forward {
        if eng.full() {
            break;
        }
        let tset: FxHashSet<u64> = targets.set.iter().copied().collect();
        want_pairs.set(!targets.set.is_empty());
        want_sources.set(targets.any);
        match spec.algorithm {
            Algorithm::Shortest | Algorithm::AllShortest if yen_shortest => {
                if targets.any {
                    return Err(bad(
                        "path:maxLength with path:weight needs a bound target in the shortest modes",
                    ));
                }
                for &t in &targets.set {
                    eng.begin();
                    if spec.min_len == 0 && s == t {
                        if !add(&mut found, s, t, Vec::new(), None, true)? {
                            break 'sources;
                        }
                        continue;
                    }
                    let mut best: Option<Cost> = None;
                    let mut out = Vec::new();
                    eng.yen(s, t, |p, c| {
                        if p.len() as u32 > spec.max_len.unwrap() || (p.len() as u32) < spec.min_len
                        {
                            return Ok(true);
                        }
                        match best {
                            None => best = Some(c),
                            Some(b) if c > b => return Ok(false),
                            Some(_) => {}
                        }
                        out.push(p.to_vec());
                        Ok(all)
                    })?;
                    for p in out {
                        if !add(&mut found, s, t, p, None, true)? {
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
                        add(&mut found, s, t, Vec::new(), None, true)?;
                        continue;
                    }
                    for p in eng.bidirectional(s, t, all, if all { cap(&eng) } else { 1 })? {
                        if !add(&mut found, s, t, p, None, true)? {
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
                        if !add(&mut found, s, t, Vec::new(), None, true)? {
                            break 'sources;
                        }
                        continue;
                    }
                    let mut stop = false;
                    let answered = eng.dijkstra_pair_all(s, t, all, &mut |p| {
                        let more = add(&mut found, s, t, p, None, true)?;
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
                        && !add(&mut found, s, t, p, None, true)?
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
                    if !add(&mut found, s, s, Vec::new(), None, true)? {
                        break 'sources;
                    }
                }
                for m in ends {
                    let end = if m == VIRT { s } else { m };
                    if by_level && !weighted {
                        if let Some(&(_, level)) = tree.first.get(&m)
                            && !add(&mut found, s, end, Vec::new(), Some(level as usize), true)?
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
                        if !add(&mut found, s, end, p, None, true)? {
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
                    eng.begin();
                    let mut n = 0usize;
                    if spec.min_len == 0 && s == t {
                        if !add(&mut found, s, t, Vec::new(), None, true)? {
                            break 'sources;
                        }
                        n += 1;
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
                        if !add(&mut found, s, t, p, None, true)? {
                            break 'sources;
                        }
                    }
                }
            }
            Algorithm::All => {
                eng.begin();
                let mut stop = false;
                eng.all_paths(s, true, (!targets.any).then_some(&tset), &mut |p, end| {
                    if !add(&mut found, s, end, p.to_vec(), None, true)? {
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
                if yen_shortest {
                    return Err(bad(
                        "path:maxLength with path:weight needs a bound source in the shortest modes",
                    ));
                }
                let (tree, cost) = if weighted {
                    let (tree, cost) = eng.dijkstra(t, false, None, all, true, None, None, t)?;
                    (tree, Some(cost))
                } else {
                    let tree = eng.bfs(t, false, None, all, true)?;
                    (tree, None)
                };
                let prune = eng.prune(&tree, cost.as_ref(), all)?;
                if spec.min_len == 0 && !add(&mut found, t, t, Vec::new(), None, false)? {
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
                        if !add(&mut found, start, t, p, None, false)? {
                            break 'targets;
                        }
                    }
                }
            }
            Algorithm::All => {
                let mut stop = false;
                eng.all_paths(t, false, None, &mut |p, start| {
                    if !add(&mut found, start, t, p.to_vec(), None, false)? {
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
    let mut triple_term = |(s, p, o): (u64, u64, u64)| -> Id {
        *terms.entry((s, p, o)).or_insert_with(|| {
            let parts = (ctx.term(Id(s)), ctx.term(Id(p)), ctx.term(Id(o)));
            let (Some(s), Some(Term::NamedNode(p)), Some(o)) = parts else {
                return Id::UNDEF;
            };
            let s = match s {
                Term::NamedNode(n) => oxrdf::NamedOrBlankNode::NamedNode(n),
                Term::BlankNode(b) => oxrdf::NamedOrBlankNode::BlankNode(b),
                _ => return Id::UNDEF,
            };
            ctx.intern_term(&Term::Triple(Box::new(oxrdf::Triple::new(s, p, o))))
        })
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
                        set(edge_c, triple_term((s, p, o)), &mut r);
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

/// Whether a path search without input or an edge pattern and without a limit runs as a
/// resumable cursor reader (item 829, group 5):
/// - `path:shortest` or `path:kShortest` between two constant endpoints (bounded by 1 or k paths)
/// - `path:all` with at least one constant endpoint (streamed DFS under a memory charge).
pub(crate) fn stream_eligible(spec: &PathSearchSpec) -> bool {
    if spec.limit.is_some() || spec.edge_pattern.is_some() {
        return false;
    }
    match spec.algorithm {
        Algorithm::Shortest | Algorithm::KShortest => {
            matches!(spec.source, PathEnd::Const(_)) && matches!(spec.target, PathEnd::Const(_))
        }
        Algorithm::All => {
            matches!(spec.source, PathEnd::Const(_)) || matches!(spec.target, PathEnd::Const(_))
        }
        Algorithm::AllShortest => false,
    }
}

/// Owned, `Send` resumable reader for eligible `SERVICE path:search` leaves (item 829).
pub(crate) struct Reader {
    spec: PathSearchSpec,
    vars: Vec<VarId>,
    mode: ReaderMode,
    current: Option<(u32, Found, usize)>,
    next_path_id: u32,
    terms: FxHashMap<(u64, u64, u64), Id>,
    cols: RowCols,
    charge: super::ctx::OwnedCharge,
    counters: serde_json::Map<String, serde_json::Value>,
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

enum ReaderMode {
    /// Bounded list of paths (`Shortest` / `KShortest` between two constants), served
    /// edge-by-edge without materializing the full edge table at once.
    Bounded(std::collections::VecDeque<Found>),
    /// Resumable DFS for `Algorithm::All` with at least one constant end.
    All(Box<AllPathsState>),
}

/// Owned, `Send` DFS state for `Algorithm::All` without input or edge pattern.
struct AllPathsState {
    dedup: bool,
    succ: Vec<Side>,
    pred: Vec<Side>,
    sinks: bool,
    degree: DegreeCount,
    forward: bool,
    root: u64,
    target: Option<u64>,
    close: bool,
    max_len: u32,
    min_len: u32,
    pending_zero: bool,
    pending_push: Option<u64>,
    dist: Option<FxHashMap<u64, u32>>,
    cache: FxHashMap<(u64, bool), std::sync::Arc<[Step]>>,
    cache_step_bytes: u64,
    weights: FxHashMap<(u64, u64, u64), f64>,
    hubs: FxHashMap<u64, bool>,
    roots: Vec<u64>,
    on_path: FxHashSet<u64>,
    stack: Vec<(u64, std::sync::Arc<[Step]>, usize)>,
    edges: Vec<Edge>,
    visited: u64,
    visited_total: u64,
    searches: u64,
    sweeps: u64,
    skipped_hubs: u64,
    paths_found: u64,
    steps: u64,
    poll_ticks: u64,
    done: bool,
}

#[inline]
fn next_cap(len: usize, cap: usize, extra: usize) -> usize {
    if len.saturating_add(extra) <= cap {
        cap
    } else {
        cap.saturating_mul(2).max(len.saturating_add(extra)).max(8)
    }
}

fn table_bytes<T>(capacity: usize) -> u64 {
    physical_table_bytes(capacity, size_of::<T>()) as u64
}

/// Additional bytes while the old and new tables are both live.
fn table_growth<T>(capacity: usize, len: usize) -> u64 {
    let new = table_grown(capacity, len, 1);
    if new == capacity {
        0
    } else {
        table_bytes::<T>(new)
    }
}

impl AllPathsState {
    fn open(
        ctx: &Ctx,
        spec: &PathSearchSpec,
        charge: &mut super::ctx::OwnedCharge,
    ) -> Result<Self> {
        let (forward, root, target) = match (&spec.source, &spec.target) {
            (PathEnd::Const(s), PathEnd::Const(t)) => (true, s.0, Some(t.0)),
            (PathEnd::Const(s), PathEnd::Var(_)) => (true, s.0, None),
            (PathEnd::Var(_), PathEnd::Const(t)) => (false, t.0, None),
            _ => unreachable!("stream_eligible requires at least one constant end"),
        };
        let lane = |p: Option<u64>, subject_side: bool| Lane {
            perm: match (p.is_some(), subject_side) {
                (true, true) => Perm::Pso,
                (true, false) => Perm::Pos,
                (false, true) => Perm::Spo,
                (false, false) => Perm::Osp,
            },
            p,
            subject_side,
            pattern: false,
        };
        let groups: Vec<(Lane, Lane, Direction)> = match &spec.predicates {
            Some(ps) => ps
                .iter()
                .zip(&spec.directions)
                .map(|(&p, &d)| (lane(Some(p), true), lane(Some(p), false), d))
                .collect(),
            None => vec![(lane(None, true), lane(None, false), spec.direction)],
        };
        let side = |lane: Lane, rev: bool, skip_self: bool| Side {
            lane,
            rev,
            skip_self,
        };
        let (mut succ, mut pred) = (Vec::new(), Vec::new());
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
        let sinks =
            succ.iter().any(|s| !s.lane.subject_side) || pred.iter().any(|s| s.lane.subject_side);
        let degree = match spec.max_degree {
            None => DegreeCount::Scan,
            Some(_) => degree_count(ctx, &spec.graph)?,
        };
        let max_len = spec.max_len.unwrap_or(0);
        let min_len = spec.min_len;
        let close = target.is_none_or(|t| t == root);
        let mut st = Self {
            dedup: spec.graph.multi(),
            succ,
            pred,
            sinks,
            degree,
            forward,
            root,
            target,
            close,
            max_len,
            min_len,
            pending_zero: min_len == 0 && close,
            pending_push: None,
            dist: None,
            cache: FxHashMap::default(),
            cache_step_bytes: 0,
            weights: FxHashMap::default(),
            hubs: FxHashMap::default(),
            roots: Vec::new(),
            on_path: FxHashSet::default(),
            stack: Vec::new(),
            edges: Vec::new(),
            visited: 0,
            visited_total: 0,
            searches: 1,
            sweeps: 0,
            skipped_hubs: 0,
            paths_found: 0,
            steps: 0,
            poll_ticks: 0,
            done: false,
        };
        if max_len == 0 {
            st.done = !st.pending_zero;
            return Ok(st);
        }
        if let Some(t) = target {
            let mut d: FxHashMap<u64, u32> = FxHashMap::default();
            let step_bytes = size_of::<(u64, Step)>() as u64;
            let node_bytes = size_of::<u64>() as u64;
            charge.resize(
                st.retained_bytes()
                    .saturating_add(table_growth::<(u64, u32)>(0, 0))
                    .saturating_add(2 * node_bytes),
            )?;
            d.insert(t, 0);
            let mut frontier = vec![t];
            st.set_roots(spec, &frontier);
            let mut level = 0;
            while !frontier.is_empty() && level < max_len {
                let d_bytes = table_bytes::<(u64, u32)>(d.capacity());
                let frontier_bytes = frontier.capacity() as u64 * node_bytes;
                let base_bfs = d_bytes.saturating_add(frontier_bytes);
                let mut found = Vec::new();
                st.expand(ctx, spec, &frontier, !forward, &mut found, charge, base_bfs)?;
                level += 1;
                let found_bytes = found.capacity() as u64 * step_bytes;
                let mut next = Vec::new();
                let mut added_nodes = 0u64;
                for (edge_idx, (_, step)) in found.into_iter().enumerate() {
                    if edge_idx.is_multiple_of(4096) {
                        ctx.check()?;
                    }
                    if !d.contains_key(&step.node) {
                        let d_growth = table_growth::<(u64, u32)>(d.capacity(), d.len());
                        let next_growth = if next.len() == next.capacity() {
                            next_cap(next.len(), next.capacity(), 1) as u64 * node_bytes
                        } else {
                            0
                        };
                        if d_growth != 0 || next_growth != 0 {
                            charge.resize(
                                st.retained_bytes()
                                    .saturating_add(found_bytes)
                                    .saturating_add(frontier_bytes)
                                    .saturating_add(table_bytes::<(u64, u32)>(d.capacity()))
                                    .saturating_add(d_growth)
                                    .saturating_add(next.capacity() as u64 * node_bytes)
                                    .saturating_add(next_growth),
                            )?;
                        }
                        d.insert(step.node, level);
                        next.push(step.node);
                        added_nodes += 1;
                        if added_nodes.is_multiple_of(1024) {
                            st.visit(spec, 1024)?;
                        }
                    }
                }
                let rem = added_nodes % 1024;
                if rem > 0 {
                    st.visit(spec, rem)?;
                }
                next.sort_unstable();
                frontier = next;
                let extra = table_bytes::<(u64, u32)>(d.capacity())
                    .saturating_add(frontier.capacity() as u64 * node_bytes);
                charge.resize(st.retained_bytes().saturating_add(extra))?;
            }
            st.dist = Some(d);
            charge.resize(st.retained_bytes())?;
        }
        charge.resize(
            st.retained_bytes()
                .saturating_add(table_growth::<u64>(st.on_path.capacity(), st.on_path.len()))
                .saturating_add(if st.roots.capacity() == 0 {
                    size_of::<u64>() as u64
                } else {
                    0
                }),
        )?;
        st.set_roots(spec, &[root]);
        st.on_path.insert(root);
        let root_ns = st.neighbours(ctx, spec, root, forward, charge, 0)?;
        if st.stack.len() == st.stack.capacity() {
            let add = (next_cap(st.stack.len(), st.stack.capacity(), 1) - st.stack.capacity())
                as u64
                * size_of::<(u64, std::sync::Arc<[Step]>, usize)>() as u64;
            charge.resize(st.retained_bytes().saturating_add(add))?;
        }
        st.stack.push((root, root_ns, 0));
        charge.resize(st.retained_bytes())?;
        Ok(st)
    }

    fn retained_bytes(&self) -> u64 {
        let dist = self
            .dist
            .as_ref()
            .map_or(0, |d| table_bytes::<(u64, u32)>(d.capacity()));
        let cache = self
            .cache_step_bytes
            .saturating_add(table_bytes::<((u64, bool), std::sync::Arc<[Step]>)>(
                self.cache.capacity(),
            ));
        let stack =
            self.stack.capacity() as u64 * size_of::<(u64, std::sync::Arc<[Step]>, usize)>() as u64;
        let on_path = table_bytes::<u64>(self.on_path.capacity());
        let edges = self.edges.capacity() as u64 * size_of::<Edge>() as u64;
        let hubs = table_bytes::<(u64, bool)>(self.hubs.capacity());
        let weights = table_bytes::<((u64, u64, u64), f64)>(self.weights.capacity());
        dist.saturating_add(cache)
            .saturating_add(stack)
            .saturating_add(on_path)
            .saturating_add(edges)
            .saturating_add(hubs)
            .saturating_add(weights)
            .saturating_add(self.roots.capacity() as u64 * size_of::<u64>() as u64)
    }

    fn set_roots(&mut self, spec: &PathSearchSpec, roots: &[u64]) {
        if spec.max_degree.is_some() || self.sinks {
            self.roots.clear();
            self.roots.reserve_exact(roots.len());
            self.roots.extend_from_slice(roots);
        }
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
        extra_bytes: u64,
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
        if self.hubs.len() == self.hubs.capacity() {
            let add = table_growth::<(u64, bool)>(self.hubs.capacity(), self.hubs.len());
            charge.resize(
                extra_bytes
                    .saturating_add(self.retained_bytes())
                    .saturating_add(add),
            )?;
        }
        self.hubs.insert(x, h);
        charge.resize(extra_bytes.saturating_add(self.retained_bytes()))?;
        Ok(h)
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
        let key = |lane: &Lane, x: u64| -> Vec<u64> { lane.p.into_iter().chain([x]).collect() };
        let mut upper = 0u64;
        for s in &self.succ {
            upper += snap.count(s.lane.perm, &key(&s.lane, x))?;
        }
        if upper <= limit {
            return Ok(false);
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
        let sides = self.succ.clone();
        for s in &sides {
            self.scan_lane(
                ctx,
                spec,
                &s.lane,
                &[x],
                s.rev,
                s.skip_self,
                &mut out,
                cap,
                &mut |_| Ok(()),
            )?;
            if out.0 >= cap {
                return Ok(true);
            }
        }
        Ok(false)
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

    #[allow(clippy::too_many_arguments)]
    fn expand(
        &mut self,
        ctx: &Ctx,
        spec: &PathSearchSpec,
        frontier: &[u64],
        succ: bool,
        out: &mut Vec<(u64, Step)>,
        charge: &mut super::ctx::OwnedCharge,
        extra_bytes: u64,
    ) -> Result<()> {
        if spec.max_degree.is_none() && !self.sinks {
            return self.expand_all(ctx, spec, frontier, succ, out, charge, extra_bytes);
        }
        let mut kept: Option<Vec<u64>> = None;
        let mut filtered_bytes = 0;
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
                extra_bytes.saturating_add(filtered_bytes),
            )? {
                self.skipped_hubs += 1;
                true
            } else {
                false
            };
            if skip && kept.is_none() {
                // The first rejection needs a copy of the accepted prefix.
                filtered_bytes = i as u64 * size_of::<u64>() as u64;
                charge.resize(
                    self.retained_bytes()
                        .saturating_add(extra_bytes)
                        .saturating_add(filtered_bytes),
                )?;
                let mut filtered = Vec::with_capacity(i);
                filtered.extend_from_slice(&frontier[..i]);
                kept = Some(filtered);
            } else if !skip && let Some(filtered) = &mut kept {
                if filtered.len() == filtered.capacity() {
                    let elem = size_of::<u64>();
                    let next = vec_grown(filtered.capacity(), filtered.len(), 1, elem);
                    let next_bytes = next as u64 * elem as u64;
                    charge.resize(
                        self.retained_bytes()
                            .saturating_add(extra_bytes)
                            .saturating_add(filtered_bytes)
                            .saturating_add(next_bytes),
                    )?;
                    filtered_bytes = next_bytes;
                }
                filtered.push(x);
            }
        }
        match kept {
            None => self.expand_all(ctx, spec, frontier, succ, out, charge, extra_bytes),
            Some(k) => {
                let k_bytes = k.capacity() as u64 * size_of::<u64>() as u64;
                self.expand_all(
                    ctx,
                    spec,
                    &k,
                    succ,
                    out,
                    charge,
                    extra_bytes.saturating_add(k_bytes),
                )
            }
        }
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
        extra_bytes: u64,
    ) -> Result<()> {
        if frontier.is_empty() {
            return Ok(());
        }
        let sides = if succ {
            self.succ.clone()
        } else {
            self.pred.clone()
        };
        let base = extra_bytes.saturating_add(self.retained_bytes());
        for s in &sides {
            let start = out.len();
            let mut cur_cap = out.capacity();
            self.scan_lane(
                ctx,
                spec,
                &s.lane,
                frontier,
                s.rev,
                s.skip_self,
                out,
                usize::MAX,
                &mut |new_len| {
                    if new_len > cur_cap {
                        let elem = size_of::<(u64, Step)>();
                        let next_c = vec_grown(cur_cap, new_len - 1, 1, elem);
                        charge.resize(
                            base.saturating_add(
                                next_c.saturating_add(cur_cap) as u64 * elem as u64,
                            ),
                        )?;
                        cur_cap = next_c;
                    }
                    Ok(())
                },
            )?;
            if !succ && s.lane.subject_side {
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

    #[allow(clippy::too_many_arguments)]
    fn scan_lane<O: Sink>(
        &mut self,
        ctx: &Ctx,
        spec: &PathSearchSpec,
        lane: &Lane,
        frontier: &[u64],
        rev: bool,
        skip_self: bool,
        out: &mut O,
        cap: usize,
        admit_len: &mut dyn FnMut(usize) -> Result<()>,
    ) -> Result<()> {
        let snap = &ctx.snap;
        let (kc, nc, pc) = lane.cols();
        let gc = lane.perm.col_of(crate::index::G);
        let prefix: Vec<u64> = lane.p.into_iter().collect();
        let mask = (1 << kc) | (1 << nc) | (1 << gc) | pc.map_or(0, |c| 1 << c);
        let graph = &spec.graph;
        let dedup = self.dedup;
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
            admit_len(out.len().saturating_add(1))?;
            out.put(x, Step { node, p, rev });
            Ok(())
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
                let mut key = prefix.clone();
                key.push(x);
                snap.scan_between_cols(lane.perm, pad(&key, 0), pad(&key, u64::MAX), mask, |c| {
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
                })?;
            }
            return Ok(());
        }
        self.sweeps += 1;
        let (first, last_node) = (frontier[0], frontier[frontier.len() - 1]);
        let (mut lo, mut hi) = (prefix.clone(), prefix);
        lo.push(first);
        hi.push(last_node);
        let mut j = 0;
        let mut scanned = 0usize;
        snap.scan_between_cols(lane.perm, pad(&lo, 0), pad(&hi, u64::MAX), mask, |c| {
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
        })?;
        Ok(())
    }

    fn neighbours(
        &mut self,
        ctx: &Ctx,
        spec: &PathSearchSpec,
        x: u64,
        succ: bool,
        charge: &mut super::ctx::OwnedCharge,
        extra_bytes: u64,
    ) -> Result<std::sync::Arc<[Step]>> {
        if self.sink(ctx, x, succ) {
            return Ok(std::sync::Arc::from(Vec::new()));
        }
        if self.blocked(ctx, spec, x, charge, extra_bytes)? {
            self.skipped_hubs += 1;
            return Ok(std::sync::Arc::from(Vec::new()));
        }
        if let Some(n) = self.cache.get(&(x, succ)) {
            return Ok(n.clone());
        }
        let mut out = Vec::new();
        self.expand_all(ctx, spec, &[x], succ, &mut out, charge, extra_bytes)?;
        let out_bytes = out.capacity() as u64 * size_of::<(u64, Step)>() as u64;
        let added = out.len() as u64 * size_of::<Step>() as u64 + (2 * size_of::<usize>()) as u64;
        let next_cache_bytes = self.cache_step_bytes.saturating_add(added);
        // During HashMap rehash both old and new tables are simultaneously live.
        let growth = table_growth::<((u64, bool), std::sync::Arc<[Step]>)>(
            self.cache.capacity(),
            self.cache.len(),
        );
        charge.resize(
            extra_bytes
                .saturating_add(self.retained_bytes())
                .saturating_add(out_bytes)
                .saturating_add(added)
                .saturating_add(growth),
        )?;
        let steps: std::sync::Arc<[Step]> = out.into_iter().map(|(_, s)| s).collect();
        self.cache_step_bytes = next_cache_bytes;
        self.cache.insert((x, succ), steps.clone());
        charge.resize(extra_bytes.saturating_add(self.retained_bytes()))?;
        Ok(steps)
    }

    fn path_cost(
        &mut self,
        ctx: &Ctx,
        spec: &PathSearchSpec,
        edges: &[Edge],
        charge: &mut super::ctx::OwnedCharge,
        extra_bytes: u64,
    ) -> Result<f64> {
        let Some(w) = spec.weight else {
            return Ok(edges.len() as f64);
        };
        let mut sum = Cost::ZERO;
        for e in edges {
            let weight = if w.is_none() || spec.reifies.is_none() {
                spec.default_weight
            } else {
                let key = e.triple();
                if let Some(&x) = self.weights.get(&key) {
                    x
                } else {
                    let x = lookup_weight_standalone(ctx, spec, w, key)?;
                    if self.weights.len() == self.weights.capacity() {
                        let add = table_growth::<((u64, u64, u64), f64)>(
                            self.weights.capacity(),
                            self.weights.len(),
                        );
                        charge.resize(
                            extra_bytes
                                .saturating_add(self.retained_bytes())
                                .saturating_add(add),
                        )?;
                    }
                    self.weights.insert(key, x);
                    charge.resize(extra_bytes.saturating_add(self.retained_bytes()))?;
                    x
                }
            };
            sum = sum.add(Cost::of(weight));
        }
        Ok(sum.hi)
    }

    fn next_path(
        &mut self,
        ctx: &Ctx,
        spec: &PathSearchSpec,
        charge: &mut super::ctx::OwnedCharge,
        extra_bytes: u64,
    ) -> Result<Option<Found>> {
        if self.done {
            return Ok(None);
        }
        if self.pending_zero {
            self.pending_zero = false;
            if self.max_len == 0 {
                self.done = true;
            }
            self.paths_found += 1;
            return Ok(Some(Found {
                start: self.root,
                end: self.root,
                edges: Vec::new(),
                len: 0,
                cost: 0.0,
            }));
        }
        if let Some(y) = self.pending_push.take() {
            if self.on_path.len() == self.on_path.capacity() && !self.on_path.contains(&y) {
                let add = table_growth::<u64>(self.on_path.capacity(), self.on_path.len());
                charge.resize(
                    extra_bytes
                        .saturating_add(self.retained_bytes())
                        .saturating_add(add),
                )?;
            }
            self.on_path.insert(y);
            let ns = self.neighbours(ctx, spec, y, self.forward, charge, extra_bytes)?;
            if self.stack.len() == self.stack.capacity() {
                let add = next_cap(self.stack.len(), self.stack.capacity(), 1) as u64
                    * size_of::<(u64, std::sync::Arc<[Step]>, usize)>() as u64;
                charge.resize(
                    extra_bytes
                        .saturating_add(self.retained_bytes())
                        .saturating_add(add),
                )?;
            }
            self.stack.push((y, ns, 0));
            charge.resize(extra_bytes.saturating_add(self.retained_bytes()))?;
        }
        let need_edges = spec.per_edge() || spec.weighted();
        while let Some(top) = self.stack.last_mut() {
            self.poll_ticks = self.poll_ticks.wrapping_add(1);
            if self.poll_ticks.is_multiple_of(1024) {
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
                self.visit(spec, 1024)?;
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
            if self.edges.len() == self.edges.capacity() {
                let add = next_cap(self.edges.len(), self.edges.capacity(), 1) as u64
                    * size_of::<Edge>() as u64;
                charge.resize(
                    extra_bytes
                        .saturating_add(self.retained_bytes())
                        .saturating_add(add),
                )?;
            }
            self.edges.push(e);
            charge.resize(extra_bytes.saturating_add(self.retained_bytes()))?;
            let matched = (closing || self.target.is_none_or(|t| t == y)) && len >= self.min_len;
            let stop_branch = closing || len >= self.max_len;
            if matched {
                let plen = self.edges.len();
                let (ordered, cost) = if need_edges {
                    let ordered_bytes = (plen as u64).saturating_mul(size_of::<Edge>() as u64);
                    charge.resize(
                        extra_bytes
                            .saturating_add(self.retained_bytes())
                            .saturating_add(ordered_bytes),
                    )?;
                    let ordered: Vec<Edge> = if self.forward {
                        self.edges.clone()
                    } else {
                        self.edges.iter().rev().copied().collect()
                    };
                    let cost = self.path_cost(
                        ctx,
                        spec,
                        &ordered,
                        charge,
                        extra_bytes.saturating_add(ordered_bytes),
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
                self.paths_found += 1;
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
            if self.on_path.len() == self.on_path.capacity() && !self.on_path.contains(&y) {
                let add = table_growth::<u64>(self.on_path.capacity(), self.on_path.len());
                charge.resize(
                    extra_bytes
                        .saturating_add(self.retained_bytes())
                        .saturating_add(add),
                )?;
            }
            self.on_path.insert(y);
            let child_ns = self.neighbours(ctx, spec, y, self.forward, charge, extra_bytes)?;
            if self.stack.len() == self.stack.capacity() {
                let add = next_cap(self.stack.len(), self.stack.capacity(), 1) as u64
                    * size_of::<(u64, std::sync::Arc<[Step]>, usize)>() as u64;
                charge.resize(
                    extra_bytes
                        .saturating_add(self.retained_bytes())
                        .saturating_add(add),
                )?;
            }
            self.stack.push((y, child_ns, 0));
            charge.resize(extra_bytes.saturating_add(self.retained_bytes()))?;
        }
        self.done = true;
        Ok(None)
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

fn lookup_weight_standalone(
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
    let mut reifiers = Vec::new();
    for k in ctx.snap.scan_keys(Perm::Pos, &[reifies, tt.0])? {
        if graph.accepts(k[3]) {
            reifiers.push(k[2]);
        }
    }
    let mut best: Option<f64> = None;
    for r in reifiers {
        for k in ctx.snap.scan_keys(Perm::Pso, &[w, r])? {
            if !graph.accepts(k[3]) {
                continue;
            }
            let v = ctx
                .value(Id(k[2]))
                .as_ref()
                .and_then(super::value::approx_f64);
            match v {
                Some(v) if v >= 0.0 && v.is_finite() => {
                    best = Some(best.map_or(v, |b: f64| b.min(v)));
                }
                _ => {
                    let t = ctx
                        .term(Id(k[2]))
                        .map_or_else(|| "?".to_string(), |t| t.to_string());
                    return Err(bad(format!("the weight {t} is not a non-negative number")));
                }
            }
        }
    }
    Ok(best.unwrap_or(default))
}

fn find_const_bounded(
    ctx: &Ctx,
    spec: &PathSearchSpec,
    s: u64,
    t: u64,
    out_charge: &mut super::ctx::OwnedCharge,
) -> Result<(
    std::collections::VecDeque<Found>,
    serde_json::Map<String, serde_json::Value>,
)> {
    let eng = Engine::new(ctx, spec, None)?;
    let weighted = spec.weighted();
    let need_edges = spec.per_edge() || weighted;
    let mut found: std::collections::VecDeque<Found> = std::collections::VecDeque::new();
    let mut found_edge_bytes = 0u64;
    let mut push = |edges: Vec<Edge>, len: Option<usize>| -> Result<bool> {
        if !eng.take() {
            return Ok(false);
        }
        let cost = eng.path_cost(&edges)?;
        let plen = len.unwrap_or(edges.len());
        let edges = if need_edges { edges } else { Vec::new() };
        let add_edge = edges.capacity() as u64 * size_of::<Edge>() as u64;
        let q_cap = next_cap(found.len(), found.capacity(), 1) as u64 * size_of::<Found>() as u64;
        out_charge.resize(
            found_edge_bytes
                .saturating_add(add_edge)
                .saturating_add(q_cap),
        )?;
        found_edge_bytes = found_edge_bytes.saturating_add(add_edge);
        found.push_back(Found {
            start: s,
            end: t,
            edges,
            len: plen,
            cost,
        });
        Ok(true)
    };
    match spec.algorithm {
        Algorithm::Shortest => {
            if weighted && spec.max_len.is_some() {
                eng.begin();
                if spec.min_len == 0 && s == t {
                    push(Vec::new(), None)?;
                } else {
                    let mut best: Option<Cost> = None;
                    let mut out = Vec::new();
                    eng.yen(s, t, |p, c| {
                        if p.len() as u32 > spec.max_len.unwrap() || (p.len() as u32) < spec.min_len
                        {
                            return Ok(true);
                        }
                        match best {
                            None => best = Some(c),
                            Some(b) if c > b => return Ok(false),
                            Some(_) => {}
                        }
                        out.push(p.to_vec());
                        Ok(false)
                    })?;
                    for p in out {
                        if !push(p, None)? {
                            break;
                        }
                    }
                }
            } else if !weighted {
                eng.begin();
                if spec.min_len == 0 && s == t {
                    push(Vec::new(), None)?;
                } else {
                    for p in eng.bidirectional(s, t, false, 1)? {
                        if !push(p, None)? {
                            break;
                        }
                    }
                }
            } else if s == t {
                eng.begin();
                if spec.min_len == 0 {
                    push(Vec::new(), None)?;
                } else {
                    let answered = eng.dijkstra_pair_all(s, t, false, &mut |p| {
                        push(p, None)?;
                        Ok(false)
                    })?;
                    if !answered {
                        let mut tset = FxHashSet::default();
                        tset.insert(t);
                        let (tree, _) =
                            eng.dijkstra(s, true, Some(&tset), false, true, None, None, s)?;
                        for p in tree.paths(VIRT, false, 1, None, &|| eng.ctx.check())? {
                            let p = p
                                .into_iter()
                                .map(|e| e.map(|x| if x == VIRT { s } else { x }))
                                .collect();
                            if !push(p, None)? {
                                break;
                            }
                        }
                    }
                }
            } else {
                eng.begin();
                if let Some((p, _)) = eng.dijkstra_pair(s, t, None, None)? {
                    push(p, None)?;
                }
            }
        }
        Algorithm::KShortest => {
            eng.begin();
            let mut n = 0usize;
            if spec.min_len == 0 && s == t {
                push(Vec::new(), None)?;
                n += 1;
            }
            if n < spec.k {
                let mut out = Vec::new();
                let max = spec.max_len.unwrap_or(u32::MAX);
                let cap = eng.left.get();
                eng.yen(s, t, |p, _| {
                    let len = p.len() as u32;
                    if len >= spec.min_len && len <= max {
                        out.push(p.to_vec());
                        n += 1;
                    }
                    Ok(n < spec.k && out.len() < cap)
                })?;
                for p in out {
                    if !push(p, None)? {
                        break;
                    }
                }
            }
        }
        _ => unreachable!("find_const_bounded handles Shortest and KShortest"),
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
    Ok((found, counters))
}

impl Reader {
    pub(crate) fn open(
        ctx: &std::sync::Arc<Ctx>,
        spec: &PathSearchSpec,
        vars: &[VarId],
    ) -> Result<Self> {
        let mut charge = super::ctx::OwnedCharge::new(ctx, 0)?;
        let cols = RowCols::new(spec, vars);
        let (mode, counters) = match spec.algorithm {
            Algorithm::Shortest | Algorithm::KShortest => {
                let (PathEnd::Const(s), PathEnd::Const(t)) = (&spec.source, &spec.target) else {
                    unreachable!("stream_eligible requires constant source and target")
                };
                let (paths, counters) = find_const_bounded(ctx, spec, s.0, t.0, &mut charge)?;
                let bytes: u64 = paths
                    .iter()
                    .map(|f| (f.edges.capacity() * size_of::<Edge>()) as u64)
                    .fold(
                        paths.capacity() as u64 * size_of::<Found>() as u64,
                        u64::saturating_add,
                    );
                charge.resize(bytes)?;
                (ReaderMode::Bounded(paths), counters)
            }
            Algorithm::All => {
                let state = AllPathsState::open(ctx, spec, &mut charge)?;
                let counters = state.counters(spec);
                (ReaderMode::All(Box::new(state)), counters)
            }
            Algorithm::AllShortest => unreachable!("AllShortest is not stream_eligible"),
        };
        Ok(Self {
            spec: spec.clone(),
            vars: vars.to_vec(),
            mode,
            current: None,
            next_path_id: 0,
            terms: FxHashMap::default(),
            cols,
            charge,
            counters,
        })
    }

    pub(crate) fn counters(&self) -> &serde_json::Map<String, serde_json::Value> {
        &self.counters
    }

    fn terms_bytes(&self) -> u64 {
        self.terms.capacity() as u64 * (size_of::<(u64, u64, u64)>() + size_of::<Id>() + 1) as u64
    }

    fn current_bytes(&self) -> u64 {
        self.current.as_ref().map_or(0, |(_, f, _)| {
            (f.edges.capacity() * size_of::<Edge>() + size_of::<Found>()) as u64
        })
    }

    fn mode_bytes(&self) -> u64 {
        match &self.mode {
            ReaderMode::Bounded(q) => q
                .iter()
                .map(|f| (f.edges.capacity() * size_of::<Edge>()) as u64)
                .fold(
                    q.capacity() as u64 * size_of::<Found>() as u64,
                    u64::saturating_add,
                ),
            ReaderMode::All(st) => st.retained_bytes(),
        }
    }

    fn update_charge(&mut self) -> Result<()> {
        let total = self
            .mode_bytes()
            .saturating_add(self.current_bytes())
            .saturating_add(self.terms_bytes());
        self.charge.resize(total)
    }

    /// Fill `out` with up to `cap` rows. Returns `true` when the search is exhausted.
    pub(crate) fn next(&mut self, ctx: &Ctx, out: &mut Table, cap: usize) -> Result<bool> {
        let per_edge = self.spec.per_edge();
        let weighted = self.spec.weighted();
        let width = self.vars.len();
        let mut path_row = vec![Id::UNDEF; width];
        let mut r = vec![Id::UNDEF; width];
        let int = |n: usize| Id::from_i64(n as i64).unwrap_or(Id::UNDEF);
        let set = |c: Option<usize>, id: Id, row: &mut [Id]| {
            if let Some(c) = c {
                row[c] = id;
            }
        };
        while out.len() < cap {
            ctx.check()?;
            if self.current.is_none() {
                let extra_bytes = self.terms_bytes();
                let next = match &mut self.mode {
                    ReaderMode::Bounded(q) => q.pop_front(),
                    ReaderMode::All(st) => {
                        let p = st.next_path(ctx, &self.spec, &mut self.charge, extra_bytes)?;
                        self.counters = st.counters(&self.spec);
                        p
                    }
                };
                let Some(found) = next else {
                    self.update_charge()?;
                    return Ok(true);
                };
                let pid = self.next_path_id;
                self.next_path_id = self.next_path_id.saturating_add(1);
                self.current = Some((pid, found, 0));
                self.update_charge()?;
            }
            let mode_bytes = self.mode_bytes();
            let (pid, found, edge_at) = self.current.as_mut().expect("current path");
            let cost_id = if weighted {
                Id::from_f64(found.cost)
                    .unwrap_or_else(|| ctx.intern_value(&Value::Double(found.cost.into())))
            } else {
                int(found.len)
            };
            ctx.check()?;
            path_row.fill(Id::UNDEF);
            set(self.cols.src_c, Id(found.start), &mut path_row);
            set(self.cols.tgt_c, Id(found.end), &mut path_row);
            set(self.cols.pi_c, int(*pid as usize), &mut path_row);
            set(self.cols.len_c, int(found.len), &mut path_row);
            set(self.cols.cost_c, cost_id, &mut path_row);
            let n_rows = if per_edge {
                found.edges.len().max(1)
            } else {
                1
            };
            let mode_and_cur_bytes = mode_bytes.saturating_add(
                (found.edges.capacity() * size_of::<Edge>() + size_of::<Found>()) as u64,
            );
            while *edge_at < n_rows && out.len() < cap {
                let k = *edge_at;
                *edge_at += 1;
                r.copy_from_slice(&path_row);
                if per_edge && let Some(e) = found.edges.get(k) {
                    let (s, p, o) = e.triple();
                    set(self.cols.ei_c, int(k), &mut r);
                    set(self.cols.es_c, Id(s), &mut r);
                    set(self.cols.ep_c, Id(p), &mut r);
                    set(self.cols.eo_c, Id(o), &mut r);
                    if self.cols.edge_c.is_some() {
                        let tid = if let Some(&existing) = self.terms.get(&(s, p, o)) {
                            existing
                        } else {
                            if self.terms.len() == self.terms.capacity() {
                                let next_terms =
                                    next_cap(self.terms.len(), self.terms.capacity(), 1) as u64
                                        * (size_of::<(u64, u64, u64)>() + size_of::<Id>() + 1)
                                            as u64;
                                self.charge
                                    .resize(mode_and_cur_bytes.saturating_add(next_terms))?;
                            }
                            let computed = {
                                let parts = (ctx.term(Id(s)), ctx.term(Id(p)), ctx.term(Id(o)));
                                if let (Some(s), Some(Term::NamedNode(p)), Some(o)) = parts {
                                    match s {
                                        Term::NamedNode(n) => ctx.intern_term(&Term::Triple(
                                            Box::new(oxrdf::Triple::new(
                                                oxrdf::NamedOrBlankNode::NamedNode(n),
                                                p,
                                                o,
                                            )),
                                        )),
                                        Term::BlankNode(b) => ctx.intern_term(&Term::Triple(
                                            Box::new(oxrdf::Triple::new(
                                                oxrdf::NamedOrBlankNode::BlankNode(b),
                                                p,
                                                o,
                                            )),
                                        )),
                                        _ => Id::UNDEF,
                                    }
                                } else {
                                    Id::UNDEF
                                }
                            };
                            self.terms.insert((s, p, o), computed);
                            computed
                        };
                        ctx.check()?;
                        set(self.cols.edge_c, tid, &mut r);
                    }
                }
                out.push_row(&r);
            }
            if *edge_at >= n_rows {
                self.current = None;
            }
            self.update_charge()?;
        }
        Ok(false)
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
        let mut state = AllPathsState::open(&ctx, &spec, &mut held).unwrap();
        ctx.cancel.store(true, Ordering::Relaxed);
        assert!(matches!(
            state.next_path(&ctx, &spec, &mut held, 0),
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
        let retained = state.retained_bytes();
        limited.mem_limit = retained + 128;
        let limited = Arc::new(limited);
        let mut held = super::super::ctx::OwnedCharge::new(&limited, retained).unwrap();
        state.sinks = true;
        let mut frontier = vec![target.0; 1024];
        frontier.push(literal.0);
        let mut edges = Vec::new();
        let result = state.expand(&limited, &spec, &frontier, false, &mut edges, &mut held, 0);
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
}
