//! Graph algorithms in SPARQL (spec F14): `SERVICE algo:pageRank { … }` and the others of
//! [`Algo`].
//!
//! ```sparql
//! PREFIX algo: <urn:x-sparkles:algo#>
//! SELECT ?person ?rank WHERE {
//!   SERVICE algo:pageRank {
//!     [] algo:predicate foaf:knows ; algo:node ?person ; algo:score ?rank .
//!   }
//! }
//! ORDER BY DESC(?rank) LIMIT 20
//! ```
//!
//! The call is planned as an `AlgoCall` leaf. It builds a projection of the active graph
//! (see [`crate::algo`]) from the predicates of the block, runs the algorithm on it, and
//! returns one row per node, per node pair or per node and level. A variable `algo:source`
//! makes the leaf read the rest of its join group, which binds the sources. The leaf then
//! runs once for all of them and joins its rows with the group's.
//!
//! The projection is read from the permutations of the query's snapshot, so it holds
//! what the caller may read: the graphs of the view and the masked triples.

use super::ctx::Ctx;
use super::plan::{ActiveGraph, GraphFilter, Kind, Node, PT, PathEnd, Planner};
use super::table::{Table, VarId};
use super::value::Value;
use crate::algo::graph::OwnedRawEdges;
use crate::algo::memory::{self, Buffer, Reservation, Work};
use crate::algo::{self, Aggregate, Control, Graph, Par};
use crate::error::{Error, Result};
use crate::id::{Id, Tag};
use crate::index::{Perm, pad};
use crate::store::Chunk;
use oxrdf::Term;
use spargebra::algebra::GraphPattern;
use spargebra::term::{NamedNodePattern, TermPattern};
use std::collections::BTreeMap;
use std::hash::{Hash, Hasher};
use std::time::Instant;

/// The namespace of the algorithms and their parameters (`PREFIX algo:
/// <urn:x-sparkles:algo#>`).
pub const NS: &str = "urn:x-sparkles:algo#";
const RDF_TYPE: &str = "http://www.w3.org/1999/02/22-rdf-syntax-ns#type";
const RDF_REIFIES: &str = "http://www.w3.org/1999/02/22-rdf-syntax-ns#reifies";
const RDF_JSON: &str = "http://www.w3.org/1999/02/22-rdf-syntax-ns#JSON";

/// Whether a SERVICE name is one of the algorithms.
pub fn is_algo(name: &NamedNodePattern) -> bool {
    matches!(name, NamedNodePattern::NamedNode(n) if Algo::from_iri(n.as_str()).is_some())
}

fn bad(m: impl std::fmt::Display) -> Error {
    Error::invalid(format!("algo: {m}"))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Algo {
    PageRank,
    Betweenness,
    Wcc,
    Scc,
    LabelPropagation,
    Louvain,
    Leiden,
    NodeSimilarity,
    TriangleCount,
    Clustering,
    KCore,
    Bfs,
    Sssp,
    FastRp,
    Node2Vec,
}

/// How a parameter's value is read.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum Kd {
    /// a non-negative integer
    Int,
    /// a finite number
    Float,
}

/// The numeric parameters the algorithms take.
pub(super) const NUMERIC: &[(&str, Kd)] = &[
    ("dampingFactor", Kd::Float),
    ("maxIterations", Kd::Int),
    ("tolerance", Kd::Float),
    ("samples", Kd::Int),
    ("seed", Kd::Int),
    ("resolution", Kd::Float),
    ("maxLevels", Kd::Int),
    ("randomness", Kd::Float),
    ("topK", Kd::Int),
    ("similarityCutoff", Kd::Float),
    ("degreeCutoff", Kd::Int),
    ("maxPairs", Kd::Int),
    ("dimension", Kd::Int),
    ("normalizationStrength", Kd::Float),
    ("walkLength", Kd::Int),
    ("walksPerNode", Kd::Int),
    ("windowSize", Kd::Int),
    ("returnFactor", Kd::Float),
    ("inOutFactor", Kd::Float),
    ("negativeSamples", Kd::Int),
    ("epochs", Kd::Int),
    ("learningRate", Kd::Float),
    ("concurrency", Kd::Int),
    ("maxMemory", Kd::Int),
    ("limit", Kd::Int),
];

/// Parameters every algorithm takes besides the projection's.
const COMMON: &[&str] = &["concurrency", "maxMemory", "limit"];

impl Algo {
    /// The algorithm named `name` (`pageRank`, `fastRP`, …).
    pub fn from_name(name: &str) -> Option<Algo> {
        Algo::from_iri(&format!("{NS}{name}"))
    }

    fn from_iri(iri: &str) -> Option<Algo> {
        Some(match iri.strip_prefix(NS)? {
            "pageRank" => Algo::PageRank,
            "betweenness" => Algo::Betweenness,
            "wcc" => Algo::Wcc,
            "scc" => Algo::Scc,
            "labelPropagation" => Algo::LabelPropagation,
            "louvain" => Algo::Louvain,
            "leiden" => Algo::Leiden,
            "nodeSimilarity" => Algo::NodeSimilarity,
            "triangleCount" => Algo::TriangleCount,
            "clustering" => Algo::Clustering,
            "kCore" => Algo::KCore,
            "bfs" => Algo::Bfs,
            "sssp" => Algo::Sssp,
            "fastRP" => Algo::FastRp,
            "node2vec" => Algo::Node2Vec,
            _ => return None,
        })
    }

