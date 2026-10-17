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
use super::table::{Materialized, Table, VarId};
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
    ("nodeSelfInfluence", Kd::Float),
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
                "nodeSelfInfluence",
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

/// The value of a numeric parameter. An integer parameter keeps its exact value, so
/// two seeds above 2^53 stay apart.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Num {
    Int(u64),
    Float(f64),
}

impl Num {
    /// The value as a number, for bounds and floating-point parameters.
    pub fn f64(self) -> f64 {
        match self {
            Num::Int(v) => v as f64,
            Num::Float(v) => v,
        }
    }

    /// The value of an integer parameter.
    pub fn u64(self) -> u64 {
        match self {
            Num::Int(v) => v,
            Num::Float(v) => v as u64,
        }
    }
}

/// A planned algorithm call.
#[derive(Clone, Debug)]
pub struct AlgoSpec {
    pub algo: Algo,
    pub proj: ProjectionSpec,
    pub nums: BTreeMap<String, Num>,
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

    fn num(&self, name: &str) -> Option<Num> {
        self.nums.get(name).copied()
    }

    fn int(&self, name: &str, default: u64) -> u64 {
        self.num(name).map_or(default, Num::u64)
    }

    /// A count that the algorithm holds in 32 bits. Planning refuses a larger value, so
    /// the clamp only guards a spec that was built without planning.
    fn int_u32(&self, name: &str, default: u64) -> u32 {
        u32::try_from(self.int(name, default)).unwrap_or(u32::MAX)
    }

    fn float(&self, name: &str, default: f64) -> f64 {
        self.num(name).map_or(default, Num::f64)
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

/// The datatypes of exact integers, `xsd:integer` and the types derived from it.
const INTEGER_TYPES: &[&str] = &[
    "http://www.w3.org/2001/XMLSchema#integer",
    "http://www.w3.org/2001/XMLSchema#nonNegativeInteger",
    "http://www.w3.org/2001/XMLSchema#positiveInteger",
    "http://www.w3.org/2001/XMLSchema#nonPositiveInteger",
    "http://www.w3.org/2001/XMLSchema#negativeInteger",
    "http://www.w3.org/2001/XMLSchema#long",
    "http://www.w3.org/2001/XMLSchema#int",
    "http://www.w3.org/2001/XMLSchema#short",
    "http://www.w3.org/2001/XMLSchema#byte",
    "http://www.w3.org/2001/XMLSchema#unsignedLong",
    "http://www.w3.org/2001/XMLSchema#unsignedInt",
    "http://www.w3.org/2001/XMLSchema#unsignedShort",
    "http://www.w3.org/2001/XMLSchema#unsignedByte",
];

/// 2^53. A double holds every integer up to it exactly.
const EXACT_DOUBLE: f64 = 9_007_199_254_740_992.0;

/// The value of a numeric parameter. An integer parameter written as an integer keeps
/// its exact value. One written as a decimal or a double must be a whole number of at
/// most 2^53, which a double holds exactly.
fn param(t: &TermPattern, name: &str, kind: Kd) -> Result<Num> {
    if kind == Kd::Float {
        return number(t, name, kind).map(Num::Float);
    }
    if let TermPattern::Literal(l) = t
        && INTEGER_TYPES.contains(&l.datatype().as_str())
    {
        // an exact integer outside 0..=i64::MAX is refused by name, not as "not a number"
        let text = l.value().trim();
        let digits = text.strip_prefix('+').unwrap_or(text);
        if !digits.is_empty()
            && digits.bytes().all(|b| b.is_ascii_digit())
            && digits.parse::<i64>().is_err()
        {
            return Err(bad(format!(
                "algo:{name} takes an integer from 0 to {}",
                i64::MAX
            )));
        }
    }
    if let TermPattern::Literal(l) = t
        && let Value::Integer(i) = Value::from_literal(l)
    {
        return u64::try_from(i64::from(i))
            .map(Num::Int)
            .map_err(|_| bad(format!("algo:{name} takes a non-negative integer")));
    }
    let v = number(t, name, kind)?;
    if v > EXACT_DOUBLE {
        return Err(bad(format!(
            "algo:{name} is above 2^53, where a decimal or a double is not exact; write it as an integer"
        )));
    }
    Ok(Num::Int(v as u64))
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
    p.ctx.note_algorithm_call();
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
    let mut nums: BTreeMap<String, Num> = BTreeMap::new();
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
                // `()` is rdf:nil, the empty list, which nodeSelfInfluence can stand alone with
                let items = if matches!(o, TermPattern::NamedNode(n) if *n == oxrdf::vocab::rdf::NIL)
                {
                    Some(Vec::new())
                } else {
                    super::pathsearch::list_items(o)
                };
                let Some(items) = items else {
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
                nums.insert(n.to_string(), param(o, name, *kind)?);
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
    if let Some(m) = nums.get("dampingFactor").map(|v| v.f64())
        && !(0.0..1.0).contains(&m)
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
        if nums.get(name).is_some_and(|v| v.f64() < min) {
            return Err(bad(format!("algo:{name} must be at least {min}")));
        }
    }
    // these counts are 32-bit; a larger value is refused, not wrapped
    for name in ["maxIterations", "maxLevels"] {
        if nums
            .get(name)
            .is_some_and(|v| v.u64() > u64::from(u32::MAX))
        {
            return Err(bad(format!("algo:{name} must be at most {}", u32::MAX)));
        }
    }
    // an embedding is a `spk:vector`, which a vector index caps at the same value
    if let Some(d) = nums.get("dimension")
        && d.u64() > crate::vector::MAX_DIM as u64
    {
        return Err(bad(format!(
            "algo:dimension must be at most {}",
            crate::vector::MAX_DIM
        )));
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
) -> Result<(Materialized, serde_json::Map<String, serde_json::Value>)> {
    let started = Instant::now();
    let limit = spec.num("maxMemory").map_or(u64::MAX, Num::u64);
    let ctl = Control::for_query(ctx, limit)?;
    let par = Par::acquire(spec.int("concurrency", 0) as usize, &ctl)?;
    let mut result_vars = Buffer::new("algorithm-result-vars", vars.len(), &ctl)?;
    let mut work = Work::new(&ctl)?;
    for &v in vars {
        work.tick("algorithm-result-vars")?;
        if spec.outs.vars().contains(&v) || Some(v) == spec.graph_var {
            result_vars.push(v)?;
        }
    }
    let mut t = Some(Builder::new(&result_vars, &ctl)?.finish(0, ctx)?);
    let mut built = std::time::Duration::ZERO;
    let (mut nodes, mut edge_count, mut cache_hits) = (0usize, 0usize, 0usize);
    let mut each = |filter: &GraphFilter, gid: Option<Id>| -> Result<()> {
        let frame = ctl.query_frame()?;
        let at = Instant::now();
        let (g, hit) = projection_in_graph(ctx, spec, edges.as_ref(), &par, &frame, filter)?;
        built += at.elapsed();
        cache_hits += hit as usize;
        nodes += g.n();
        edge_count += g.m();
        let bound = spec
            .sources
            .iter()
            .try_fold(0usize, |n, s| {
                n.checked_add(match s {
                    PathEnd::Const(_) => 1,
                    PathEnd::Var(v) => input
                        .as_ref()
                        .and_then(|t| t.col_of(*v).map(|_| t.len()))
                        .unwrap_or(0),
                })
            })
            .ok_or_else(|| bad("source count overflows"))?;
        let mut sources = Buffer::new("algorithm-sources", bound, &frame)?;
        let mut work = Work::new(&frame)?;
        for s in &spec.sources {
            let mut add = |id: Id| -> Result<()> {
                work.tick("algorithm-sources")?;
                if let Ok(i) = g.ids.binary_search(&id.0) {
                    sources.push(i as u32)?;
                }
                Ok(())
            };
            match s {
                PathEnd::Const(id) => add(*id)?,
                PathEnd::Var(v) => {
                    if let Some(t) = input.as_ref()
                        && let Some(c) = t.col_of(*v)
                    {
                        // a row of the group bound to another graph is not a source of
                        // this graph's run
                        let scope = match (gid, spec.graph_var) {
                            (Some(g), Some(gv)) => t.col_of(gv).map(|cg| (cg, g)),
                            _ => None,
                        };
                        for (i, &id) in t.cols[c].iter().enumerate() {
                            if scope.is_some_and(|(cg, g)| t.cols[cg][i] != g) {
                                continue;
                            }
                            if !id.is_undef() {
                                add(id)?;
                            }
                        }
                    }
                }
            }
        }
        sources.sort_unstable();
        sources.dedup(&frame)?;
        let mut part = if !spec.sources.is_empty()
            && sources.is_empty()
            && matches!(spec.algo, Algo::Bfs | Algo::Sssp)
            && !spec.stats_only()
        {
            Builder::new(&result_vars, &frame)?.finish(0, ctx)?
        } else {
            rows(ctx, spec, &g, &sources, &par, &frame, &result_vars)?
        };
        if let (Some(gid), Some(gv)) = (gid, spec.graph_var)
            && let Some(c) = part.table.col_of(gv)
        {
            let replacement =
                Buffer::filled("algorithm-graph-column", part.table.len, gid, &frame)?;
            let bytes = memory::bytes::<Id>(replacement.capacity())?;
            let replacement = replacement.into_output();
            let mut lease = frame.output_lease(bytes)?;
            let Some(held) = &mut part.lease else {
                drop(replacement);
                return Err(bad("missing algorithm part admission"));
            };
            if let Err(error) = held.merge(&mut lease) {
                drop(replacement);
                return Err(error);
            }
            part.table.cols[c] = replacement;
            if let Some(held) = &mut part.lease {
                held.resize(ctx.mem_limit, table_capacity(&part.table)?)?;
            }
        }
        t = Some(append(
            ctx,
            t.take().expect("previous graph output"),
            part,
            &ctl,
        )?);
        // Graph/Csr cache ownership is plain data; this call's base admission ends
        // only after every local Arc dies. Output leases own their separate columns.
        drop(g);
        Ok(())
    };
    match spec.graph_var {
        None => each(&spec.proj.graph, None)?,
        Some(_) => ctx.snap.for_each_graph_id_checked(
            || ctl.check(),
            |g| {
                if spec.proj.graph.accepts(g.0) {
                    each(&GraphFilter::One(g.0), Some(g))?;
                }
                Ok(())
            },
        )?,
    }
    let mut t = t.expect("graph output");
    // No SERVICE phase survives this boundary. Native frames and scratch die first;
    // the final backing keeps exactly the same query charge through retirement.
    drop(result_vars);
    drop(par);
    let peak = ctl.peak();
    drop(ctl);
    if let Some(lease) = &mut t.lease {
        #[cfg(any(test, feature = "failpoints"))]
        algo::observe_projection(algo::ProjectionEvent::Materialized(
            "algorithm-retirement-before",
            lease.bytes as usize,
        ));
        lease.retire_service()?;
        #[cfg(any(test, feature = "failpoints"))]
        algo::observe_projection(algo::ProjectionEvent::Materialized(
            "algorithm-retirement-after",
            lease.bytes as usize,
        ));
    }
    let mut counters = serde_json::Map::new();
    counters.insert("nodes".into(), nodes.into());
    counters.insert("cachedProjections".into(), cache_hits.into());
    counters.insert("edges".into(), edge_count.into());
    counters.insert("projectionMs".into(), (built.as_secs_f64() * 1000.0).into());
    counters.insert(
        "runMs".into(),
        ((started.elapsed() - built).as_secs_f64() * 1000.0).into(),
    );
    counters.insert("peakBytes".into(), peak.into());
    let out = match input {
        Some(inp) => super::exec::join_algorithm_tables(ctx, &inp, &t.table)?,
        None => t,
    };
    // `run_node` counts the table once, when this call returns to it
    Ok((out, counters))
}
fn append(
    ctx: &Ctx,
    mut left: Materialized,
    right: Materialized,
    ctl: &Control,
) -> Result<Materialized> {
    ctl.check()?;
    let n = left
        .table
        .len
        .checked_add(right.table.len)
        .ok_or_else(|| bad("result rows overflow"))?;
    ctx.check_output_rows(n)?;
    if left.table.width() == 0 {
        // Zero-width rows still have the inherited logical Id-sized estimate.
        // Admit it before publishing the larger length, including on refusal.
        let bytes = memory::sum(&[
            table_capacity(&left.table)?,
            memory::bytes::<Id>(right.table.len)?,
        ])?;
        left.lease
            .as_mut()
            .ok_or_else(|| bad("missing algorithm output admission"))?
            .resize(ctx.mem_limit, bytes)?;
    } else {
        let mut work = Work::new(ctl)?;
        for c in 0..left.table.width() {
            ctl.check()?;
            if n <= left.table.cols[c].capacity() {
                for &value in &right.table.cols[c] {
                    work.tick("algorithm-append-copy")?;
                    // n was checked against the admitted capacity above. No push
                    // here can trigger implicit growth.
                    left.table.cols[c].push(value);
                }
            } else {
                let capacity = left.table.cols[c]
                    .capacity()
                    .checked_mul(2)
                    .map(|v| v.max(4).max(n))
                    .ok_or_else(|| bad("result column growth overflows"))?;
                let mut replacement = Buffer::new("algorithm-append-column", capacity, ctl)?;
                for &value in &left.table.cols[c] {
                    work.tick("algorithm-append-relocate")?;
                    replacement.push(value)?;
                }
                for &value in &right.table.cols[c] {
                    work.tick("algorithm-append-copy")?;
                    replacement.push(value)?;
                }
                ctl.check()?;
                let bytes = memory::bytes::<Id>(replacement.capacity())?;
                let replacement = replacement.into_output();
                let mut lease = ctl.output_lease(bytes)?;
                let Some(held) = &mut left.lease else {
                    drop(replacement);
                    return Err(bad("missing algorithm output admission"));
                };
                if let Err(error) = held.merge(&mut lease) {
                    drop(replacement);
                    return Err(error);
                }
                #[cfg(test)]
                memory::probe::at(memory::probe::Point::Work("algorithm-append-transferred"));
                let old = std::mem::replace(&mut left.table.cols[c], replacement);
                drop(old); // Backing dies before its capacity leaves the lease.
                #[cfg(test)]
                memory::probe::at(memory::probe::Point::Work("algorithm-append-old-dropped"));
                let bytes = table_capacity(&left.table)?;
                left.lease
                    .as_mut()
                    .expect("admitted accumulator")
                    .resize(ctx.mem_limit, bytes)?;
                #[cfg(test)]
                memory::probe::at(memory::probe::Point::Work("algorithm-append-resized"));
            }
        }
    }
    ctl.check()?;
    left.table.len = n;
    left.table.sorted.clear();
    Ok(left)
}

/// The id of an `xsd:double`. A value whose low four mantissa bits are not zero loses
/// them (a relative error under 2^-48), so that every score is an inline id and the
/// vocabulary is not locked once for each of millions of rows.
fn dbl(x: f64) -> Id {
    Id::from_f64(f64::from_bits(x.to_bits() & !0xF)).unwrap_or(Id::UNDEF)
}

fn int(ctx: &Ctx, x: i64, ctl: &Control) -> Result<Id> {
    if let Some(id) = Id::from_i64(x) {
        return Ok(id);
    }
    let mut text = Text::new(ctl)?;
    use std::fmt::Write;
    let result = write!(&mut text, "{x}");
    text.formatted(result)?;
    literal(
        ctx,
        &text.bytes,
        "http://www.w3.org/2001/XMLSchema#integer",
        ctl,
    )
}

/// The same incremental serde/Debug encoding, with old/new capacity admitted before
/// every growth. Preserve the original typed cancellation/resource error across Write.
struct Text<'a> {
    bytes: Buffer<u8>,
    ctl: &'a Control,
    error: Option<Error>,
}
impl<'a> Text<'a> {
    fn new(ctl: &'a Control) -> Result<Self> {
        Ok(Self {
            bytes: Buffer::new("algorithm-text", 0, ctl)?,
            ctl,
            error: None,
        })
    }
    fn append(&mut self, text: &[u8]) -> Result<()> {
        let n = self
            .bytes
            .len()
            .checked_add(text.len())
            .ok_or_else(|| bad("text capacity overflows"))?;
        self.bytes.reserve("algorithm-text", n, self.ctl)?;
        self.bytes.extend(text)
    }
    fn formatted(&mut self, result: std::fmt::Result) -> Result<()> {
        if let Some(error) = self.error.take() {
            return Err(error);
        }
        result.map_err(|_| bad("text formatting failed"))
    }
}
impl std::fmt::Write for Text<'_> {
    fn write_str(&mut self, s: &str) -> std::fmt::Result {
        self.append(s.as_bytes()).map_err(|e| {
            self.error = Some(e);
            std::fmt::Error
        })
    }
}
impl std::io::Write for Text<'_> {
    fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
        match self.append(b) {
            Ok(()) => Ok(b.len()),
            Err(e) => {
                self.error = Some(e);
                Err(std::io::Error::other("algorithm text refused"))
            }
        }
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}
fn literal(ctx: &Ctx, text: &[u8], datatype: &str, ctl: &Control) -> Result<Id> {
    let n = text
        .len()
        .checked_add(datatype.len())
        .and_then(|n| n.checked_add(3))
        .ok_or_else(|| bad("literal capacity overflows"))?;
    let mut key = Buffer::new("algorithm-literal-key", n, ctl)?;
    key.push(b'"')?;
    key.extend(text)?;
    key.push(crate::id::KEY_SEP)?;
    key.push(b'^')?;
    key.extend(datatype.as_bytes())?;
    ctx.intern_algorithm_key(&key, ctl)
}
fn embedding(ctx: &Ctx, values: &[f32], ctl: &Control) -> Result<Id> {
    use std::fmt::Write;
    let mut text = Text::new(ctl)?;
    text.append(b"[")?;
    let mut work = Work::new(ctl)?;
    for (i, value) in values.iter().enumerate() {
        work.tick("algorithm-embedding")?;
        if i != 0 {
            text.append(b",")?;
        }
        let result = write!(&mut text, "{value:?}");
        text.formatted(result)?;
    }
    text.append(b"]")?;
    #[cfg(any(test, feature = "failpoints"))]
    algo::observe_projection(algo::ProjectionEvent::Materialized(
        "algorithm-embedding-text",
        text.bytes.capacity(),
    ));
    literal(ctx, &text.bytes, crate::vector::DATATYPE, ctl)
}

