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

use super::ctx::{Ctx, TermKind};
use super::plan::{ActiveGraph, GraphFilter, Kind, Node, PT, PathEnd, Planner};
use super::table::{Table, VarId};
use super::value::Value;
use crate::algo::{self, Aggregate, Control, Graph, Par, RawEdges};
use crate::error::{Error, Result};
use crate::id::{Id, Tag};
use crate::index::{Perm, pad};
use crate::store::Chunk;
use oxrdf::Term;
use rustc_hash::FxHashMap;
use spargebra::algebra::GraphPattern;
use spargebra::term::{NamedNodePattern, TermPattern};
use std::collections::BTreeMap;
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
        self.proj.direction.unwrap_or(self.algo.default_direction())
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
    let ctl = Control::new(
        ctx.cancel.clone(),
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
        let mut part =
            if want_sources && sources.is_empty() && matches!(spec.algo, Algo::Bfs | Algo::Sssp) {
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
    let stats_only = o.summary.is_some() && o.vars().len() == 1;
    let limit = if stats_only {
        Some(1)
    } else {
        spec.num("limit").map(|l| l as usize)
    };
    let mut b = Builder::new(vars);
    let c_node = b.col(o.node);
    let c_summary = b.col(o.summary);
    let n = g.n();
    let node_id = |i: usize| Id(g.ids[i]);
    // columns of the per-node algorithms: the reported nodes and their values
    macro_rules! per_node {
        ($key:expr, $desc:expr, $stats:expr, $cols:expr) => {{
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

/// Raw edges being collected from the store, with the memory they hold accounted.
struct Collect<'a> {
    ctl: &'a Control,
    src: Vec<u64>,
    dst: Vec<u64>,
    w: Option<Vec<f32>>,
    charged: usize,
    symmetric: bool,
}

impl Collect<'_> {
    #[inline]
    fn push(&mut self, s: u64, o: u64, w: f32, d: Dir) -> Result<()> {
        let (a, b) = if d == Dir::Backward { (o, s) } else { (s, o) };
        self.add(a, b, w);
        if d == Dir::Undirected && a != b {
            self.add(b, a, w);
        }
        if self.src.len() >= self.charged {
            // charge ahead in doubling steps, so a small projection holds little
            let step = self.charged.max(4096);
            self.ctl.check()?;
            self.ctl
                .charge(step as u64 * if self.w.is_some() { 20 } else { 16 })?;
            self.charged += step;
        }
        Ok(())
    }

    #[inline]
    fn add(&mut self, a: u64, b: u64, w: f32) {
        self.src.push(a);
        self.dst.push(b);
        if let Some(ws) = &mut self.w {
            ws.push(w);
        }
    }
}

/// The weights of the edges, from the weight property of the reifiers of their triples.
fn weights(
    ctx: &Ctx,
    spec: &ProjectionSpec,
    wprop: u64,
) -> Result<FxHashMap<(u64, u64, u64), f32>> {
    let mut map: FxHashMap<(u64, u64, u64), f32> = FxHashMap::default();
    let Some(reifies) = spec.reifies else {
        return Ok(map);
    };
    let snap = &ctx.snap;
    for k in snap.scan_keys(Perm::Pso, &[wprop])? {
        if !spec.graph.accepts(k[3]) {
            continue;
        }
        let w = ctx
            .value(Id(k[2]))
            .as_ref()
            .and_then(super::value::approx_f64);
        let w = match w {
            Some(w) if w.is_finite() => w as f32,
            _ => {
                let t = ctx
                    .term(Id(k[2]))
                    .map_or_else(|| "?".to_string(), |t| t.to_string());
                return Err(bad(format!("the weight {t} is not a number")));
            }
        };
        ctx.check()?;
        for r in snap.scan_keys(Perm::Pso, &[reifies, k[1]])? {
            if !spec.graph.accepts(r[3]) {
                continue;
            }
            let Some(Term::Triple(t)) = ctx.term(Id(r[2])) else {
                continue;
            };
            let s = match &t.subject {
                oxrdf::NamedOrBlankNode::NamedNode(n) => Term::NamedNode(n.clone()),
                oxrdf::NamedOrBlankNode::BlankNode(b) => Term::BlankNode(b.clone()),
            };
            let ids = (
                snap.lookup_term(&s),
                snap.lookup_term(&Term::NamedNode(t.predicate.clone())),
                snap.lookup_term(&t.object),
            );
            if let (Some(s), Some(p), Some(o)) = ids {
                map.entry((s.0, p.0, o.0))
                    .and_modify(|x| *x = x.min(w))
                    .or_insert(w);
            }
        }
    }
    Ok(map)
}

/// The projection of a call: a cached build of the same data and edges, or a new one.
/// Projections from a nested pattern depend on the query and are not cached.
fn projection(
    ctx: &Ctx,
    spec: &AlgoSpec,
    edges: Option<&Table>,
    par: &Par,
    ctl: &Control,
) -> Result<(std::sync::Arc<Graph>, bool)> {
    let key = (spec.proj.pattern.is_none() && ctx.use_cache).then(|| {
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
        ctl.charge(g.bytes())?;
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
    let snap = &ctx.snap;
    let call_dir = spec.direction();
    let has_weights = proj.weight.is_some() || proj.pattern.is_some_and(|p| p.2.is_some());
    // fail before reading anything when the projection cannot fit
    let m_est = match (&proj.predicates, edges) {
        (_, Some(t)) => t.len() as u64,
        (Some(ps), None) => ps
            .iter()
            .map(|(p, _)| snap.estimate(Perm::Pso, &[*p]))
            .sum(),
        (None, None) => snap.len(),
    };
    let both = matches!(call_dir, Dir::Undirected)
        || proj
            .predicates
            .as_ref()
            .is_some_and(|ps| ps.iter().any(|(_, d)| *d == Some(Dir::Undirected)));
    let m_est = m_est * if both { 2 } else { 1 };
    let stored_weights = has_weights || matches!(proj.aggregate, Aggregate::Sum | Aggregate::Count);
    ctl.require(Graph::estimate(0, m_est, stored_weights))?;
    let mut col = Collect {
        ctl,
        src: Vec::new(),
        dst: Vec::new(),
        w: has_weights.then(Vec::new),
        charged: 0,
        symmetric: call_dir == Dir::Undirected
            && proj.predicates.as_ref().is_none_or(|ps| {
                ps.iter()
                    .all(|(_, d)| d.is_none_or(|d| d == Dir::Undirected))
            }),
    };
    let default_w = proj.default_weight as f32;
    if let Some(t) = edges {
        let (s, e, w) = proj.pattern.expect("a nested pattern");
        let (cs, ce) = (t.col_of(s), t.col_of(e));
        let cw = w.and_then(|w| t.col_of(w));
        let (Some(cs), Some(ce)) = (cs, ce) else {
            return Err(bad(
                "the nested pattern does not bind algo:start and algo:end",
            ));
        };
        let mut seen: rustc_hash::FxHashSet<(u64, u64)> = rustc_hash::FxHashSet::default();
        for i in 0..t.len() {
            if i % 65536 == 0 {
                ctl.check()?;
            }
            let (a, b) = (t.cols[cs][i], t.cols[ce][i]);
            if a.is_undef() || b.is_undef() {
                continue;
            }
            let is_node = |x: Id| matches!(ctx.kind(x), TermKind::Iri | TermKind::BNode);
            if !is_node(a) || !is_node(b) {
                continue;
            }
            // distinct pairs become the projection
            if !seen.insert((a.0, b.0)) {
                continue;
            }
            let w = match cw {
                Some(c) => {
                    let id = t.cols[c][i];
                    match ctx.value(id).as_ref().and_then(super::value::approx_f64) {
                        Some(x) if x.is_finite() => x as f32,
                        _ if id.is_undef() => default_w,
                        _ => {
                            return Err(bad(
                                "an edge weight of the nested pattern is not a number",
                            ));
                        }
                    }
                }
                None => default_w,
            };
            col.push(a.0, b.0, w, call_dir)?;
        }
    } else {
        let wmap = match proj.weight {
            Some(Some(wp)) => Some(weights(ctx, proj, wp)?),
            _ => None,
        };
        let mask: crate::index::ColMask = 0b1111;
        let node_kind = |o: u64| -> bool {
            match Id(o).tag() {
                Tag::BNode => true,
                Tag::Vocab | Tag::Delta | Tag::Local => {
                    matches!(ctx.kind(Id(o)), TermKind::Iri | TermKind::BNode)
                }
                _ => false,
            }
        };
        // scan the rows of the Pso range [lo, hi] of predicates
        let mut scan = |lo: u64, hi: u64, dir_of: &dyn Fn(u64) -> Dir| -> Result<()> {
            let mut last: Option<(u64, u64, u64)> = None;
            let mut last_o: (u64, bool) = (u64::MAX, false);
            let mut err = None;
            snap.scan_between_cols(Perm::Pso, pad(&[lo], 0), pad(&[hi], u64::MAX), mask, |c| {
                let mut each = |p: u64, s: u64, o: u64, g: u64| -> bool {
                    if !proj.graph.accepts(g) {
                        return true;
                    }
                    if last == Some((p, s, o)) {
                        return true;
                    }
                    last = Some((p, s, o));
                    if last_o.0 != o {
                        last_o = (o, node_kind(o));
                    }
                    if !last_o.1 {
                        return true;
                    }
                    let w = match &wmap {
                        Some(m) => m.get(&(s, p, o)).copied().unwrap_or(default_w),
                        None if has_weights => default_w,
                        None => 0.0,
                    };
                    match col.push(s, o, w, dir_of(p)) {
                        Ok(()) => true,
                        Err(e) => {
                            err = Some(e);
                            false
                        }
                    }
                };
                match c {
                    Chunk::Block(b, s, e) => {
                        for i in s..e {
                            if !each(b.cols[0][i], b.cols[1][i], b.cols[2][i], b.cols[3][i]) {
                                return Ok(false);
                            }
                        }
                    }
                    Chunk::Row(k) => {
                        if !each(k[0], k[1], k[2], k[3]) {
                            return Ok(false);
                        }
                    }
                }
                Ok(true)
            })?;
            match err {
                Some(e) => Err(e),
                None => Ok(()),
            }
        };
        match &proj.predicates {
            Some(ps) => {
                let dirs: FxHashMap<u64, Dir> = ps
                    .iter()
                    .map(|(p, d)| (*p, d.unwrap_or(call_dir)))
                    .collect();
                let mut ids: Vec<u64> = ps.iter().map(|(p, _)| *p).collect();
                ids.sort_unstable();
                ids.dedup();
                for p in ids {
                    scan(p, p, &|p| dirs[&p])?;
                }
            }
            None => {
                // every predicate but rdf:type, in at most two ranges
                match proj.rdf_type {
                    Some(t) => {
                        if t > 0 {
                            scan(0, t - 1, &|_| call_dir)?;
                        }
                        if t < u64::MAX {
                            scan(t + 1, u64::MAX, &|_| call_dir)?;
                        }
                    }
                    None => scan(0, u64::MAX, &|_| call_dir)?,
                }
            }
        }
    }
    // the nodes: the members of the classes, or every end of an edge
    let mut nodes: Vec<u64> = if !proj.node_class_names.is_empty() {
        let Some(ty) = proj.rdf_type else {
            return Err(bad(
                "the dataset has no rdf:type triples for algo:nodeClass",
            ));
        };
        let mut v = Vec::new();
        for &c in &proj.node_classes {
            for k in snap.scan_keys(Perm::Pos, &[ty, c])? {
                if proj.graph.accepts(k[3]) {
                    v.push(k[2]);
                }
            }
        }
        v
    } else {
        ctl.charge(col.src.len() as u64 * 16)?;
        let mut v = Vec::with_capacity(col.src.len() * 2);
        v.extend_from_slice(&col.src);
        v.extend_from_slice(&col.dst);
        v
    };
    par.sort(&mut nodes);
    nodes.dedup();
    let raw = RawEdges {
        nodes,
        src: col.src,
        dst: col.dst,
        w: col.w,
        symmetric: col.symmetric,
    };
    Graph::build(raw, proj.aggregate, par, ctl)
}