    pub fn name(self) -> &'static str {
        match self {
            Algo::PageRank => "pageRank",
            Algo::Betweenness => "betweenness",
            Algo::Wcc => "wcc",
            Algo::Scc => "scc",
            Algo::LabelPropagation => "labelPropagation",
            Algo::Louvain => "louvain",
            Algo::Leiden => "leiden",
            Algo::NodeSimilarity => "nodeSimilarity",
            Algo::TriangleCount => "triangleCount",
            Algo::Clustering => "clustering",
            Algo::KCore => "kCore",
            Algo::Bfs => "bfs",
            Algo::Sssp => "sssp",
            Algo::FastRp => "fastRP",
            Algo::Node2Vec => "node2vec",
        }
    }

    fn default_direction(self) -> Dir {
        match self {
            Algo::PageRank
            | Algo::Betweenness
            | Algo::Scc
            | Algo::NodeSimilarity
            | Algo::Bfs
            | Algo::Sssp
            | Algo::Node2Vec => Dir::Forward,
            _ => Dir::Undirected,
        }
    }

    /// The outputs of the algorithm besides `node` and `summary`.
    pub fn outputs(self) -> &'static [&'static str] {
        match self {
            Algo::PageRank | Algo::Betweenness => &["score"],
            Algo::Wcc | Algo::Scc => &["component"],
            Algo::LabelPropagation => &["community"],
            Algo::Louvain | Algo::Leiden => &["community", "level"],
            Algo::NodeSimilarity => &["otherNode", "similarity"],
            Algo::TriangleCount => &["triangles"],
            Algo::Clustering => &["coefficient", "triangles"],
            Algo::KCore => &["core"],
            Algo::Bfs => &["depth"],
            Algo::Sssp => &["distance"],
            Algo::FastRp | Algo::Node2Vec => &["embedding"],
        }
    }

    /// The numeric and other parameters of the algorithm.
    fn params(self) -> &'static [&'static str] {
        match self {
            Algo::PageRank => &[
                "dampingFactor",
                "maxIterations",
                "tolerance",
                "source",
                "graphalytics",
            ],
            Algo::Betweenness => &["samples", "seed"],
            Algo::Wcc | Algo::Scc | Algo::TriangleCount | Algo::KCore => &[],
            Algo::LabelPropagation => &["maxIterations"],
            Algo::Louvain => &["resolution", "maxLevels", "tolerance", "maxIterations"],
            Algo::Leiden => &[
                "resolution",
                "maxLevels",
                "tolerance",
                "maxIterations",
                "randomness",
                "seed",
            ],
            Algo::NodeSimilarity => &[
                "metric",
                "topK",
                "similarityCutoff",
                "degreeCutoff",
                "maxPairs",
            ],
            Algo::Clustering => &["graphalytics"],
            Algo::Bfs => &["source", "graphalytics"],
            Algo::Sssp => &["source", "graphalytics"],
            Algo::FastRp => &[
                "dimension",
                "iterationWeights",
                "normalizationStrength",
                "seed",
            ],
            Algo::Node2Vec => &[
                "dimension",
                "walkLength",
                "walksPerNode",
                "windowSize",
                "returnFactor",
                "inOutFactor",
                "negativeSamples",
                "epochs",
                "learningRate",
                "seed",
            ],
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Dir {
    Forward,
    Backward,
    Undirected,
}

impl Dir {
    fn name(self) -> &'static str {
        match self {
            Dir::Forward => "forward",
            Dir::Backward => "backward",
            Dir::Undirected => "undirected",
        }
    }
}

/// The edges of a projection.
#[derive(Clone, Debug)]
pub struct ProjectionSpec {
    /// predicate ids with the direction each is read in; `None`: every predicate between
    /// resources except `rdf:type`. A predicate the store does not have is left out.
    pub predicates: Option<Vec<(u64, Option<Dir>)>>,
    pub direction: Option<Dir>,
    /// the `rdf:type` classes whose instances are the nodes (an empty list: any node)
    pub node_classes: Vec<u64>,
    pub node_class_names: Vec<String>,
    /// the weight property: `Some(None)` when the store does not have it
    pub weight: Option<Option<u64>>,
    pub default_weight: f64,
    pub aggregate: Aggregate,
    /// edges from a nested pattern (the leaf's last child): the variables of an edge's
    /// ends and weight
    pub pattern: Option<(VarId, VarId, Option<VarId>)>,
    pub graph: GraphFilter,
    pub rdf_type: Option<u64>,
    pub reifies: Option<u64>,
}

/// The variables the outputs are bound to.
#[derive(Clone, Debug, Default)]
pub struct Outs {
    pub node: Option<VarId>,
    pub score: Option<VarId>,
    pub component: Option<VarId>,
    pub community: Option<VarId>,
    pub level: Option<VarId>,
    pub other_node: Option<VarId>,
    pub similarity: Option<VarId>,
    pub triangles: Option<VarId>,
    pub coefficient: Option<VarId>,
    pub core: Option<VarId>,
    pub depth: Option<VarId>,
    pub distance: Option<VarId>,
    pub embedding: Option<VarId>,
    pub summary: Option<VarId>,
}

impl Outs {
    fn slot(&mut self, name: &str) -> Option<&mut Option<VarId>> {
        Some(match name {
            "node" => &mut self.node,
            "score" => &mut self.score,
            "component" => &mut self.component,
            "community" => &mut self.community,
            "level" => &mut self.level,
            "otherNode" => &mut self.other_node,
            "similarity" => &mut self.similarity,
            "triangles" => &mut self.triangles,
            "coefficient" => &mut self.coefficient,
            "core" => &mut self.core,
            "depth" => &mut self.depth,
            "distance" => &mut self.distance,
            "embedding" => &mut self.embedding,
            "summary" => &mut self.summary,
            _ => return None,
        })
    }

    fn vars(&self) -> Vec<VarId> {
        let mut v: Vec<VarId> = [
            self.node,
            self.score,
            self.component,
            self.community,
            self.level,
            self.other_node,
            self.similarity,
            self.triangles,
            self.coefficient,
            self.core,
            self.depth,
            self.distance,
            self.embedding,
            self.summary,
        ]
        .into_iter()
        .flatten()
        .collect();
        v.dedup();
        v
    }
}

/// A planned algorithm call.
#[derive(Clone, Debug)]
pub struct AlgoSpec {
    pub algo: Algo,
    pub proj: ProjectionSpec,
    pub nums: BTreeMap<String, f64>,
    pub metric: Option<algo::similarity::Metric>,
    pub graphalytics: bool,
    pub iteration_weights: Option<Vec<f64>>,
    pub sources: Vec<PathEnd>,
    pub outs: Outs,
    /// `GRAPH ?g` around the call: the algorithm runs once per named graph and binds the
    /// graph here
    pub graph_var: Option<VarId>,
}

impl AlgoSpec {
    /// Whether the call reads the rest of its group: a variable source.
    pub fn needs_input(&self) -> bool {
        self.sources.iter().any(|s| matches!(s, PathEnd::Var(_)))
    }

    fn num(&self, name: &str) -> Option<f64> {
        self.nums.get(name).copied()
    }

    fn int(&self, name: &str, default: u64) -> u64 {
        self.num(name).map_or(default, |x| x as u64)
    }

    fn float(&self, name: &str, default: f64) -> f64 {
        self.num(name).unwrap_or(default)
    }

    fn direction(&self) -> Dir {
        self.proj.direction.unwrap_or_else(|| {
            if matches!(self.algo, Algo::Clustering) && self.graphalytics {
                Dir::Forward
            } else {
                self.algo.default_direction()
            }
        })
    }

    fn stats_only(&self) -> bool {
        self.outs.summary.is_some() && self.outs.vars().len() == 1
    }
}

// ----------------------------------------------------------------- planning ------

fn number(t: &TermPattern, name: &str, kind: Kd) -> Result<f64> {
    let TermPattern::Literal(l) = t else {
        return Err(bad(format!("algo:{name} takes a number")));
    };
    let v = super::value::approx_f64(&Value::from_literal(l))
        .filter(|x| x.is_finite())
        .ok_or_else(|| bad(format!("algo:{name} takes a number")))?;
    if kind == Kd::Int && (v < 0.0 || v.fract() != 0.0) {
        return Err(bad(format!("algo:{name} takes a non-negative integer")));
    }
    Ok(v)
}

fn dir_of(t: &TermPattern, name: &str) -> Result<Dir> {
    match t {
        TermPattern::NamedNode(n) => match n.as_str().strip_prefix(NS) {
            Some("forward") => Ok(Dir::Forward),
            Some("backward") => Ok(Dir::Backward),
            Some("undirected") => Ok(Dir::Undirected),
            _ => Err(bad(format!(
                "unknown direction {n} in algo:{name} (algo:forward, algo:backward or algo:undirected)"
            ))),
        },
        _ => Err(bad(format!("algo:{name} takes an algo: IRI"))),
    }
}

/// Plan `SERVICE algo:<name> { … }`.
pub(super) fn algo_leaf(
    p: &mut Planner<'_>,
    service: &NamedNodePattern,
    inner: &GraphPattern,
    g: &ActiveGraph,
) -> Result<Node> {
    let NamedNodePattern::NamedNode(sn) = service else {
        return Err(bad("the service must be an algo: IRI"));
    };
    let algo = Algo::from_iri(sn.as_str()).ok_or_else(|| bad(format!("unknown algorithm {sn}")))?;
    let what = format!("algo:{}", algo.name());
    let (patterns, edge_gp) = super::pathsearch::split_block(inner, NS, &what)?;
    let ctx = p.ctx;
    let mut proj = ProjectionSpec {
        predicates: None,
        direction: None,
        node_classes: Vec::new(),
        node_class_names: Vec::new(),
        weight: None,
        default_weight: 1.0,
        aggregate: Aggregate::None,
        pattern: None,
        graph: GraphFilter::All,
        rdf_type: ctx.snap.lookup_iri(RDF_TYPE).map(|i| i.0),
        reifies: ctx.snap.lookup_iri(RDF_REIFIES).map(|i| i.0),
    };
    let mut nums: BTreeMap<String, f64> = BTreeMap::new();
    let mut metric = None;
    let mut graphalytics = false;
    let mut iteration_weights = None;
    let mut sources: Vec<PathEnd> = Vec::new();
    let mut outs = Outs::default();
    let (mut start, mut end_var, mut edge_weight) = (None, None, None);
    let mut seen: Vec<String> = Vec::new();
    let mut default_weight_given = false;
    let mut named: Option<String> = None;
    let mut pred_names: Vec<String> = Vec::new();
    let subject = patterns.first().map(|t| t.subject.clone());
    let output = |p: &Planner<'_>, t: &TermPattern, name: &str| -> Result<VarId> {
        match t {
            TermPattern::Variable(_) | TermPattern::BlankNode(_) => match p.term_pattern(t) {
                PT::V(v) => Ok(v),
                PT::C(_) => Err(bad(format!("algo:{name} must be an unbound variable"))),
            },
            _ => Err(bad(format!("algo:{name} must be a variable"))),
        }
    };
    for tp in &patterns {
        if Some(&tp.subject) != subject.as_ref() {
            return Err(bad(
                "the parameters must share one subject (one call per SERVICE)",
            ));
        }
        let NamedNodePattern::NamedNode(pn) = &tp.predicate else {
            return Err(bad("a parameter must be an algo: IRI, not a variable"));
        };
        let Some(name) = pn.as_str().strip_prefix(NS) else {
            return Err(bad(format!("unknown parameter <{}>", pn.as_str())));
        };
        let o = &tp.object;
        let repeatable = matches!(name, "predicate" | "nodeClass" | "source");
        if !repeatable && seen.iter().any(|s| s == name) {
            return Err(bad(format!("algo:{name} is given twice")));
        }
        seen.push(name.to_string());
        let projection_param = matches!(
            name,
            "predicate"
                | "direction"
                | "nodeClass"
                | "weight"
                | "defaultWeight"
                | "aggregate"
                | "start"
                | "end"
                | "edgeWeight"
                | "projection"
        );
        let takes = projection_param
            || COMMON.contains(&name)
            || algo.params().contains(&name)
            || algo.outputs().contains(&name)
            || matches!(name, "node" | "summary");
        if !takes {
            let known = NUMERIC.iter().any(|(n, _)| *n == name)
                || matches!(
                    name,
                    "metric" | "graphalytics" | "iterationWeights" | "source"
                )
                || Outs::default().slot(name).is_some();
            return Err(bad(if known {
                format!("algo:{name} is not a parameter of algo:{}", algo.name())
            } else {
                format!("unknown parameter algo:{name}")
            }));
        }
        if let Some(slot) = outs.slot(name) {
            *slot = Some(output(p, o, name)?);
            continue;
        }
        match name {
            "predicate" => {
                let (n, d) = match (o, super::pathsearch::list_items(o)) {
                    (TermPattern::NamedNode(n), _) => (n.clone(), None),
                    (_, Some(items)) => match items.as_slice() {
                        [TermPattern::NamedNode(n), d] => (n.clone(), Some(dir_of(d, name)?)),
                        _ => {
                            return Err(bad(
                                "algo:predicate takes an IRI, or a list of an IRI and a direction such as (ex:p algo:backward)",
                            ));
                        }
                    },
                    _ => return Err(bad("algo:predicate takes an IRI")),
                };
                if pred_names.iter().any(|x| x == n.as_str()) {
                    return Err(bad(format!("algo:predicate {n} is given twice")));
                }
                pred_names.push(n.as_str().to_string());
                let list = proj.predicates.get_or_insert_with(Vec::new);
                if let Some(id) = ctx.snap.lookup_iri(n.as_str()) {
                    list.push((id.0, d));
                }
            }
            "direction" => proj.direction = Some(dir_of(o, name)?),
            "nodeClass" => {
                let TermPattern::NamedNode(n) = o else {
                    return Err(bad("algo:nodeClass takes an IRI"));
                };
                proj.node_class_names.push(n.as_str().to_string());
                if let Some(id) = ctx.snap.lookup_iri(n.as_str()) {
                    proj.node_classes.push(id.0);
                }
            }
            "weight" => {
                let TermPattern::NamedNode(n) = o else {
                    return Err(bad("algo:weight takes an IRI"));
                };
                proj.weight = Some(ctx.snap.lookup_iri(n.as_str()).map(|i| i.0));
            }
            "defaultWeight" => {
                let w = number(o, name, Kd::Float)?;
                if w < 0.0 {
                    return Err(bad("algo:defaultWeight takes a non-negative number"));
                }
                proj.default_weight = w;
                default_weight_given = true;
            }
            "aggregate" => {
                proj.aggregate = match o {
                    TermPattern::NamedNode(n) => match n.as_str().strip_prefix(NS) {
                        Some("none") => Aggregate::None,
                        Some("sum") => Aggregate::Sum,
                        Some("max") => Aggregate::Max,
                        Some("count") => Aggregate::Count,
                        _ => {
                            return Err(bad(
                                "algo:aggregate is algo:none, algo:sum, algo:max or algo:count",
                            ));
                        }
                    },
                    _ => return Err(bad("algo:aggregate takes an algo: IRI")),
                }
            }
            "start" => start = Some(output(p, o, name)?),
            "end" => end_var = Some(output(p, o, name)?),
            "edgeWeight" => edge_weight = Some(output(p, o, name)?),
            "projection" => {
                let TermPattern::Literal(l) = o else {
                    return Err(bad(
                        "algo:projection takes the name of a projection as a string",
                    ));
                };
                named = Some(l.value().to_string());
            }
            "source" => match o {
                TermPattern::Triple(_) => return Err(bad("algo:source takes a term")),
                _ => sources.push(match p.term_pattern(o) {
                    PT::C(id) => PathEnd::Const(id),
                    PT::V(v) => PathEnd::Var(v),
                }),
            },
            "metric" => {
                let m = match o {
                    TermPattern::NamedNode(n) => n.as_str().strip_prefix(NS).map(str::to_string),
                    TermPattern::Literal(l) => Some(l.value().to_string()),
                    _ => None,
                };
                metric = Some(match m.as_deref() {
                    Some("jaccard") => algo::similarity::Metric::Jaccard,
                    Some("overlap") => algo::similarity::Metric::Overlap,
                    _ => return Err(bad("algo:metric is \"jaccard\" or \"overlap\"")),
                });
            }
            "graphalytics" => {
                graphalytics = match o {
                    TermPattern::Literal(l) if l.value() == "true" || l.value() == "1" => true,
                    TermPattern::Literal(l) if l.value() == "false" || l.value() == "0" => false,
                    _ => return Err(bad("algo:graphalytics takes true or false")),
                }
            }
            "iterationWeights" => {
                let Some(items) = super::pathsearch::list_items(o) else {
                    return Err(bad("algo:iterationWeights takes a list of numbers"));
                };
                iteration_weights = Some(
                    items
                        .iter()
                        .map(|i| number(i, name, Kd::Float))
                        .collect::<Result<Vec<f64>>>()?,
                );
            }
            _ => {
                let Some((n, kind)) = NUMERIC.iter().find(|(n, _)| *n == name) else {
                    return Err(bad(format!("unknown parameter algo:{name}")));
                };
                nums.insert(n.to_string(), number(o, name, *kind)?);
            }
        }
    }
    if let Some(name) = &named {
        let explicit = seen.iter().any(|s| {
            matches!(
                s.as_str(),
                "predicate"
                    | "direction"
                    | "nodeClass"
                    | "weight"
                    | "defaultWeight"
                    | "aggregate"
                    | "start"
                    | "end"
                    | "edgeWeight"
            )
        });
        if explicit || edge_gp.is_some() {
            return Err(bad(
                "algo:projection cannot be combined with other projection parameters or a nested pattern",
            ));
        }
        let def = ctx
            .algo_projections
            .as_ref()
            .and_then(|c| c.get(name))
            .ok_or_else(|| bad(format!("no named projection '{name}' on this dataset")))?;
        let dir = |d: &str| match d {
            "forward" => Dir::Forward,
            "backward" => Dir::Backward,
            _ => Dir::Undirected,
        };
        if let Some(preds) = &def.predicates {
            let list = proj.predicates.get_or_insert_with(Vec::new);
            for pd in preds {
                pred_names.push(pd.iri().to_string());
                if let Some(id) = ctx.snap.lookup_iri(pd.iri()) {
                    list.push((id.0, pd.direction().map(dir)));
                }
            }
        }
        proj.direction = def.direction.as_deref().map(dir);
        for c in &def.node_classes {
            proj.node_class_names.push(c.clone());
            if let Some(id) = ctx.snap.lookup_iri(c) {
                proj.node_classes.push(id.0);
            }
        }
        if let Some(w) = &def.weight {
            proj.weight = Some(ctx.snap.lookup_iri(w).map(|i| i.0));
        }
        if let Some(w) = def.default_weight {
            proj.default_weight = w;
        }
        proj.aggregate = match def.aggregate.as_deref() {
            Some("sum") => Aggregate::Sum,
            Some("max") => Aggregate::Max,
            Some("count") => Aggregate::Count,
            _ => Aggregate::None,
        };
    }
    if default_weight_given && proj.weight.is_none() && edge_weight.is_none() {
        return Err(bad("algo:defaultWeight needs algo:weight"));
    }
    // edges from a nested pattern
    let edges_node = match (&edge_gp, start, end_var) {
        (None, None, None) if edge_weight.is_none() => None,
        (Some(gp), Some(s), Some(e)) if s != e => {
            if proj.predicates.is_some() || proj.weight.is_some() || !proj.node_classes.is_empty() {
                return Err(bad(
                    "algo:predicate, algo:weight and algo:nodeClass cannot be combined with a nested pattern of edges; put them in the pattern",
                ));
            }
            let n = p.plan(gp, g, Vec::new())?;
            for (v, name) in [
                (Some(s), "start"),
                (Some(e), "end"),
                (edge_weight, "edgeWeight"),
            ] {
                if let Some(v) = v
                    && !n.vars.contains(&v)
                {
                    return Err(bad(format!(
                        "algo:{name} ?{} is not a variable of the nested pattern",
                        ctx.var_name(v)
                    )));
                }
            }
            proj.pattern = Some((s, e, edge_weight));
            Some(n)
        }
        (Some(_), Some(_), Some(_)) => {
            return Err(bad("algo:start and algo:end need two variables"));
        }
        (Some(_), _, _) => {
            return Err(bad(
                "a nested pattern of edges needs algo:start and algo:end, the variables of each edge's ends",
            ));
        }
        _ => {
            return Err(bad(
                "algo:start, algo:end and algo:edgeWeight name variables of a nested pattern of edges, and the block has none",
            ));
        }
    };
    // required and conflicting parameters
    match algo {
        Algo::Bfs | Algo::Sssp if sources.is_empty() => {
            return Err(bad(format!("algo:{} needs algo:source", algo.name())));
        }
        _ => {}
    }
    if sources
        .iter()
        .filter(|s| matches!(s, PathEnd::Var(_)))
        .count()
        > 1
    {
        return Err(bad("at most one algo:source can be a variable"));
    }
    if let Some(m) = nums.get("dampingFactor")
        && !(0.0..1.0).contains(m)
    {
        return Err(bad("algo:dampingFactor must be in [0, 1)"));
    }
    for (name, min) in [
        ("dimension", 1.0),
        ("topK", 1.0),
        ("walkLength", 2.0),
        ("walksPerNode", 1.0),
        ("windowSize", 1.0),
        ("epochs", 1.0),
        ("maxLevels", 1.0),
    ] {
        if nums.get(name).is_some_and(|v| *v < min) {
            return Err(bad(format!("algo:{name} must be at least {min}")));
        }
    }
    // each output its own variable
    let ovars: Vec<VarId> = [
        outs.node,
        outs.score,
        outs.component,
        outs.community,
        outs.level,
        outs.other_node,
        outs.similarity,
        outs.triangles,
        outs.coefficient,
        outs.core,
        outs.depth,
        outs.distance,
        outs.embedding,
        outs.summary,
    ]
    .into_iter()
    .flatten()
    .collect();
    for (i, v) in ovars.iter().enumerate() {
        if ovars[..i].contains(v) {
            return Err(bad(format!(
                "?{} is bound by two parameters; each output needs its own variable",
                ctx.var_name(*v)
            )));
        }
        if sources
            .iter()
            .any(|s| matches!(s, PathEnd::Var(x) if x == v))
        {
            return Err(bad(format!(
                "?{} is both algo:source and an output",
                ctx.var_name(*v)
            )));
        }
    }
    let mut vars = outs.vars();
    for s in &sources {
        if let PathEnd::Var(v) = s
            && !vars.contains(v)
        {
            vars.push(*v);
        }
    }
    let Some((graph, graph_var)) = p.graph_filter(g) else {
        return Ok(Node::empty(vars));
    };
    if let Some(gv) = graph_var {
        if outs.vars().contains(&gv)
            || sources
                .iter()
                .any(|s| matches!(s, PathEnd::Var(x) if *x == gv))
        {
            return Err(bad("the graph variable is also bound by a parameter"));
        }
        if !vars.contains(&gv) {
            vars.push(gv);
        }
    }
    proj.graph = graph;
    let spec = AlgoSpec {
        algo,
        proj,
        nums,
        metric,
        graphalytics,
        iteration_weights,
        sources,
        outs,
        graph_var,
    };
    let desc = format!(
        "{} on {} {}{}",
        algo.name(),
        if spec.proj.pattern.is_some() {
            "the nested pattern".to_string()
        } else if pred_names.is_empty() {
            "every predicate but rdf:type".to_string()
        } else {
            pred_names
                .iter()
                .map(|p| format!("<{p}>"))
                .collect::<Vec<_>>()
                .join(" ")
        },
        spec.direction().name(),
        if spec.proj.node_class_names.is_empty() {
            String::new()
        } else {
            format!(" nodes of {}", spec.proj.node_class_names.join(" "))
        },
    );
    let est = 1000.0;
    let mut n = Node::leaf(Kind::AlgoCall(Box::new(spec)), vars, est, desc);
    if let Some(e) = edges_node {
        n.cost += e.cost;
        n.children.push(e);
    }
    n.cost = 100_000.0;
    Ok(n)
}