/// Table columns own exact capacities until their backing moves to the private carrier.
struct Col {
    pos: usize,
}
struct Builder<'a> {
    vars: Buffer<VarId>,
    cols: Buffer<Buffer<Id>>,
    ctl: &'a Control,
}
impl<'a> Builder<'a> {
    fn new(vars: &[VarId], ctl: &'a Control) -> Result<Self> {
        let mut owned = Buffer::new("algorithm-table-vars", vars.len(), ctl)?;
        owned.extend(vars)?;
        let mut cols = Buffer::new("algorithm-builder", vars.len(), ctl)?;
        for _ in vars {
            cols.push(Buffer::new("algorithm-empty-column", 0, ctl)?)?;
        }
        Ok(Self {
            vars: owned,
            cols,
            ctl,
        })
    }
    fn col(&self, v: Option<VarId>) -> Option<Col> {
        self.vars
            .iter()
            .position(|x| Some(*x) == v)
            .map(|pos| Col { pos })
    }
    fn set(
        &mut self,
        c: &Option<Col>,
        mut f: impl FnMut(usize) -> Result<Id>,
        n: usize,
    ) -> Result<()> {
        if let Some(c) = c {
            let mut next = Buffer::new("algorithm-column", n, self.ctl)?;
            let mut work = Work::new(self.ctl)?;
            for i in 0..n {
                work.tick("algorithm-column")?;
                next.push(f(i)?)?;
            }
            self.cols[c.pos] = next;
        }
        Ok(())
    }
    fn finish(mut self, len: usize, ctx: &Ctx) -> Result<Materialized> {
        ctx.check_output_rows(len)?;
        let mut cols = Buffer::new("algorithm-table-cols", self.cols.len(), self.ctl)?;
        for col in self.cols.drain() {
            cols.push(col.into_output())?;
        }
        let table = Table {
            vars: self.vars.into_output(),
            cols: cols.into_output(),
            len,
            sorted: Vec::new(),
        };
        let extra = 128u64
            .checked_add(if table.width() == 0 {
                memory::bytes::<Id>(len)?
            } else {
                0
            })
            .ok_or_else(|| bad("table capacity overflows"))?;
        // Metadata/zero-width estimate is owned as well as every real Vec capacity.
        self.ctl.charge(extra)?;
        let bytes = table_capacity(&table)?;
        let lease = self.ctl.output_lease(bytes)?;
        Ok(Materialized { table, lease })
    }
}
pub(super) fn table_capacity(t: &Table) -> Result<u64> {
    let data = t.cols.iter().try_fold(0u64, |n, c| {
        memory::sum(&[n, memory::bytes::<Id>(c.capacity())?])
    })?;
    memory::sum(&[
        data.max(if t.width() == 0 {
            memory::bytes::<Id>(t.len)?
        } else {
            0
        }),
        memory::bytes::<VarId>(t.vars.capacity())?,
        memory::bytes::<Vec<Id>>(t.cols.capacity())?,
        memory::bytes::<VarId>(t.sorted.capacity())?,
        128,
    ])
}
fn summary(ctx: &Ctx, g: &Graph, mut stats: algo::Stats, ctl: &Control) -> Result<Id> {
    stats.insert("nodes".into(), g.n().into());
    stats.insert("edges".into(), g.m().into());
    let mut text = Text::new(ctl)?;
    let result = serde_json::to_writer(&mut text, &stats);
    if let Some(error) = text.error.take() {
        return Err(error);
    }
    result.map_err(|e| bad(format_args!("summary encoding failed: {e}")))?;
    #[cfg(any(test, feature = "failpoints"))]
    algo::observe_projection(algo::ProjectionEvent::Materialized(
        "algorithm-summary-text",
        text.bytes.capacity(),
    ));
    literal(ctx, &text.bytes, RDF_JSON, ctl)
}

/// Stats have one row independently of the algorithm's node, pair or level rows.
fn summary_row(
    ctx: &Ctx,
    spec: &AlgoSpec,
    g: &Graph,
    stats: algo::Stats,
    vars: &[VarId],
    ctl: &Control,
) -> Result<Materialized> {
    ctx.check()?;
    ctx.check_output_rows(1)?;
    let mut b = Builder::new(vars, ctl)?;
    let col = b.col(spec.outs.summary);
    let sm = summary(ctx, g, stats, ctl)?;
    b.set(&col, |_| Ok(sm), 1)?;
    b.finish(1, ctx)
}

