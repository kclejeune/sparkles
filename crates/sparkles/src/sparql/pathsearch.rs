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
//!
//! The searches: bidirectional breadth-first search for one unweighted pair, one
//! breadth-first search per source (or per target) for several or unbound ends,
//! Dijkstra's algorithm with weights, Yen's algorithm for the k shortest paths, and a
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
use spargebra::term::{NamedNodePattern, TermPattern};
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
    pub direction: Direction,
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
    }

    fn weighted(&self) -> bool {
        self.weight.is_some()
    }
}

// ----------------------------------------------------------------- planning ------

/// A `SERVICE path:search { … }` call as a leaf.
pub(super) fn path_search_leaf(
    p: &Planner<'_>,
    inner: &GraphPattern,
    g: &ActiveGraph,
) -> Result<Node> {
    let GraphPattern::Bgp { patterns } = inner else {
        return Err(bad(
            "the block must hold only configuration triples (path:source, path:target, …)",
        ));
    };
    if patterns.is_empty() {
        return Err(bad("missing path:source and path:target"));
    }
    let subject = &patterns[0].subject;
    let ctx = p.ctx;
    let mut algorithm = None;
    let (mut source, mut target) = (None, None);
    let mut preds: Option<Vec<u64>> = None;
    let mut pred_names = Vec::new();
    let mut direction = None;
    let (mut min_len, mut max_len, mut k, mut limit, mut max_visited) =
        (None, None, None, None, None);
    let (mut weight, mut default_weight) = (None, None);
    let mut outs: [Option<VarId>; 7] = [None; 7];
    const OUT_NAMES: [&str; 7] = [
        "pathIndex",
        "edgeIndex",
        "edgeSubject",
        "edgePredicate",
        "edgeObject",
        "length",
        "cost",
    ];
    fn once<T>(slot: &mut Option<T>, v: T, name: &str) -> Result<()> {
        if slot.replace(v).is_some() {
            return Err(bad(format!("path:{name} is given twice")));
        }
        Ok(())
    }
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
                let TermPattern::NamedNode(n) = o else {
                    return Err(bad("path:predicate takes an IRI"));
                };
                pred_names.push(n.as_str().to_string());
                let list = preds.get_or_insert_with(Vec::new);
                if let Some(id) = ctx.snap.lookup_iri(n.as_str())
                    && !list.contains(&id.0)
                {
                    list.push(id.0);
                }
            }
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
    let Some((graph, graph_var)) = p.graph_filter(g) else {
        return Ok(Node::empty(vars));
    };
    if graph_var.is_some() {
        return Err(bad(
            "GRAPH ?g around a path search is not supported; name the graph or use the default graph",
        ));
    }
    let direction = direction.unwrap_or(Direction::Forward);
    let end = |e: &PathEnd| match e {
        PathEnd::Var(v) => format!("?{}", ctx.var_name(*v)),
        PathEnd::Const(id) => ctx
            .term(*id)
            .map_or_else(|| format!("{id:?}"), |t| t.to_string()),
    };
    let desc = format!(
        "{} {} → {} via {} {}{}{}{}{}{}",
        algorithm.name(),
        end(&source),
        end(&target),
        if pred_names.is_empty() {
            "every predicate".to_string()
        } else {
            pred_names
                .iter()
                .map(|p| format!("<{p}>"))
                .collect::<Vec<_>>()
                .join(" ")
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
        weight
            .as_ref()
            .map_or(String::new(), |(w, _)| format!(" weight=<{w}>")),
    );
    let edge_vars = [outs[1], outs[2], outs[3], outs[4]];
    let spec = PathSearchSpec {
        algorithm,
        source,
        target,
        predicates: preds,
        direction,
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
        length: outs[5],
        cost: outs[6],
        graph,
    };
    let est = 100.0;
    let mut n = Node::leaf(Kind::PathSearch(Box::new(spec)), vars, est, desc);
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
    Ok(Node {
        dist: vars.iter().map(|&v| (v, est)).collect(),
        cost: left.cost + est * 100.0,
        vars,
        certain,
        sorted: Vec::new(),
        est,
        desc: search.desc,
        kind: search.kind,
        children: vec![left],
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
}

impl Tree {
    fn new(root: u64, forward: bool) -> Tree {
        Tree {
            root,
            forward,
            first: FxHashMap::default(),
            extra: FxHashMap::default(),
        }
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
    fn paths(&self, m: u64, all: bool, cap: usize) -> Vec<Vec<Edge>> {
        let mut out = Vec::new();
        if cap == 0 || !self.contains(m) {
            return out;
        }
        let mut stack: Vec<(u64, usize)> = vec![(m, 0)];
        let mut links: Vec<(u64, Parent)> = Vec::new();
        while let Some(top) = stack.last_mut() {
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
                links.pop();
                if out.len() >= cap {
                    break;
                }
                continue;
            }
            let i = top.1;
            top.1 += 1;
            match self.parent_at(x, i, all) {
                Some(q) => {
                    links.push((x, q));
                    stack.push((q.node, 0));
                }
                None => {
                    stack.pop();
                    links.pop();
                }
            }
        }
        out
    }
}

/// An `f64` ordered by `total_cmp`, for the heaps.
#[derive(Clone, Copy, Debug, PartialEq)]
struct OrdF64(f64);
impl Eq for OrdF64 {}
impl PartialOrd for OrdF64 {
    fn partial_cmp(&self, o: &Self) -> Option<Ordering> {
        Some(self.cmp(o))
    }
}
impl Ord for OrdF64 {
    fn cmp(&self, o: &Self) -> Ordering {
        self.0.total_cmp(&o.0)
    }
}

/// One index range that holds edges: `perm` with the predicate fixed or not.
#[derive(Clone, Copy)]
struct Lane {
    perm: Perm,
    p: Option<u64>,
    /// the lane is read by the subject of its triples (else by the object)
    subject_side: bool,
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

/// The neighbours of each node (and direction) a search looked up.
type Neighbours = FxHashMap<(u64, bool), Rc<[Step]>>;

/// A path found: its ends, edges and cost.
struct Found {
    start: u64,
    end: u64,
    edges: Vec<Edge>,
    cost: f64,
}

/// The state of one call: index access, caches, budgets and counters.
struct Engine<'a> {
    ctx: &'a Ctx,
    spec: &'a PathSearchSpec,
    dedup: bool,
    subject_lanes: Vec<Lane>,
    object_lanes: Vec<Lane>,
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
}

impl<'a> Engine<'a> {
    fn new(ctx: &'a Ctx, spec: &'a PathSearchSpec) -> Result<Engine<'a>> {
        let lanes = |subject_side: bool| -> Vec<Lane> {
            let perm = match (spec.predicates.is_some(), subject_side) {
                (true, true) => Perm::Pso,
                (true, false) => Perm::Pos,
                (false, true) => Perm::Spo,
                (false, false) => Perm::Osp,
            };
            match &spec.predicates {
                Some(ps) => ps
                    .iter()
                    .map(|&p| Lane {
                        perm,
                        p: Some(p),
                        subject_side,
                    })
                    .collect(),
                None => vec![Lane {
                    perm,
                    p: None,
                    subject_side,
                }],
            }
        };
        Ok(Engine {
            ctx,
            spec,
            dedup: spec.graph.multi(),
            subject_lanes: lanes(true),
            object_lanes: lanes(false),
            cache: Default::default(),
            weights: Default::default(),
            charge: ctx.charge(0)?,
            visited: Cell::new(0),
            visited_total: Cell::new(0),
            searches: Cell::new(0),
            sweeps: Cell::new(0),
            left: Cell::new(spec.limit.unwrap_or(usize::MAX)),
        })
    }

    /// Start a new search: its visited count starts at zero.
    fn begin(&self) {
        self.visited.set(0);
        self.searches.set(self.searches.get() + 1);
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

    /// The (lane, rev) pairs that give the neighbours of a node: its successors on a
    /// path when `succ`, else its predecessors.
    fn sides(&self, succ: bool) -> Vec<(&[Lane], bool)> {
        use Direction::*;
        let (s, o) = (&self.subject_lanes[..], &self.object_lanes[..]);
        match (self.spec.direction, succ) {
            (Forward, true) => vec![(s, false)],
            (Forward, false) => vec![(o, false)],
            (Backward, true) => vec![(o, true)],
            (Backward, false) => vec![(s, true)],
            (Both, true) => vec![(s, false), (o, true)],
            (Both, false) => vec![(o, false), (s, true)],
        }
    }

    /// The neighbours of every node of a sorted, duplicate-free frontier, as
    /// (node, neighbour) pairs.
    fn expand(&self, frontier: &[u64], succ: bool, out: &mut Vec<(u64, Step)>) -> Result<()> {
        if frontier.is_empty() {
            return Ok(());
        }
        let sides = self.sides(succ);
        let both = sides.len() > 1;
        for (i, (lanes, rev)) in sides.into_iter().enumerate() {
            for lane in lanes {
                // with both directions a self-loop is one edge, read from the first side
                self.scan_lane(lane, frontier, rev, both && i == 1, out)?;
            }
        }
        self.ctx.check()
    }

    fn scan_lane(
        &self,
        lane: &Lane,
        frontier: &[u64],
        rev: bool,
        skip_self: bool,
        out: &mut Vec<(u64, Step)>,
    ) -> Result<()> {
        let snap = &self.ctx.snap;
        let (kc, nc, pc) = lane.cols();
        let gc = lane.perm.col_of(crate::index::G);
        let prefix: Vec<u64> = lane.p.into_iter().collect();
        let mask = if self.ctx.opt.selective_columns {
            (1 << kc) | (1 << nc) | (1 << gc) | pc.map_or(0, |c| 1 << c)
        } else {
            crate::index::ALL_COLS
        };
        let graph = &self.spec.graph;
        let dedup = self.dedup;
        let fixed_p = lane.p.unwrap_or(0);
        let mut last: Option<(u64, u64, u64)> = None;
        let mut emit = |x: u64, node: u64, p: u64, g: u64, out: &mut Vec<(u64, Step)>| {
            if !graph.accepts(g) || (skip_self && node == x) {
                return;
            }
            if dedup && last == Some((x, node, p)) {
                return;
            }
            last = Some((x, node, p));
            out.push((x, Step { node, p, rev }));
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
                    Ok(true)
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
        if let Some(n) = self.cache.borrow().get(&(x, succ)) {
            return Ok(n.clone());
        }
        let mut out = Vec::new();
        self.expand(&[x], succ, &mut out)?;
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

    fn path_cost(&self, edges: &[Edge]) -> Result<f64> {
        if !self.spec.weighted() {
            return Ok(edges.len() as f64);
        }
        let mut c = 0.0;
        for e in edges {
            c += self.weight(e)?;
        }
        Ok(c)
    }

    /// Grow a breadth-first tree by one level. `virt` is the node the virtual id stands
    /// for: when the tree's root is virtual it is expanded as that node, and when
    /// `close` is set a neighbour equal to it is recorded as the virtual id (the end of
    /// a cycle) and never expanded. Returns the nodes of the new level.
    fn grow(
        &self,
        tree: &mut Tree,
        frontier: &[u64],
        depth: u32,
        all: bool,
        virt: u64,
        close: bool,
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
            let parent = Parent {
                node: x,
                p: st.p,
                rev: st.rev,
            };
            match tree.first.get(&y) {
                None => {
                    tree.first.insert(y, (parent, depth + 1));
                    next.push(y);
                }
                Some(&(_, l)) if all && l == depth + 1 => {
                    tree.extra.entry(y).or_default().push(parent);
                }
                Some(_) => {}
            }
        }
        self.visit(next.len() as u64)?;
        next.sort_unstable();
        Ok(next)
    }

    /// Unweighted shortest paths between one pair by bidirectional breadth-first
    /// search.
    fn bidirectional(&self, s: u64, t: u64, all: bool, cap: usize) -> Result<Vec<Vec<Edge>>> {
        let cycle = s == t;
        let tt = if cycle { VIRT } else { t };
        let mut f = Tree::new(s, true);
        let mut b = Tree::new(tt, false);
        let (mut ff, mut bf) = (vec![s], vec![tt]);
        let (mut fd, mut bd) = (0u32, 0u32);
        let max = self.spec.max_len.unwrap_or(u32::MAX);
        self.visit(2)?;
        loop {
            if fd.saturating_add(bd) >= max || ff.is_empty() || bf.is_empty() {
                return Ok(Vec::new());
            }
            let forward = ff.len() <= bf.len();
            let (new, other) = if forward {
                ff = self.grow(&mut f, &ff, fd, all, s, cycle)?;
                fd += 1;
                (&ff, &b)
            } else {
                bf = self.grow(&mut b, &bf, bd, all, s, false)?;
                bd += 1;
                (&bf, &f)
            };
            let meet: Vec<u64> = new.iter().copied().filter(|y| other.contains(*y)).collect();
            if meet.is_empty() {
                continue;
            }
            let mut out = Vec::new();
            let real = |x: u64| if x == VIRT { s } else { x };
            for m in meet {
                let left = cap.saturating_sub(out.len());
                if left == 0 {
                    break;
                }
                let fp = f.paths(m, all, left);
                let bp = b.paths(m, all, left);
                'combine: for a in &fp {
                    for z in &bp {
                        let mut e: Vec<Edge> = a.iter().chain(z).map(|e| e.map(real)).collect();
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
            frontier = self.grow(&mut tree, &frontier, depth, all, root, close)?;
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
    ) -> Result<(Tree, FxHashMap<u64, f64>)> {
        let mut tree = Tree::new(root, forward);
        let mut dist: FxHashMap<u64, f64> = FxHashMap::default();
        let mut settled: FxHashSet<u64> = FxHashSet::default();
        let mut heap = BinaryHeap::new();
        let mut missing: FxHashSet<u64> = targets
            .map(|t| {
                t.iter()
                    .map(|&x| if close && x == virt { VIRT } else { x })
                    .collect()
            })
            .unwrap_or_default();
        dist.insert(root, 0.0);
        heap.push(Reverse((OrdF64(0.0), root)));
        let mut n = 0u64;
        while let Some(Reverse((OrdF64(d), u))) = heap.pop() {
            if !settled.insert(u) {
                continue;
            }
            self.visit(1)?;
            n += 1;
            if n.is_multiple_of(4096) {
                self.ctx.check()?;
            }
            if targets.is_some() {
                missing.remove(&u);
                if missing.is_empty() {
                    break;
                }
            }
            // a virtual end is not expanded
            if u == VIRT && root != VIRT {
                continue;
            }
            let scan = if u == VIRT { virt } else { u };
            for st in self.neighbours(scan, forward)?.iter() {
                let y = if close && st.node == virt {
                    VIRT
                } else {
                    st.node
                };
                if settled.contains(&y) || banned_nodes.is_some_and(|b| b.contains(&y)) {
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
                let nd = d + self.weight(&e.map(real))?;
                let parent = Parent {
                    node: u,
                    p: st.p,
                    rev: st.rev,
                };
                match dist.get(&y) {
                    Some(&cur) if nd > cur => {}
                    Some(&cur) if nd == cur => {
                        if all {
                            tree.extra.entry(y).or_default().push(parent);
                        }
                    }
                    _ => {
                        dist.insert(y, nd);
                        tree.first.insert(y, (parent, 0));
                        tree.extra.remove(&y);
                        heap.push(Reverse((OrdF64(nd), y)));
                    }
                }
            }
        }
        // only settled nodes have final parents
        tree.first.retain(|k, _| settled.contains(k));
        dist.retain(|k, _| settled.contains(k));
        Ok((tree, dist))
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
        let close = to == VIRT;
        if self.spec.weighted() {
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
            let Some(&c) = dist.get(&to) else {
                return Ok(None);
            };
            return Ok(tree.paths(to, false, 1).pop().map(|p| (p, c)));
        }
        // breadth-first, one parent per node
        let mut parent: FxHashMap<u64, (u64, Step)> = FxHashMap::default();
        let mut frontier = vec![from];
        let mut depth = 0u32;
        let max = budget.unwrap_or(u32::MAX);
        self.visit(1)?;
        while !frontier.is_empty() && depth < max {
            let mut next = Vec::new();
            for &x in &frontier {
                if x == VIRT {
                    continue;
                }
                for st in self.neighbours(x, true)?.iter() {
                    let y = if close && st.node == virt {
                        VIRT
                    } else {
                        st.node
                    };
                    if y == from || parent.contains_key(&y) || banned_nodes.contains(&y) {
                        continue;
                    }
                    let e = Edge {
                        from: x,
                        to: y,
                        p: st.p,
                        rev: st.rev,
                    };
                    if banned_edges.contains(&e) {
                        continue;
                    }
                    parent.insert(y, (x, *st));
                    next.push(y);
                }
            }
            self.visit(next.len() as u64)?;
            depth += 1;
            if parent.contains_key(&to) {
                let mut edges = Vec::new();
                let mut y = to;
                while y != from {
                    let (x, st) = parent[&y];
                    edges.push(Edge {
                        from: x,
                        to: y,
                        p: st.p,
                        rev: st.rev,
                    });
                    y = x;
                }
                edges.reverse();
                let c = edges.len() as f64;
                return Ok(Some((edges, c)));
            }
            self.ctx.check()?;
            frontier = next;
        }
        Ok(None)
    }

    /// Simple paths in nondecreasing cost by Yen's algorithm; `accept` sees each one
    /// and returns whether to go on.
    fn yen(
        &self,
        s: u64,
        t: u64,
        mut accept: impl FnMut(&[Edge], f64) -> Result<bool>,
    ) -> Result<()> {
        let tt = if s == t { VIRT } else { t };
        let real = |x: u64| if x == VIRT { s } else { x };
        let max = self.spec.max_len;
        let none_n = FxHashSet::default();
        let none_e = FxHashSet::default();
        let budget = if self.spec.weighted() { None } else { max };
        let Some(first) = self.spur(s, tt, s, &none_n, &none_e, budget)? else {
            return Ok(());
        };
        let mut a: Vec<(Vec<Edge>, f64)> = vec![first];
        let mut seen: FxHashSet<Vec<Edge>> = FxHashSet::default();
        seen.insert(a[0].0.clone());
        // candidates by (cost, length, discovery)
        let mut heap: BinaryHeap<Reverse<(OrdF64, usize, u64)>> = BinaryHeap::new();
        let mut cands: FxHashMap<u64, (Vec<Edge>, f64)> = FxHashMap::default();
        let mut seq = 0u64;
        loop {
            let (cur, cost) = a.last().cloned().unwrap();
            let real_edges: Vec<Edge> = cur.iter().map(|e| e.map(real)).collect();
            if !accept(&real_edges, cost)? {
                return Ok(());
            }
            let nodes: Vec<u64> = std::iter::once(s).chain(cur.iter().map(|e| e.to)).collect();
            let mut root_cost = 0.0;
            for i in 0..cur.len() {
                if i > 0 {
                    root_cost += if self.spec.weighted() {
                        self.weight(&cur[i - 1].map(real))?
                    } else {
                        1.0
                    };
                }
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
                if let Some((sp, c)) =
                    self.spur(nodes[i], tt, s, &banned_nodes, &banned_edges, budget)?
                {
                    let mut path = root.to_vec();
                    path.extend(sp);
                    if seen.insert(path.clone()) {
                        let len = path.len();
                        cands.insert(seq, (path, root_cost + c));
                        heap.push(Reverse((OrdF64(root_cost + c), len, seq)));
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
    let eng = Engine::new(ctx, spec)?;
    let mut found: Vec<Found> = Vec::new();
    let mut by_pair: FxHashMap<(u64, u64), Vec<u32>> = FxHashMap::default();
    let mut by_source: FxHashMap<u64, Vec<u32>> = FxHashMap::default();
    let mut by_target: FxHashMap<u64, Vec<u32>> = FxHashMap::default();
    let weighted = spec.weighted();
    let all = matches!(spec.algorithm, Algorithm::AllShortest);
    // add a path found from a source (`fwd`) or into a target
    let mut add = |found: &mut Vec<Found>,
                   start: u64,
                   end: u64,
                   edges: Vec<Edge>,
                   fwd: bool|
     -> Result<bool> {
        if !eng.take() {
            return Ok(false);
        }
        let cost = eng.path_cost(&edges)?;
        let i = found.len() as u32;
        eng.charge.add(edges.len() as u64 * 32 + 64)?;
        found.push(Found {
            start,
            end,
            edges,
            cost,
        });
        if fwd {
            by_pair.entry((start, end)).or_default().push(i);
            by_source.entry(start).or_default().push(i);
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
                        if !add(&mut found, s, t, Vec::new(), true)? {
                            break 'sources;
                        }
                        continue;
                    }
                    let mut best: Option<f64> = None;
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
                        if !add(&mut found, s, t, p, true)? {
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
                        add(&mut found, s, t, Vec::new(), true)?;
                        continue;
                    }
                    for p in eng.bidirectional(s, t, all, if all { cap(&eng) } else { 1 })? {
                        if !add(&mut found, s, t, p, true)? {
                            break 'sources;
                        }
                    }
                    continue;
                }
                eng.begin();
                let close = targets.any || tset.contains(&s);
                let tree = if weighted {
                    eng.dijkstra(
                        s,
                        true,
                        (!targets.any).then_some(&tset),
                        all,
                        close,
                        None,
                        None,
                        s,
                    )?
                    .0
                } else {
                    eng.bfs(s, true, (!targets.any).then_some(&tset), all, close)?
                };
                let mut ends: Vec<u64> = if targets.any {
                    let mut v: Vec<u64> = tree.first.keys().copied().collect();
                    v.sort_unstable_by_key(|&x| if x == VIRT { s } else { x });
                    v
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
                    if !add(&mut found, s, s, Vec::new(), true)? {
                        break 'sources;
                    }
                }
                for m in ends {
                    let end = if m == VIRT { s } else { m };
                    for p in tree.paths(m, all, if all { cap(&eng) } else { 1 }) {
                        let p = p
                            .into_iter()
                            .map(|e| e.map(|x| if x == VIRT { s } else { x }))
                            .collect();
                        if !add(&mut found, s, end, p, true)? {
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
                        if !add(&mut found, s, t, Vec::new(), true)? {
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
                        if !add(&mut found, s, t, p, true)? {
                            break 'sources;
                        }
                    }
                }
            }
            Algorithm::All => {
                eng.begin();
                let mut stop = false;
                eng.all_paths(s, true, (!targets.any).then_some(&tset), &mut |p, end| {
                    if !add(&mut found, s, end, p.to_vec(), true)? {
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
                let tree = if weighted {
                    eng.dijkstra(t, false, None, all, true, None, None, t)?.0
                } else {
                    eng.bfs(t, false, None, all, true)?
                };
                if spec.min_len == 0 && !add(&mut found, t, t, Vec::new(), false)? {
                    break 'targets;
                }
                let mut starts: Vec<u64> = tree.first.keys().copied().collect();
                if spec.min_len == 0 {
                    starts.retain(|&m| m != VIRT);
                }
                starts.sort_unstable_by_key(|&x| if x == VIRT { t } else { x });
                for m in starts {
                    let start = if m == VIRT { t } else { m };
                    for p in tree.paths(m, all, if all { cap(&eng) } else { 1 }) {
                        let p = p
                            .into_iter()
                            .map(|e| e.map(|x| if x == VIRT { t } else { x }))
                            .collect();
                        if !add(&mut found, start, t, p, false)? {
                            break 'targets;
                        }
                    }
                }
            }
            Algorithm::All => {
                let mut stop = false;
                eng.all_paths(t, false, None, &mut |p, start| {
                    if !add(&mut found, start, t, p.to_vec(), false)? {
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
    let int = |n: usize| Id::from_i64(n as i64).unwrap_or(Id::UNDEF);
    let cost_id = |f: &Found| -> Id {
        if weighted {
            Id::from_f64(f.cost).unwrap_or_else(|| ctx.intern_value(&Value::Double(f.cost.into())))
        } else {
            int(f.edges.len())
        }
    };
    let empty: Vec<u32> = Vec::new();
    let mut row = vec![Id::UNDEF; vars.len()];
    let mut path_row = vec![Id::UNDEF; vars.len()];
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
            set(len_c, int(f.edges.len()), &mut path_row);
            set(cost_c, cost_id(f), &mut path_row);
            let edge_rows: Vec<Option<(usize, &Edge)>> = if per_edge && !f.edges.is_empty() {
                f.edges.iter().enumerate().map(Some).collect()
            } else {
                vec![None]
            };
            for er in edge_rows {
                let mut r = path_row.clone();
                if let Some((k, e)) = er {
                    let (s, p, o) = e.triple();
                    set(ei_c, int(k), &mut r);
                    set(es_c, Id(s), &mut r);
                    set(ep_c, Id(p), &mut r);
                    set(eo_c, Id(o), &mut r);
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
    Ok((out, counters))
}