/// The call attached to the rest of its group (`left`), which binds its source.
pub(super) fn attach(p: &Planner<'_>, left: Node, call: Node) -> Result<Node> {
    let Kind::AlgoCall(spec) = &call.kind else {
        unreachable!("an algorithm call");
    };
    for s in &spec.sources {
        if let PathEnd::Var(v) = s
            && !left.vars.contains(v)
        {
            return Err(bad(format!(
                "algo:source ?{} is not bound by the rest of the group; bind it, for example with VALUES",
                p.ctx.var_name(*v)
            )));
        }
    }
    let mut vars = left.vars.clone();
    for v in &call.vars {
        if !vars.contains(v) {
            vars.push(*v);
        }
    }
    let mut certain = left.certain.clone();
    certain.extend(call.certain.iter().copied());
    let est = (left.est * call.est).max(1.0);
    let mut children = vec![left];
    children.extend(call.children);
    Ok(Node {
        dist: vars.iter().map(|&v| (v, est)).collect(),
        cost: children[0].cost + call.cost,
        vars,
        certain,
        sorted: Vec::new(),
        est,
        desc: call.desc,
        kind: call.kind,
        children,
    })
}

// ----------------------------------------------------------------- execution ------

/// Run a call: over the rows of `input` when the leaf reads its group.
pub(super) fn run(
    ctx: &Ctx,
    spec: &AlgoSpec,
    input: Option<Table>,
    edges: Option<Table>,
    vars: &[VarId],
) -> Result<(Table, serde_json::Map<String, serde_json::Value>)> {
    let started = Instant::now();
    let limit = spec.num("maxMemory").map_or(u64::MAX, |m| m as u64);
    let ctl = Control::with_cancellation(
        ctx.cancel.clone(),
        ctx.cancellation.clone(),
        ctx.deadline,
        limit.min(ctx.memory_remaining()),
    );
    let par = Par::acquire(spec.int("concurrency", 0) as usize, &ctl)?;
    let mut counters = serde_json::Map::new();
    let result_vars: Vec<VarId> = vars
        .iter()
        .copied()
        .filter(|v| spec.outs.vars().contains(v) || Some(*v) == spec.graph_var)
        .collect();
    // one projection, or one for each named graph under GRAPH ?g
    let graphs: Vec<(GraphFilter, Option<Id>)> = match spec.graph_var {
        None => vec![(spec.proj.graph.clone(), None)],
        Some(_) => ctx
            .snap
            .graph_ids()?
            .into_iter()
            .filter(|g| spec.proj.graph.accepts(g.0))
            .map(|g| (GraphFilter::One(g.0), Some(g)))
            .collect(),
    };
    let mut t = Table::empty(result_vars.clone());
    let mut built = std::time::Duration::ZERO;
    let (mut nodes, mut edge_count, mut cache_hits) = (0usize, 0usize, 0usize);
    for (filter, gid) in graphs {
        let at = Instant::now();
        let mut one = spec.clone();
        one.proj.graph = filter;
        let (g, hit) = projection(ctx, &one, edges.as_ref(), &par, &ctl)?;
        built += at.elapsed();
        cache_hits += hit as usize;
        nodes += g.n();
        edge_count += g.m();
        // the sources: node indexes of the constants and of the group's bound values
        let mut sources: Vec<u32> = Vec::new();
        let mut want_sources = false;
        for s in &spec.sources {
            want_sources = true;
            let mut add = |id: Id| {
                if let Ok(i) = g.ids.binary_search(&id.0) {
                    sources.push(i as u32);
                }
            };
            match s {
                PathEnd::Const(id) => add(*id),
                PathEnd::Var(v) => {
                    if let Some(t) = input.as_ref()
                        && let Some(c) = t.col_of(*v)
                    {
                        for &id in &t.cols[c] {
                            if !id.is_undef() {
                                add(id);
                            }
                        }
                    }
                }
            }
        }
        sources.sort_unstable();
        sources.dedup();
        let mut part = if want_sources
            && sources.is_empty()
            && matches!(spec.algo, Algo::Bfs | Algo::Sssp)
            && !spec.stats_only()
        {
            // no source is a node of the projection: nothing is reached
            Table::empty(result_vars.clone())
        } else {
            rows(ctx, spec, &g, &sources, &par, &ctl, &result_vars)?
        };
        if let (Some(gid), Some(gv)) = (gid, spec.graph_var)
            && let Some(c) = part.col_of(gv)
        {
            part.cols[c] = vec![gid; part.len];
        }
        t.append(part);
    }
    counters.insert("nodes".into(), nodes.into());
    counters.insert("cachedProjections".into(), cache_hits.into());
    counters.insert("edges".into(), edge_count.into());
    counters.insert("projectionMs".into(), (built.as_secs_f64() * 1000.0).into());
    counters.insert(
        "runMs".into(),
        ((started.elapsed() - built).as_secs_f64() * 1000.0).into(),
    );
    counters.insert("peakBytes".into(), ctl.peak().into());
    let _ = ctx.charge(ctl.peak())?;
    // the call's rows joined with the rest of its group
    let out = match input {
        Some(inp) => super::exec::join_tables(ctx, &inp, &t, &[], false)?,
        None => t,
    };
    ctx.produced(out.len())?;
    Ok((out, counters))
}