/// The indexes of the nodes to report, best first by `key` when a limit asks for the top
/// rows, else in node order.
fn copied<T: Copy>(name: &'static str, source: &[T], ctl: &Control) -> Result<Buffer<T>> {
    let mut out = Buffer::new(name, source.len(), ctl)?;
    out.extend(source)?;
    Ok(out)
}
fn ranking<T>(source: &[T], mut f: impl FnMut(&T) -> f64, ctl: &Control) -> Result<Buffer<f64>> {
    let mut out = Buffer::new("algorithm-rank", source.len(), ctl)?;
    let mut work = Work::new(ctl)?;
    for x in source {
        work.tick("algorithm-rank")?;
        out.push(f(x))?;
    }
    Ok(out)
}
fn selected(n: usize, mut keep: impl FnMut(usize) -> bool, ctl: &Control) -> Result<Buffer<u32>> {
    let mut out = Buffer::new("algorithm-pick", n, ctl)?;
    let mut work = Work::new(ctl)?;
    for i in 0..n {
        work.tick("algorithm-pick")?;
        if keep(i) {
            out.push(u32::try_from(i).map_err(|_| bad("node index overflows"))?)?;
        }
    }
    Ok(out)
}
fn pick(
    n: usize,
    limit: Option<usize>,
    key: Option<&[f64]>,
    descending: bool,
    ctl: &Control,
) -> Result<Buffer<u32>> {
    let mut idx = selected(n, |_| true, ctl)?;
    if let (Some(l), Some(k)) = (limit, key) {
        ctl.check()?;
        idx.sort_unstable_by(|&a, &b| {
            let o = k[a as usize].total_cmp(&k[b as usize]);
            let o = if descending { o.reverse() } else { o };
            o.then(a.cmp(&b))
        });
        ctl.check()?;
        idx.truncate(l);
    } else if let Some(l) = limit {
        idx.truncate(l);
    }
    Ok(idx)
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
) -> Result<Materialized> {
    let o = &spec.outs;
    // a call that asks for the summary alone reports one row (GDS's stats mode)
    let stats_only = spec.stats_only();
    let limit = spec
        .num("limit")
        .map(|l| usize::try_from(l.u64()).unwrap_or(usize::MAX));
    let mut b = Builder::new(vars, ctl)?;
    let c_node = b.col(o.node);
    let c_summary = b.col(o.summary);
    let n = g.n();
    let node_id = |i: usize| Id(g.ids[i]);
    // columns of the per-node algorithms: the reported nodes and their values
    macro_rules! per_node {
        ($key:expr, $desc:expr, $stats:expr, $cols:expr) => {{
            if stats_only {
                return summary_row(ctx, spec, g, $stats, vars, ctl);
            }
            let key: Option<&[f64]> = $key;
            let idx = pick(n, limit, key, $desc, ctl)?;
            ctx.check_output_rows(idx.len())?;
            b.set(&c_node, |i| Ok(node_id(idx[i] as usize)), idx.len())?;
            let sm = summary(ctx, g, $stats, ctl)?;
            b.set(&c_summary, |_| Ok(sm), idx.len())?;
            #[allow(clippy::redundant_closure_call)]
            ($cols)(&mut b, &idx)?;
            return b.finish(idx.len(), ctx);
        }};
    }
    match spec.algo {
        Algo::PageRank => {
            let p = algo::pagerank::Params {
                damping: spec.float("dampingFactor", 0.85),
                max_iterations: spec.int_u32("maxIterations", 20),
                tolerance: spec.float("tolerance", 1e-7),
                sources: copied("algorithm-pagerank-sources", sources, ctl)?.into_output(),
                fixed: spec.graphalytics,
            };
            if !spec.sources.is_empty() && sources.is_empty() {
                if stats_only {
                    // An explicitly empty personalization is no work, not a uniform run.
                    let mut stats = algo::Stats::new();
                    stats.insert("iterations".into(), 0.into());
                    stats.insert("converged".into(), true.into());
                    return summary_row(ctx, spec, g, stats, vars, ctl);
                }
                return b.finish(0, ctx);
            }
            let r = algo::pagerank::run(g, &p, par, ctl)?;
            let c = b.col(o.score);
            let score = r.score;
            per_node!(Some(&score), true, r.stats, |b: &mut Builder<'_>,
                                                    idx: &[u32]|
             -> Result<()> {
                b.set(&c, |i| Ok(dbl(score[idx[i] as usize])), idx.len())?;
                Ok(())
            })
        }
        Algo::Betweenness => {
            let max = crate::algo::pool::max_exact_betweenness();
            let samples = spec.int("samples", 0) as usize;
            // samples at or above the node count is an exact run as well
            if (samples == 0 || samples >= n) && n > max {
                return Err(bad(format!(
                    "betweenness without algo:samples below the node count on {n} nodes passes the limit of {max}; use algo:samples to estimate it"
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
            per_node!(Some(&score), true, stats, |b: &mut Builder<'_>,
                                                  idx: &[u32]|
             -> Result<()> {
                b.set(&c, |i| Ok(dbl(score[idx[i] as usize])), idx.len())?;
                Ok(())
            })
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
            per_node!(None, false, stats, |b: &mut Builder<'_>,
                                           idx: &[u32]|
             -> Result<()> {
                b.set(
                    &c,
                    |i| int(ctx, r.comp[idx[i] as usize] as i64, ctl),
                    idx.len(),
                )?;
                Ok(())
            })
        }
        Algo::LabelPropagation => {
            let r = algo::labelprop::run(g, spec.int_u32("maxIterations", 10), par, ctl)?;
            // the community is the node it is named for, as a store term
            let c = b.col(o.community);
            let mut stats = algo::Stats::new();
            stats.insert("iterations".into(), r.iterations.into());
            stats.insert("converged".into(), r.converged.into());
            per_node!(None, false, stats, |b: &mut Builder<'_>,
                                           idx: &[u32]|
             -> Result<()> {
                b.set(
                    &c,
                    |i| int(ctx, r.label[idx[i] as usize] as i64, ctl),
                    idx.len(),
                )?;
                Ok(())
            })
        }
        Algo::Louvain | Algo::Leiden => {
            let p = algo::community::Params {
                resolution: spec.float("resolution", 1.0),
                max_levels: spec.int_u32("maxLevels", 10),
                tolerance: spec.float("tolerance", 1e-4),
                leiden: spec.algo == Algo::Leiden,
                randomness: spec.float("randomness", 0.01),
                seed: spec.int("seed", 0),
                max_sweeps: spec.int_u32("maxIterations", 100),
            };
            let r = algo::community::run(g, &p, ctl)?;
            let mut stats = algo::Stats::new();
            stats.insert("levels".into(), r.levels.len().into());
            stats.insert(
                "modularity".into(),
                serde_json::json!(r.modularity.last().copied()),
            );
            let mut modularities =
                Buffer::new("algorithm-modularity-values", r.modularity.len(), ctl)?;
            let mut work = Work::new(ctl)?;
            for value in &r.modularity {
                work.tick("algorithm-modularity-values")?;
                modularities.push(serde_json::json!(value))?;
            }
            stats.insert(
                "modularities".into(),
                serde_json::Value::Array(modularities.into_output()),
            );
            stats.insert(
                "communities".into(),
                r.levels
                    .last()
                    .map_or(0, |l| l.iter().max().map_or(0, |m| *m as usize + 1))
                    .into(),
            );
            if stats_only {
                return summary_row(ctx, spec, g, stats, vars, ctl);
            }
            let sm = summary(ctx, g, stats, ctl)?;
            let (c_comm, c_level) = (b.col(o.community), b.col(o.level));
            // one row per node and level when the level is asked for, else the last level
            let level_count = if o.level.is_some() {
                r.levels.len()
            } else {
                usize::from(!r.levels.is_empty())
            };
            let count = level_count
                .checked_mul(n)
                .ok_or_else(|| bad("level rows overflow"))?;
            let mut pairs = Buffer::new("algorithm-level-pairs", count, ctl)?;
            let mut work = Work::new(ctl)?;
            let first = if o.level.is_some() {
                0
            } else {
                r.levels.len().saturating_sub(1)
            };
            for l in first..r.levels.len() {
                for i in 0..n as u32 {
                    work.tick("algorithm-level-pairs")?;
                    pairs.push((i, l))?;
                }
            }
            if let Some(l) = limit {
                pairs.truncate(l);
            }
            ctx.check_output_rows(pairs.len())?;
            b.set(&c_node, |i| Ok(node_id(pairs[i].0 as usize)), pairs.len())?;
            b.set(
                &c_comm,
                |i| int(ctx, r.levels[pairs[i].1][pairs[i].0 as usize] as i64, ctl),
                pairs.len(),
            )?;
            b.set(&c_level, |i| int(ctx, pairs[i].1 as i64, ctl), pairs.len())?;
            b.set(&c_summary, |_| Ok(sm), pairs.len())?;
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
                return summary_row(ctx, spec, g, stats, vars, ctl);
            }
            if let Some(l) = limit {
                r.sort_unstable_by(|a, b| {
                    b.2.total_cmp(&a.2).then(a.0.cmp(&b.0)).then(a.1.cmp(&b.1))
                });
                r.truncate(l);
            }
            ctx.check_output_rows(r.len())?;
            let (c_other, c_sim) = (b.col(o.other_node), b.col(o.similarity));
            b.set(&c_node, |i| Ok(node_id(r[i].0 as usize)), r.len())?;
            b.set(&c_other, |i| Ok(node_id(r[i].1 as usize)), r.len())?;
            b.set(&c_sim, |i| Ok(dbl(r[i].2)), r.len())?;
            let mut stats = algo::Stats::new();
            stats.insert("pairs".into(), r.len().into());
            let sm = summary(ctx, g, stats, ctl)?;
            b.set(&c_summary, |_| Ok(sm), r.len())?;
            b.finish(r.len(), ctx)
        }
        Algo::TriangleCount => {
            let t = algo::triangles::count(g, par, ctl)?;
            let rank = ranking(&t, |&x| x as f64, ctl)?;
            let c = b.col(o.triangles);
            let mut stats = algo::Stats::new();
            stats.insert(
                "triangles".into(),
                (t.iter().map(|&x| x as u64).sum::<u64>() / 3).into(),
            );
            per_node!(Some(&rank), true, stats, |b: &mut Builder<'_>,
                                                 idx: &[u32]|
             -> Result<()> {
                b.set(&c, |i| int(ctx, t[idx[i] as usize] as i64, ctl), idx.len())?;
                Ok(())
            })
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
            per_node!(Some(&coef), true, stats, |b: &mut Builder<'_>,
                                                 idx: &[u32]|
             -> Result<()> {
                b.set(&c_coef, |i| Ok(dbl(coef[idx[i] as usize])), idx.len())?;
                if let Some(t) = &tri {
                    b.set(
                        &c_tri,
                        |i| int(ctx, t[idx[i] as usize] as i64, ctl),
                        idx.len(),
                    )?;
                }
                Ok(())
            })
        }
        Algo::KCore => {
            let core = algo::kcore::run(g, ctl)?;
            let rank = ranking(&core, |&x| x as f64, ctl)?;
            let c = b.col(o.core);
            let mut stats = algo::Stats::new();
            stats.insert(
                "degeneracy".into(),
                core.iter().copied().max().unwrap_or(0).into(),
            );
            per_node!(Some(&rank), true, stats, |b: &mut Builder<'_>,
                                                 idx: &[u32]|
             -> Result<()> {
                b.set(
                    &c,
                    |i| int(ctx, core[idx[i] as usize] as i64, ctl),
                    idx.len(),
                )?;
                Ok(())
            })
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
                return summary_row(ctx, spec, g, stats, vars, ctl);
            }
            let c = b.col(o.depth);
            let inf = ctx.intern_term(&Term::Literal(oxrdf::Literal::new_simple_literal("inf")));
            // unreached nodes are left out, or bound to "inf" under Graphalytics
            let mut idx = selected(
                n,
                |i| spec.graphalytics || depth[i] != algo::traverse::UNREACHED,
                ctl,
            )?;
            let key = ranking(&depth, |&d| d as f64, ctl)?;
            if let Some(l) = limit {
                idx.sort_unstable_by(|&a, &b2| {
                    key[a as usize]
                        .total_cmp(&key[b2 as usize])
                        .then(a.cmp(&b2))
                });
                idx.truncate(l);
            }
            ctx.check_output_rows(idx.len())?;
            b.set(&c_node, |i| Ok(node_id(idx[i] as usize)), idx.len())?;
            b.set(
                &c,
                |i| {
                    Ok({
                        let d = depth[idx[i] as usize];
                        if d == algo::traverse::UNREACHED {
                            inf
                        } else {
                            int(ctx, d as i64, ctl)?
                        }
                    })
                },
                idx.len(),
            )?;
            let mut stats = algo::Stats::new();
            stats.insert(
                "reached".into(),
                depth
                    .iter()
                    .filter(|&&d| d != algo::traverse::UNREACHED)
                    .count()
                    .into(),
            );
            let sm = summary(ctx, g, stats, ctl)?;
            b.set(&c_summary, |_| Ok(sm), idx.len())?;
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
                return summary_row(ctx, spec, g, stats, vars, ctl);
            }
            let c = b.col(o.distance);
            let mut idx = selected(n, |i| spec.graphalytics || dist[i].is_finite(), ctl)?;
            if let Some(l) = limit {
                idx.sort_unstable_by(|&a, &b2| {
                    dist[a as usize]
                        .total_cmp(&dist[b2 as usize])
                        .then(a.cmp(&b2))
                });
                idx.truncate(l);
            }
            ctx.check_output_rows(idx.len())?;
            b.set(&c_node, |i| Ok(node_id(idx[i] as usize)), idx.len())?;
            b.set(&c, |i| Ok(dbl(dist[idx[i] as usize])), idx.len())?;
            let mut stats = algo::Stats::new();
            stats.insert(
                "reached".into(),
                dist.iter().filter(|d| d.is_finite()).count().into(),
            );
            let sm = summary(ctx, g, stats, ctl)?;
            b.set(&c_summary, |_| Ok(sm), idx.len())?;
            b.finish(idx.len(), ctx)
        }
        Algo::FastRp | Algo::Node2Vec => {
            let dim = spec.int("dimension", 128) as usize;
            let emb = if spec.algo == Algo::FastRp {
                let p = algo::fastrp::Params {
                    dimension: dim,
                    iteration_weights: copied(
                        "algorithm-iteration-weights",
                        spec.iteration_weights
                            .as_deref()
                            .unwrap_or(&[0.0, 1.0, 1.0]),
                        ctl,
                    )?
                    .into_output(),
                    node_self_influence: spec.float("nodeSelfInfluence", 0.0),
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
            per_node!(None, false, stats, |b: &mut Builder<'_>,
                                           idx: &[u32]|
             -> Result<()> {
                b.set(
                    &c,
                    |i| {
                        Ok({
                            let u = idx[i] as usize;
                            embedding(ctx, &emb[u * dim..(u + 1) * dim], ctl)?
                        })
                    },
                    idx.len(),
                )?;
                Ok(())
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
fn weight_in_graph(
    ctx: &Ctx,
    proj: &ProjectionSpec,
    s: u64,
    p: u64,
    o: u64,
    ctl: &Control,
    filter: &GraphFilter,
) -> Result<f32> {
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
        if !filter.accepts(r[3]) {
            return Ok(());
        }
        projection_rows(ctx, Perm::Pso, &[wprop, r[2]], |k| {
            work.tick("projection-weights")?;
            if !filter.accepts(k[3]) {
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
#[cfg(test)]
fn projection(
    ctx: &Ctx,
    spec: &AlgoSpec,
    edges: Option<&Table>,
    par: &Par,
    ctl: &Control,
) -> Result<(std::sync::Arc<Graph>, bool)> {
    projection_in_graph(ctx, spec, edges, par, ctl, &spec.proj.graph)
}
struct ProjectionInGraph<'a>(&'a ProjectionSpec, &'a GraphFilter);
impl std::fmt::Debug for ProjectionInGraph<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let p = self.0;
        f.debug_struct("ProjectionSpec")
            .field("predicates", &p.predicates)
            .field("direction", &p.direction)
            .field("node_classes", &p.node_classes)
            .field("node_class_names", &p.node_class_names)
            .field("weight", &p.weight)
            .field("default_weight", &p.default_weight)
            .field("aggregate", &p.aggregate)
            .field("pattern", &p.pattern)
            .field("graph", self.1)
            .field("rdf_type", &p.rdf_type)
            .field("reifies", &p.reifies)
            .finish()
    }
}
fn projection_key<'a>(
    ctx: &Ctx,
    spec: &AlgoSpec,
    filter: &GraphFilter,
    ctl: &'a Control,
) -> Result<Text<'a>> {
    let mut key = Text::new(ctl)?;
    let result = super::cache::write_view_key(ctx, &mut key);
    key.formatted(result)?;
    use std::fmt::Write;
    let result = write!(
        &mut key,
        "{:?}|{}",
        ProjectionInGraph(&spec.proj, filter),
        spec.direction().name()
    );
    key.formatted(result)?;
    Ok(key)
}
fn projection_in_graph(
    ctx: &Ctx,
    spec: &AlgoSpec,
    edges: Option<&Table>,
    par: &Par,
    ctl: &Control,
    filter: &GraphFilter,
) -> Result<(std::sync::Arc<Graph>, bool)> {
    let key = if ctx.snap.cacheable && spec.proj.pattern.is_none() && ctx.use_cache {
        Some(projection_key(ctx, spec, filter, ctl)?)
    } else {
        None
    };
    if let Some(k) = &key
        && let Some(g) = algo::cache::get(std::str::from_utf8(&k.bytes).expect("formatted UTF8"))
    {
        ctl.charge(g.base_bytes()?)?;
        return Ok((g, true));
    }
    let g = std::sync::Arc::new(build_graph_in_graph(ctx, spec, edges, par, ctl, filter)?);
    if let Some(key) = key {
        let bytes = key.bytes.capacity() as u64;
        algo::cache::put(
            String::from_utf8(key.bytes.into_output()).expect("formatted UTF8"),
            g.clone(),
        );
        // Ownership moved to the independently budgeted plain cache key.
        ctl.release(bytes);
    }
    Ok((g, false))
}

/// Build the projection of a call from the store, or from the rows of a nested pattern.
#[cfg(test)]
fn build_graph(
    ctx: &Ctx,
    spec: &AlgoSpec,
    edges: Option<&Table>,
    par: &Par,
    ctl: &Control,
) -> Result<Graph> {
    build_graph_in_graph(ctx, spec, edges, par, ctl, &spec.proj.graph)
}
fn build_graph_in_graph(
    ctx: &Ctx,
    spec: &AlgoSpec,
    edges: Option<&Table>,
    par: &Par,
    ctl: &Control,
    filter: &GraphFilter,
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
                if filter.accepts(k[3]) {
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
        // Under `GRAPH ?g` the call runs once for each graph, and the nested pattern's
        // rows of the other graphs are not edges of this one. The planner gives each
        // graph its own call with a constant graph, so this applies only when a call
        // carries the graph variable itself.
        let scope = match (spec.graph_var, filter) {
            (Some(gv), GraphFilter::One(g)) => Some((
                t.col_of(gv).ok_or_else(|| {
                    bad("the nested pattern does not bind the graph variable of GRAPH")
                })?,
                *g,
            )),
            (Some(_), _) => return Err(bad("a call under GRAPH runs for one graph at a time")),
            (None, _) => None,
        };
        let mut seen = Distinct::new(ctl)?;
        for i in 0..t.len() {
            work.tick("projection-pattern")?;
            if scope.is_some_and(|(c, g)| t.cols[c][i].0 != g) {
                continue;
            }
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
        // the direction of each projected predicate: the last listing of one wins
        let listed = proj.predicates.as_ref().map_or(0, Vec::len);
        let mut dirs = Buffer::new("projection-predicates", listed, ctl)?;
        if let Some(ps) = &proj.predicates {
            for (i, &(p, d)) in ps.iter().enumerate() {
                work.tick("projection-predicates")?;
                dirs.push((p, i, d.unwrap_or(call_dir)))?;
            }
            ctl.check()?;
            dirs.sort_unstable_by_key(|d| (d.0, d.1));
            ctl.check()?;
            let mut kept = 0;
            for i in 0..dirs.len() {
                work.tick("projection-predicates")?;
                if i + 1 == dirs.len() || dirs[i + 1].0 != dirs[i].0 {
                    dirs[kept] = dirs[i];
                    kept += 1;
                }
            }
            dirs.truncate(kept);
        }
        let direction_of = |p: u64| -> Option<Dir> {
            match &proj.predicates {
                Some(_) => dirs
                    .binary_search_by_key(&p, |d| d.0)
                    .ok()
                    .map(|i| dirs[i].2),
                None => (Some(p) != proj.rdf_type).then_some(call_dir),
            }
        };
        let edge = |col: &mut Collect<'_>, p: u64, s: u64, o: u64, dir: Dir| -> Result<()> {
            if !resource(ctx, Id(s), ctl)? || !resource(ctx, Id(o), ctl)? || !allowed(s, o) {
                return Ok(());
            }
            let value = if has_weights {
                weight_in_graph(ctx, proj, s, p, o, ctl, filter)?
            } else {
                0.0
            };
            col.push(s, o, value, dir)
        };
        let mut scan_work = Work::new(ctl)?;
        // A call for one graph reads that graph's own quads when they are fewer than the
        // quads of its predicates in all graphs. The calls of `GRAPH ?g` then read each
        // graph once between them, instead of every graph's edges once per graph.
        let graph_first = match filter {
            GraphFilter::One(g) => {
                let in_graph = ctx.snap.count(Perm::Gspo, &[*g])?;
                let mut by_predicate = 0u64;
                if proj.predicates.is_some() {
                    for d in dirs.iter() {
                        work.tick("projection-predicates")?;
                        by_predicate =
                            by_predicate.saturating_add(ctx.snap.count(Perm::Pso, &[d.0])?);
                    }
                } else {
                    by_predicate = ctx.snap.count(Perm::Pso, &[])?;
                }
                (in_graph < by_predicate).then_some(*g)
            }
            _ => None,
        };
        if let Some(g) = graph_first {
            projection_rows(ctx, Perm::Gspo, &[g], |k| {
                scan_work.tick("projection-edge")?;
                let [_, s, p, o] = k;
                match direction_of(p) {
                    Some(dir) => edge(&mut col, p, s, o, dir),
                    None => Ok(()),
                }
            })?;
        } else {
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
                            if !filter.accepts(g) || last == Some((p, s, o)) {
                                return Ok(());
                            }
                            last = Some((p, s, o));
                            edge(&mut col, p, s, o, dir)
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
            if proj.predicates.is_some() {
                for &(p, _, d) in dirs.iter() {
                    scan(p, p, d)?;
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

    const GRAPH_SERVICE: &str = "PREFIX ex:<http://ex/> PREFIX algo:<urn:x-sparkles:algo#> SELECT ?g ?n ?c WHERE { SERVICE algo:wcc { [] algo:predicate ex:p;algo:concurrency 1;algo:node ?n;algo:component ?c } }";
    fn graph_fixture(graphs: usize) -> crate::store::Store {
        use crate::io::{RdfFormat, Source};
        let store = crate::store::Store::in_memory(crate::store::StoreOptions {
            result_cache_bytes: 0,
            ..Default::default()
        });
        let mut data = String::from("@prefix ex:<http://ex/>.");
        for g in 0..graphs {
            data.push_str(&format!("ex:g{g:04} {{ex:a ex:p ex:b.}}"));
        }
        store
            .load(&[Source::from_bytes(data.into_bytes(), RdfFormat::TriG, None)])
            .unwrap();
        store
    }
    fn adapter(ctx: &Ctx) -> Node {
        let parsed = super::super::parse_query(GRAPH_SERVICE, None, &[]).unwrap();
        let spargebra::Query::Select { pattern, .. } = &parsed else {
            panic!("SELECT");
        };
        let mut planner = super::super::plan::Planner::new(ctx);
        planner.source = Some(&parsed);
        planner
            .plan(
                pattern,
                &super::super::plan::ActiveGraph::Var(ctx.var("g")),
                Vec::new(),
            )
            .unwrap()
    }
    fn algo_nodes<'a>(node: &'a Node, leaves: &mut Vec<&'a Node>) {
        if matches!(node.kind, Kind::AlgoCall(_)) {
            leaves.push(node);
        }
        for child in &node.children {
            algo_nodes(child, leaves);
        }
    }
    fn algo_infos<'a>(
        info: &'a super::super::PlanInfo,
        leaves: &mut Vec<&'a super::super::PlanInfo>,
    ) {
        if info.operator == "AlgoCall" {
            leaves.push(info);
        }
        for child in &info.children {
            algo_infos(child, leaves);
        }
    }
    fn graph_payload(snapshot: &crate::store::Snapshot, graph_ids: &[Id]) -> Vec<Vec<Id>> {
        let mut nodes = [
            snapshot.lookup_iri("http://ex/a").unwrap(),
            snapshot.lookup_iri("http://ex/b").unwrap(),
        ];
        nodes.sort_unstable();
        let mut expected = vec![Vec::new(), Vec::new(), Vec::new()];
        for &g in graph_ids {
            for n in nodes {
                expected[0].push(g);
                expected[1].push(n);
                expected[2].push(Id::from_i64(0).unwrap());
            }
        }
        expected
    }
    fn warmed_graph_accumulation_counts() -> Vec<(usize, usize, usize, usize)> {
        use std::cell::Cell;
        use std::rc::Rc;
        let mut observed = Vec::new();
        for graphs in [64, 128, 256] {
            let store = graph_fixture(graphs);
            let snapshot = store.snapshot();
            {
                let warm = Ctx::new(snapshot.clone());
                let node = adapter(&warm);
                drop(super::super::exec::execute_owned(&warm, &node).unwrap());
            }
            let ctx = Ctx::new(snapshot.clone());
            let node = adapter(&ctx);
            let mut leaves = Vec::new();
            algo_nodes(&node, &mut leaves);
            assert_eq!(leaves.len(), 1);
            let Kind::AlgoCall(spec) = &leaves[0].kind else {
                unreachable!()
            };
            assert_eq!(spec.graph_var, Some(ctx.var("g")));
            assert_eq!(spec.proj.graph, GraphFilter::Named);
            assert!(leaves[0].children.is_empty());
            let visits = Rc::new(Cell::new(0usize));
            let cells = Rc::new(Cell::new(0usize));
            let v = visits.clone();
            let a = cells.clone();
            let hook = memory::probe::install(
                move |event| match event {
                    memory::probe::Point::Work(
                        "algorithm-column"
                        | "algorithm-append-copy"
                        | "algorithm-append-relocate"
                        | "buffer-fill",
                    ) => v.set(v.get() + 1),
                    memory::probe::Point::Allocation(
                        "algorithm-column" | "algorithm-graph-column" | "algorithm-append-column",
                        capacity,
                    ) => a.set(a.get() + capacity),
                    _ => {}
                },
                None,
            );
            let (out, info) = super::super::exec::execute_owned(&ctx, &node).unwrap();
            drop(hook);
            assert!(!info.cached);
            assert!(ctx.use_cache);
            let mut infos = Vec::new();
            algo_infos(&info, &mut infos);
            assert_eq!(infos.len(), 1);
            assert_eq!(
                infos[0].counters.as_ref().unwrap()["cachedProjections"].as_u64(),
                Some(graphs as u64)
            );
            let graph_ids = snapshot.graph_ids().unwrap();
            assert_eq!(graph_ids.len(), graphs);
            assert_eq!(out.table.cols, graph_payload(&snapshot, &graph_ids));
            assert_eq!(out.table.vars, [ctx.var("g"), ctx.var("n"), ctx.var("c")]);
            assert_eq!(out.table.len, 2 * graphs);
            let capacity = table_capacity(&out.table).unwrap();
            assert_eq!(out.lease.as_ref().unwrap().bytes, capacity);
            assert!(out.lease.as_ref().unwrap().service.is_none());
            let bound = 8 * out.table.width() * out.table.len;
            eprintln!(
                "GRAPH_ACCUMULATION {}",
                serde_json::json!({"context":"public-planner-ActiveGraph-Var", "graphs":graphs, "rows":out.table.len, "width":out.table.width(), "copy_visits":visits.get(), "allocated_column_cells":cells.get(), "linear_bound":bound, "cached_projections":graphs, "retained_capacity_bytes":capacity,"column_capacities":out.table.cols.iter().map(Vec::capacity).collect::<Vec<_>>(), "columns":out.table.cols.iter().map(|c|c.iter().map(|id|id.0).collect::<Vec<_>>()).collect::<Vec<_>>()})
            );
            observed.push((graphs, visits.get(), cells.get(), bound));
        }
        observed
    }
    #[test]
    fn warmed_graph_accumulation_copy_work_stays_linear() {
        let observed = warmed_graph_accumulation_counts();
        assert!(
            observed.iter().all(|(_, visits, _, bound)| visits <= bound),
            "actual column copy visits exceed linear bound: {observed:?}"
        );
    }
    #[test]
    fn warmed_graph_accumulation_allocation_volume_stays_linear() {
        let observed = warmed_graph_accumulation_counts();
        assert!(
            observed.iter().all(|(_, _, cells, bound)| cells <= bound),
            "actual requested column cells exceed linear bound: {observed:?}"
        );
    }
    #[test]
    fn ordinary_graph_query_keeps_named_leaf_cache_and_payload_semantics() {
        let store = graph_fixture(64);
        let snapshot = store.snapshot();
        let query = GRAPH_SERVICE.replace("WHERE { SERVICE", "WHERE { GRAPH ?g { SERVICE");
        let query = format!("{query} }}");
        drop(super::super::query(snapshot.clone(), &query, &Default::default()).unwrap());
        let result = super::super::query(snapshot.clone(), &query, &Default::default()).unwrap();
        assert!(!result.plan.cached);
        assert!(result.ctx.use_cache);
        let mut leaves = Vec::new();
        algo_infos(&result.plan, &mut leaves);
        assert_eq!(leaves.len(), 64);
        for info in leaves {
            assert_eq!(
                info.counters.as_ref().unwrap()["cachedProjections"].as_u64(),
                Some(1)
            );
        }
        let parsed = super::super::parse_query(&query, None, &[]).unwrap();
        let prepared =
            super::super::prepare_query(snapshot.clone(), &parsed, &Default::default(), None, None)
                .unwrap();
        let mut nodes = Vec::new();
        algo_nodes(&prepared.node, &mut nodes);
        assert_eq!(nodes.len(), 64);
        for node in nodes {
            let Kind::AlgoCall(spec) = &node.kind else {
                unreachable!()
            };
            assert_eq!(spec.graph_var, None);
            assert!(matches!(spec.proj.graph, GraphFilter::One(_)));
        }
        assert_eq!(
            result.table.cols,
            graph_payload(&snapshot, &snapshot.graph_ids().unwrap())
        );
    }
    #[test]
    fn adapter_graph_subset_substitution_and_empty_visibility_are_preserved() {
        let store = graph_fixture(3);
        let snapshot = store.snapshot();
        let ids = snapshot.graph_ids().unwrap();
        for case in 0..3 {
            let mut ctx = Ctx::new(snapshot.clone());
            if case != 1 {
                ctx.dataset.named = Some(if case == 0 { vec![ids[1]] } else { vec![] });
            }
            let parsed = super::super::parse_query(GRAPH_SERVICE, None, &[]).unwrap();
            let spargebra::Query::Select { pattern, .. } = &parsed else {
                unreachable!()
            };
            let mut planner = super::super::plan::Planner::new(&ctx);
            planner.source = Some(&parsed);
            if case == 1 {
                planner.subst.insert(ctx.var("g"), ids[2]);
            }
            let node = planner
                .plan(
                    pattern,
                    &super::super::plan::ActiveGraph::Var(ctx.var("g")),
                    Vec::new(),
                )
                .unwrap();
            let mut leaves = Vec::new();
            algo_nodes(&node, &mut leaves);
            if case == 2 {
                assert!(leaves.is_empty());
            } else {
                assert_eq!(leaves.len(), 1);
                let Kind::AlgoCall(spec) = &leaves[0].kind else {
                    unreachable!()
                };
                if case == 0 {
                    assert_eq!(spec.graph_var, Some(ctx.var("g")));
                    assert_eq!(spec.proj.graph, GraphFilter::Set(vec![ids[1].0]));
                } else {
                    assert_eq!(spec.graph_var, None);
                    assert_eq!(spec.proj.graph, GraphFilter::One(ids[2].0));
                }
            }
            let (out, _) = super::super::exec::execute_owned(&ctx, &node).unwrap();
            if case == 2 {
                assert_eq!(out.table.len, 0);
            } else {
                let mut expected = graph_payload(&snapshot, &[ids[if case == 0 { 1 } else { 2 }]]);
                // Substitution selects the active graph; an unbound SELECT output
                // still has no input binding table supplying g.
                if case == 1 {
                    expected[0].fill(Id::UNDEF);
                }
                for (column, &var) in out.table.cols.iter().zip(&out.table.vars) {
                    let pos = [ctx.var("g"), ctx.var("n"), ctx.var("c")]
                        .iter()
                        .position(|&v| v == var)
                        .unwrap();
                    assert_eq!(column, &expected[pos]);
                }
                assert_eq!(out.table.len, 2);
            }
        }
    }
    #[test]
    fn adapter_all_graphs_share_peak_and_exact_service_limit() {
        let store = graph_fixture(8);
        let snapshot = store.snapshot();
        let peak = {
            let ctx = Ctx::new(snapshot.clone());
            let (out, info) = super::super::exec::execute_owned(&ctx, &adapter(&ctx)).unwrap();
            let mut leaves = Vec::new();
            algo_infos(&info, &mut leaves);
            let peak = leaves[0].counters.as_ref().unwrap()["peakBytes"]
                .as_u64()
                .unwrap();
            assert_eq!(
                out.table.cols,
                graph_payload(&snapshot, &snapshot.graph_ids().unwrap())
            );
            peak
        };
        // Warm the exact cached projection path before measuring its own peak.
        let warm_peak = {
            let ctx = Ctx::new(snapshot.clone());
            let (_out, info) = super::super::exec::execute_owned(&ctx, &adapter(&ctx)).unwrap();
            let mut leaves = Vec::new();
            algo_infos(&info, &mut leaves);
            leaves[0].counters.as_ref().unwrap()["peakBytes"]
                .as_u64()
                .unwrap()
        };
        assert!(peak >= warm_peak);
        for short in [false, true] {
            let query = GRAPH_SERVICE.replace(
                "algo:concurrency 1;",
                &format!(
                    "algo:concurrency 1;algo:maxMemory {};",
                    warm_peak - u64::from(short)
                ),
            );
            let parsed = super::super::parse_query(&query, None, &[]).unwrap();
            let spargebra::Query::Select { pattern, .. } = &parsed else {
                unreachable!()
            };
            let mut ctx = Ctx::new(snapshot.clone());
            let pool = super::super::pool::QueryPool::new(64 << 20);
            let grant = Arc::new(pool.empty_grant());
            ctx.set_grant(Some(grant.clone()));
            let mut planner = Planner::new(&ctx);
            planner.source = Some(&parsed);
            let node = planner
                .plan(pattern, &ActiveGraph::Var(ctx.var("g")), Vec::new())
                .unwrap();
            let planned_live = ctx.memory().live.load(Ordering::Acquire);
            let planned_local = ctx.local_len();
            let result = super::super::exec::execute_owned(&ctx, &node);
            if short {
                assert!(
                    matches!(result,Err(Error::BudgetExceeded(e)) if e.kind==crate::error::BudgetKind::Memory)
                );
            } else {
                let (out, info) = result.unwrap();
                let mut leaves = Vec::new();
                algo_infos(&info, &mut leaves);
                assert_eq!(
                    leaves[0].counters.as_ref().unwrap()["peakBytes"].as_u64(),
                    Some(warm_peak)
                );
                assert_eq!(
                    out.table.cols,
                    graph_payload(&snapshot, &snapshot.graph_ids().unwrap())
                );
                assert_eq!(
                    out.lease.as_ref().unwrap().bytes,
                    table_capacity(&out.table).unwrap()
                );
            }
            // The inherited per-node path interns its JSON summary even if the
            // summary column was not requested. This is context-owned key storage,
            // independent of native graph/output capacity.
            let retained = (planned_local..ctx.local_len())
                .map(|id| ctx.with_local_key(id, |key| key.len() as u64 + 96).unwrap())
                .sum::<u64>();
            assert!(retained > 0);
            assert_eq!(
                ctx.memory().live.load(Ordering::Acquire),
                planned_live + retained
            );
            let second = super::super::exec::execute_owned(&ctx, &node);
            assert_eq!(second.is_err(), short);
            drop(second);
            assert_eq!(
                ctx.memory().live.load(Ordering::Acquire),
                planned_live + retained
            );
            drop(planner);
            drop(node);
            let weak = Arc::downgrade(ctx.memory());
            drop(ctx);
            assert!(weak.upgrade().is_none());
            assert_eq!(grant.used(), 0);
        }
    }
    // These bounded fixture Vecs exist before admission/observation. The helper
    // adopts their real capacities, rather than claiming to observe their allocation.
    fn append_table(width: usize, len: usize, capacity: usize, start: u64) -> Table {
        let mut cols = Vec::with_capacity(width);
        for c in 0..width {
            let mut values = Vec::with_capacity(capacity);
            for r in 0..len {
                values.push(Id(start + r as u64 + (c as u64) * 100));
            }
            cols.push(values);
        }
        Table {
            vars: (0..width as VarId).collect(),
            cols,
            len,
            sorted: Vec::new(),
        }
    }
    fn admit_append(table: Table, ctl: &Control) -> Materialized {
        let bytes = table_capacity(&table).unwrap();
        ctl.charge(bytes).unwrap();
        Materialized {
            table,
            lease: ctl.output_lease(bytes).unwrap(),
        }
    }
    #[test]
    fn append_reuses_spare_capacity_and_grows_once_for_large_parts() {
        use std::{cell::RefCell, rc::Rc};
        let store = graph_fixture(1);
        let ctx = Ctx::new(store.snapshot());
        let ctl = Control::for_query(&ctx, u64::MAX).unwrap();
        let mut out = admit_append(append_table(2, 0, 0, 0), &ctl);
        let events = Rc::new(RefCell::new(Vec::new()));
        let captured = events.clone();
        let scope = memory::probe::install(move |p| captured.borrow_mut().push(p), None);
        let mut expected = vec![Vec::new(), Vec::new()];
        for (len, cap) in [(3, 4), (1, 4), (1, 8), (0, 8), (19, 24)] {
            // Construction and adoption are independent of append's observed allocs.
            let part = admit_append(
                append_table(2, len, len, 1000 + expected[0].len() as u64),
                &ctl,
            );
            for (e, c) in expected.iter_mut().zip(&part.table.cols) {
                e.extend(c);
            }
            let before = events.borrow().len();
            let ptrs = out.table.cols.iter().map(Vec::as_ptr).collect::<Vec<_>>();
            out = append(&ctx, out, part, &ctl).unwrap();
            assert_eq!(out.table.cols, expected);
            assert_eq!(out.table.len, expected[0].len());
            assert_eq!(
                out.lease.as_ref().unwrap().bytes,
                table_capacity(&out.table).unwrap()
            );
            assert!(out.table.cols.iter().all(|c| c.capacity() == cap));
            let new_events = &events.borrow()[before..];
            let allocations = new_events
                .iter()
                .filter(|p| {
                    matches!(
                        p,
                        memory::probe::Point::Allocation("algorithm-append-column", _)
                    )
                })
                .count();
            if len == 1 && cap == 4 || len == 0 {
                assert_eq!(allocations, 0);
                assert_eq!(
                    out.table.cols.iter().map(Vec::as_ptr).collect::<Vec<_>>(),
                    ptrs
                );
            } else {
                assert_eq!(allocations, 2);
            }
            if len == 0 {
                assert!(!new_events.iter().any(|p| matches!(
                    p,
                    memory::probe::Point::Work(
                        "algorithm-append-copy" | "algorithm-append-relocate"
                    )
                )));
            }
        }
        drop(scope);
        drop(out);
        assert_eq!(ctx.memory().live.load(Ordering::Acquire), 0);
    }
    #[test]
    fn grown_outputs_of_two_services_keep_each_original_query_owner() {
        use crate::sparql::pool::QueryPool;
        for shared in [false, true] {
            let store = graph_fixture(8);
            let snapshot = store.snapshot();
            // Independent warm caller ends before the two measured owners begin.
            {
                let ctx = Ctx::new(snapshot.clone());
                drop(super::super::exec::execute_owned(&ctx, &adapter(&ctx)).unwrap());
            }
            let pool = QueryPool::new(64 << 20);
            let a = Arc::new(pool.empty_grant());
            let b = if shared {
                a.clone()
            } else {
                Arc::new(pool.empty_grant())
            };
            let mut ctx = Ctx::new(snapshot.clone());
            ctx.set_grant(Some(a.clone()));
            let (first, info) = super::super::exec::execute_owned(&ctx, &adapter(&ctx)).unwrap();
            let mut leaves = Vec::new();
            algo_infos(&info, &mut leaves);
            assert_eq!(
                leaves[0].counters.as_ref().unwrap()["cachedProjections"].as_u64(),
                Some(8)
            );
            assert!(first.lease.as_ref().unwrap().service.is_none());
            let live = ctx.memory().live.load(Ordering::Acquire);
            let held_a = a.used();
            ctx.reconfigure_grant(Some(b.clone()));
            let (second, info) = super::super::exec::execute_owned(&ctx, &adapter(&ctx)).unwrap();
            let mut leaves = Vec::new();
            algo_infos(&info, &mut leaves);
            assert_eq!(
                leaves[0].counters.as_ref().unwrap()["cachedProjections"].as_u64(),
                Some(8)
            );
            assert_eq!(
                first.table.cols,
                graph_payload(&snapshot, &snapshot.graph_ids().unwrap())
            );
            assert_eq!(second.table.cols, first.table.cols);
            let bytes = table_capacity(&second.table).unwrap();
            assert_eq!(second.lease.as_ref().unwrap().bytes, bytes);
            assert_eq!(ctx.memory().live.load(Ordering::Acquire), live + bytes);
            if !shared {
                assert_eq!(a.used(), held_a);
            }
            drop(second);
            assert_eq!(ctx.memory().live.load(Ordering::Acquire), live);
            if !shared {
                assert_eq!(a.used(), held_a);
            }
            let weak = Arc::downgrade(ctx.memory());
            drop(ctx);
            assert!(weak.upgrade().is_some());
            assert!(a.used() > 0);
            drop(first);
            assert!(weak.upgrade().is_none());
            assert_eq!(a.used(), 0);
            assert_eq!(b.used(), 0);
        }
    }
    #[test]
    fn append_growth_admits_full_overlap_and_transfers_without_a_gap() {
        use crate::sparql::pool::QueryPool;
        use std::{cell::RefCell, rc::Rc};
        for boundary in ["service", "query", "pool"] {
            for short in [false, true] {
                let store = graph_fixture(1);
                let mut ctx = Ctx::new(store.snapshot());
                let mut other = Ctx::new(store.snapshot());
                let left_table = append_table(1, 3, 4, 10);
                let right_table = append_table(1, 3, 3, 20);
                let left_bytes = table_capacity(&left_table).unwrap();
                let right_bytes = table_capacity(&right_table).unwrap();
                let replacement = memory::bytes::<Id>(8).unwrap();
                let service_peak = left_bytes + right_bytes + replacement;
                let query_peak = service_peak + 135;
                let pool_peak = query_peak + 77;
                ctx.mem_limit = if boundary == "query" {
                    query_peak - u64::from(short)
                } else {
                    u64::MAX
                };
                let pool = QueryPool::new(if boundary == "pool" {
                    pool_peak - u64::from(short)
                } else {
                    64 << 20
                });
                let grant = Arc::new(pool.empty_grant());
                ctx.set_grant(Some(grant.clone()));
                other.set_grant(Some(grant.clone()));
                let other_held = other.charge(77).unwrap();
                let held = ctx.charge(135).unwrap();
                let ctl = Control::for_query(
                    &ctx,
                    if boundary == "service" {
                        service_peak - u64::from(short)
                    } else {
                        u64::MAX
                    },
                )
                .unwrap();
                let left = admit_append(left_table, &ctl);
                let right = admit_append(right_table, &ctl);
                let records = Rc::new(RefCell::new(Vec::new()));
                let captured = records.clone();
                let ledger = ctx.memory().clone();
                let native = ctl.clone();
                let observed = grant.clone();
                let hook = memory::probe::install(
                    move |p| {
                        if matches!(
                            p,
                            memory::probe::Point::Allocation("algorithm-append-column", _)
                                | memory::probe::Point::Work(
                                    "algorithm-append-transferred"
                                        | "algorithm-append-old-dropped"
                                        | "algorithm-append-resized"
                                )
                        ) {
                            captured.borrow_mut().push((
                                p,
                                native.observed_used(),
                                ledger.live.load(Ordering::Acquire),
                                observed.used(),
                            ));
                        }
                    },
                    None,
                );
                let result = append(&ctx, left, right, &ctl);
                drop(hook);
                if short {
                    assert!(
                        records.borrow().is_empty(),
                        "{boundary} must refuse before allocation"
                    );
                    let Err(Error::BudgetExceeded(error)) = result else {
                        panic!("expected {boundary} capacity refusal")
                    };
                    assert_eq!(
                        error.kind,
                        if boundary == "pool" {
                            crate::error::BudgetKind::ServerMemory
                        } else {
                            crate::error::BudgetKind::Memory
                        }
                    );
                    assert_eq!(ctx.memory().live.load(Ordering::Acquire), 135);
                } else {
                    let out = result.unwrap();
                    let records = records.borrow();
                    assert_eq!(records.len(), 4);
                    for (_, native, live, pool) in &records[..3] {
                        assert_eq!((*native, *live), (service_peak, query_peak));
                        assert!(*pool >= pool_peak);
                    }
                    assert_eq!(
                        (records[3].1, records[3].2),
                        (
                            service_peak - memory::bytes::<Id>(4).unwrap(),
                            query_peak - memory::bytes::<Id>(4).unwrap()
                        )
                    );
                    assert_eq!(
                        out.table.cols[0],
                        [Id(10), Id(11), Id(12), Id(20), Id(21), Id(22)]
                    );
                    let retained = table_capacity(&out.table).unwrap();
                    assert_eq!(out.lease.as_ref().unwrap().bytes, retained);
                    assert_eq!(ctx.memory().live.load(Ordering::Acquire), 135 + retained);
                    assert_eq!(ctl.observed_used(), retained);
                    drop(out);
                }
                assert_eq!(other.memory().live.load(Ordering::Acquire), 77);
                drop(ctl);
                drop(held);
                drop(ctx);
                assert!(grant.used() >= 77);
                drop(other_held);
                drop(other);
                assert_eq!(grant.used(), 0);
            }
        }
    }
    #[test]
    fn append_zero_width_precharges_rows_and_checks_arithmetic_without_allocating() {
        let store = graph_fixture(1);
        for short in [false, true] {
            let ctx = Ctx::new(store.snapshot());
            let left = append_table(0, 2, 0, 0);
            let right = append_table(0, 3, 0, 0);
            let peak = table_capacity(&left).unwrap() + table_capacity(&right).unwrap() + 24;
            let ctl = Control::for_query(&ctx, peak - u64::from(short)).unwrap();
            let left = admit_append(left, &ctl);
            let right = admit_append(right, &ctl);
            let hook = memory::probe::install(
                |p| assert!(!matches!(p, memory::probe::Point::Allocation(..))),
                None,
            );
            let out = append(&ctx, left, right, &ctl);
            drop(hook);
            if short {
                assert!(matches!(out, Err(Error::BudgetExceeded(_))));
            } else {
                let out = out.unwrap();
                assert_eq!(out.table.len, 5);
                assert_eq!(
                    out.lease.as_ref().unwrap().bytes,
                    table_capacity(&out.table).unwrap()
                );
                drop(out);
            }
            assert_eq!(ctx.memory().live.load(Ordering::Acquire), 0);
        }
        let ctx = Ctx::new(store.snapshot());
        let ctl = Control::for_query(&ctx, u64::MAX).unwrap();
        // Deliberately impossible logical lengths, with no large backing allocation.
        let fake = |len| {
            Materialized::plain(Table {
                vars: Vec::new(),
                cols: Vec::new(),
                len,
                sorted: Vec::new(),
            })
        };
        let hook = memory::probe::install(
            |p| assert!(!matches!(p, memory::probe::Point::Allocation(..))),
            None,
        );
        assert!(matches!(
            append(&ctx, fake(usize::MAX), fake(1), &ctl),
            Err(Error::Invalid(_))
        ));
        assert!(matches!(
            memory::bytes::<Id>(usize::MAX),
            Err(Error::Invalid(_))
        ));
        drop(hook);
    }
    #[test]
    fn append_growth_failure_unwind_and_owner_mismatch_drop_backing_before_charge() {
        for mode in ["allocation", "unwind", "owner"] {
            let store = graph_fixture(1);
            let ctx = Ctx::new(store.snapshot());
            let ctl = Control::for_query(&ctx, u64::MAX).unwrap();
            let left = admit_append(append_table(1, 3, 4, 10), &ctl);
            let right = admit_append(append_table(1, 3, 3, 20), &ctl);
            let foreign = Control::for_query(&ctx, u64::MAX).unwrap();
            let hook = memory::probe::install(
                move |p| {
                    if mode == "unwind"
                        && matches!(p, memory::probe::Point::Work("algorithm-append-relocate"))
                    {
                        panic!("caught actual relocation unwind");
                    }
                },
                if mode == "allocation" {
                    Some("algorithm-append-column")
                } else {
                    None
                },
            );
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                append(
                    &ctx,
                    left,
                    right,
                    if mode == "owner" { &foreign } else { &ctl },
                )
            }));
            drop(hook);
            if mode == "unwind" {
                assert!(result.is_err());
            } else {
                assert!(matches!(result.unwrap(), Err(Error::Invalid(_))));
            }
            drop(foreign);
            drop(ctl);
            assert_eq!(ctx.memory().live.load(Ordering::Acquire), 0);
        }
    }
    #[test]
    fn append_polls_cancellation_and_deadline_at_real_copy_boundaries() {
        for timeout in [false, true] {
            let store = graph_fixture(1);
            let mut ctx = Ctx::new(store.snapshot());
            let left_table = append_table(1, 1, 1, 10);
            let right_table = append_table(
                1,
                crate::algo::CHECK_EVERY + 2,
                crate::algo::CHECK_EVERY + 2,
                20,
            );
            if timeout {
                ctx.deadline = Some(Instant::now() + std::time::Duration::from_secs(1));
            }
            let ctl = Control::for_query(&ctx, u64::MAX).unwrap();
            let left = admit_append(left_table, &ctl);
            let right = admit_append(right_table, &ctl);
            let cancel = ctx.cancel.clone();
            let deadline = ctx.deadline;
            let seen = std::rc::Rc::new(std::cell::Cell::new(0));
            let visits = seen.clone();
            let scope = memory::probe::install(
                move |p| {
                    if matches!(p, memory::probe::Point::Work("algorithm-append-copy")) {
                        visits.set(visits.get() + 1);
                        if let Some(deadline) = deadline {
                            while Instant::now() < deadline {
                                std::hint::spin_loop();
                            }
                        } else {
                            cancel.store(true, Ordering::Release);
                        }
                    }
                },
                None,
            );
            let result = append(&ctx, left, right, &ctl);
            drop(scope);
            assert!(seen.get() > 0, "must reach a real incoming-copy boundary");
            assert!(
                seen.get() <= crate::algo::CHECK_EVERY,
                "must stop at a bounded check, not only after copying the whole incoming part"
            );
            if timeout {
                assert!(matches!(result, Err(Error::Timeout)));
            } else {
                assert!(matches!(result, Err(Error::Cancelled)));
            }
            drop(ctl);
            assert_eq!(ctx.memory().live.load(Ordering::Acquire), 0);
        }
    }
    #[test]
    fn effective_graph_key_is_exactly_the_original_owned_debug_encoding() {
        let (_store, mut ctx, mut spec) = weighted();
        ctx.dataset.default = Some(vec![Id(13), Id(21)]);
        ctx.dataset.named = Some(vec![Id(42), Id(78)]);
        ctx.dataset.union_default = true;
        spec.proj.predicates = Some(vec![(11, Some(Dir::Backward)), (15, None)]);
        spec.proj.direction = Some(Dir::Undirected);
        spec.proj.node_classes = vec![7, 8];
        spec.proj.node_class_names = vec!["quoted \" name".into(), "unicode λ".into()];
        spec.proj.weight = Some(None);
        spec.proj.default_weight = 2.5;
        spec.proj.aggregate = Aggregate::Sum;
        spec.proj.pattern = Some((3, 8, Some(9)));
        spec.proj.rdf_type = Some(18);
        spec.proj.reifies = Some(19);
        let ctl = Control::unlimited();
        for graph in [
            GraphFilter::All,
            GraphFilter::Default,
            GraphFilter::Named,
            GraphFilter::One(0),
            GraphFilter::One(47),
            GraphFilter::Set(vec![]),
            GraphFilter::Set(vec![4, 7, 12]),
        ] {
            let mut owned = spec.proj.clone();
            owned.graph = graph.clone();
            let expected = format!(
                "{}{:?}|{}",
                super::super::cache::view_key(&ctx),
                owned,
                spec.direction().name()
            );
            let key = projection_key(&ctx, &spec, &graph, &ctl).unwrap();
            assert_eq!(&*key.bytes, expected.as_bytes());
        }
    }
    #[test]
    fn checked_serializers_preserve_lexical_bytes_and_refuse_growth_with_original_error() {
        let (_store, ctx, _spec) = weighted();
        for values in [
            &[][..],
            &[
                0.0,
                -0.0,
                1.25,
                f32::MAX,
                f32::MIN_POSITIVE,
                f32::INFINITY,
                f32::NAN,
            ][..],
        ] {
            let ctl = Control::for_query(&ctx, u64::MAX).unwrap();
            let id = embedding(&ctx, values, &ctl).unwrap();
            assert_eq!(
                ctx.term(id).unwrap(),
                Term::Literal(crate::vector::literal(values))
            );
        }
        let ctl = Control::for_query(&ctx, 3).unwrap();
        assert!(
            matches!(embedding(&ctx,&[1.25],&ctl),Err(Error::BudgetExceeded(b)) if b.kind==crate::error::BudgetKind::Memory)
        );
        let ctl = Control::for_query(&ctx, u64::MAX).unwrap();
        let mut text = Text::new(&ctl).unwrap();
        ctx.cancel.store(true, Ordering::Relaxed);
        use std::fmt::Write;
        let result = write!(&mut text, "must refuse before growth");
        assert!(matches!(text.formatted(result), Err(Error::Cancelled)));
    }
    #[test]
    fn actual_service_buffers_and_output_lease_share_one_query_owner() {
        use crate::sparql::pool::QueryPool;
        for case in 0..5 {
            let (store, _old, mut spec) = weighted();
            if case == 3 {
                super::super::update::update(&store,"INSERT DATA { GRAPH <urn:g1> { <http://ex/a> <http://ex/p> <http://ex/b> } GRAPH <urn:g2> { <http://ex/b> <http://ex/p> <http://ex/a> } }",&Default::default()).unwrap();
            }
            let mut ctx = Ctx::new(store.snapshot());
            let pool = QueryPool::new(64 << 20);
            let grant = Arc::new(pool.empty_grant());
            ctx.set_grant(Some(grant.clone()));
            spec.algo = match case {
                1 => Algo::FastRp,
                2 => Algo::Bfs,
                _ => Algo::Wcc,
            };
            spec.proj.weight = None;
            spec.proj.node_classes.clear();
            spec.proj.node_class_names.clear();
            spec.outs = Outs {
                node: Some(0),
                summary: Some(2),
                ..Default::default()
            };
            match case {
                1 => {
                    spec.outs.embedding = Some(1);
                    spec.nums.insert("dimension".into(), Num::Int(8));
                }
                2 => {
                    spec.outs.depth = Some(1);
                    spec.sources = vec![PathEnd::Const(Id::UNDEF)];
                }
                _ => spec.outs.component = Some(1),
            }
            if case == 0 {
                spec.nums.insert("limit".into(), Num::Int(1));
            }
            if case == 3 {
                spec.graph_var = Some(3);
            }
            let input = (case == 4).then(|| Table {
                vars: vec![4],
                cols: vec![vec![Id(77), Id(88)]],
                len: 2,
                sorted: vec![],
            });
            let held = ctx
                .charge(input.as_ref().map_or(0, Table::mem_bytes))
                .unwrap();
            let ledger = ctx.memory().clone();
            let observed_grant = grant.clone();
            let events = Arc::new(std::sync::Mutex::new(Vec::new()));
            let seen = events.clone();
            let retirement = Arc::new(std::sync::Mutex::new(Vec::new()));
            let retired = retirement.clone();
            algo::set_projection_observer(Some(Arc::new(move |event| {
                if let algo::ProjectionEvent::Allocation(name, capacity, bytes, used) = event {
                    let live = ledger.live.load(Ordering::Acquire);
                    assert!(
                        live >= used && observed_grant.used() >= live,
                        "{name} before query admission: native={used}, query={live}"
                    );
                    seen.lock().unwrap().push((name, capacity, bytes));
                } else if let algo::ProjectionEvent::Materialized(name, bytes) = event
                    && matches!(
                        name,
                        "algorithm-retirement-before" | "algorithm-retirement-after"
                    )
                {
                    retired.lock().unwrap().push((
                        name,
                        bytes,
                        ledger.live.load(Ordering::Acquire),
                        observed_grant.used(),
                    ));
                }
            })));
            let (out, _) = run(
                &ctx,
                &spec,
                input,
                None,
                if case == 3 { &[0, 1, 2, 3] } else { &[0, 1, 2] },
            )
            .unwrap();
            algo::set_projection_observer(None);
            let retired = retirement.lock().unwrap();
            assert_eq!(retired.len(), 2);
            assert_eq!(retired[0].0, "algorithm-retirement-before");
            assert_eq!(retired[1].0, "algorithm-retirement-after");
            assert_eq!(
                (retired[0].1, retired[0].2, retired[0].3),
                (retired[1].1, retired[1].2, retired[1].3),
                "retirement must not release/re-admit backing"
            );
            assert_eq!(
                out.lease.as_ref().unwrap().bytes,
                table_capacity(&out.table).unwrap()
            );
            assert!(grant.used() >= out.lease.as_ref().unwrap().bytes);
            if case == 0 {
                assert_eq!(out.table.len, 1);
                assert!(
                    events
                        .lock()
                        .unwrap()
                        .iter()
                        .any(|(n, c, _)| *n == "algorithm-pick" && *c == 2)
                );
            }
            if case == 2 {
                assert_eq!(out.table.len, 0);
                assert!(out.lease.as_ref().unwrap().bytes > 0);
            }
            if case == 3 {
                assert_eq!(out.table.len, 4);
                assert!(
                    events
                        .lock()
                        .unwrap()
                        .iter()
                        .any(|(name, _, _)| *name == "algorithm-append-column"),
                    "actual growth boundary"
                );
                assert_eq!(
                    out.lease.as_ref().unwrap().bytes,
                    table_capacity(&out.table).unwrap()
                );
            }
            if case == 4 {
                assert_eq!((out.table.len, out.table.width()), (4, 4));
            }
            drop(out);
            drop(held);
            drop(ctx);
            assert_eq!(grant.used(), 0);
        }
    }
    #[test]
    fn cached_projection_and_incoming_owners_never_retain_initializing_query() {
        use crate::sparql::pool::QueryPool;
        let (_store, mut ctx, mut spec) = weighted();
        spec.proj.direction = Some(Dir::Forward);
        let snapshot = ctx.snap.clone();
        let pool = QueryPool::new(64 << 20);
        let grant = Arc::new(pool.empty_grant());
        ctx.set_grant(Some(grant.clone()));
        let ctl = Control::for_query(&ctx, u64::MAX).unwrap();
        let (g, hit) = projection(&ctx, &spec, None, &Par::single(), &ctl).unwrap();
        assert!(!hit);
        // Genuine refused initialization does not publish partial incoming storage.
        let short = Control::for_query(&ctx, 0).unwrap();
        assert!(matches!(g.inn(&short), Err(Error::BudgetExceeded(_))));
        drop(short);
        let prior = ctx.memory().live.load(Ordering::Acquire);
        let scope = memory::probe::install(|_| {}, Some("incoming-edges"));
        assert!(matches!(g.inn(&ctl), Err(Error::Invalid(_))));
        drop(scope);
        assert_eq!(ctx.memory().live.load(Ordering::Acquire), prior);
        let incoming = g.inn(&ctl).unwrap();
        assert_eq!(incoming.n(), g.n());
        drop(incoming);
        let weak = Arc::downgrade(ctx.memory());
        drop(ctl);
        drop(ctx);
        assert!(weak.upgrade().is_none());
        assert_eq!(grant.used(), 0);
        for _ in 0..2 {
            let mut next = Ctx::new(snapshot.clone());
            let next_grant = Arc::new(pool.empty_grant());
            next.set_grant(Some(next_grant.clone()));
            let ctl = Control::for_query(&next, u64::MAX).unwrap();
            let (warm, hit) = projection(&next, &spec, None, &Par::single(), &ctl).unwrap();
            assert!(hit);
            assert!(Arc::ptr_eq(&warm, &g));
            let base = next.memory().live.load(Ordering::Acquire);
            let incoming = warm.inn(&ctl).unwrap();
            assert!(next.memory().live.load(Ordering::Acquire) > base);
            assert!(next_grant.used() >= next.memory().live.load(Ordering::Acquire));
            drop(incoming);
            assert_eq!(next.memory().live.load(Ordering::Acquire), base);
            let weak = Arc::downgrade(next.memory());
            drop(warm);
            drop(ctl);
            drop(next);
            assert!(weak.upgrade().is_none());
            assert_eq!(next_grant.used(), 0);
        }
    }
    #[test]
    fn service_allocation_failure_and_unwind_release_only_their_query_contribution() {
        use crate::sparql::pool::QueryPool;
        for unwind in [false, true] {
            let (_store, mut ctx, mut spec) = weighted();
            spec.algo = Algo::Wcc;
            spec.outs = Outs {
                node: Some(0),
                component: Some(1),
                ..Default::default()
            };
            let pool = QueryPool::new(64 << 20);
            let grant = Arc::new(pool.empty_grant());
            ctx.set_grant(Some(grant.clone()));
            let mut other = Ctx::new(ctx.snap.clone());
            other.set_grant(Some(grant.clone()));
            let other_held = other.charge(135).unwrap();
            let scope = memory::probe::install(
                move |point| {
                    if unwind
                        && let memory::probe::Point::Allocation("algorithm-column", n) = point
                        && n > 0
                    {
                        panic!("service capacity unwind");
                    }
                },
                (!unwind).then_some("algorithm-column"),
            );
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                run(&ctx, &spec, None, None, &[0, 1])
            }));
            drop(scope);
            if unwind {
                assert!(result.is_err());
            } else {
                assert!(matches!(result.unwrap(), Err(Error::Invalid(_))));
            }
            assert_eq!(ctx.memory().live.load(Ordering::Acquire), 0);
            drop(ctx);
            assert_eq!(grant.used(), 135);
            assert_eq!(other.memory().live.load(Ordering::Acquire), 135);
            drop(other_held);
            drop(other);
            assert_eq!(grant.used(), 0);
        }
    }
    #[test]
    fn owned_execution_forwards_backing_and_warm_cache_without_losing_the_query_lease() {
        use crate::sparql::pool::QueryPool;
        let (store, _ctx, _spec) = weighted();
        let service = "SERVICE <urn:x-sparkles:algo#wcc> { [] <urn:x-sparkles:algo#predicate> <http://ex/p>; <urn:x-sparkles:algo#node> ?n; <urn:x-sparkles:algo#component> ?c }";
        let queries = [
            format!("SELECT ?n WHERE {{ {service} }}"),
            format!("SELECT ?n ?missing WHERE {{ {service} FILTER(?c>=0) }}"),
            format!("SELECT ?n (?c+1 AS ?next) WHERE {{ {service} }}"),
            format!("SELECT DISTINCT ?c WHERE {{ {service} }}"),
            format!("SELECT ?n ?c WHERE {{ {service} }} ORDER BY DESC(?n)"),
            format!("SELECT ?n ?c WHERE {{ {service} FILTER(?c>=0) }} LIMIT 1"),
            format!("SELECT ?n ?c WHERE {{ {{ {service} }} UNION {{ {service} }} }} LIMIT 3"),
        ];
        let pool = QueryPool::new(64 << 20);
        let grant = Arc::new(pool.empty_grant());
        for q in queries {
            let parsed = super::super::parse_query(&q, None, &[]).unwrap();
            let prepared = super::super::prepare_query(
                store.snapshot(),
                &parsed,
                &super::super::QueryOptions {
                    grant: Some(grant.clone()),
                    ..Default::default()
                },
                None,
                None,
            )
            .unwrap();
            let (out, _) =
                super::super::exec::execute_owned(&prepared.ctx, &prepared.node).unwrap();
            assert!(
                out.lease.as_ref().is_some_and(|l| l.service.is_none()),
                "{q}"
            );
            assert_eq!(
                out.lease.as_ref().unwrap().bytes,
                table_capacity(&out.table).unwrap(),
                "{q}"
            );
            assert!(
                prepared.ctx.memory().live.load(Ordering::Acquire)
                    >= table_capacity(&out.table).unwrap()
            );
            assert!(grant.used() >= prepared.ctx.memory().live.load(Ordering::Acquire));
            let weak = Arc::downgrade(prepared.ctx.memory());
            drop(out);
            drop(prepared);
            assert!(weak.upgrade().is_none());
            assert_eq!(grant.used(), 0);
        }
        // Actual result-cache hit through the private ownership-aware eager entry.
        let s = crate::store::Store::in_memory(crate::store::StoreOptions {
            result_cache_min_ms: 0.0,
            ..Default::default()
        });
        s.load(&[crate::io::Source::from_bytes(
            b"<urn:a> <http://ex/p> <urn:b>.".to_vec(),
            crate::io::RdfFormat::Turtle,
            None,
        )])
        .unwrap();
        let q = format!("SELECT ?n ?c WHERE {{ {service} }}");
        let parsed = super::super::parse_query(&q, None, &[]).unwrap();
        let options = super::super::QueryOptions {
            grant: Some(grant.clone()),
            ..Default::default()
        };
        let first = super::super::query(s.snapshot(), &q, &options).unwrap();
        let expected = first.table.clone();
        drop(first);
        assert_eq!(grant.used(), 0);
        let prepared =
            super::super::prepare_query(s.snapshot(), &parsed, &options, None, None).unwrap();
        let (out, info) = super::super::exec::execute_owned(&prepared.ctx, &prepared.node).unwrap();
        assert!(info.cached);
        assert_eq!(out.table.cols, expected.cols);
        assert_eq!(
            out.lease.as_ref().unwrap().bytes,
            table_capacity(&out.table).unwrap()
        );
        let weak = Arc::downgrade(prepared.ctx.memory());
        drop(prepared);
        assert!(weak.upgrade().is_some());
        assert!(grant.used() > 0);
        drop(out);
        assert!(weak.upgrade().is_none());
        assert_eq!(grant.used(), 0);
    }
    #[test]
    fn final_input_join_is_query_owned_after_the_service_peak_is_closed() {
        use crate::sparql::pool::QueryPool;
        let (_store, mut ctx, mut spec) = weighted();
        spec.algo = Algo::Wcc;
        spec.outs = Outs {
            node: Some(0),
            component: Some(1),
            ..Default::default()
        };
        spec.nums.insert("maxMemory".into(), Num::Int(2048));
        let pool = QueryPool::new(64 << 20);
        let grant = Arc::new(pool.empty_grant());
        ctx.set_grant(Some(grant.clone()));
        let input = Table {
            vars: vec![4],
            cols: vec![vec![Id(77); 1000]],
            len: 1000,
            sorted: Vec::new(),
        };
        let held = ctx.charge(input.mem_bytes()).unwrap();
        let (out, counters) = run(&ctx, &spec, Some(input), None, &[0, 1]).unwrap();
        assert_eq!(out.table.len, 2000);
        assert!(table_capacity(&out.table).unwrap() > 2048);
        assert!(counters["peakBytes"].as_u64().unwrap() <= 2048);
        assert!(out.lease.as_ref().unwrap().service.is_none());
        drop(out);
        drop(held);
        drop(ctx);
        assert_eq!(grant.used(), 0);
    }
    #[test]
    fn repeated_graphs_share_one_service_limit_and_reclaim_dead_native_frames() {
        use crate::io::{RdfFormat, Source};
        let store = crate::store::Store::in_memory(Default::default());
        let mut data = String::from("@prefix ex:<http://ex/>.");
        for g in 0..8 {
            data.push_str(&format!("ex:g{g} {{ex:a ex:p ex:b.}}"));
        }
        store
            .load(&[Source::from_bytes(data.into_bytes(), RdfFormat::TriG, None)])
            .unwrap();
        let (_other, _old, mut spec) = weighted();
        let ctx = Ctx::new(store.snapshot());
        spec.proj.predicates = Some(vec![(ctx.snap.lookup_iri("http://ex/p").unwrap().0, None)]);
        spec.proj.weight = None;
        spec.proj.node_classes.clear();
        spec.proj.node_class_names.clear();
        spec.algo = Algo::Wcc;
        spec.outs = Outs {
            node: Some(0),
            component: Some(1),
            ..Default::default()
        };
        spec.graph_var = Some(3);
        let mut one = spec.clone();
        one.proj.graph = GraphFilter::One(ctx.snap.lookup_iri("http://ex/g0").unwrap().0);
        let mut ctx_one = Ctx::new(ctx.snap.clone());
        ctx_one.use_cache = false;
        let (out, one_stats) = run(&ctx_one, &one, None, None, &[0, 1, 3]).unwrap();
        assert_eq!(out.table.len, 2);
        drop(out);
        let mut ctx_all = Ctx::new(ctx.snap.clone());
        ctx_all.use_cache = false;
        let (out, all_stats) = run(&ctx_all, &spec, None, None, &[0, 1, 3]).unwrap();
        assert_eq!(out.table.len, 16);
        drop(out);
        let first = one_stats["peakBytes"].as_u64().unwrap();
        let all = all_stats["peakBytes"].as_u64().unwrap();
        assert!(
            all > first,
            "aggregate GRAPH/output overlap must exceed one isolated graph: {first}/{all}"
        );
        let mut limited = spec.clone();
        limited.nums.insert("maxMemory".into(), Num::Int(first));
        assert!(
            matches!(run(&ctx_all,&limited,None,None,&[0,1,3]),Err(Error::BudgetExceeded(b)) if b.kind==crate::error::BudgetKind::Memory && b.limit==first)
        );
        // No reached source means empty rows. Native graph capacity is still real,
        // but each dead frame must end before the next equal graph starts.
        let mut empty = spec.clone();
        empty.algo = Algo::Bfs;
        empty.outs = Outs {
            node: Some(0),
            depth: Some(1),
            ..Default::default()
        };
        empty.sources = vec![PathEnd::Const(Id::UNDEF)];
        let (out, stats) = run(&ctx_all, &empty, None, None, &[0, 1, 3]).unwrap();
        assert_eq!(out.table.len, 0);
        drop(out);
        let mut ids = vec![
            ctx.snap.lookup_iri("http://ex/g0").unwrap().0,
            ctx.snap.lookup_iri("http://ex/g1").unwrap().0,
        ];
        ids.sort_unstable();
        let mut two = empty.clone();
        two.proj.graph = GraphFilter::Set(ids);
        let (out, two_stats) = run(&ctx_all, &two, None, None, &[0, 1, 3]).unwrap();
        assert_eq!(out.table.len, 0);
        drop(out);
        assert_eq!(
            stats["peakBytes"], two_stats["peakBytes"],
            "dead native reservations must not accumulate over eight graphs"
        );
    }
    fn serialization_admission(embedding_case: bool) {
        let (_store, ctx, spec) = weighted();
        let graph_ctl = Control::unlimited();
        let g = build_graph(&ctx, &spec, None, &Par::single(), &graph_ctl).unwrap();
        let ledger = ctx.memory().clone();
        let observed = Arc::new(std::sync::Mutex::new(Vec::new()));
        let witness = observed.clone();
        algo::set_projection_observer(Some(Arc::new(move |event| {
            if let algo::ProjectionEvent::Materialized(name, bytes) = event
                && name
                    == if embedding_case {
                        "algorithm-embedding-text"
                    } else {
                        "algorithm-summary-text"
                    }
            {
                witness
                    .lock()
                    .unwrap()
                    .push((bytes, ledger.live.load(Ordering::Acquire)));
            }
        })));
        let ctl = Control::for_query(&ctx, u64::MAX).unwrap();
        if embedding_case {
            embedding(&ctx, &[1.25; 2048], &ctl).unwrap();
        } else {
            let mut stats = algo::Stats::new();
            stats.insert("test".into(), "x".repeat(16384).into());
            summary(&ctx, &g, stats, &ctl).unwrap();
        }
        algo::set_projection_observer(None);
        let got = observed.lock().unwrap();
        assert_eq!(got.len(), 1);
        assert!(got[0].0 > 1024);
        assert!(
            got[0].1 >= got[0].0 as u64,
            "actual serialized backing is unadmitted: {got:?}"
        );
    }
    #[test]
    fn summary_serialization_is_admitted_before_materialization() {
        serialization_admission(false);
    }
    #[test]
    fn embedding_serialization_is_admitted_before_materialization() {
        serialization_admission(true);
    }
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
        // a set of graphs reads by predicate and rejects every row; a single empty
        // graph would read its own quads, which are none
        spec.proj.graph = GraphFilter::Set(vec![0]);
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

/// A call that carries the graph variable of `GRAPH ?g` itself. Query text never plans
/// one, because the planner gives each graph its own call, so these tests plan the
/// pattern under the variable directly.
#[cfg(test)]
mod graph_variable_tests {
    use super::*;
    use std::collections::BTreeSet;

    fn store(data: &str) -> crate::store::Store {
        use crate::io::{RdfFormat, Source};
        let store = crate::store::Store::in_memory(crate::store::StoreOptions {
            result_cache_bytes: 0,
            ..Default::default()
        });
        store
            .load(&[Source::from_bytes(
                format!("@prefix ex:<http://ex/>.\n{data}").into_bytes(),
                RdfFormat::TriG,
                None,
            )])
            .unwrap();
        store
    }

    /// The rows of `query` planned under `GRAPH ?g`, as local names and integers.
    fn rows_under_graph_var(store: &crate::store::Store, query: &str) -> BTreeSet<Vec<String>> {
        let text = format!("PREFIX ex:<http://ex/> PREFIX algo:<urn:x-sparkles:algo#> {query}");
        let parsed = super::super::parse_query(&text, None, &[]).unwrap();
        let spargebra::Query::Select { pattern, .. } = &parsed else {
            panic!("SELECT");
        };
        let snapshot = store.snapshot();
        let ctx = Ctx::new(snapshot.clone());
        let mut planner = Planner::new(&ctx);
        planner.source = Some(&parsed);
        let node = planner
            .plan(pattern, &ActiveGraph::Var(ctx.var("g")), Vec::new())
            .unwrap();
        let (out, _) = super::super::exec::execute_owned(&ctx, &node).unwrap();
        let name = |id: Id| -> String {
            if let Some(i) = (0..10).find(|&i| Id::from_i64(i) == Some(id)) {
                return i.to_string();
            }
            for local in ["a", "b", "c", "d", "g1", "g2"] {
                if snapshot.lookup_iri(&format!("http://ex/{local}")) == Some(id) {
                    return local.to_string();
                }
            }
            format!("{id:?}")
        };
        let wanted: Vec<usize> = ["g", "n", "d"]
            .iter()
            .filter_map(|v| out.table.col_of(ctx.var(v)))
            .collect();
        (0..out.table.len)
            .map(|i| wanted.iter().map(|&c| name(out.table.cols[c][i])).collect())
            .collect()
    }

    fn set(rows: &[&[&str]]) -> BTreeSet<Vec<String>> {
        rows.iter()
            .map(|r| r.iter().map(|s| s.to_string()).collect())
            .collect()
    }

    #[test]
    fn a_nested_pattern_gives_each_graph_its_own_edges() {
        let s = store("ex:g1 { ex:a ex:p ex:b } ex:g2 { ex:c ex:p ex:d }");
        let got = rows_under_graph_var(
            &s,
            "SELECT ?g ?n WHERE { SERVICE algo:wcc { [] algo:start ?s ; algo:end ?e ; algo:node ?n ; algo:component ?c . ?s ex:p ?e } }",
        );
        assert_eq!(
            got,
            set(&[&["g1", "a"], &["g1", "b"], &["g2", "c"], &["g2", "d"]])
        );
    }

    #[test]
    fn a_variable_source_uses_the_rows_of_its_own_graph() {
        let s = store(
            "ex:g1 { ex:a ex:p ex:b . ex:b ex:p ex:c . ex:a a ex:Seed } ex:g2 { ex:a ex:p ex:b . ex:b ex:p ex:c . ex:c a ex:Seed }",
        );
        let got = rows_under_graph_var(
            &s,
            "SELECT ?g ?n ?d WHERE { ?s a ex:Seed . SERVICE algo:bfs { [] algo:predicate ex:p ; algo:source ?s ; algo:node ?n ; algo:depth ?d } }",
        );
        assert_eq!(
            got,
            set(&[
                &["g1", "a", "0"],
                &["g1", "b", "1"],
                &["g1", "c", "2"],
                &["g2", "c", "0"],
            ])
        );
    }

    /// Under `GRAPH ?g` the call of each graph reads that graph's quads, so the edges
    /// read grow with the data and not with the data times the graphs.
    #[test]
    fn the_calls_of_graph_g_read_each_graph_once() {
        use std::cell::Cell;
        use std::rc::Rc;
        let graphs = 40;
        let mut data = String::new();
        for g in 0..graphs {
            data.push_str(&format!(
                "ex:g{g:03} {{ ex:a ex:p ex:b{g} . ex:b{g} ex:q ex:c }}"
            ));
        }
        let s = store(&data);
        let read = Rc::new(Cell::new(0usize));
        let r = read.clone();
        let hook = memory::probe::install(
            move |point| {
                if let memory::probe::Point::Work("projection-edge") = point {
                    r.set(r.get() + 1);
                }
            },
            None,
        );
        let q = "PREFIX ex:<http://ex/> PREFIX algo:<urn:x-sparkles:algo#> SELECT ?g ?n ?c WHERE { GRAPH ?g { SERVICE algo:wcc { [] algo:predicate ex:p ; algo:concurrency 1 ; algo:node ?n ; algo:component ?c } } }";
        let o = super::super::QueryOptions {
            no_cache: true,
            ..Default::default()
        };
        let result = super::super::query(s.snapshot(), q, &o).unwrap();
        drop(hook);
        assert_eq!(result.len(), 2 * graphs);
        // two quads in each graph, read once
        assert!(
            read.get() <= 2 * graphs,
            "{} edges read for {graphs} graphs",
            read.get()
        );
        // a predicate with fewer quads than the graph still reads by predicate
        let read = Rc::new(Cell::new(0usize));
        let r = read.clone();
        let hook = memory::probe::install(
            move |point| {
                if let memory::probe::Point::Work("projection-edge") = point {
                    r.set(r.get() + 1);
                }
            },
            None,
        );
        let q = "PREFIX ex:<http://ex/> PREFIX algo:<urn:x-sparkles:algo#> SELECT ?n WHERE { GRAPH ex:g000 { SERVICE algo:wcc { [] algo:predicate ex:missing ; algo:concurrency 1 ; algo:node ?n ; algo:component ?c } } }";
        let result = super::super::query(s.snapshot(), q, &o).unwrap();
        drop(hook);
        assert_eq!(result.len(), 0);
        assert_eq!(read.get(), 0);
    }
}