/// The id of an `xsd:double`. A value whose low four mantissa bits are not zero loses
/// them (a relative error under 2^-48), so that every score is an inline id and the
/// vocabulary is not locked once for each of millions of rows.
fn dbl(x: f64) -> Id {
    Id::from_f64(f64::from_bits(x.to_bits() & !0xF)).unwrap_or(Id::UNDEF)
}

fn int(ctx: &Ctx, x: i64) -> Id {
    Id::from_i64(x).unwrap_or_else(|| ctx.intern_value(&Value::Integer(x.into())))
}

/// One output column being filled.
struct Col {
    var: VarId,
    pos: usize,
}

/// Table columns by the variable they bind.
struct Builder {
    vars: Vec<VarId>,
    cols: Vec<Vec<Id>>,
}

impl Builder {
    fn new(vars: &[VarId]) -> Builder {
        Builder {
            vars: vars.to_vec(),
            cols: vars.iter().map(|_| Vec::new()).collect(),
        }
    }

    fn col(&self, v: Option<VarId>) -> Option<Col> {
        let v = v?;
        self.vars
            .iter()
            .position(|x| *x == v)
            .map(|pos| Col { var: v, pos })
    }

    fn set(&mut self, c: &Option<Col>, f: impl FnMut(usize) -> Id, n: usize) {
        if let Some(c) = c {
            let mut f = f;
            self.cols[c.pos] = (0..n).map(&mut f).collect();
            let _ = c.var;
        }
    }

    fn finish(self, len: usize, ctx: &Ctx) -> Result<Table> {
        ctx.check_output(len, self.vars.len())?;
        Ok(Table {
            vars: self.vars,
            cols: self.cols,
            len,
            sorted: Vec::new(),
        })
    }
}

/// The summary literal of a run.
fn summary(ctx: &Ctx, g: &Graph, stats: algo::Stats) -> Id {
    let mut m = stats;
    m.insert("nodes".into(), g.n().into());
    m.insert("edges".into(), g.m().into());
    let text = serde_json::Value::Object(m).to_string();
    ctx.intern_term(&Term::Literal(oxrdf::Literal::new_typed_literal(
        text,
        oxrdf::NamedNode::new_unchecked(RDF_JSON),
    )))
}

/// Stats have one row independently of the algorithm's node, pair or level rows.
fn summary_row(
    ctx: &Ctx,
    spec: &AlgoSpec,
    g: &Graph,
    stats: algo::Stats,
    vars: &[VarId],
) -> Result<Table> {
    ctx.check()?;
    ctx.check_output(1, vars.len())?;
    let mut b = Builder::new(vars);
    let col = b.col(spec.outs.summary);
    let sm = summary(ctx, g, stats);
    b.set(&col, |_| sm, 1);
    b.finish(1, ctx)
}

/// The indexes of the nodes to report, best first by `key` when a limit asks for the top
/// rows, else in node order.
fn pick(n: usize, limit: Option<usize>, key: Option<&[f64]>, descending: bool) -> Vec<u32> {
    let mut idx: Vec<u32> = (0..n as u32).collect();
    if let (Some(l), Some(k)) = (limit, key) {
        idx.sort_by(|&a, &b| {
            let o = k[a as usize].total_cmp(&k[b as usize]);
            let o = if descending { o.reverse() } else { o };
            o.then(a.cmp(&b))
        });
        idx.truncate(l);
    } else if let Some(l) = limit {
        idx.truncate(l);
    }
    idx
}

#[allow(clippy::too_many_arguments)]
fn rows(
    ctx: &Ctx,
    spec: &AlgoSpec,
    g: &Graph,
    sources: &[u32],
    par: &Par,
    ctl: &Control,
    vars: &[VarId],
) -> Result<Table> {
    let o = &spec.outs;
    // a call that asks for the summary alone reports one row (GDS's stats mode)
    let stats_only = spec.stats_only();
    let limit = spec.num("limit").map(|l| l as usize);
    let mut b = Builder::new(vars);
    let c_node = b.col(o.node);
    let c_summary = b.col(o.summary);
    let n = g.n();
    let node_id = |i: usize| Id(g.ids[i]);
    // columns of the per-node algorithms: the reported nodes and their values
    macro_rules! per_node {
        ($key:expr, $desc:expr, $stats:expr, $cols:expr) => {{
            if stats_only {
                return summary_row(ctx, spec, g, $stats, vars);
            }
            let key: Option<Vec<f64>> = $key;
            let idx = pick(n, limit, key.as_deref(), $desc);
            ctx.check_output(idx.len(), vars.len())?;
            b.set(&c_node, |i| node_id(idx[i] as usize), idx.len());
            let sm = summary(ctx, g, $stats);
            b.set(&c_summary, |_| sm, idx.len());
            #[allow(clippy::redundant_closure_call)]
            ($cols)(&mut b, &idx);
            return b.finish(idx.len(), ctx);
        }};
    }
    match spec.algo {
        Algo::PageRank => {
            let p = algo::pagerank::Params {
                damping: spec.float("dampingFactor", 0.85),
                max_iterations: spec.int("maxIterations", 20) as u32,
                tolerance: spec.float("tolerance", 1e-7),
                sources: sources.to_vec(),
                fixed: spec.graphalytics,
            };
            if !spec.sources.is_empty() && sources.is_empty() {
                if stats_only {
                    // An explicitly empty personalization is no work, not a uniform run.
                    let mut stats = algo::Stats::new();
                    stats.insert("iterations".into(), 0.into());
                    stats.insert("converged".into(), true.into());
                    return summary_row(ctx, spec, g, stats, vars);
                }
                return b.finish(0, ctx);
            }
            let r = algo::pagerank::run(g, &p, par, ctl)?;
            let c = b.col(o.score);
            let score = r.score;
            per_node!(
                Some(score.clone()),
                true,
                r.stats,
                |b: &mut Builder, idx: &Vec<u32>| {
                    b.set(&c, |i| dbl(score[idx[i] as usize]), idx.len());
                }
            )
        }
        Algo::Betweenness => {
            let max = crate::algo::pool::max_exact_betweenness();
            let samples = spec.int("samples", 0) as usize;
            if samples == 0 && n > max {
                return Err(bad(format!(
                    "betweenness without algo:samples on {n} nodes passes the limit of {max}; use algo:samples to estimate it"
                )));
            }
            let p = algo::betweenness::Params {
                samples,
                seed: spec.int("seed", 0),
            };
            let score = algo::betweenness::run(g, &p, par, ctl)?;
            let c = b.col(o.score);
            let mut stats = algo::Stats::new();
            stats.insert("samples".into(), samples.into());
            per_node!(
                Some(score.clone()),
                true,
                stats,
                |b: &mut Builder, idx: &Vec<u32>| {
                    b.set(&c, |i| dbl(score[idx[i] as usize]), idx.len());
                }
            )
        }
        Algo::Wcc | Algo::Scc => {
            let r = if spec.algo == Algo::Wcc {
                algo::components::wcc(g, par, ctl)?
            } else {
                algo::components::scc(g, ctl)?
            };
            let c = b.col(o.component);
            let mut stats = algo::Stats::new();
            stats.insert("components".into(), r.count.into());
            per_node!(None, false, stats, |b: &mut Builder, idx: &Vec<u32>| {
                b.set(&c, |i| int(ctx, r.comp[idx[i] as usize] as i64), idx.len());
            })
        }
        Algo::LabelPropagation => {
            let r = algo::labelprop::run(g, spec.int("maxIterations", 10) as u32, par, ctl)?;
            // the community is the node it is named for, as a store term
            let c = b.col(o.community);
            let mut stats = algo::Stats::new();
            stats.insert("iterations".into(), r.iterations.into());
            stats.insert("converged".into(), r.converged.into());
            per_node!(None, false, stats, |b: &mut Builder, idx: &Vec<u32>| {
                b.set(&c, |i| int(ctx, r.label[idx[i] as usize] as i64), idx.len());
            })
        }
        Algo::Louvain | Algo::Leiden => {
            let p = algo::community::Params {
                resolution: spec.float("resolution", 1.0),
                max_levels: spec.int("maxLevels", 10) as u32,
                tolerance: spec.float("tolerance", 1e-4),
                leiden: spec.algo == Algo::Leiden,
                randomness: spec.float("randomness", 0.01),
                seed: spec.int("seed", 0),
                max_sweeps: spec.int("maxIterations", 100) as u32,
            };
            let r = algo::community::run(g, &p, ctl)?;
            let mut stats = algo::Stats::new();
            stats.insert("levels".into(), r.levels.len().into());
            stats.insert(
                "modularity".into(),
                serde_json::json!(r.modularity.last().copied()),
            );
            stats.insert("modularities".into(), serde_json::json!(r.modularity));
            stats.insert(
                "communities".into(),
                r.levels
                    .last()
                    .map_or(0, |l| l.iter().max().map_or(0, |m| *m as usize + 1))
                    .into(),
            );
            if stats_only {
                return summary_row(ctx, spec, g, stats, vars);
            }
            let sm = summary(ctx, g, stats);
            let (c_comm, c_level) = (b.col(o.community), b.col(o.level));
            // one row per node and level when the level is asked for, else the last level
            let levels: Vec<usize> = if o.level.is_some() {
                (0..r.levels.len()).collect()
            } else {
                r.levels.len().checked_sub(1).into_iter().collect()
            };
            let mut pairs: Vec<(u32, usize)> = Vec::new();
            for &l in &levels {
                for i in 0..n as u32 {
                    pairs.push((i, l));
                }
            }
            if let Some(l) = limit {
                pairs.truncate(l);
            }
            ctx.check_output(pairs.len(), vars.len())?;
            b.set(&c_node, |i| node_id(pairs[i].0 as usize), pairs.len());
            b.set(
                &c_comm,
                |i| int(ctx, r.levels[pairs[i].1][pairs[i].0 as usize] as i64),
                pairs.len(),
            );
            b.set(&c_level, |i| int(ctx, pairs[i].1 as i64), pairs.len());
            b.set(&c_summary, |_| sm, pairs.len());
            b.finish(pairs.len(), ctx)
        }
        Algo::NodeSimilarity => {
            let p = algo::similarity::Params {
                metric: spec.metric.unwrap_or(algo::similarity::Metric::Jaccard),
                top_k: spec.int("topK", 10) as usize,
                similarity_cutoff: spec.float("similarityCutoff", 1e-42),
                degree_cutoff: spec.int("degreeCutoff", 1) as usize,
                max_pairs: spec.int("maxPairs", 10_000_000),
            };
            let mut r = algo::similarity::run(g, &p, par, ctl)?;
            if stats_only {
                let mut stats = algo::Stats::new();
                stats.insert("pairs".into(), r.len().into());
                return summary_row(ctx, spec, g, stats, vars);
            }
            if let Some(l) = limit {
                r.sort_by(|a, b| b.2.total_cmp(&a.2).then(a.0.cmp(&b.0)).then(a.1.cmp(&b.1)));
                r.truncate(l);
            }
            ctx.check_output(r.len(), vars.len())?;
            let (c_other, c_sim) = (b.col(o.other_node), b.col(o.similarity));
            b.set(&c_node, |i| node_id(r[i].0 as usize), r.len());
            b.set(&c_other, |i| node_id(r[i].1 as usize), r.len());
            b.set(&c_sim, |i| dbl(r[i].2), r.len());
            let mut stats = algo::Stats::new();
            stats.insert("pairs".into(), r.len().into());
            let sm = summary(ctx, g, stats);
            b.set(&c_summary, |_| sm, r.len());
            b.finish(r.len(), ctx)
        }
        Algo::TriangleCount => {
            let t = algo::triangles::count(g, par, ctl)?;
            let c = b.col(o.triangles);
            let mut stats = algo::Stats::new();
            stats.insert(
                "triangles".into(),
                (t.iter().map(|&x| x as u64).sum::<u64>() / 3).into(),
            );
            per_node!(
                Some(t.iter().map(|&x| x as f64).collect()),
                true,
                stats,
                |b: &mut Builder, idx: &Vec<u32>| {
                    b.set(&c, |i| int(ctx, t[idx[i] as usize] as i64), idx.len());
                }
            )
        }
        Algo::Clustering => {
            let (coef, tri) = if spec.graphalytics {
                (algo::triangles::clustering_graphalytics(g, par, ctl)?, None)
            } else {
                let r = algo::triangles::clustering(g, par, ctl)?;
                (r.coefficient, Some(r.triangles))
            };
            let (c_coef, c_tri) = (b.col(o.coefficient), b.col(o.triangles));
            if o.triangles.is_some() && tri.is_none() {
                return Err(bad(
                    "algo:triangles is not available with algo:graphalytics true; Graphalytics' coefficient counts directed edges",
                ));
            }
            let mut stats = algo::Stats::new();
            let mean = if n == 0 {
                0.0
            } else {
                coef.iter().sum::<f64>() / n as f64
            };
            stats.insert("averageCoefficient".into(), serde_json::json!(mean));
            per_node!(
                Some(coef.clone()),
                true,
                stats,
                |b: &mut Builder, idx: &Vec<u32>| {
                    b.set(&c_coef, |i| dbl(coef[idx[i] as usize]), idx.len());
                    if let Some(t) = &tri {
                        b.set(&c_tri, |i| int(ctx, t[idx[i] as usize] as i64), idx.len());
                    }
                }
            )
        }
        Algo::KCore => {
            let core = algo::kcore::run(g, ctl)?;
            let c = b.col(o.core);
            let mut stats = algo::Stats::new();
            stats.insert(
                "degeneracy".into(),
                core.iter().copied().max().unwrap_or(0).into(),
            );
            per_node!(
                Some(core.iter().map(|&x| x as f64).collect()),
                true,
                stats,
                |b: &mut Builder, idx: &Vec<u32>| {
                    b.set(&c, |i| int(ctx, core[idx[i] as usize] as i64), idx.len());
                }
            )
        }
        Algo::Bfs => {
            let depth = algo::traverse::bfs(g, sources, par, ctl)?;
            if stats_only {
                let mut stats = algo::Stats::new();
                stats.insert(
                    "reached".into(),
                    depth
                        .iter()
                        .filter(|&&d| d != algo::traverse::UNREACHED)
                        .count()
                        .into(),
                );
                return summary_row(ctx, spec, g, stats, vars);
            }
            let c = b.col(o.depth);
            let inf = ctx.intern_term(&Term::Literal(oxrdf::Literal::new_simple_literal("inf")));
            // unreached nodes are left out, or bound to "inf" under Graphalytics
            let keep: Vec<u32> = (0..n as u32)
                .filter(|&i| spec.graphalytics || depth[i as usize] != algo::traverse::UNREACHED)
                .collect();
            let key: Vec<f64> = depth.iter().map(|&d| d as f64).collect();
            let mut idx = keep;
            if let Some(l) = limit {
                idx.sort_by(|&a, &b2| {
                    key[a as usize]
                        .total_cmp(&key[b2 as usize])
                        .then(a.cmp(&b2))
                });
                idx.truncate(l);
            }
            ctx.check_output(idx.len(), vars.len())?;
            b.set(&c_node, |i| node_id(idx[i] as usize), idx.len());
            b.set(
                &c,
                |i| {
                    let d = depth[idx[i] as usize];
                    if d == algo::traverse::UNREACHED {
                        inf
                    } else {
                        int(ctx, d as i64)
                    }
                },
                idx.len(),
            );
            let mut stats = algo::Stats::new();
            stats.insert(
                "reached".into(),
                depth
                    .iter()
                    .filter(|&&d| d != algo::traverse::UNREACHED)
                    .count()
                    .into(),
            );
            let sm = summary(ctx, g, stats);
            b.set(&c_summary, |_| sm, idx.len());
            b.finish(idx.len(), ctx)
        }
        Algo::Sssp => {
            let dist = algo::traverse::sssp(g, sources, par, ctl)?;
            if stats_only {
                let mut stats = algo::Stats::new();
                stats.insert(
                    "reached".into(),
                    dist.iter().filter(|d| d.is_finite()).count().into(),
                );
                return summary_row(ctx, spec, g, stats, vars);
            }
            let c = b.col(o.distance);
            let mut idx: Vec<u32> = (0..n as u32)
                .filter(|&i| spec.graphalytics || dist[i as usize].is_finite())
                .collect();
            if let Some(l) = limit {
                idx.sort_by(|&a, &b2| {
                    dist[a as usize]
                        .total_cmp(&dist[b2 as usize])
                        .then(a.cmp(&b2))
                });
                idx.truncate(l);
            }
            ctx.check_output(idx.len(), vars.len())?;
            b.set(&c_node, |i| node_id(idx[i] as usize), idx.len());
            b.set(&c, |i| dbl(dist[idx[i] as usize]), idx.len());
            let mut stats = algo::Stats::new();
            stats.insert(
                "reached".into(),
                dist.iter().filter(|d| d.is_finite()).count().into(),
            );
            let sm = summary(ctx, g, stats);
            b.set(&c_summary, |_| sm, idx.len());
            b.finish(idx.len(), ctx)
        }
        Algo::FastRp | Algo::Node2Vec => {
            let dim = spec.int("dimension", 128) as usize;
            let emb = if spec.algo == Algo::FastRp {
                let p = algo::fastrp::Params {
                    dimension: dim,
                    iteration_weights: spec
                        .iteration_weights
                        .clone()
                        .unwrap_or_else(|| vec![0.0, 1.0, 1.0]),
                    normalization_strength: spec.float("normalizationStrength", 0.0),
                    seed: spec.int("seed", 0),
                };
                algo::fastrp::run(g, &p, par, ctl)?
            } else {
                let p = algo::node2vec::Params {
                    dimension: dim,
                    walk_length: spec.int("walkLength", 80) as usize,
                    walks_per_node: spec.int("walksPerNode", 10) as usize,
                    window: spec.int("windowSize", 10) as usize,
                    return_factor: spec.float("returnFactor", 1.0),
                    in_out_factor: spec.float("inOutFactor", 1.0),
                    negative_samples: spec.int("negativeSamples", 5) as usize,
                    epochs: spec.int("epochs", 1) as usize,
                    learning_rate: spec.float("learningRate", 0.025),
                    seed: spec.int("seed", 0),
                };
                algo::node2vec::run(g, &p, ctl)?
            };
            let c = b.col(o.embedding);
            let mut stats = algo::Stats::new();
            stats.insert("dimension".into(), dim.into());
            per_node!(None, false, stats, |b: &mut Builder, idx: &Vec<u32>| {
                b.set(
                    &c,
                    |i| {
                        let u = idx[i] as usize;
                        ctx.intern_term(&Term::Literal(crate::vector::literal(
                            &emb[u * dim..(u + 1) * dim],
                        )))
                    },
                    idx.len(),
                );
            })
        }
    }
}

// ------------------------------------------------------------- projections ------

/// Every collection buffer owns its admitted capacity until builder transfer.
struct Collect<'a> {
    ctl: &'a Control,
    src: Buffer<u64>,
    dst: Buffer<u64>,
    w: Option<Buffer<f32>>,
    symmetric: bool,
}
impl Collect<'_> {
    fn push(&mut self, s: u64, o: u64, w: f32, d: Dir) -> Result<()> {
        let (a, b) = if d == Dir::Backward { (o, s) } else { (s, o) };
        let mirror = d == Dir::Undirected && a != b;
        let need = self
            .src
            .len()
            .checked_add(if mirror { 2 } else { 1 })
            .ok_or_else(|| bad("edge count overflows"))?;
        // Admit every array before changing any length, including a mirrored edge.
        self.src.reserve("projection-source", need, self.ctl)?;
        self.dst.reserve("projection-destination", need, self.ctl)?;
        if let Some(ws) = &mut self.w {
            ws.reserve("projection-raw-weights", need, self.ctl)?;
        }
        self.add(a, b, w)?;
        if mirror {
            self.add(b, a, w)?;
        }
        Ok(())
    }
    fn add(&mut self, a: u64, b: u64, w: f32) -> Result<()> {
        self.src.push(a)?;
        self.dst.push(b)?;
        if let Some(ws) = &mut self.w {
            ws.push(w)?;
        }
        Ok(())
    }
}

// hashbrown 0.16.1, pair size16/alignment8. Group16 bounds supported SIMD/scalar
// requested layouts, including control bytes/alignment; allocator overhead is excluded.
fn distinct_bytes(capacity: usize) -> Result<u64> {
    if capacity == 0 {
        return Ok(0);
    }
    let buckets = if capacity < 15 {
        if capacity < 4 {
            4
        } else if capacity < 8 {
            8
        } else {
            16
        }
    } else {
        capacity
            .checked_mul(8)
            .map(|n| n / 7)
            .and_then(usize::checked_next_power_of_two)
            .ok_or_else(|| bad("distinct capacity overflows"))?
    };
    let bytes = buckets
        .checked_mul(16)
        .and_then(|n| n.checked_add(buckets))
        .and_then(|n| n.checked_add(16))
        .filter(|&n| n <= isize::MAX as usize - 15)
        .ok_or_else(|| bad("distinct layout overflows"))?;
    u64::try_from(bytes).map_err(|_| bad("distinct layout overflows"))
}
struct Distinct {
    table: hashbrown::HashTable<(u64, u64)>,
    _charge: Reservation,
}
fn pair_hash(pair: &(u64, u64)) -> u64 {
    let mut h = rustc_hash::FxHasher::default();
    pair.hash(&mut h);
    h.finish()
}
impl Distinct {
    fn new(ctl: &Control) -> Result<Self> {
        Ok(Self {
            table: hashbrown::HashTable::new(),
            _charge: Reservation::new(ctl, 0)?,
        })
    }
    fn insert(&mut self, pair: (u64, u64), ctl: &Control) -> Result<bool> {
        if !self._charge.belongs_to(ctl) {
            return Err(bad("distinct table belongs to another call"));
        }
        let hash = pair_hash(&pair);
        if self.table.find(hash, |p| *p == pair).is_some() {
            return Ok(false);
        }
        if self.table.len() == self.table.capacity() {
            let need = self
                .table
                .len()
                .checked_add(1)
                .ok_or_else(|| bad("distinct count overflows"))?;
            let capacity = self
                .table
                .capacity()
                .checked_mul(2)
                .map(|n| n.max(need))
                .ok_or_else(|| bad("distinct growth overflows"))?;
            let bound = distinct_bytes(capacity)?;
            let charge = Reservation::new(ctl, bound)?;
            let mut table = hashbrown::HashTable::new();
            table
                .try_reserve(capacity, pair_hash)
                .map_err(|e| bad(format!("distinct allocation failed: {e:?}")))?;
            if table.allocation_size() as u64 > bound || table.capacity() < need {
                return Err(bad("unsupported distinct allocation layout"));
            }
            #[cfg(any(test, feature = "failpoints"))]
            algo::observe_projection(algo::ProjectionEvent::Materialized(
                "distinct-allocated",
                table.capacity(),
            ));
            #[cfg(any(test, feature = "failpoints"))]
            algo::observe_projection(algo::ProjectionEvent::Allocation(
                "projection-distinct",
                table.num_buckets(),
                table.allocation_size() as u64,
                ctl.observed_used(),
            ));
            let mut work = Work::new(ctl)?;
            for item in self.table.drain() {
                work.tick("projection-distinct-move")?;
                table.insert_unique(pair_hash(&item), item, pair_hash);
            }
            ctl.check()?;
            *self = Self {
                table,
                _charge: charge,
            };
        }
        self.table.insert_unique(hash, pair, pair_hash);
        Ok(true)
    }
}

/// Uncached scalar numeric conversion under the algorithm's budget.
fn projection_number(ctx: &Ctx, id: Id, ctl: &Control) -> Result<Option<f64>> {
    let value = match id.tag() {
        Tag::Int => Some(Value::Integer(id.as_i64().into())),
        Tag::Double => Some(Value::Double(id.as_f64().into())),
        Tag::Decimal => Some(Value::Decimal(crate::id::unpack_decimal(id.payload()))),
        _ => None,
    };
    if let Some(value) = value {
        return Ok(super::value::approx_f64(&value));
    }
    Ok(ctx
        .with_algo_key(id, ctl, |key| Ok(super::value::numeric_key(key)))?
        .flatten())
}
fn resource(ctx: &Ctx, id: Id, ctl: &Control) -> Result<bool> {
    match id.tag() {
        Tag::BNode => Ok(true),
        Tag::Vocab => Ok(ctx.snap.generation.vocab.is_iri(id.payload())),
        Tag::Delta | Tag::Local => Ok(ctx
            .with_algo_key(id, ctl, |key| Ok(key.first() == Some(&b'<')))?
            .unwrap_or(false)),
        _ => Ok(false),
    }
}
fn owned_key(ctx: &Ctx, id: Id, ctl: &Control) -> Result<Buffer<u8>> {
    ctx.with_algo_key(id, ctl, |key| {
        let mut out = Buffer::new("projection-term-key", key.len(), ctl)?;
        out.extend(key)?;
        Ok(out)
    })?
    .ok_or_else(|| bad("projection term key is unavailable"))
}
fn triple_id(ctx: &Ctx, s: u64, p: u64, o: u64, ctl: &Control) -> Result<Option<Id>> {
    let a = owned_key(ctx, Id(s), ctl)?;
    let b = owned_key(ctx, Id(p), ctl)?;
    let c = owned_key(ctx, Id(o), ctl)?;
    fn width(mut n: usize) -> usize {
        let mut len = 1;
        while n >= 128 {
            n >>= 7;
            len += 1;
        }
        len
    }
    let capacity = [&a, &b, &c]
        .iter()
        .try_fold(1usize, |n, key| {
            n.checked_add(width(key.len()))
                .and_then(|n| n.checked_add(key.len()))
        })
        .ok_or_else(|| bad("triple key capacity overflows"))?;
    let mut key = Buffer::new("projection-triple-key", capacity, ctl)?;
    key.push(b'(')?;
    for part in [&a, &b, &c] {
        let mut n = u64::try_from(part.len()).map_err(|_| bad("triple key length overflows"))?;
        while n >= 128 {
            key.push((n as u8) | 0x80)?;
            n >>= 7;
        }
        key.push(n as u8)?;
        key.extend(part)?;
    }
    // The lookup reconstruction scratch is admitted independently of the encoded key.
    let mut scratch = Reservation::new(ctl, 0)?;
    if let Ok(id) = ctx.snap.generation.vocab.find_with_checked(&key, |need| {
        let bound = memory::bytes::<u8>(need)?
            .checked_mul(2)
            .ok_or_else(|| bad("lookup scratch overflows"))?;
        scratch.grow_to(bound)
    })? {
        return Ok(Some(Id::vocab(id)));
    }
    Ok(ctx
        .snap
        .generation
        .dvocab
        .find(&key)
        .filter(|&id| id < ctx.snap.dvocab_len)
        .map(Id::delta))
}
/// Iterate all requested columns without retaining a result vector or hiding errors.
fn projection_rows(
    ctx: &Ctx,
    perm: Perm,
    prefix: &[u64],
    mut each: impl FnMut([u64; 4]) -> Result<()>,
) -> Result<()> {
    ctx.snap.scan(perm, prefix, |chunk| {
        match chunk {
            Chunk::Block(b, s, e) => {
                for i in s..e {
                    each([b.cols[0][i], b.cols[1][i], b.cols[2][i], b.cols[3][i]])?;
                }
            }
            Chunk::Row(k) => each(k)?,
        }
        Ok(true)
    })
}
/// Convert only a selected projection weight, before it enters the stored CSR.
fn stored_weight(value: f64) -> Result<f32> {
    let value = value as f32;
    if !value.is_finite() {
        return Err(bad("an edge weight is not finite as f32"));
    }
    Ok(value)
}

/// Resolve only visible weights of an applicable asserted edge. All accepted graph
/// statements may participate in the union, even across its physical graphs.
fn weight(ctx: &Ctx, proj: &ProjectionSpec, s: u64, p: u64, o: u64, ctl: &Control) -> Result<f32> {
    let (Some(Some(wprop)), Some(reifies)) = (proj.weight, proj.reifies) else {
        return stored_weight(proj.default_weight);
    };
    let Some(triple) = triple_id(ctx, s, p, o, ctl)? else {
        return stored_weight(proj.default_weight);
    };
    let mut best: Option<f32> = None;
    let mut work = Work::new(ctl)?;
    projection_rows(ctx, Perm::Pos, &[reifies, triple.0], |r| {
        work.tick("projection-reifiers")?;
        if !proj.graph.accepts(r[3]) {
            return Ok(());
        }
        projection_rows(ctx, Perm::Pso, &[wprop, r[2]], |k| {
            work.tick("projection-weights")?;
            if !proj.graph.accepts(k[3]) {
                return Ok(());
            }
            let value = stored_weight(
                projection_number(ctx, Id(k[2]), ctl)?
                    .filter(|v| v.is_finite())
                    .ok_or_else(|| bad("an applicable edge weight is not a number"))?,
            )?;
            best = Some(best.map_or(value, |b| b.min(value)));
            Ok(())
        })
    })?;
    ctl.check()?;
    match best {
        Some(value) => Ok(value),
        None => stored_weight(proj.default_weight),
    }
}

/// The projection of a call: a cached build of the same data and edges, or a new one.
/// Uncommitted views and projections from a nested pattern are not cached.
fn projection(
    ctx: &Ctx,
    spec: &AlgoSpec,
    edges: Option<&Table>,
    par: &Par,
    ctl: &Control,
) -> Result<(std::sync::Arc<Graph>, bool)> {
    let key = (ctx.snap.cacheable && spec.proj.pattern.is_none() && ctx.use_cache).then(|| {
        format!(
            "{}{:?}|{}",
            super::cache::view_key(ctx),
            spec.proj,
            spec.direction().name()
        )
    });
    if let Some(k) = &key
        && let Some(g) = algo::cache::get(k)
    {
        // the call holds the build while it runs, as if it had made it
        ctl.charge(g.base_bytes()?)?;
        return Ok((g, true));
    }
    let g = std::sync::Arc::new(build_graph(ctx, spec, edges, par, ctl)?);
    if let Some(k) = key {
        algo::cache::put(k, g.clone());
    }
    Ok((g, false))
}

/// Build the projection of a call from the store, or from the rows of a nested pattern.
fn build_graph(
    ctx: &Ctx,
    spec: &AlgoSpec,
    edges: Option<&Table>,
    par: &Par,
    ctl: &Control,
) -> Result<Graph> {
    let proj = &spec.proj;
    let call_dir = spec.direction();
    let has_weights = proj.weight.is_some() || proj.pattern.is_some_and(|p| p.2.is_some());
    let mut work = Work::new(ctl)?;
    let class_limited = !proj.node_class_names.is_empty();
    let mut nodes = Buffer::new("projection-class", 0, ctl)?;
    if class_limited {
        let ty = proj
            .rdf_type
            .ok_or_else(|| bad("the dataset has no rdf:type triples for algo:nodeClass"))?;
        for &class in &proj.node_classes {
            projection_rows(ctx, Perm::Pos, &[ty, class], |k| {
                work.tick("projection-class")?;
                if proj.graph.accepts(k[3]) {
                    let need = nodes
                        .len()
                        .checked_add(1)
                        .ok_or_else(|| bad("class count overflows"))?;
                    nodes.reserve("projection-class", need, ctl)?;
                    nodes.push(k[2])?;
                }
                Ok(())
            })?;
        }
        ctl.check()?;
        par.sort(&mut nodes);
        ctl.check()?;
        nodes.dedup(ctl)?;
    }
    let allowed = |s: u64, o: u64| {
        !class_limited || (nodes.binary_search(&s).is_ok() && nodes.binary_search(&o).is_ok())
    };
    let mut col = Collect {
        ctl,
        src: Buffer::new("projection-source", 0, ctl)?,
        dst: Buffer::new("projection-destination", 0, ctl)?,
        w: has_weights
            .then(|| Buffer::new("projection-raw-weights", 0, ctl))
            .transpose()?,
        symmetric: call_dir == Dir::Undirected
            && proj.predicates.as_ref().is_none_or(|ps| {
                ps.iter()
                    .all(|(_, d)| d.is_none_or(|d| d == Dir::Undirected))
            }),
    };
    if let Some(t) = edges {
        let (s, e, w) = proj.pattern.expect("a nested pattern");
        let (Some(cs), Some(ce)) = (t.col_of(s), t.col_of(e)) else {
            return Err(bad(
                "the nested pattern does not bind algo:start and algo:end",
            ));
        };
        let cw = w.and_then(|v| t.col_of(v));
        let mut seen = Distinct::new(ctl)?;
        for i in 0..t.len() {
            work.tick("projection-pattern")?;
            let (a, b) = (t.cols[cs][i], t.cols[ce][i]);
            if !resource(ctx, a, ctl)? || !resource(ctx, b, ctl)? || !allowed(a.0, b.0) {
                continue;
            }
            if !seen.insert((a.0, b.0), ctl)? {
                continue;
            }
            let value = match cw {
                Some(c) if !t.cols[c][i].is_undef() => stored_weight(
                    projection_number(ctx, t.cols[c][i], ctl)?
                        .filter(|v| v.is_finite())
                        .ok_or_else(|| {
                            bad("an edge weight of the nested pattern is not a number")
                        })?,
                )?,
                _ => stored_weight(proj.default_weight)?,
            };
            col.push(a.0, b.0, value, call_dir)?;
        }
    } else {
        let mut scan_work = Work::new(ctl)?;
        let mut scan = |lo: u64, hi: u64, dir: Dir| -> Result<()> {
            let mut last = None;
            ctx.snap.scan_between_cols(
                Perm::Pso,
                pad(&[lo], 0),
                pad(&[hi], u64::MAX),
                0b1111,
                |chunk| {
                    let mut each = |k: [u64; 4]| -> Result<()> {
                        scan_work.tick("projection-edge")?;
                        let [p, s, o, g] = k;
                        if !proj.graph.accepts(g) || last == Some((p, s, o)) {
                            return Ok(());
                        }
                        last = Some((p, s, o));
                        if !resource(ctx, Id(s), ctl)?
                            || !resource(ctx, Id(o), ctl)?
                            || !allowed(s, o)
                        {
                            return Ok(());
                        }
                        let value = if has_weights {
                            weight(ctx, proj, s, p, o, ctl)?
                        } else {
                            0.0
                        };
                        col.push(s, o, value, dir)
                    };
                    match chunk {
                        Chunk::Block(b, s, e) => {
                            for i in s..e {
                                each([b.cols[0][i], b.cols[1][i], b.cols[2][i], b.cols[3][i]])?;
                            }
                        }
                        Chunk::Row(k) => each(k)?,
                    }
                    Ok(true)
                },
            )
        };
        if let Some(ps) = &proj.predicates {
            let mut dirs = Buffer::new("projection-predicates", ps.len(), ctl)?;
            for (i, &(p, d)) in ps.iter().enumerate() {
                work.tick("projection-predicates")?;
                dirs.push((p, i, d.unwrap_or(call_dir)))?;
            }
            ctl.check()?;
            dirs.sort_unstable_by_key(|d| (d.0, d.1));
            ctl.check()?;
            let mut i = 0;
            while i < dirs.len() {
                work.tick("projection-predicates")?;
                let mut j = i + 1;
                while j < dirs.len() && dirs[j].0 == dirs[i].0 {
                    work.tick("projection-predicates")?;
                    j += 1;
                }
                let (p, _, d) = dirs[j - 1];
                scan(p, p, d)?;
                i = j;
            }
        } else {
            match proj.rdf_type {
                Some(t) => {
                    if t > 0 {
                        scan(0, t - 1, call_dir)?;
                    }
                    if t < u64::MAX {
                        scan(t + 1, u64::MAX, call_dir)?;
                    }
                }
                None => scan(0, u64::MAX, call_dir)?,
            }
        }
    }
    if !class_limited {
        let capacity = col
            .src
            .len()
            .checked_mul(2)
            .ok_or_else(|| bad("endpoint count overflows"))?;
        nodes = Buffer::new("projection-nodes", capacity, ctl)?;
        for ends in [&*col.src, &*col.dst] {
            for &id in ends {
                work.tick("projection-nodes")?;
                nodes.push(id)?;
            }
        }
        ctl.check()?;
        par.sort(&mut nodes);
        ctl.check()?;
        nodes.dedup(ctl)?;
    }
    Graph::build_owned(
        OwnedRawEdges {
            nodes,
            src: col.src,
            dst: col.dst,
            w: col.w,
            symmetric: col.symmetric,
        },
        proj.aggregate,
        par,
        ctl,
    )
}

#[cfg(test)]
mod projection_capacity_tests {
    use super::*;
    use std::sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    };
    #[test]
    fn hash_bound_covers_real_layout_and_replacement_overlap() {
        let ctl = Control::new(Default::default(), None, 1 << 20);
        let mut seen = Distinct::new(&ctl).unwrap();
        let observed = ctl.clone();
        let old = Arc::new(std::sync::atomic::AtomicU64::new(0));
        let old_hook = old.clone();
        algo::set_projection_observer(Some(Arc::new(move |event| {
            if let algo::ProjectionEvent::Materialized("distinct-allocated", _) = event {
                assert!(observed.observed_used() > old_hook.load(Ordering::Relaxed));
            }
        })));
        for i in 0..512 {
            old.store(ctl.observed_used(), Ordering::Relaxed);
            assert!(seen.insert((i, i + 1), &ctl).unwrap());
            assert!(seen.table.allocation_size() as u64 <= ctl.observed_used());
            assert!(!seen.insert((i, i + 1), &ctl).unwrap());
        }
        algo::set_projection_observer(None);
        drop(seen);
        assert_eq!(ctl.remaining(), 1 << 20);
        assert!(distinct_bytes(usize::MAX).is_err());
    }
    #[test]
    fn hash_first_and_rehash_refuse_before_allocation_and_restore() {
        for (budget, n) in [(83, 0), (235, 3)] {
            let ctl = Control::new(Default::default(), None, budget);
            let mut seen = Distinct::new(&ctl).unwrap();
            for i in 0..n {
                assert!(seen.insert((i, i), &ctl).unwrap());
            }
            let allocations = Arc::new(std::sync::atomic::AtomicUsize::new(0));
            let a = allocations.clone();
            algo::set_projection_observer(Some(Arc::new(move |event| {
                if let algo::ProjectionEvent::Materialized("distinct-allocated", _) = event {
                    a.fetch_add(1, Ordering::Relaxed);
                }
            })));
            assert!(matches!(
                seen.insert((n, n), &ctl),
                Err(Error::BudgetExceeded(_))
            ));
            algo::set_projection_observer(None);
            assert_eq!(allocations.load(Ordering::Relaxed), 0);
            assert_eq!(seen.table.len(), n as usize);
            drop(seen);
            assert_eq!(ctl.remaining(), budget);
        }
    }
    #[test]
    fn hash_migration_cancel_and_unwind_release_new_table() {
        for unwind in [false, true] {
            let cancel = Arc::new(AtomicBool::new(false));
            let ctl = Control::new(cancel.clone(), None, 1024);
            let mut seen = Distinct::new(&ctl).unwrap();
            for i in 0..3 {
                seen.insert((i, i), &ctl).unwrap();
            }
            let prior = ctl.remaining();
            let scope = memory::probe::install(
                move |point| {
                    if let memory::probe::Point::Work("projection-distinct-move") = point {
                        if unwind {
                            panic!("distinct migration unwind");
                        } else {
                            cancel.store(true, Ordering::Relaxed);
                        }
                    }
                },
                None,
            );
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                seen.insert((3, 3), &ctl)
            }));
            drop(scope);
            if unwind {
                assert!(result.is_err());
            } else {
                assert!(matches!(result.unwrap(), Err(Error::Cancelled)));
            }
            assert_eq!(ctl.remaining(), prior);
            drop(seen);
            assert_eq!(ctl.remaining(), 1024);
        }
    }
    #[test]
    fn collector_mirror_arrays_and_failure_lifetimes() {
        for name in [
            "projection-source",
            "projection-destination",
            "projection-raw-weights",
        ] {
            let ctl = Control::new(Default::default(), None, 1024);
            let mut c = Collect {
                ctl: &ctl,
                src: Buffer::new("empty", 0, &ctl).unwrap(),
                dst: Buffer::new("empty", 0, &ctl).unwrap(),
                w: Some(Buffer::new("empty", 0, &ctl).unwrap()),
                symmetric: true,
            };
            let scope = memory::probe::install(|_| {}, Some(name));
            assert!(c.push(1, 2, 3., Dir::Undirected).is_err());
            drop(scope);
            assert!(c.src.is_empty() && c.dst.is_empty() && c.w.as_ref().unwrap().is_empty());
            drop(c);
            assert_eq!(ctl.remaining(), 1024);
        }
        let ctl = Control::unlimited();
        let mut c = Collect {
            ctl: &ctl,
            src: Buffer::new("empty", 0, &ctl).unwrap(),
            dst: Buffer::new("empty", 0, &ctl).unwrap(),
            w: None,
            symmetric: true,
        };
        c.push(1, 1, 0., Dir::Undirected).unwrap();
        c.push(1, 2, 0., Dir::Undirected).unwrap();
        assert_eq!(&*c.src, &[1, 1, 2]);
        assert_eq!(&*c.dst, &[1, 2, 1]);
        drop(c);
        assert_eq!(ctl.remaining(), u64::MAX);
    }
    fn weighted() -> (crate::store::Store, Ctx, AlgoSpec) {
        use crate::io::{RdfFormat, Source};
        let store = crate::store::Store::in_memory(Default::default());
        store
            .load(&[Source::from_bytes(
                b"@prefix ex:<http://ex/>. ex:a a ex:N;ex:p ex:b {|ex:w 2|}. ex:b a ex:N.".to_vec(),
                RdfFormat::Turtle,
                None,
            )])
            .unwrap();
        let ctx = Ctx::new(store.snapshot());
        let id = |iri: &str| ctx.snap.lookup_iri(iri).unwrap().0;
        let proj = ProjectionSpec {
            predicates: Some(vec![(id("http://ex/p"), None)]),
            direction: None,
            node_classes: vec![id("http://ex/N")],
            node_class_names: vec!["http://ex/N".into()],
            weight: Some(Some(id("http://ex/w"))),
            default_weight: 1.,
            aggregate: Aggregate::None,
            pattern: None,
            graph: GraphFilter::All,
            rdf_type: Some(id(RDF_TYPE)),
            reifies: Some(id(RDF_REIFIES)),
        };
        (
            store,
            ctx,
            AlgoSpec {
                algo: Algo::Sssp,
                proj,
                nums: BTreeMap::new(),
                metric: None,
                graphalytics: false,
                iteration_weights: None,
                sources: vec![],
                outs: Outs::default(),
                graph_var: None,
            },
        )
    }
    #[test]
    fn callback_cancellation_preserves_type_and_drops_all_owners() {
        for phase in [
            "projection-class",
            "projection-edge",
            "projection-reifiers",
            "projection-weights",
        ] {
            let (_store, ctx, spec) = weighted();
            let cancel = Arc::new(AtomicBool::new(false));
            let ctl = Control::new(cancel.clone(), None, 1 << 20);
            let scope = memory::probe::install(
                move |point| {
                    if let memory::probe::Point::Work(p) = point
                        && p == phase
                    {
                        cancel.store(true, Ordering::Relaxed);
                    }
                },
                None,
            );
            assert!(
                matches!(
                    build_graph(&ctx, &spec, None, &Par::single(), &ctl),
                    Err(Error::Cancelled)
                ),
                "{phase}"
            );
            drop(scope);
            assert_eq!(ctl.remaining(), 1 << 20, "{phase}");
        }
        let (_store, ctx, spec) = weighted();
        let ctl = Control::new(Default::default(), Some(Instant::now()), 1 << 20);
        assert!(matches!(
            build_graph(&ctx, &spec, None, &Par::single(), &ctl),
            Err(Error::Timeout)
        ));
        assert_eq!(ctl.remaining(), 1 << 20);
        let mut visited = 0;
        let result = projection_rows(&ctx, Perm::Pso, &[], |_| {
            visited += 1;
            Err(Error::Timeout)
        });
        assert!(matches!(result, Err(Error::Timeout)));
        assert_eq!(visited, 1);
    }
    #[test]
    fn rejected_rows_still_bound_cancellation_work() {
        use crate::io::{RdfFormat, Source};
        let store = crate::store::Store::in_memory(Default::default());
        let mut ttl = String::from("@prefix ex:<http://ex/>.");
        for i in 0..=algo::CHECK_EVERY {
            ttl.push_str(&format!("ex:a ex:p ex:n{i}.\n"));
        }
        store
            .load(&[Source::from_bytes(
                ttl.into_bytes(),
                RdfFormat::Turtle,
                None,
            )])
            .unwrap();
        let (_other, _ctx, mut spec) = weighted();
        let ctx = Ctx::new(store.snapshot());
        spec.proj.predicates = Some(vec![(ctx.snap.lookup_iri("http://ex/p").unwrap().0, None)]);
        spec.proj.node_classes.clear();
        spec.proj.node_class_names.clear();
        spec.proj.weight = None;
        spec.proj.graph = GraphFilter::One(0);
        let cancel = Arc::new(AtomicBool::new(false));
        let ctl = Control::new(cancel.clone(), None, 64);
        let observed = std::rc::Rc::new(std::cell::Cell::new(0));
        let count = observed.clone();
        let scope = memory::probe::install(
            move |point| {
                if let memory::probe::Point::Work("projection-edge") = point {
                    count.set(count.get() + 1);
                    cancel.store(true, Ordering::Relaxed);
                }
            },
            None,
        );
        assert!(matches!(
            build_graph(&ctx, &spec, None, &Par::single(), &ctl),
            Err(Error::Cancelled)
        ));
        drop(scope);
        assert_eq!(observed.get(), algo::CHECK_EVERY);
        assert_eq!(ctl.remaining(), 64);
    }
    #[test]
    fn graph_handoff_and_cache_hit_charge_base_once() {
        let (_store, ctx, spec) = weighted();
        let cold = Control::new(Default::default(), None, 1 << 20);
        let (graph, hit) = projection(&ctx, &spec, None, &Par::single(), &cold).unwrap();
        assert!(!hit);
        assert_eq!((1 << 20) - cold.remaining(), graph.base_bytes().unwrap());
        let warm = Control::new(Default::default(), None, 1 << 20);
        let (_, hit) = projection(&ctx, &spec, None, &Par::single(), &warm).unwrap();
        assert!(hit);
        assert_eq!((1 << 20) - warm.remaining(), graph.base_bytes().unwrap());
        let prior = warm.remaining();
        let incoming = graph.inn(&warm).unwrap();
        assert!(warm.remaining() < prior);
        drop(incoming);
        assert_eq!(warm.remaining(), prior);
    }
}
