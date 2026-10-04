//! Query planner: spargebra algebra → physical operator tree.
//!
//! Join groups (BGPs, joins, `GRAPH` blocks, filters) are flattened into items and
//! ordered by a QLever-style dynamic program over connected components that keeps the
//! cheapest plan per (subset, sort order) ("interesting orders"), switching to a greedy
//! strategy for large groups. Filters are placed as soon as all their variables are
//! bound. Other operators follow the algebra, with Jena-style rewrites (filter
//! placement through OPTIONAL / UNION / MINUS / BIND, filter-equality substitution,
//! top-k for ORDER BY + LIMIT).

use super::ctx::Ctx;
use super::expr::{Compiler, ExistsSpec, Expr};
use super::table::{Table, VarId};
use crate::error::{Error, Result};
use crate::id::{Id, Tag};
use crate::index::{G, O, P, Perm, S};
use oxrdf::vocab::xsd;
use oxrdf::{Literal, Term};
use rustc_hash::{FxHashMap, FxHashSet};
use spargebra::algebra::{
    AggregateExpression, AggregateFunction, Expression, GraphPattern, OrderExpression,
    PropertyPathExpression,
};
use spargebra::term::{GroundTerm, NamedNodePattern, TermPattern, TriplePattern};
use std::sync::Arc;

/// The graph that triple patterns are matched against.
#[derive(Clone, Debug)]
pub enum ActiveGraph {
    Default,
    /// union of all named graphs (`GRAPH <urn:x-arq:UnionGraph>`)
    Union,
    Named(Id),
    Var(VarId),
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum GraphFilter {
    /// no restriction (graph is an output column or the prefix)
    All,
    Default,
    /// any named graph (not the default graph)
    Named,
    One(u64),
    Set(Vec<u64>),
}

impl GraphFilter {
    #[inline]
    pub fn accepts(&self, g: u64) -> bool {
        match self {
            GraphFilter::All => true,
            GraphFilter::Default => g == Id::DEFAULT_GRAPH.0,
            GraphFilter::Named => g != Id::DEFAULT_GRAPH.0,
            GraphFilter::One(x) => g == *x,
            GraphFilter::Set(s) => s.binary_search(&g).is_ok(),
        }
    }
    /// Whether rows of several graphs can pass (so equal triples may repeat).
    pub fn multi(&self) -> bool {
        matches!(
            self,
            GraphFilter::All | GraphFilter::Named | GraphFilter::Set(_)
        )
    }
}

#[derive(Clone, Debug)]
pub struct ScanSpec {
    pub perm: Perm,
    pub prefix: Vec<u64>,
    /// key column → variable (first occurrence)
    pub cols: Vec<(usize, VarId)>,
    /// key columns that must be equal (repeated variables)
    pub eqs: Vec<(usize, usize)>,
    pub graph: GraphFilter,
    /// key column holding the graph
    pub graph_col: usize,
    /// drop adjacent duplicate rows (same triple in several graphs)
    pub dedup: bool,
}

/// A numeric range FILTER pushed into a scan whose first free key column holds `var`.
///
/// That column is sorted by id, and ids sort by tag, then payload. For inline integers
/// (non-negative, then negative payloads) and inline decimals (per scale, non-negative,
/// then negative mantissas), payload order is value order, so the values satisfying a
/// conjunction of comparisons with numeric constants form one contiguous id range per
/// such segment; it is found by binary search with the ordinary comparison. Ids whose
/// order says nothing about their value (doubles because of NaN, vocabulary and delta
/// literals) are read and tested with the filter. Every other inline kind (booleans,
/// dates, blank nodes) can never compare true with a number and is skipped.
#[derive(Clone)]
pub struct RangeSpec {
    pub var: VarId,
    /// sorted, disjoint id intervals `[lo, hi]` to read; `exact` means every id in it
    /// satisfies the filter, otherwise its rows are tested
    pub ranges: Vec<IdRange>,
    /// the pushed filter (a conjunction), evaluated on rows of inexact ranges
    pub filter: Vec<Expr>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct IdRange {
    pub lo: u64,
    pub hi: u64,
    pub exact: bool,
}

/// `ORDER BY ?v LIMIT k` over a single scan whose first free key column holds `?v`: the
/// scan is read in the order of `?v`'s values, best first, until the first `k` rows are
/// proven.
///
/// The column's ids are split into pieces. In a strictly monotone numeric piece (inline
/// integers, inline decimals of one scale, inline doubles of one sign) id order is value
/// order, so the piece is read from its best end in geometrically growing steps, and only
/// its best `k` rows (with their ties) are kept. Every other piece (vocabulary and delta
/// literals, IRIs, blank nodes, NaN, dates…) is read whole. The candidates are ranked by
/// the generic ORDER BY in the generic plan's row order, so ties break identically; a
/// piece stops once the `k`-th candidate is strictly better than any of its unread
/// values.
#[derive(Clone)]
pub struct OrderedTopK {
    /// the scan, re-targeted so that `var` is its first free column
    pub scan: ScanSpec,
    pub var: VarId,
    pub asc: bool,
    /// OFFSET + LIMIT
    pub k: usize,
    /// non-empty id intervals of the order column, in id order
    pub pieces: Vec<TopKPiece>,
    /// FILTER conjuncts over the scan, tested on every row
    pub filter: Vec<Expr>,
    /// a pushed numeric range filter, tested on the rows of inexact pieces
    pub range_filter: Vec<Expr>,
    /// the scan's variables in the key order of the generic plan's scan: the order its
    /// rows (and so ORDER BY's ties) come in
    pub tie_order: Vec<VarId>,
    /// the generic plan, run when the values are not totally ordered (NaN, dates)
    pub fallback: Box<Node>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TopKPiece {
    pub lo: u64,
    pub hi: u64,
    /// every id satisfies the pushed range filter (otherwise its rows are tested)
    pub exact: bool,
    /// `Some(true)`: larger ids are larger values, `Some(false)`: smaller values (inline
    /// negative doubles); `None`: ids say nothing about value order
    pub mono: Option<bool>,
}

/// The query of a vector search.
#[derive(Clone, Debug)]
pub enum VectorQuery {
    Vector(Arc<[f32]>),
    /// an entity whose (single) vector under the predicate is the query
    Entity(Id),
    /// bound by the rest of the group: a vector literal or an entity per input row
    Var(VarId),
    /// a text, embedded with the provider of the predicate's index when the search runs
    Text(Arc<str>),
}

/// A `spk:vectorSearch` call planned as a leaf (exact top-k similarity search).
#[derive(Clone, Debug)]
pub struct VectorSpec {
    /// the embedding predicate (`None`: not in the store, so no rows)
    pub pred: Option<Id>,
    pub query: VectorQuery,
    pub k: usize,
    pub metric: crate::vector::Metric,
    pub subject: PathEnd,
    pub score: Option<VarId>,
    pub vector: Option<VarId>,
    pub graph: GraphFilter,
    pub graph_var: Option<VarId>,
    /// merge rows with the same (s, vector) from different graphs (merged default graph)
    pub dedup: bool,
    /// `exact:true` and `ef:N`
    pub mode: crate::vector::SearchMode,
    /// `distinct:subject`: at most one row (the best) per subject
    pub distinct_subject: bool,
    /// `candidates:join`: only subjects the rest of the group binds
    pub candidates: bool,
    /// the `k` best rows of an ORDER BY over one pattern (see `vectortopk`): child 0 is
    /// the pattern, whose generic plan runs when fewer than `k` rows have a score
    pub order_fallback: bool,
}

impl VectorSpec {
    /// Whether the search reads the rest of its group (child 0): a variable query or
    /// `candidates:join`.
    pub fn needs_input(&self) -> bool {
        self.candidates || matches!(self.query, VectorQuery::Var(_))
    }
}

/// A `text:query` call planned as a leaf (full-text search), or as a node over the rest
/// of its group when its query string is a variable.
#[derive(Clone, Debug)]
pub struct TextSpec {
    pub query: String,
    /// a variable query string that the rest of the group binds (child 0): one search
    /// per distinct value
    pub query_var: Option<VarId>,
    /// predicate IRIs to search (empty: all indexed)
    pub predicates: Vec<String>,
    pub lang: Option<String>,
    pub limit: Option<usize>,
    /// the subject slot: a variable, or a constant restricting the search
    pub subject: PathEnd,
    pub score: Option<VarId>,
    pub literal: Option<VarId>,
    /// the call's graph slot
    pub graph_out: Option<VarId>,
    pub prop: Option<VarId>,
    /// the hit's rank: 1 + the number of hits with a higher score
    pub rank: Option<VarId>,
    /// graph scope of the active graph
    pub graph: GraphFilter,
    /// `GRAPH ?g { … }` around the call: bound from each hit's graph
    pub graph_var: Option<VarId>,
    /// merge hits of the same (s, p, o) from different graphs (merged default graph)
    pub dedup: bool,
    /// `highlight:` options: the literal output is the highlighted fragments
    pub highlight: Option<crate::text::HighlightOpts>,
}

impl TextSpec {
    /// Whether the search reads the rest of its group (child 0): a variable query string.
    pub fn needs_input(&self) -> bool {
        self.query_var.is_some()
    }

    /// Whether a join may restrict the search to the subjects of its other side: without
    /// a limit (the top n are taken before any join) and without a rank (counted over
    /// every hit), restricting the subjects drops only hits the join drops.
    pub fn subjects_pushable(&self) -> Option<VarId> {
        match self.subject {
            PathEnd::Var(v) if self.limit.is_none() && self.rank.is_none() => Some(v),
            _ => None,
        }
    }

    /// The variables the search itself binds, in the order of its output columns.
    pub fn output_vars(&self) -> Vec<VarId> {
        let subject = match self.subject {
            PathEnd::Var(v) => Some(v),
            _ => None,
        };
        let mut vars = Vec::new();
        for v in [
            subject,
            self.score,
            self.literal,
            self.graph_out,
            self.graph_var,
            self.prop,
            self.rank,
        ]
        .into_iter()
        .flatten()
        {
            if !vars.contains(&v) {
                vars.push(v);
            }
        }
        vars
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum JoinAlgo {
    Merge,
    Hash,
    Cross,
}

#[derive(Clone)]
pub struct Agg {
    pub func: AggregateFunction,
    pub expr: Option<Expr>,
    pub distinct: bool,
    /// ARQ's FOLD: `expr` is the list element or the map key
    pub fold: Option<Box<Fold>>,
}

/// The parts of ARQ's `FOLD(expr, value ORDER BY …)` besides its first expression.
#[derive(Clone)]
pub struct Fold {
    /// the map value; `None` folds into a `cdt:List`
    pub value: Option<Expr>,
    /// sort keys of the group's solutions (ascending when `true`)
    pub order: Vec<(Expr, bool)>,
}

/// The IRI that names FOLD in EXPLAIN and in the result cache's keys.
pub const FOLD: &str = "http://w3id.org/awslabs/neptune/SPARQL-CDTs/fold";

#[derive(Clone, Debug)]
pub enum PathEnd {
    Const(Id),
    Var(VarId),
}

#[derive(Clone)]
pub struct PathSpec {
    pub subj: PathEnd,
    pub obj: PathEnd,
    /// minimum path length (0 or 1)
    pub min: u8,
    /// maximum length 1 (`?` paths)
    pub max_one: bool,
    /// for index-based traversal: predicate + direction (true = reverse)
    pub simple: Option<(u64, bool)>,
    pub graph: GraphFilter,
    pub graph_var: Option<VarId>,
    /// edge variables of the child plan (src, dst) when not simple
    pub edge_vars: Option<(VarId, VarId)>,
    /// an ARQ path range `{min,max}` (`max` absent: unbounded), whose solutions are
    /// counted: one per walk, and per simple path after `min` steps when unbounded (see
    /// [`PropertyPathExpression::Range`]). `min` above then tells only whether the
    /// range can match a zero-length path.
    pub count: Option<(u64, Option<u64>)>,
}

// `Values` / `Scan` are much larger than the other variants; nodes are few, so this is fine.
#[allow(clippy::large_enum_variant)]
#[derive(Clone)]
pub enum Kind {
    Scan(ScanSpec),
    /// scan restricted to the id ranges of a numeric range filter on its sorted column
    RangeScan(ScanSpec, RangeSpec),
    /// ORDER BY + LIMIT over a scan, read in value order (see [`OrderedTopK`])
    OrderedTopK(Box<OrderedTopK>),
    Values(Table),
    Empty,
    Join {
        algo: JoinAlgo,
        keys: Vec<VarId>,
    },
    LeftJoin {
        expr: Option<Expr>,
    },
    Minus,
    /// ARQ's SEMIJOIN (`anti`: ANTIJOIN): the rows of child 0 compatible with a row of
    /// child 1 (with none)
    HalfJoin {
        anti: bool,
    },
    Union,
    Filter(Vec<Expr>),
    Extend(VarId, Expr),
    /// ARQ's `LET` of a variable that the child may bind: where it is bound, a solution
    /// whose value is not the same value as the expression's is dropped
    Assign(VarId, Expr),
    /// ARQ's `UNFOLD`: a solution per element of the `cdt:List` or entry of the
    /// `cdt:Map` that `expr` gives
    Unfold {
        expr: Expr,
        var: VarId,
        second: Option<VarId>,
    },
    Sort(Vec<VarId>),
    OrderBy {
        keys: Vec<(Expr, bool)>,
        limit: Option<usize>,
    },
    Project(Vec<VarId>),
    Distinct,
    Slice {
        offset: usize,
        limit: Option<usize>,
    },
    Group {
        keys: Vec<VarId>,
        aggs: Vec<(VarId, Agg)>,
    },
    /// `COUNT(*)` over a single scan answered from index metadata
    /// `COUNT(*)` over a join: counts matching pairs without materializing them
    CountJoin {
        algo: JoinAlgo,
        var: VarId,
    },
    /// `GROUP BY ?k` + `COUNT` over a single scan sorted on ?k: counts runs in the index
    /// blocks without materializing the scan (QLever `computeGroupByObjectWithCount`)
    GroupCountScan {
        spec: ScanSpec,
        key: VarId,
        counts: Vec<VarId>,
        /// the counts per key from the index statistics (exact for this snapshot)
        metadata: Option<Arc<super::stats::Counts>>,
    },
    /// `COUNT(*)` over a join of two scans on one variable: both children are scans
    /// sorted on it, read as (key, run length) pairs; the count is Σ left × right
    CountJoinRuns {
        var: VarId,
    },
    CountScan {
        spec: ScanSpec,
        var: VarId,
    },
    /// `COUNT(DISTINCT ?k)` over a single scan sorted on ?k: counts runs of equal ids
    CountDistinctScan {
        spec: ScanSpec,
        var: VarId,
        /// the count read from the index statistics (exact for this snapshot)
        metadata: Option<u64>,
    },
    /// `COUNT(*)` over a FILTER on one variable of a single scan: the scan is read as
    /// runs of a permutation sorted on the variable, the filter is tested once per run
    /// and the lengths of the runs that pass are summed
    CountFilterScan {
        spec: ScanSpec,
        key: VarId,
        filter: Vec<Expr>,
        var: VarId,
        /// `COUNT(DISTINCT ?key)`: the number of runs that pass
        distinct: bool,
    },
    /// decompose the triple term in `t` into `parts` (RDF 1.2)
    Unpack {
        t: VarId,
        parts: [PathEnd; 3],
    },
    /// transitive / optional path; child 0 = edges plan (unless simple);
    /// if `bound_from_left`, the last child is the input binding the start variable
    Path {
        spec: Box<PathSpec>,
        bound_from_left: bool,
    },
    Service {
        endpoint: PathEnd,
        query: String,
        silent: bool,
        /// the `cache` option of Jena's service enhancer
        cache: super::enhancer::CacheMode,
    },
    /// full-text search (`text:query`)
    TextSearch(Box<TextSpec>),
    /// exact vector similarity search (`spk:vectorSearch`)
    VectorSearch(Box<VectorSpec>),
    /// a text and a vector ranking fused (`spk:hybridSearch`)
    HybridSearch(Box<super::hybrid::HybridSpec>),
    /// scan of a spatially indexed predicate restricted by spatial filters on its object
    SpatialScan(Box<super::geopf::SpatialScanSpec>),
    /// a `spatial:` property function
    SpatialPf(Box<super::geopf::SpatialPfSpec>),
    /// triple patterns read only for the keys of the input (child 0); see
    /// [`super::indexjoin`]
    IndexJoin(Box<super::indexjoin::IndexJoinSpec>),
    /// two inputs joined on a spatial test of one geometry from each
    SpatialJoin(Box<super::geojoin::SpatialJoinSpec>),
    /// the template plan (child 0) over the nearest geometries first, until `k` rows are
    /// proven
    SpatialKnn(Box<super::geojoin::SpatialKnnSpec>),
    /// a topological property matched against asserted and derived triples (Query
    /// Rewrite), or `spatial:equals`
    SpatialRelate(Box<super::georewrite::SpatialRelateSpec>),
    /// `LATERAL`: the right side evaluated per group of the rows of the left side (child
    /// 0); see [`super::lateral`]
    Lateral(Box<super::lateral::LateralSpec>),
    /// a property function of ARQ's library (`list:member`, `apf:strSplit`, …); child 0,
    /// if any, is the rest of the group, which binds variables the call reads
    PropertyFn(Box<super::arqpf::PfSpec>),
    /// paths as solutions (`SERVICE path:search`); child 0, if any, is the rest of the
    /// group, which binds the source or the target
    PathSearch(Box<super::pathsearch::PathSearchSpec>),
    /// the recorded changes of the dataset (`SERVICE hist:changes`)
    HistoryChanges(Box<super::history_svc::HistorySpec>),
}

#[derive(Clone)]
pub struct Node {
    pub kind: Kind,
    pub children: Vec<Node>,
    pub vars: Vec<VarId>,
    pub certain: Vec<VarId>,
    pub sorted: Vec<VarId>,
    pub est: f64,
    pub cost: f64,
    pub dist: FxHashMap<VarId, f64>,
    pub desc: String,
}

impl Node {
    pub(super) fn leaf(kind: Kind, vars: Vec<VarId>, est: f64, desc: String) -> Node {
        let dist = vars.iter().map(|&v| (v, est.max(1.0))).collect();
        Node {
            kind,
            children: Vec::new(),
            certain: vars.clone(),
            vars,
            sorted: Vec::new(),
            est,
            cost: est,
            dist,
            desc,
        }
    }

    pub fn empty(vars: Vec<VarId>) -> Node {
        Node::leaf(Kind::Empty, vars, 0.0, "empty".into())
    }

    pub fn unit() -> Node {
        let mut n = Node::leaf(Kind::Values(Table::unit()), Vec::new(), 1.0, "unit".into());
        n.cost = 0.0;
        n
    }

    fn unary(kind: Kind, child: Node, desc: String) -> Node {
        Node {
            kind,
            vars: child.vars.clone(),
            certain: child.certain.clone(),
            sorted: child.sorted.clone(),
            est: child.est,
            cost: child.cost + child.est,
            dist: child.dist.clone(),
            desc,
            children: vec![child],
        }
    }

    pub fn is_empty(&self) -> bool {
        matches!(self.kind, Kind::Empty)
    }

    pub(super) fn d(&self, v: VarId) -> f64 {
        self.dist
            .get(&v)
            .copied()
            .unwrap_or(self.est)
            .clamp(1.0, self.est.max(1.0))
    }

    pub fn operator(&self) -> &'static str {
        match &self.kind {
            Kind::Scan(_) => "IndexScan",
            Kind::RangeScan(..) => "IndexRangeScan",
            Kind::OrderedTopK(_) => "IndexTopK",
            Kind::Values(_) => "Values",
            Kind::Empty => "Empty",
            Kind::Join {
                algo: JoinAlgo::Merge,
                ..
            } => "MergeJoin",
            Kind::Join {
                algo: JoinAlgo::Hash,
                ..
            } => "HashJoin",
            Kind::Join {
                algo: JoinAlgo::Cross,
                ..
            } => "CartesianProduct",
            Kind::LeftJoin { .. } => "OptionalJoin",
            Kind::Minus => "Minus",
            Kind::HalfJoin { anti: false } => "SemiJoin",
            Kind::HalfJoin { anti: true } => "AntiJoin",
            Kind::Union => "Union",
            Kind::Filter(_) => "Filter",
            Kind::Extend(..) => "Bind",
            Kind::Assign(..) => "Let",
            Kind::Unfold { .. } => "Unfold",
            Kind::Sort(_) => "Sort",
            Kind::OrderBy { limit: Some(_), .. } => "TopK",
            Kind::OrderBy { .. } => "OrderBy",
            Kind::Project(_) => "Project",
            Kind::Distinct => "Distinct",
            Kind::Slice { .. } => "Limit",
            Kind::Group { .. } => "GroupBy",
            Kind::CountScan { .. } => "CountFromIndex",
            Kind::CountDistinctScan {
                metadata: Some(_), ..
            } => "CountDistinctFromMetadata",
            Kind::CountDistinctScan { .. } => "CountDistinctFromIndex",
            Kind::CountFilterScan { .. } => "CountFilterFromRuns",
            Kind::GroupCountScan {
                metadata: Some(_), ..
            } => "GroupCountFromMetadata",
            Kind::GroupCountScan { .. } => "GroupCountFromIndex",
            Kind::CountJoinRuns { .. } => "CountJoinFromRuns",
            Kind::CountJoin { .. } => "CountJoin",
            Kind::Unpack { .. } => "TripleTerm",
            Kind::Path { .. } => "TransitivePath",
            Kind::Lateral(_) => "Lateral",
            Kind::PropertyFn(_) => "PropertyFunction",
            Kind::Service { .. } => "Service",
            Kind::TextSearch(_) => "TextSearch",
            Kind::VectorSearch(_) => "VectorSearch",
            Kind::HybridSearch(_) => "HybridSearch",
            Kind::SpatialScan(_) => "SpatialScan",
            Kind::SpatialPf(_) => "SpatialPf",
            Kind::IndexJoin(j) if j.probes.len() > 1 => "StarJoin",
            Kind::IndexJoin(_) => "IndexJoin",
            Kind::SpatialJoin(_) => "SpatialJoin",
            Kind::SpatialKnn(_) => "SpatialKnn",
            Kind::SpatialRelate(_) => "SpatialRelate",
            Kind::PathSearch(_) => "PathSearch",
            Kind::HistoryChanges(_) => "HistoryChanges",
        }
    }
}

/// A term in a triple pattern.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) enum PT {
    C(Id),
    V(VarId),
}

#[derive(Clone)]
struct Triple {
    t: [PT; 3],
    graph: ActiveGraph,
}

// a Node is much larger than a triple pattern, but items are short-lived and few
#[allow(clippy::large_enum_variant)]
enum Item {
    Unpack(UnpackItem),
    Triple(Triple),
    Path(PathItem),
    Node(Node),
}

#[derive(Clone)]
struct PathItem {
    s: PT,
    path: PropertyPathExpression,
    o: PT,
    graph: ActiveGraph,
}

pub(super) const FILTER_SELECTIVITY: f64 = 0.3;
const DP_LIMIT: usize = 12;

pub struct Planner<'a> {
    pub ctx: &'a Ctx,
    pub subst: FxHashMap<VarId, Id>,
    /// The variables of `subst` that a sub-select hides unless it projects them: a
    /// `LATERAL`'s substitutions, where a variable a sub-select does not project is a
    /// different variable. Initial bindings and EXISTS substitute everywhere.
    pub scoped: FxHashSet<VarId>,
    bnode_scope: u32,
    /// RDF 1.2 triple-term patterns created while translating triple patterns
    unpacks: std::cell::RefCell<Vec<UnpackItem>>,
    /// the whole query being planned, if known: a search output it mentions nowhere else
    /// is left out (see [`Planner::used_elsewhere`])
    pub source: Option<&'a spargebra::Query>,
    /// the source's SSE form, or `None` when every variable counts as used
    source_sse: std::cell::OnceCell<Option<String>>,
}

/// `<<( s p o )>>` with variables: the triple term bound to `t` is decomposed into
/// `parts` (constants must match, variables are bound).
#[derive(Clone)]
struct UnpackItem {
    t: VarId,
    parts: [PT; 3],
}

impl<'a> Planner<'a> {
    pub fn new(ctx: &'a Ctx) -> Planner<'a> {
        Planner {
            ctx,
            subst: FxHashMap::default(),
            scoped: FxHashSet::default(),
            bnode_scope: 0,
            unpacks: Default::default(),
            source: None,
            source_sse: Default::default(),
        }
    }

    /// Whether the variable or blank node `t`, which a property function call binds,
    /// occurs in the query anywhere but that call. Every occurrence in the algebra's SSE
    /// form counts, so the answer errs towards "used" (a string that happens to contain
    /// the name counts too). Without the source query, and with `COUNT(DISTINCT *)`,
    /// which depends on every variable, everything is used.
    fn used_elsewhere(&self, t: &TermPattern) -> bool {
        let sse = self.source_sse.get_or_init(|| {
            let sse = self.source?.to_sse();
            (!sse.contains("(count distinct)")).then_some(sse)
        });
        let Some(sse) = sse else {
            return true;
        };
        let name = match t {
            TermPattern::Variable(v) => format!("?{}", v.as_str()),
            TermPattern::BlankNode(b) => format!("_:{}", b.as_str()),
            _ => return true,
        };
        let mentions = sse
            .match_indices(&name)
            .filter(|(i, _)| {
                !sse[i + name.len()..]
                    .starts_with(|c: char| c.is_alphanumeric() || c == '_' || c == '\u{b7}')
            })
            .count();
        mentions != 1
    }

    pub fn compile(&self, e: &Expression, graph: &ActiveGraph) -> Expr {
        let ctx = self.ctx;
        let g = graph.clone();
        let subst = &self.subst;
        let exists = move |p: &GraphPattern| -> Arc<ExistsSpec> {
            let mut names = Vec::new();
            collect_pattern_vars(p, &mut names);
            let mut vars: Vec<VarId> = names.iter().map(|n| ctx.var(n)).collect();
            vars.sort_unstable();
            vars.dedup();
            let bound = vars
                .iter()
                .filter_map(|v| subst.get(v).map(|id| (*v, *id)))
                .collect();
            Arc::new(ExistsSpec::new(p.clone(), g.clone(), vars, bound))
        };
        Compiler {
            ctx,
            subst: &self.subst,
            exists: &exists,
        }
        .compile(e)
    }

    fn var(&self, name: &str) -> PT {
        let v = self.ctx.var(name);
        match self.subst.get(&v) {
            Some(id) => PT::C(*id),
            None => PT::V(v),
        }
    }

    pub(super) fn term_pattern(&self, t: &TermPattern) -> PT {
        match t {
            TermPattern::Variable(v) => self.var(v.as_str()),
            TermPattern::BlankNode(b) => {
                // blank nodes in patterns are non-distinguished variables
                PT::V(
                    self.ctx
                        .var(&format!(" bn{}_{}", self.bnode_scope, b.as_str())),
                )
            }
            TermPattern::NamedNode(n) => PT::C(self.ctx.intern_term(&Term::NamedNode(n.clone()))),
            TermPattern::Literal(l) => PT::C(self.ctx.intern_query_literal(l)),
            TermPattern::Triple(tp) => match ground_triple(tp) {
                Some(t) => PT::C(self.ctx.intern_term(&Term::Triple(Box::new(t)))),
                None => {
                    let t = self.ctx.fresh_var();
                    let parts = [
                        self.term_pattern(&tp.subject),
                        self.named_pattern(&tp.predicate),
                        self.term_pattern(&tp.object),
                    ];
                    self.unpacks.borrow_mut().push(UnpackItem { t, parts });
                    PT::V(t)
                }
            },
        }
    }

    fn named_pattern(&self, n: &NamedNodePattern) -> PT {
        match n {
            NamedNodePattern::NamedNode(n) => {
                PT::C(self.ctx.intern_term(&Term::NamedNode(n.clone())))
            }
            NamedNodePattern::Variable(v) => self.var(v.as_str()),
        }
    }

    // ------------------------------------------------------------------ plan ------

    pub fn plan(&mut self, gp: &GraphPattern, g: &ActiveGraph, filters: Vec<Expr>) -> Result<Node> {
        self.ctx.check()?;
        use GraphPattern as GP;
        match gp {
            // Jena's `SERVICE <loop:…>`: evaluated per solution of the patterns before it
            GP::Join { left, right } if super::enhancer::is_loop(right) => {
                let certain = certain_vars(left, self.ctx);
                let (push, top) = self.split_filters(filters, |v| certain.contains(v));
                let l = self.plan(left, g, push)?;
                let n = super::enhancer::plan_loop(self, l, right, g, None)?;
                Ok(self.apply_filters(n, top))
            }
            GP::LeftJoin {
                left,
                right,
                expression,
            } if super::enhancer::is_loop(right) => {
                let certain = certain_vars(left, self.ctx);
                let (push, top) = self.split_filters(filters, |v| certain.contains(v));
                let l = self.plan(left, g, push)?;
                let n = super::enhancer::plan_loop(self, l, right, g, Some(expression.clone()))?;
                Ok(self.apply_filters(n, top))
            }
            GP::Bgp { .. } | GP::Path { .. } | GP::Join { .. } | GP::Graph { .. } => {
                let mut items = Vec::new();
                self.collect(gp, g, &mut items)?;
                self.plan_group(items, filters)
            }
            GP::Lateral { left, right } if super::lateral::join_equivalent(self, left, right) => {
                let mut items = Vec::new();
                self.collect(gp, g, &mut items)?;
                self.plan_group(items, filters)
            }
            GP::Lateral { left, right } => {
                // filters on variables every left row binds keep their value in the
                // merged rows, as the right side cannot bind them differently
                let certain = certain_vars(left, self.ctx);
                let (push, top) = self.split_filters(filters, |v| certain.contains(v));
                let l = self.plan(left, g, push)?;
                let n = super::lateral::plan(self, l, right, g)?;
                Ok(self.apply_filters(n, top))
            }
            GP::Filter { expr, inner } => {
                let mut fs = filters;
                fs.extend(self.compile(expr, g).conjuncts());
                self.plan(inner, g, fs)
            }
            GP::LeftJoin {
                left,
                right,
                expression,
            } if super::arqpf::reads_left(self, left, right) => {
                // ARQ evaluates the OPTIONAL per left row, so that the property functions
                // of the right side read the left side's values
                let certain = certain_vars(left, self.ctx);
                let (push, top) = self.split_filters(filters, |v| certain.contains(v));
                let l = self.plan(left, g, push)?;
                let n = super::lateral::plan_optional(self, l, right, expression.as_ref(), g)?;
                Ok(self.apply_filters(n, top))
            }
            GP::LeftJoin {
                left,
                right,
                expression,
            } => {
                let certain = certain_vars(left, self.ctx);
                let (push, top) = self.split_filters(filters, |v| certain.contains(v));
                let l = self.plan(left, g, push)?;
                let r = self.plan(right, g, Vec::new())?;
                let expr = expression.as_ref().map(|e| self.compile(e, g));
                let n = left_join(l, r, expr, self.ctx);
                Ok(self.apply_filters(n, top))
            }
            GP::Union { .. } => {
                let mut branches = Vec::new();
                flatten_union(gp, &mut branches);
                let mut children = Vec::new();
                for b in branches {
                    let c = self.plan(b, g, filters.clone())?;
                    if !c.is_empty() {
                        children.push(c);
                    }
                }
                Ok(union(children))
            }
            GP::SemiJoin { left, right } | GP::AntiJoin { left, right } => {
                // the left side's solutions are kept unchanged, so a filter over the
                // result is a filter over the left side
                let anti = matches!(gp, GP::AntiJoin { .. });
                let l = self.plan(left, g, filters)?;
                let r = self.plan(right, g, Vec::new())?;
                if l.is_empty() || (r.is_empty() && anti) {
                    return Ok(l);
                }
                if r.is_empty() {
                    return Ok(Node::empty(l.vars.clone()));
                }
                let shared: Vec<String> = l
                    .vars
                    .iter()
                    .filter(|v| r.vars.contains(v))
                    .map(|v| format!("?{}", self.ctx.var_name(*v)))
                    .collect();
                let desc = format!("on {}", shared.join(" "));
                let mut n = Node::unary(Kind::HalfJoin { anti }, l, desc);
                n.est = (n.est * 0.5).max(1.0);
                n.cost += r.cost + r.est;
                n.children.push(r);
                Ok(n)
            }
            GP::Minus { left, right } => {
                let l = self.plan(left, g, filters)?;
                let r = self.plan(right, g, Vec::new())?;
                if l.is_empty() {
                    return Ok(l);
                }
                let shared: Vec<VarId> = l
                    .vars
                    .iter()
                    .filter(|v| r.vars.contains(v))
                    .copied()
                    .collect();
                if r.is_empty() || shared.is_empty() {
                    return Ok(l);
                }
                let mut n = Node::unary(Kind::Minus, l, "MINUS".into());
                n.cost += r.cost + r.est;
                n.children.push(r);
                Ok(n)
            }
            GP::Extend {
                inner,
                variable,
                expression,
            } => {
                let v = self.ctx.var(variable.as_str());
                let (push, top) = self.split_filters(filters, |x| *x != v);
                let child = self.plan(inner, g, push)?;
                let e = self.compile(expression, g);
                let desc = format!("?{} := {}", variable.as_str(), e.display(self.ctx));
                let mut n = Node::unary(Kind::Extend(v, e), child, desc);
                n.vars.push(v);
                n.dist.insert(v, n.est);
                Ok(self.apply_filters(n, top))
            }
            GP::Assign {
                inner,
                variable,
                expression,
            } => {
                let v = self.ctx.var(variable.as_str());
                let (push, top) = self.split_filters(filters, |x| *x != v);
                let child = self.plan(inner, g, push)?;
                let e = self.compile(expression, g);
                let desc = format!("?{} := {}", variable.as_str(), e.display(self.ctx));
                if let Some(&c) = self.subst.get(&v) {
                    // the outer solution binds ?v (a LATERAL or EXISTS substitution): a
                    // different value drops the solution, and an error keeps it
                    let keep = Expr::Coalesce(vec![
                        Expr::Eq(Box::new(e), Box::new(Expr::Const(c))),
                        Expr::Const(Id::from_bool(true)),
                    ]);
                    return Ok(self.apply_filters(filter(child, vec![keep], self.ctx), top));
                }
                let n = if child.vars.contains(&v) {
                    // LET of a variable the pattern may bind: compared where it is bound
                    let mut n = Node::unary(Kind::Assign(v, e), child, desc);
                    n.est *= FILTER_SELECTIVITY.max(0.5);
                    n
                } else {
                    // LET of a new variable is BIND
                    let mut n = Node::unary(Kind::Extend(v, e), child, desc);
                    n.vars.push(v);
                    n.dist.insert(v, n.est);
                    n
                };
                Ok(self.apply_filters(n, top))
            }
            GP::Unfold {
                inner,
                expression,
                variable,
                second,
            } => {
                let v = self.ctx.var(variable.as_str());
                let w = second.as_ref().map(|s| self.ctx.var(s.as_str()));
                let (push, top) = self.split_filters(filters, |x| *x != v && Some(*x) != w);
                let child = self.plan(inner, g, push)?;
                let e = self.compile(expression, g);
                let desc = match second {
                    Some(s) => format!(
                        "{} AS ?{}, ?{}",
                        e.display(self.ctx),
                        variable.as_str(),
                        s.as_str()
                    ),
                    None => format!("{} AS ?{}", e.display(self.ctx), variable.as_str()),
                };
                let mut n = Node::unary(
                    Kind::Unfold {
                        expr: e,
                        var: v,
                        second: w,
                    },
                    child,
                    desc,
                );
                // a list or map of a few elements per solution
                n.est *= 4.0;
                n.cost += n.est;
                for x in [Some(v), w].into_iter().flatten() {
                    n.vars.push(x);
                    n.dist.insert(x, n.est);
                }
                Ok(self.apply_filters(n, top))
            }
            GP::Values {
                variables,
                bindings,
            } => {
                let vars: Vec<VarId> = variables.iter().map(|v| self.ctx.var(v.as_str())).collect();
                let mut t = Table::new(vars.clone());
                // a term repeated in the table is looked up once
                let mut seen: FxHashMap<&GroundTerm, Id> = FxHashMap::default();
                for row in bindings {
                    let ids: Vec<Id> = row
                        .iter()
                        .map(|t| match t {
                            None => Id::UNDEF,
                            Some(t) => *seen.entry(t).or_insert_with(|| match t {
                                GroundTerm::NamedNode(n) => {
                                    self.ctx.intern_term(&Term::NamedNode(n.clone()))
                                }
                                GroundTerm::Literal(l) => self.ctx.intern_query_literal(l),
                                GroundTerm::Triple(_) => self.ctx.intern_term(&ground_term(t)),
                            }),
                        })
                        .collect();
                    t.push_row(&ids);
                }
                // substituted variables (initial bindings, filter equalities): keep only
                // compatible rows and fill their UNDEF cells with the bound value, so the
                // table agrees with the substitution the rest of the plan assumes
                for (c, v) in vars.iter().enumerate() {
                    let Some(&bound) = self.subst.get(v) else {
                        continue;
                    };
                    let keep: Vec<bool> = t.cols[c]
                        .iter()
                        .map(|id| id.is_undef() || *id == bound)
                        .collect();
                    t.filter_rows(&keep);
                    t.cols[c].fill(bound);
                }
                let certain: Vec<VarId> = vars
                    .iter()
                    .enumerate()
                    .filter(|(c, _)| t.cols[*c].iter().all(|id| !id.is_undef()))
                    .map(|(_, v)| *v)
                    .collect();
                let est = t.len() as f64;
                let mut n = Node::leaf(Kind::Values(t), vars, est, format!("{} rows", est));
                n.certain = certain;
                Ok(self.apply_filters(n, filters))
            }
            GP::OrderBy { inner, expression } => {
                let child = self.plan(inner, g, Vec::new())?;
                let keys = expression
                    .iter()
                    .map(|o| match o {
                        OrderExpression::Asc(e) => (self.compile(e, g), true),
                        OrderExpression::Desc(e) => (self.compile(e, g), false),
                    })
                    .collect::<Vec<_>>();
                let desc = keys
                    .iter()
                    .map(|(e, asc)| {
                        format!(
                            "{}({})",
                            if *asc { "ASC" } else { "DESC" },
                            e.display(self.ctx)
                        )
                    })
                    .collect::<Vec<_>>()
                    .join(" ");
                let mut n = Node::unary(Kind::OrderBy { keys, limit: None }, child, desc);
                n.sorted.clear();
                n.cost += n.est * n.est.max(2.0).log2() * 0.5;
                Ok(self.apply_filters(n, filters))
            }
            GP::Project { inner, variables } => {
                // sub-select: fresh scope for blank node labels
                self.bnode_scope += 1;
                let vars: Vec<VarId> = variables.iter().map(|v| self.ctx.var(v.as_str())).collect();
                // a LATERAL's substitution does not reach a variable the sub-select hides
                let hidden: Vec<(VarId, Id)> = self
                    .scoped
                    .iter()
                    .filter(|v| !vars.contains(v))
                    .filter_map(|v| self.subst.get(v).map(|id| (*v, *id)))
                    .collect();
                for (v, _) in &hidden {
                    self.subst.remove(v);
                }
                let child = self.plan(inner, g, Vec::new());
                self.subst.extend(hidden);
                let child = child?;
                let n = project(child, vars, self.ctx);
                Ok(self.apply_filters(n, filters))
            }
            GP::Distinct { inner } => {
                let child = self.plan(inner, g, Vec::new())?;
                let mut n = Node::unary(Kind::Distinct, child, String::new());
                n.est = n.children[0].est; // unknown reduction
                Ok(self.apply_filters(n, filters))
            }
            GP::Reduced { inner } => {
                let child = self.plan(inner, g, Vec::new())?;
                Ok(self.apply_filters(child, filters))
            }
            GP::Slice {
                inner,
                start,
                length,
            } => {
                let child = self.plan(inner, g, Vec::new())?;
                let n = slice(child, *start, *length, self.ctx);
                Ok(self.apply_filters(n, filters))
            }
            GP::Group {
                inner,
                variables,
                aggregates,
            } => {
                let child = self.plan(inner, g, Vec::new())?;
                let keys: Vec<VarId> = variables.iter().map(|v| self.ctx.var(v.as_str())).collect();
                let aggs: Vec<(VarId, Agg)> = aggregates
                    .iter()
                    .map(|(v, a)| {
                        let agg = match a {
                            AggregateExpression::CountSolutions { distinct } => Agg {
                                func: AggregateFunction::Count,
                                expr: None,
                                distinct: *distinct,
                                fold: None,
                            },
                            AggregateExpression::FunctionCall {
                                name,
                                expr,
                                distinct,
                            } => Agg {
                                func: name.clone(),
                                expr: Some(self.compile(expr, g)),
                                distinct: *distinct,
                                fold: None,
                            },
                            AggregateExpression::Fold {
                                expr,
                                value,
                                distinct,
                                order,
                            } => Agg {
                                func: AggregateFunction::Custom(
                                    spargebra::term::NamedNode::new_unchecked(FOLD),
                                ),
                                expr: Some(self.compile(expr, g)),
                                distinct: *distinct,
                                fold: Some(Box::new(Fold {
                                    value: value.as_ref().map(|e| self.compile(e, g)),
                                    order: order
                                        .iter()
                                        .map(|o| match o {
                                            OrderExpression::Asc(e) => (self.compile(e, g), true),
                                            OrderExpression::Desc(e) => (self.compile(e, g), false),
                                        })
                                        .collect(),
                                })),
                            },
                        };
                        (self.ctx.var(v.as_str()), agg)
                    })
                    .collect();
                let n = group(child, keys, aggs, self.ctx);
                Ok(self.apply_filters(n, filters))
            }
            GP::Service { inner, name, .. } if super::history_svc::is_changes(name) => {
                let n = super::history_svc::history_leaf(self, inner)?;
                Ok(self.apply_filters(n, filters))
            }
            GP::Service { name, inner, .. } if super::pathsearch::is_search(name) => {
                let n = super::pathsearch::path_search_leaf(self, inner, g)?;
                Ok(self.apply_filters(n, filters))
            }
            GP::Service {
                name,
                inner,
                silent,
            } => {
                // Jena's service enhancer: options in front of the IRI, and the dataset
                // itself
                let (inner, silent, cache, endpoint) =
                    match super::enhancer::effective(name, inner, *silent)? {
                        Some(e) => match e.target {
                            super::enhancer::Target::SelfDataset => {
                                // a separate query on the same dataset and view
                                return self.plan(e.inner, &ActiveGraph::Default, filters);
                            }
                            super::enhancer::Target::Remote(url) => (
                                e.inner,
                                e.silent,
                                e.opts.cache,
                                PathEnd::Const(self.ctx.intern_term(&Term::NamedNode(url))),
                            ),
                        },
                        None => (
                            &**inner,
                            *silent,
                            super::enhancer::CacheMode::None,
                            match self.named_pattern(name) {
                                PT::C(id) => PathEnd::Const(id),
                                PT::V(v) => PathEnd::Var(v),
                            },
                        ),
                    };
                // the constants in force (initial bindings, the row of an EXISTS or a
                // LATERAL) are sent as values
                let subst = super::enhancer::planner_subst(self, inner);
                let mut names = Vec::new();
                collect_pattern_vars(inner, &mut names);
                let mut vars: Vec<VarId> = names
                    .iter()
                    .filter(|n| !subst.map.contains_key(*n))
                    .map(|n| self.ctx.var(n))
                    .collect();
                vars.sort_unstable();
                vars.dedup();
                if let PathEnd::Var(v) = endpoint
                    && !vars.contains(&v)
                {
                    vars.push(v);
                }
                let query = if subst.map.is_empty() {
                    super::enhancer::single_request(inner)
                } else {
                    super::enhancer::single_request(&super::enhancer::substitute(inner, &subst))
                };
                let mut n = Node::leaf(
                    Kind::Service {
                        endpoint,
                        query: query.clone(),
                        silent,
                        cache,
                    },
                    vars,
                    1000.0,
                    query,
                );
                n.certain.clear();
                n.cost = 100_000.0;
                Ok(self.apply_filters(n, filters))
            }
        }
    }

    fn split_filters(
        &self,
        filters: Vec<Expr>,
        ok: impl Fn(&VarId) -> bool,
    ) -> (Vec<Expr>, Vec<Expr>) {
        filters
            .into_iter()
            .partition(|f| !f.has_exists() && f.var_set().iter().all(&ok))
    }

    fn apply_filters(&self, n: Node, filters: Vec<Expr>) -> Node {
        if filters.is_empty() || n.is_empty() {
            return n;
        }
        filter(n, filters, self.ctx)
    }

    /// Flatten a join group into items + filters.
    fn collect(&mut self, gp: &GraphPattern, g: &ActiveGraph, items: &mut Vec<Item>) -> Result<()> {
        use GraphPattern as GP;
        match gp {
            GP::Bgp { patterns } => {
                // ARQ's property function library: the variables bound before a call
                // are those of the group's earlier elements
                let members;
                let patterns = match super::arqpf::container_members(self.ctx, patterns) {
                    Some(m) => {
                        members = m;
                        &members
                    }
                    None => patterns,
                };
                let rest;
                let patterns = if super::arqpf::has_calls(patterns) {
                    let (pcalls, others) = super::arqpf::take_calls(patterns)?;
                    let mut bound = item_vars(items);
                    for c in pcalls {
                        let n = super::arqpf::leaf(self, c, g, &bound)?;
                        bound.extend(n.vars.iter().copied());
                        items.push(Item::Node(n));
                    }
                    rest = others;
                    &rest
                } else {
                    patterns
                };
                let (calls, patterns) = super::textpf::extract(patterns)?;
                let (vcalls, patterns) = super::textpf::take_calls(
                    &patterns,
                    crate::vector::VECTOR_SEARCH,
                    "spk:vectorSearch",
                )?;
                let (hcalls, mut patterns) = super::textpf::take_calls(
                    &patterns,
                    super::hybrid::HYBRID_SEARCH,
                    "spk:hybridSearch",
                )?;
                let hcalls = hcalls
                    .into_iter()
                    .map(|(s, o)| Ok((s, super::hybrid::take_lists(&mut patterns, o)?)))
                    .collect::<Result<Vec<_>>>()?;
                let (rcalls, patterns) =
                    super::georewrite::take_rewrite_triples(patterns, self.ctx)?;
                let (scalls, patterns) = super::geopf::take_spatial_calls(&patterns)?;
                for tp in &patterns {
                    items.push(Item::Triple(self.triple(tp, g)));
                }
                for c in calls {
                    items.push(Item::Node(self.text_leaf(c, g)?));
                }
                for (subjects, args) in vcalls {
                    items.push(Item::Node(self.vector_leaf(subjects, args, g)?));
                }
                for (subjects, args) in hcalls {
                    items.push(Item::Node(super::hybrid::hybrid_leaf(
                        self, subjects, args, g,
                    )?));
                }
                for c in scalls {
                    items.push(Item::Node(super::geopf::spatial_leaf(self, c, g)?));
                }
                for c in rcalls {
                    items.push(Item::Node(super::georewrite::rewrite_leaf(self, c, g)?));
                }
                items.extend(self.unpacks.borrow_mut().drain(..).map(Item::Unpack));
            }
            GP::Path {
                subject,
                path,
                object,
            } => {
                let s = self.term_pattern(subject);
                let o = self.term_pattern(object);
                self.collect_path(s, path, o, g, items)?;
                items.extend(self.unpacks.borrow_mut().drain(..).map(Item::Unpack));
            }
            GP::Join { left, right } if !super::enhancer::is_loop(right) => {
                self.collect(left, g, items)?;
                self.collect(right, g, items)?;
            }
            GP::Lateral { left, right } if super::lateral::join_equivalent(self, left, right) => {
                self.collect(left, g, items)?;
                self.collect(right, g, items)?;
            }
            GP::Graph { name, inner } if !self.simple_graph_group(name, inner) => {
                // Per-graph evaluation (Jena OpGraph semantics): evaluate the inner pattern
                // against each named graph with the graph variable unbound, then join
                // with ?g = graph.
                let NamedNodePattern::Variable(gv) = name else {
                    unreachable!()
                };
                let v = self.ctx.var(gv.as_str());
                if let Some(&gid) = self.subst.get(&v) {
                    // a substituted graph variable (EXISTS, LATERAL, initial bindings)
                    // names one graph
                    let named = ActiveGraph::Named(gid);
                    let node = match self.graph_filter(&named) {
                        Some(_) => self.plan(inner, &named, Vec::new())?,
                        None => Node::empty(Vec::new()),
                    };
                    items.push(Item::Node(node));
                    items.push(Item::Node(self.graph_names(&named)?));
                    return Ok(());
                }
                let accept = |gid: &Id| {
                    self.ctx
                        .dataset
                        .named
                        .as_ref()
                        .is_none_or(|s| s.contains(gid))
                };
                let graphs: Vec<Id> = self
                    .ctx
                    .snap
                    .graph_ids()?
                    .into_iter()
                    .filter(accept)
                    .collect();
                let mut branches = Vec::new();
                for gid in graphs {
                    let node = self.plan(inner, &ActiveGraph::Named(gid), Vec::new())?;
                    if node.is_empty() {
                        continue;
                    }
                    let mut t = Table::new(vec![v]);
                    t.push_row(&[gid]);
                    let bind = Node::leaf(Kind::Values(t), vec![v], 1.0, "graph".into());
                    branches.push(join(node, bind, self.ctx));
                }
                let mut n = union(branches);
                if n.vars.is_empty() && n.children.is_empty() {
                    n = Node::empty(vec![v]);
                }
                items.push(Item::Node(n));
            }
            GP::Graph { name, inner } => {
                let g2 = match name {
                    NamedNodePattern::NamedNode(n)
                        if n.as_str() == super::ctx::DEFAULT_GRAPH_IRI =>
                    {
                        ActiveGraph::Named(Id::DEFAULT_GRAPH)
                    }
                    NamedNodePattern::NamedNode(n) if n.as_str() == super::ctx::UNION_GRAPH_IRI => {
                        ActiveGraph::Union
                    }
                    _ => match self.named_pattern(name) {
                        PT::C(id) => ActiveGraph::Named(id),
                        PT::V(v) => ActiveGraph::Var(v),
                    },
                };
                if let GP::Filter { .. } = &**inner {
                    items.push(Item::Node(self.plan(inner, &g2, Vec::new())?));
                } else {
                    self.collect(inner, &g2, items)?;
                }
                // GRAPH ?g / GRAPH <g> iterate over (existing) named graphs even when the
                // inner pattern does not touch the data
                items.push(Item::Node(self.graph_names(&g2)?));
            }
            other => items.push(Item::Node(self.plan(other, g, Vec::new())?)),
        }
        Ok(())
    }

    fn collect_path(
        &mut self,
        s: PT,
        path: &PropertyPathExpression,
        o: PT,
        g: &ActiveGraph,
        items: &mut Vec<Item>,
    ) -> Result<()> {
        use PropertyPathExpression as PP;
        match path {
            // ARQ's `multi(path)`: the path with its closures counting walks
            PP::Multi(p) => return self.collect_path(s, &counted(p), o, g, items),
            // ARQ's `distinct(path)`: the pairs the path connects, each once
            PP::Distinct(p) => {
                let mut sub = Vec::new();
                self.collect_path(s, p, o, g, &mut sub)?;
                let node = self.plan_group(sub, Vec::new())?;
                let mut vars = Vec::new();
                for x in [s, o] {
                    if let PT::V(v) = x
                        && !vars.contains(&v)
                    {
                        vars.push(v);
                    }
                }
                let n = project(node, vars, self.ctx);
                let mut d = Node::unary(Kind::Distinct, n, "distinct(path)".into());
                d.est = d.children[0].est;
                items.push(Item::Node(d));
                return Ok(());
            }
            PP::Shortest(_) => {
                return Err(Error::invalid(
                    "shortest(…) paths are not implemented (ARQ does not evaluate them either)",
                ));
            }
            PP::NamedNode(n) => items.push(Item::Triple(Triple {
                t: [
                    s,
                    PT::C(self.ctx.intern_term(&Term::NamedNode(n.clone()))),
                    o,
                ],
                graph: g.clone(),
            })),
            PP::Reverse(p) => self.collect_path(o, p, s, g, items)?,
            PP::Sequence(a, b) => {
                let mid = PT::V(self.ctx.fresh_var());
                self.collect_path(s, a, mid, g, items)?;
                self.collect_path(mid, b, o, g, items)?;
            }
            // ARQ's PathCompiler: `p{n,m}` with n ≥ 1 is n steps of p, then p{0,m-n}
            // (p{*} when unbounded); the steps are joined like a sequence's
            PP::Range { path, min, max } if (1..=RANGE_UNROLL).contains(min) => {
                let mut from = s;
                for i in 0..*min {
                    let to = if i + 1 == *min && *max == Some(*min) {
                        o
                    } else {
                        PT::V(self.ctx.fresh_var())
                    };
                    self.collect_path(from, path, to, g, items)?;
                    from = to;
                }
                if *max != Some(*min) {
                    let rest = PP::Range {
                        path: path.clone(),
                        min: 0,
                        max: max.map(|m| m - min),
                    };
                    items.push(Item::Path(PathItem {
                        s: from,
                        path: rest,
                        o,
                        graph: g.clone(),
                    }));
                }
            }
            _ => items.push(Item::Path(PathItem {
                s,
                path: path.clone(),
                o,
                graph: g.clone(),
            })),
        }
        Ok(())
    }

    /// A `spk:vectorSearch` call as a search leaf:
    /// `(?s ?score ?vector) spk:vectorSearch (predicate query [k] ["metric:…"])`.
    pub(super) fn vector_leaf(
        &self,
        subjects: Vec<TermPattern>,
        args: Vec<TermPattern>,
        g: &ActiveGraph,
    ) -> Result<Node> {
        use crate::vector::{self, Metric};
        let bad = |m: &str| Error::invalid(format!("spk:vectorSearch: {m}"));
        let shape = || bad("expected (predicate query [k] [options])");
        if subjects.is_empty() || subjects.len() > 3 {
            return Err(bad(
                "the subject is ?s, (?s), (?s ?score) or (?s ?score ?vector)",
            ));
        }
        let mut slots = subjects.iter();
        let subject = match self.term_pattern(slots.next().unwrap()) {
            PT::V(v) => PathEnd::Var(v),
            PT::C(id) => PathEnd::Const(id),
        };
        let mut var_slot = |name: &str| -> Result<Option<VarId>> {
            match slots.next().map(|t| self.term_pattern(t)) {
                None => Ok(None),
                Some(PT::V(v)) => Ok(Some(v)),
                Some(PT::C(_)) => Err(bad(&format!("{name} must be a variable"))),
            }
        };
        let (score, vector_var) = (var_slot("the score")?, var_slot("the vector")?);
        let mut args = args.into_iter();
        let (pred, args_pred) = match args.next() {
            Some(TermPattern::NamedNode(p)) => {
                (self.ctx.snap.lookup_iri(p.as_str()), p.as_str().to_string())
            }
            _ => return Err(shape()),
        };
        let query = match args.next() {
            Some(TermPattern::Literal(l)) if vector::is_datatype(l.datatype().as_str()) => {
                let v = vector::parse_typed(l.value(), l.datatype().as_str())
                    .expect("a vector datatype")
                    .map_err(Error::invalid)?;
                VectorQuery::Vector(v.into())
            }
            Some(TermPattern::Literal(l))
                if l.datatype() == oxrdf::vocab::xsd::STRING
                    || l.datatype() == oxrdf::vocab::rdf::LANG_STRING =>
            {
                // a text: the predicate's index must name an embedding provider
                self.check_text_query(&args_pred)?;
                VectorQuery::Text(l.value().into())
            }
            Some(TermPattern::Literal(_)) => {
                return Err(bad(
                    "the query must be an spk:vector literal, a text or an entity",
                ));
            }
            Some(
                t @ (TermPattern::NamedNode(_)
                | TermPattern::BlankNode(_)
                | TermPattern::Variable(_)),
            ) => match self.term_pattern(&t) {
                PT::C(id) => VectorQuery::Entity(id),
                // a variable (or a blank node, which is one in a pattern)
                PT::V(v) => VectorQuery::Var(v),
            },
            _ => return Err(shape()),
        };
        let mut k = 10;
        let mut metric = None;
        let mut mode = vector::SearchMode::default();
        let (mut distinct_subject, mut candidates) = (false, false);
        for a in args {
            let TermPattern::Literal(l) = a else {
                return Err(shape());
            };
            if l.datatype() == oxrdf::vocab::xsd::STRING {
                match l.value().split_once(':') {
                    Some(("metric", m)) => {
                        metric = Some(
                            Metric::parse(m)
                                .ok_or_else(|| bad(&format!("unknown metric {m:?}")))?,
                        );
                    }
                    Some(("exact", "true")) => mode.exact = true,
                    Some(("exact", "false")) => mode.exact = false,
                    Some(("ef", n)) => {
                        let ef = n.parse::<usize>().map_err(|_| {
                            bad(&format!("ef must be a positive integer, got {n:?}"))
                        })?;
                        mode.ef = Some(vector::SearchMode::validate_ef(ef)?);
                    }
                    Some(("distinct", "subject")) => distinct_subject = true,
                    Some(("candidates", "join")) => candidates = true,
                    _ => return Err(bad(&format!("unknown option {:?}", l.value()))),
                }
            } else {
                k = l
                    .value()
                    .parse::<usize>()
                    .ok()
                    .filter(|k| (1..=vector::MAX_K).contains(k))
                    .ok_or_else(|| bad(&format!("k must be 1..={}", vector::MAX_K)))?;
            }
        }
        let empty = |vars: Vec<VarId>| {
            let mut vars = vars;
            vars.sort_unstable();
            vars.dedup();
            Ok(Node::empty(vars))
        };
        let mut vars = Vec::new();
        for v in [
            match subject {
                PathEnd::Var(v) => Some(v),
                _ => None,
            },
            score,
            vector_var,
        ]
        .into_iter()
        .flatten()
        {
            if !vars.contains(&v) {
                vars.push(v);
            }
        }
        let Some((graph, graph_var)) = self.graph_filter(g) else {
            return empty(vars);
        };
        if let Some(gv) = graph_var
            && !vars.contains(&gv)
        {
            vars.push(gv);
        }
        let dedup = graph_var.is_none() && graph.multi();
        // the metric of the predicate's configured index, else cosine
        let metric = metric.unwrap_or_else(|| {
            pred.and_then(|p| {
                self.ctx
                    .snap
                    .generation
                    .vectors
                    .configured_for(&self.ctx.snap, p.0)
            })
            .map_or(Metric::Cosine, |(_, c)| c.metric)
        });
        if candidates && !matches!(subject, PathEnd::Var(_)) {
            return Err(bad("candidates:join needs a variable subject"));
        }
        let desc = format!(
            "{} ← {} {} k={}{}",
            vars.iter()
                .map(|v| format!("?{}", self.ctx.var_name(*v)))
                .collect::<Vec<_>>()
                .join(" "),
            pred.map_or("?".into(), |p| self.pt_str(&PT::C(p))),
            metric.name(),
            k,
            match &query {
                VectorQuery::Vector(v) => format!(" dim={}", v.len()),
                VectorQuery::Entity(e) => format!(" like {}", self.pt_str(&PT::C(*e))),
                VectorQuery::Var(v) => format!(" like ?{}", self.ctx.var_name(*v)),
                VectorQuery::Text(t) =>
                    format!(" text {:?}", t.chars().take(40).collect::<String>()),
            } + if mode.exact { " exact" } else { "" }
                + &mode.ef.map_or(String::new(), |e| format!(" ef={e}"))
                + if distinct_subject {
                    " distinct:subject"
                } else {
                    ""
                }
                + if candidates { " candidates:join" } else { "" }
        );
        let spec = VectorSpec {
            pred,
            query,
            k,
            metric,
            subject,
            score,
            vector: vector_var,
            graph,
            graph_var,
            dedup,
            mode,
            distinct_subject,
            candidates,
            order_fallback: false,
        };
        let mut n = Node::leaf(Kind::VectorSearch(Box::new(spec)), vars, k as f64, desc);
        n.cost = k as f64 * 16.0;
        Ok(n)
    }

    /// A text query needs an index on `pred` with an embedding provider that embeds
    /// query text.
    fn check_text_query(&self, pred: &str) -> Result<()> {
        let configured = self.ctx.snap.generation.vectors.configured();
        let emb = configured
            .values()
            .find(|c| c.predicate == pred)
            .and_then(|c| c.embedding.as_ref());
        match emb {
            None => Err(Error::invalid(format!(
                "spk:vectorSearch: <{pred}> has no embedding provider; pass an spk:vector literal"
            ))),
            Some(e) if !e.query_text => Err(Error::invalid(format!(
                "spk:vectorSearch: the index of <{pred}> does not embed query text (queryText is false)"
            ))),
            Some(_) => Ok(()),
        }
    }

    /// A search that reads the rest of its group (`left`): a variable query bound there,
    /// or `candidates:join` over the subjects bound there.
    fn attach_vector(&self, left: Node, search: Node) -> Result<Node> {
        let Kind::VectorSearch(spec) = &search.kind else {
            unreachable!("only vector searches depend on their group");
        };
        let bad = |m: String| Error::invalid(format!("spk:vectorSearch: {m}"));
        if let VectorQuery::Var(v) = spec.query
            && !left.vars.contains(&v)
        {
            return Err(bad(format!(
                "the query variable ?{} is not bound by the rest of the group",
                self.ctx.var_name(v)
            )));
        }
        if spec.candidates
            && let PathEnd::Var(s) = spec.subject
            && !left.vars.contains(&s)
        {
            return Err(bad(format!(
                "candidates:join needs ?{} bound by the rest of the group",
                self.ctx.var_name(s)
            )));
        }
        let mut vars = left.vars.clone();
        for v in &search.vars {
            if !vars.contains(v) {
                vars.push(*v);
            }
        }
        let mut certain = left.certain.clone();
        certain.extend(search.vars.iter().copied());
        let est = (left.est * spec.k as f64).max(1.0);
        Ok(Node {
            dist: vars.iter().map(|&v| (v, est)).collect(),
            cost: left.cost + est * 16.0,
            vars,
            certain,
            sorted: Vec::new(),
            est,
            desc: search.desc,
            kind: search.kind,
            children: vec![left],
        })
    }

    /// A `text:query` call with a variable query string over the rest of its group.
    fn attach_text(&self, left: Node, search: Node) -> Result<Node> {
        let Kind::TextSearch(spec) = &search.kind else {
            unreachable!("a text search that depends on its group");
        };
        if let Some(q) = spec.query_var
            && !left.vars.contains(&q)
        {
            return Err(Error::invalid(format!(
                "text:query: the query variable ?{} is not bound by the rest of the group",
                self.ctx.var_name(q)
            )));
        }
        let mut vars = left.vars.clone();
        for v in &search.vars {
            if !vars.contains(v) {
                vars.push(*v);
            }
        }
        let mut certain = left.certain.clone();
        certain.extend(search.vars.iter().copied());
        let est = (left.est * spec.limit.unwrap_or(10) as f64).max(1.0);
        Ok(Node {
            dist: vars.iter().map(|&v| (v, est)).collect(),
            cost: left.cost + est * 8.0,
            vars,
            certain,
            sorted: Vec::new(),
            est,
            desc: search.desc,
            kind: search.kind,
            children: vec![left],
        })
    }

    /// A `text:query` call as a search leaf.
    pub(super) fn text_leaf(&self, c: super::textpf::TextCall, g: &ActiveGraph) -> Result<Node> {
        let slot = |t: &Option<TermPattern>| -> Option<VarId> {
            match t.as_ref().map(|t| self.term_pattern(t)) {
                Some(PT::V(v)) => Some(v),
                _ => None,
            }
        };
        let subject = match self.term_pattern(&c.subject) {
            PT::V(v) => PathEnd::Var(v),
            PT::C(id) => PathEnd::Const(id),
        };
        let query_var = slot(&c.query_var);
        // a score or literal the rest of the query never reads is not produced, and the
        // search then reads no literal
        let output =
            |t: &Option<TermPattern>| t.as_ref().filter(|t| self.used_elsewhere(t)).cloned();
        let (score, literal, graph_out, prop, rank) = (
            slot(&output(&c.score)),
            slot(&output(&c.literal)),
            slot(&c.graph),
            slot(&c.prop),
            slot(&output(&c.rank)),
        );
        let Some((graph, graph_var)) = self.graph_filter(g) else {
            let mut vars: Vec<VarId> = [score, literal, graph_out, prop, rank]
                .into_iter()
                .flatten()
                .collect();
            if let PathEnd::Var(v) = subject {
                vars.push(v);
            }
            vars.sort_unstable();
            vars.dedup();
            return Ok(Node::empty(vars));
        };
        let dedup = graph_out.is_none() && graph_var.is_none() && graph.multi();
        let mut vars = Vec::new();
        for v in [
            match subject {
                PathEnd::Var(v) => Some(v),
                _ => None,
            },
            score,
            literal,
            graph_out,
            graph_var,
            prop,
            rank,
        ]
        .into_iter()
        .flatten()
        {
            if !vars.contains(&v) {
                vars.push(v);
            }
        }
        let desc = format!(
            "{} ← {}{}{}{}{}",
            vars.iter()
                .map(|v| format!("?{}", self.ctx.var_name(*v)))
                .collect::<Vec<_>>()
                .join(" "),
            match query_var {
                Some(q) => format!("?{}", self.ctx.var_name(q)),
                None => format!("{:?}", c.query),
            },
            if c.predicates.is_empty() {
                String::new()
            } else {
                format!(
                    " [{}]",
                    c.predicates
                        .iter()
                        .map(|p| format!("<{}>", p.as_str()))
                        .collect::<Vec<_>>()
                        .join(" ")
                )
            },
            c.lang
                .as_ref()
                .map(|l| format!(" lang={l}"))
                .unwrap_or_default(),
            c.limit.map(|l| format!(" limit {l}")).unwrap_or_default(),
            // the options are part of the description, and so of the result cache key
            match (&c.highlight, literal) {
                (Some(h), Some(_)) => format!(" highlight {h:?}"),
                _ => String::new(),
            },
        );
        let spec = TextSpec {
            query: c.query,
            query_var,
            predicates: c
                .predicates
                .iter()
                .map(|p| p.as_str().to_string())
                .collect(),
            lang: c.lang,
            limit: c.limit,
            subject,
            score,
            literal,
            graph_out,
            prop,
            rank,
            graph,
            graph_var,
            dedup,
            highlight: c.highlight.filter(|_| literal.is_some()),
        };
        let est = spec.limit.unwrap_or(1000) as f64;
        let mut n = Node::leaf(Kind::TextSearch(Box::new(spec)), vars, est, desc);
        n.cost = est * 4.0;
        Ok(n)
    }

    fn triple(&self, tp: &TriplePattern, g: &ActiveGraph) -> Triple {
        Triple {
            t: [
                self.term_pattern(&tp.subject),
                self.named_pattern(&tp.predicate),
                self.term_pattern(&tp.object),
            ],
            graph: g.clone(),
        }
    }

    fn unpack(&self, input: Node, u: UnpackItem) -> Node {
        let parts = u.parts.map(|p| match p {
            PT::C(id) => PathEnd::Const(id),
            PT::V(v) => PathEnd::Var(v),
        });
        let desc = format!(
            "{} = <<( {} )>>",
            self.pt_str(&PT::V(u.t)),
            u.parts
                .iter()
                .map(|p| self.pt_str(p))
                .collect::<Vec<_>>()
                .join(" ")
        );
        let mut n = Node::unary(Kind::Unpack { t: u.t, parts }, input, desc);
        for p in u.parts {
            if let PT::V(v) = p
                && !n.vars.contains(&v)
            {
                n.vars.push(v);
                n.certain.push(v);
                n.dist.insert(v, n.est.max(1.0));
            }
        }
        n.est *= 0.9;
        n
    }

    /// `GRAPH ?g { P }` can bind ?g as a scan column when P is a plain join group that
    /// does not mention ?g itself.
    fn simple_graph_group(&self, name: &NamedNodePattern, inner: &GraphPattern) -> bool {
        let NamedNodePattern::Variable(v) = name else {
            return true;
        };
        fn plain(gp: &GraphPattern) -> bool {
            match gp {
                // ARQ's property functions read lists per graph
                GraphPattern::Bgp { patterns } => !super::arqpf::has_calls(patterns),
                GraphPattern::Path { .. } => true,
                GraphPattern::Join { left, right } => plain(left) && plain(right),
                GraphPattern::Filter { inner, expr } => plain(inner) && !has_exists(expr),
                _ => false,
            }
        }
        let mut names = Vec::new();
        collect_pattern_vars(inner, &mut names);
        plain(inner) && !names.iter().any(|n| n == v.as_str())
    }

    /// One row per named graph of the active dataset (bound to the graph variable), or
    /// the unit table if a constant graph exists.
    fn graph_names(&self, g: &ActiveGraph) -> Result<Node> {
        let accept = |gid: Id| match &self.ctx.dataset.named {
            Some(set) => set.contains(&gid),
            None => true,
        };
        let exists = |gid: Id| -> Result<bool> {
            Ok(accept(gid)
                && gid.tag() != Tag::Local
                && self.ctx.snap.count(Perm::Gspo, &[gid.0])? > 0)
        };
        Ok(match g {
            ActiveGraph::Var(v) => match self.subst.get(v) {
                Some(id) => {
                    if exists(*id)? {
                        Node::unit()
                    } else {
                        Node::empty(Vec::new())
                    }
                }
                None => {
                    let mut t = Table::new(vec![*v]);
                    for gid in self.ctx.snap.graph_ids()? {
                        if accept(gid) {
                            t.push_row(&[gid]);
                        }
                    }
                    let est = t.len() as f64;
                    let mut n = Node::leaf(
                        Kind::Values(t),
                        vec![*v],
                        est,
                        format!("{est} named graphs"),
                    );
                    n.cost = est;
                    n
                }
            },
            ActiveGraph::Named(id) if *id == Id::DEFAULT_GRAPH => Node::unit(),
            ActiveGraph::Named(id) => {
                if exists(*id)? {
                    Node::unit()
                } else {
                    Node::empty(Vec::new())
                }
            }
            _ => Node::unit(),
        })
    }

    // --------------------------------------------------------- graph filters ------

    pub(super) fn graph_filter(&self, g: &ActiveGraph) -> Option<(GraphFilter, Option<VarId>)> {
        let ds = &self.ctx.dataset;
        let set_of = |set: &[Id]| {
            let mut s: Vec<u64> = set.iter().map(|i| i.0).collect();
            s.sort_unstable();
            s.dedup();
            s
        };
        Some(match g {
            // an empty default graph (a dataset of named graphs only) matches nothing
            ActiveGraph::Default => match &ds.default {
                Some(set) if set.is_empty() => return None,
                Some(set) if set.len() == 1 => (GraphFilter::One(set[0].0), None),
                Some(set) => (GraphFilter::Set(set_of(set)), None),
                None if ds.union_default || self.ctx.snap.union_default_graph => {
                    (GraphFilter::Named, None)
                }
                None => (GraphFilter::Default, None),
            },
            // the union graph of a graph view is the union of the named graphs it sees
            ActiveGraph::Union => match (&self.ctx.graphs, &ds.named) {
                (Some(_), Some(set)) if set.is_empty() => return None,
                (Some(_), Some(set)) => (GraphFilter::Set(set_of(set)), None),
                _ => (GraphFilter::Named, None),
            },
            ActiveGraph::Named(id) => {
                if id.tag() == Tag::Local {
                    return None;
                }
                if let Some(named) = &ds.named
                    && !named.contains(id)
                {
                    return None;
                }
                (GraphFilter::One(id.0), None)
            }
            ActiveGraph::Var(v) => match self.subst.get(v) {
                Some(id) => return self.graph_filter(&ActiveGraph::Named(*id)),
                None => match &ds.named {
                    Some(set) if set.is_empty() => return None,
                    Some(set) => (GraphFilter::Set(set_of(set)), Some(*v)),
                    None => (GraphFilter::Named, Some(*v)),
                },
            },
        })
    }

    // ----------------------------------------------------------------- scans ------

    /// All useful scan plans for a triple pattern (one per sort order).
    fn scan_options(&self, t: &Triple) -> Result<Vec<Node>> {
        let vars_of = |t: &Triple| {
            let mut v: Vec<VarId> =
                t.t.iter()
                    .filter_map(|x| if let PT::V(v) = x { Some(*v) } else { None })
                    .collect();
            if let ActiveGraph::Var(g) = t.graph {
                v.push(g);
            }
            v.sort_unstable();
            v.dedup();
            v
        };
        let Some((gf, gvar)) = self.graph_filter(&t.graph) else {
            return Ok(vec![Node::empty(vars_of(t))]);
        };
        // unknown constants → empty
        if t.t
            .iter()
            .any(|x| matches!(x, PT::C(id) if id.tag() == Tag::Local))
        {
            return Ok(vec![Node::empty(vars_of(t))]);
        }
        // positions S=0,P=1,O=2,G=3 → PT
        let mut pos: [Option<PT>; 4] = [Some(t.t[0]), Some(t.t[1]), Some(t.t[2]), None];
        if let Some(gv) = gvar {
            pos[3] = Some(PT::V(gv));
        }
        let bound: Vec<usize> = (0..3)
            .filter(|&i| matches!(pos[i], Some(PT::C(_))))
            .collect();
        let mut out = Vec::new();
        let mut perms: Vec<Perm> = [
            Perm::Spo,
            Perm::Sop,
            Perm::Pso,
            Perm::Pos,
            Perm::Osp,
            Perm::Ops,
        ]
        .into_iter()
        .filter(|p| {
            let o = p.order();
            let mut first: Vec<usize> = o[..bound.len()].to_vec();
            first.sort_unstable();
            first == bound
        })
        .collect();
        // GSPO for a single constant graph with a compatible prefix
        let use_gspo = matches!(gf, GraphFilter::One(_))
            && (bound.is_empty() || bound == [S] || bound == [S, P] || bound == [S, P, O]);
        if use_gspo {
            perms.push(Perm::Gspo);
        }
        // de-duplicate perms that produce the same sort order for this pattern
        let mut seen_orders: FxHashSet<Vec<VarId>> = FxHashSet::default();
        let stats = &self.ctx.snap.generation.stats;
        let snap = &self.ctx.snap;
        for perm in perms {
            let order = perm.order();
            let (prefix, first_free): (Vec<u64>, usize) = if perm == Perm::Gspo {
                let GraphFilter::One(gid) = gf else {
                    unreachable!()
                };
                let mut p = vec![gid];
                for &c in &order[1..] {
                    match pos[c] {
                        Some(PT::C(id)) => p.push(id.0),
                        _ => break,
                    }
                }
                let n = p.len();
                (p, n)
            } else {
                (
                    bound
                        .iter()
                        .map(|_| 0)
                        .enumerate()
                        .map(|(i, _)| match pos[order[i]] {
                            Some(PT::C(id)) => id.0,
                            _ => unreachable!(),
                        })
                        .collect(),
                    bound.len(),
                )
            };
            let mut cols: Vec<(usize, VarId)> = Vec::new();
            let mut eqs = Vec::new();
            for (kc, &c) in order.iter().enumerate().skip(first_free) {
                if let Some(PT::V(v)) = pos[c] {
                    match cols.iter().find(|(_, x)| *x == v) {
                        Some((k0, _)) => eqs.push((*k0, kc)),
                        None => cols.push((kc, v)),
                    }
                }
            }
            let sort_vars: Vec<VarId> = cols.iter().map(|(_, v)| *v).collect();
            if !seen_orders.insert(sort_vars.iter().take(1).copied().collect()) {
                continue;
            }
            let graph_col = perm.col_of(G);
            let graph = if perm == Perm::Gspo
                || gvar.is_some() && !matches!(gf, GraphFilter::Set(_) | GraphFilter::Named)
            {
                GraphFilter::All
            } else {
                gf.clone()
            };
            let dedup = gvar.is_none() && gf.multi() && perm != Perm::Gspo;
            let mut est = if prefix.is_empty() {
                snap.len() as f64
            } else {
                snap.count(perm, &prefix)? as f64
            };
            if matches!(gf, GraphFilter::Default) && !stats.graphs.is_empty() && perm != Perm::Gspo
            {
                let total: u64 = stats.graphs.iter().map(|(_, n)| n).sum();
                let dflt = stats
                    .graphs
                    .iter()
                    .find(|(g, _)| *g == Id::DEFAULT_GRAPH.0)
                    .map_or(0, |(_, n)| *n);
                if total > 0 {
                    est *= (dflt as f64 / total as f64).max(0.01);
                }
            }
            if !eqs.is_empty() {
                est *= 0.1;
            }
            let est = est.max(if est > 0.0 { 1.0 } else { 0.0 });
            // distinct estimates
            let mut dist = FxHashMap::default();
            let pstat = match pos[P] {
                Some(PT::C(p)) => snap.predicate_stat(p.0),
                _ => None,
            };
            let quads = snap.len().max(1) as f64;
            // a pattern with one free subject, predicate or object column has a value of it
            // per triple
            let free_spo = cols.iter().filter(|(kc, _)| order[*kc] != G).count();
            for &(kc, v) in &cols {
                let comp = order[kc];
                let d = match (comp, pstat.as_ref()) {
                    (S | P | O, _) if free_spo == 1 => est,
                    (S, Some(ps)) if pstat.is_some() => {
                        ps.distinct_subjects as f64 * est / ps.count.max(1) as f64
                    }
                    (O, Some(ps)) => ps.distinct_objects as f64 * est / ps.count.max(1) as f64,
                    (S, None) => est * (stats.distinct_subjects as f64 / quads).min(1.0),
                    (O, None) => est * (stats.distinct_objects as f64 / quads).min(1.0),
                    (P, _) => (stats.distinct_predicates as f64).min(est),
                    _ => est,
                };
                // per-predicate statistics give distinct counts for both the subject and the
                // object column; without them only the first free column is estimated
                let d = if pstat.is_some() || kc == first_free {
                    d
                } else {
                    est
                };
                dist.insert(v, d.clamp(1.0, est.max(1.0)));
            }
            let desc = format!(
                "{} {}",
                perm.name().to_uppercase(),
                t.t.iter()
                    .map(|x| self.pt_str(x))
                    .collect::<Vec<_>>()
                    .join(" ")
            ) + &match &t.graph {
                ActiveGraph::Default => String::new(),
                g => format!(
                    " GRAPH {}",
                    match g {
                        ActiveGraph::Named(id) => self.pt_str(&PT::C(*id)),
                        ActiveGraph::Var(v) => self.pt_str(&PT::V(*v)),
                        _ => String::new(),
                    }
                ),
            };
            let vars: Vec<VarId> = cols.iter().map(|(_, v)| *v).collect();
            let mut n = Node::leaf(
                Kind::Scan(ScanSpec {
                    perm,
                    prefix,
                    cols,
                    eqs,
                    graph,
                    graph_col,
                    dedup,
                }),
                vars,
                est,
                desc,
            );
            n.sorted = sort_vars;
            n.dist = dist;
            if est == 0.0 {
                return Ok(vec![Node::empty(vars_of(t))]);
            }
            out.push(n);
        }
        if out.is_empty() {
            return Ok(vec![Node::empty(vars_of(t))]);
        }
        Ok(out)
    }

    pub(super) fn pt_str(&self, x: &PT) -> String {
        match x {
            PT::V(v) => {
                let n = self.ctx.var_name(*v);
                if n.starts_with(' ') {
                    "?_".into()
                } else {
                    format!("?{n}")
                }
            }
            PT::C(id) => self
                .ctx
                .term(*id)
                .map_or_else(|| format!("{id:?}"), |t| short(&t)),
        }
    }

    // ------------------------------------------------------------- join groups ------

    fn plan_group(&mut self, items: Vec<Item>, mut filters: Vec<Expr>) -> Result<Node> {
        let mut triples = Vec::new();
        let mut paths: Vec<PathItem> = Vec::new();
        let mut nodes = Vec::new();
        let mut unpacks: Vec<UnpackItem> = Vec::new();
        for it in items {
            match it {
                Item::Unpack(u) => unpacks.push(u),
                Item::Triple(t) => triples.push(t),
                Item::Path(p) => paths.push(p),
                Item::Node(n) => nodes.push(n),
            }
        }

        // Filter-equality substitution (Jena TransformFilterEquality): FILTER(?x = <iri>)
        // with ?x only used in triple patterns → substitute the constant.
        let mut binds: Vec<(VarId, Id)> = Vec::new();
        let mut i = 0;
        while i < filters.len() {
            if let Some((v, c)) = equality_constant(&filters[i], self.ctx)
                && triples.iter().any(|t| t.t.contains(&PT::V(v)))
                && !nodes.iter().any(|n| n.vars.contains(&v))
                && !paths.iter().any(|p| p.s == PT::V(v) || p.o == PT::V(v))
                && !unpacks
                    .iter()
                    .any(|u| u.t == v || u.parts.contains(&PT::V(v)))
                && !triples
                    .iter()
                    .any(|t| matches!(t.graph, ActiveGraph::Var(g) if g == v))
            {
                filters.remove(i);
                for t in &mut triples {
                    for x in &mut t.t {
                        if *x == PT::V(v) {
                            *x = PT::C(c);
                        }
                    }
                }
                for f in &mut filters {
                    substitute(f, v, c);
                }
                binds.push((v, c));
                continue;
            }
            i += 1;
        }

        let mut leaves: Vec<Vec<Node>> = Vec::new();
        for t in &triples {
            leaves.push(self.scan_options(t)?);
        }
        if self.ctx.opt.sampled_filters && !filters.is_empty() {
            super::sample::prepare(self.ctx, &leaves, &filters);
        }
        if self.ctx.opt.characteristic_sets {
            let stars: Vec<(VarId, u64, Option<u64>)> = triples
                .iter()
                .filter_map(|t| match (t.t[0], t.t[1], t.t[2]) {
                    (PT::V(v), PT::C(p), PT::V(_)) => Some((v, p.0, None)),
                    (PT::V(v), PT::C(p), PT::C(o)) => Some((v, p.0, Some(o.0))),
                    _ => None,
                })
                .collect();
            super::charsets::register(self.ctx, &stars);
        }
        // searches that read the rest of the group are attached to it at the end
        let (dependent, nodes): (Vec<Node>, Vec<Node>) =
            nodes.into_iter().partition(|n| match &n.kind {
                Kind::VectorSearch(s) => s.needs_input(),
                Kind::TextSearch(s) => s.needs_input(),
                Kind::HybridSearch(s) => s.needs_input(),
                Kind::SpatialPf(s) => s.needs_input(),
                Kind::PathSearch(s) => s.needs_input(),
                Kind::PropertyFn(s) => s.needs_input(),
                Kind::HistoryChanges(s) => s.needs_input(),
                _ => false,
            });
        for n in nodes {
            leaves.push(vec![n]);
        }
        if self.ctx.opt.probed_keys && leaves.len() > 1 {
            super::keyprobe::prepare(self.ctx, &leaves);
        }
        let mut result = if leaves.is_empty() {
            Node::unit()
        } else if leaves.iter().any(|l| l[0].is_empty()) {
            let mut vars: Vec<VarId> = leaves.iter().flat_map(|l| l[0].vars.clone()).collect();
            vars.sort_unstable();
            vars.dedup();
            Node::empty(vars)
        } else {
            self.join_order(leaves, &mut filters)?
        };

        // attach property paths, preferring those whose endpoint is already bound
        while !paths.is_empty() {
            let idx = paths
                .iter()
                .position(|p| {
                    [p.s, p.o].iter().any(|e| {
                        matches!(e, PT::C(_)) || matches!(e, PT::V(v) if result.vars.contains(v))
                    })
                })
                .unwrap_or(0);
            let p = paths.remove(idx);
            result = self.attach_path(result, p)?;
            let (now, later): (Vec<Expr>, Vec<Expr>) =
                std::mem::take(&mut filters).into_iter().partition(|f| {
                    !f.has_exists() && f.var_set().iter().all(|v| result.vars.contains(v))
                });
            filters = later;
            if !now.is_empty() {
                result = filter(result, now, self.ctx);
            }
        }
        // decompose triple terms once their variable is bound (outer before nested)
        while let Some(i) = unpacks.iter().position(|u| result.vars.contains(&u.t)) {
            let u = unpacks.remove(i);
            result = self.unpack(result, u);
            let (now, later): (Vec<Expr>, Vec<Expr>) =
                std::mem::take(&mut filters).into_iter().partition(|f| {
                    !f.has_exists() && f.var_set().iter().all(|v| result.vars.contains(v))
                });
            filters = later;
            if !now.is_empty() {
                result = filter(result, now, self.ctx);
            }
        }
        for d in dependent {
            result = match &d.kind {
                Kind::SpatialPf(_) => super::geopf::attach_spatial(self, result, d)?,
                Kind::PathSearch(_) => super::pathsearch::attach(self, result, d)?,
                Kind::HistoryChanges(_) => super::history_svc::attach(self, result, d)?,
                Kind::PropertyFn(_) => super::arqpf::attach(result, d),
                Kind::TextSearch(_) => self.attach_text(result, d)?,
                Kind::HybridSearch(_) => super::hybrid::attach(self, result, d)?,
                _ => self.attach_vector(result, d)?,
            };
            let (now, later): (Vec<Expr>, Vec<Expr>) =
                std::mem::take(&mut filters).into_iter().partition(|f| {
                    !f.has_exists() && f.var_set().iter().all(|v| result.vars.contains(v))
                });
            filters = later;
            if !now.is_empty() {
                result = filter(result, now, self.ctx);
            }
        }
        if !filters.is_empty() {
            result = filter(result, filters, self.ctx);
        }
        for (v, c) in binds {
            let mut n = Node::unary(
                Kind::Extend(v, Expr::Const(c)),
                result,
                format!("?{} := {}", self.ctx.var_name(v), self.pt_str(&PT::C(c))),
            );
            n.vars.push(v);
            n.certain.push(v);
            n.dist.insert(v, 1.0);
            result = n;
        }
        Ok(result)
    }

    /// DP / greedy join ordering over connected components (QLever QueryPlanner).
    fn join_order(&self, leaves: Vec<Vec<Node>>, filters: &mut Vec<Expr>) -> Result<Node> {
        let n = leaves.len();
        // connected components by shared variables
        let mut comp: Vec<usize> = (0..n).collect();
        fn find(c: &mut [usize], x: usize) -> usize {
            if c[x] != x {
                let r = find(c, c[x]);
                c[x] = r;
            }
            c[x]
        }
        for i in 0..n {
            for j in i + 1..n {
                if leaves[i][0]
                    .vars
                    .iter()
                    .any(|v| leaves[j][0].vars.contains(v))
                {
                    let (a, b) = (find(&mut comp, i), find(&mut comp, j));
                    comp[a] = b;
                }
            }
        }
        let mut groups: FxHashMap<usize, Vec<usize>> = FxHashMap::default();
        for i in 0..n {
            let r = find(&mut comp, i);
            groups.entry(r).or_default().push(i);
        }
        let mut leaves: Vec<Option<Vec<Node>>> = leaves.into_iter().map(Some).collect();
        let mut parts: Vec<Node> = Vec::new();
        let mut group_list: Vec<Vec<usize>> = groups.into_values().collect();
        group_list.sort();
        for members in group_list {
            let items: Vec<Vec<Node>> =
                members.iter().map(|&i| leaves[i].take().unwrap()).collect();
            let plan = if items.len() == 1 {
                // filters can change which access path is cheapest (range pushdown)
                let opts = items.into_iter().next().unwrap();
                let (best, rest) = opts
                    .into_iter()
                    .map(|o| {
                        let mut f = filters.clone();
                        (self.place_filters(o, &mut f), f)
                    })
                    .min_by(|a, b| a.0.cost.total_cmp(&b.0.cost))
                    .unwrap();
                *filters = rest;
                best
            } else if self.ctx.opt.pruned_join_order {
                match super::joinorder::order(self, items, filters)? {
                    Ok(plan) => plan,
                    Err(items) => self.dp(items, filters)?,
                }
            } else if items.len() <= DP_LIMIT {
                self.dp(items, filters)?
            } else {
                self.greedy(items, filters)?
            };
            parts.push(plan);
        }
        // components connected by a spatial conjunct are joined on it
        super::geojoin::spatial_joins(&mut parts, filters, self.ctx);
        // cross products between components, smallest first
        parts.sort_by(|a, b| a.est.total_cmp(&b.est));
        let mut it = parts.into_iter();
        let mut acc = it.next().unwrap();
        for p in it {
            acc = self.place_filters(join(acc, p, self.ctx), filters);
        }
        Ok(super::indexjoin::fuse_stars(acc, self.ctx))
    }

    /// Apply (and remove) filters whose variables are all bound by `n`.
    pub(super) fn place_filters(&self, n: Node, filters: &mut Vec<Expr>) -> Node {
        let (now, later): (Vec<Expr>, Vec<Expr>) =
            std::mem::take(filters).into_iter().partition(|f| {
                !f.has_exists() && {
                    let vs = f.var_set();
                    !vs.is_empty() && vs.iter().all(|v| n.vars.contains(v))
                }
            });
        *filters = later;
        if now.is_empty() {
            n
        } else {
            filter(n, now, self.ctx)
        }
    }

    fn dp(&self, items: Vec<Vec<Node>>, filters: &mut Vec<Expr>) -> Result<Node> {
        let n = items.len();
        let filter_vars: Vec<Vec<VarId>> = filters
            .iter()
            .map(|f| {
                if f.has_exists() {
                    vec![VarId::MAX]
                } else {
                    f.var_set()
                }
            })
            .collect();
        // table: mask → candidates (one per first sort var)
        let mut table: FxHashMap<u32, Vec<(Node, u64)>> = FxHashMap::default();
        for (i, opts) in items.into_iter().enumerate() {
            let mut v = Vec::new();
            for o in opts {
                let (o, fm) = self.apply_dp_filters(o, 0, filters, &filter_vars);
                v.push((o, fm));
            }
            table.insert(1 << i, v);
        }
        for size in 2..=n {
            let masks: Vec<u32> = (1u32..(1 << n))
                .filter(|m| m.count_ones() as usize == size)
                .collect();
            for mask in masks {
                self.ctx.check()?;
                let mut cands: Vec<(Node, u64)> = Vec::new();
                // enumerate splits (sub, mask^sub) with sub < rest to avoid duplicates
                let mut sub = (mask - 1) & mask;
                while sub > 0 {
                    let rest = mask ^ sub;
                    if sub < rest
                        && let (Some(l), Some(r)) = (table.get(&sub), table.get(&rest))
                    {
                        for (a, fa) in l {
                            for (b, fb) in r {
                                if !a.vars.iter().any(|v| b.vars.contains(v)) {
                                    continue;
                                }
                                for c in join_candidates(a, b, self.ctx) {
                                    let (c, fm) =
                                        self.apply_dp_filters(c, fa | fb, filters, &filter_vars);
                                    cands.push((c, fm));
                                }
                            }
                        }
                    }
                    sub = (sub - 1) & mask;
                }
                if cands.is_empty() {
                    continue;
                }
                // prune: best per first sort var
                let mut best: FxHashMap<Option<VarId>, (Node, u64)> = FxHashMap::default();
                for (c, fm) in cands {
                    let key = c.sorted.first().copied();
                    match best.get(&key) {
                        Some((b, _)) if b.cost <= c.cost => {}
                        _ => {
                            best.insert(key, (c, fm));
                        }
                    }
                }
                // in order of the sort variable, so that ties break the same way in every
                // run and in the join ordering on cost summaries
                let mut best: Vec<(Option<VarId>, (Node, u64))> = best.into_iter().collect();
                best.sort_unstable_by_key(|(k, _)| *k);
                table.insert(mask, best.into_iter().map(|(_, c)| c).collect());
            }
        }
        let full = (1u32 << n) - 1;
        let cands = table
            .remove(&full)
            .ok_or_else(|| Error::invalid("join planning failed"))?;
        let (best, fm) = cands
            .into_iter()
            .min_by(|a, b| a.0.cost.total_cmp(&b.0.cost))
            .unwrap();
        // remove applied filters; the mask covers only the first 64, which are the only
        // ones apply_dp_filters places, so any later filter stays for the caller
        let mut i = 0;
        filters.retain(|_| {
            let keep = i >= 64 || fm & (1u64 << i) == 0;
            i += 1;
            keep
        });
        Ok(best)
    }

    pub(super) fn apply_dp_filters(
        &self,
        n: Node,
        applied: u64,
        filters: &[Expr],
        fvars: &[Vec<VarId>],
    ) -> (Node, u64) {
        let mut now = Vec::new();
        let mut mask = applied;
        for (i, f) in filters.iter().enumerate().take(64) {
            if mask & (1 << i) == 0
                && !fvars[i].is_empty()
                && fvars[i].iter().all(|v| n.vars.contains(v))
            {
                now.push(f.clone());
                mask |= 1 << i;
            }
        }
        if now.is_empty() {
            (n, mask)
        } else {
            (filter(n, now, self.ctx), mask)
        }
    }

    fn greedy(&self, items: Vec<Vec<Node>>, filters: &mut Vec<Expr>) -> Result<Node> {
        let mut plans: Vec<Node> = items
            .into_iter()
            .map(|mut o| {
                o.sort_by(|a, b| a.cost.total_cmp(&b.cost));
                o.swap_remove(0)
            })
            .map(|p| self.place_filters(p, filters))
            .collect();
        while plans.len() > 1 {
            self.ctx.check()?;
            let mut best: Option<(usize, usize, Node)> = None;
            for i in 0..plans.len() {
                for j in i + 1..plans.len() {
                    if !plans[i].vars.iter().any(|v| plans[j].vars.contains(v)) {
                        continue;
                    }
                    for c in join_candidates(&plans[i], &plans[j], self.ctx) {
                        if best.as_ref().is_none_or(|(_, _, b)| {
                            c.est < b.est || (c.est == b.est && c.cost < b.cost)
                        }) {
                            best = Some((i, j, c));
                        }
                    }
                }
            }
            let Some((i, j, c)) = best else { break };
            plans.remove(j);
            plans.remove(i);
            plans.push(self.place_filters(c, filters));
        }
        let mut it = plans.into_iter();
        let mut acc = it.next().unwrap();
        for p in it {
            acc = self.place_filters(join(acc, p, self.ctx), filters);
        }
        Ok(acc)
    }

    // --------------------------------------------------------------- paths ------

    fn attach_path(&mut self, left: Node, p: PathItem) -> Result<Node> {
        use PropertyPathExpression as PP;
        let mut count = None;
        let (min, max_one, inner) = match &p.path {
            PP::ZeroOrMore(i) => (0u8, false, &**i),
            PP::OneOrMore(i) => (1, false, &**i),
            PP::ZeroOrOne(i) => (0, true, &**i),
            PP::Range { path, min, max } => {
                count = Some((*min, *max));
                (u8::from(*min > 0), false, &**path)
            }
            PP::Alternative(..) => {
                let mut alts = Vec::new();
                flatten_alt(&p.path, &mut alts);
                let mut branches = Vec::new();
                for a in alts {
                    let mut items = Vec::new();
                    self.collect_path(p.s, a, p.o, &p.graph, &mut items)?;
                    branches.push(self.plan_group(items, Vec::new())?);
                }
                return Ok(join(left, union(branches), self.ctx));
            }
            PP::NegatedPropertySet(set) => {
                // ?s ?p ?o FILTER(?p NOT IN set) (forward only; spargebra splits inverses)
                let pv = self.ctx.fresh_var();
                let t = Triple {
                    t: [p.s, PT::V(pv), p.o],
                    graph: p.graph.clone(),
                };
                let mut opts = self.scan_options(&t)?;
                opts.sort_by(|a, b| a.cost.total_cmp(&b.cost));
                let scan = opts.swap_remove(0);
                let list: Vec<Expr> = set
                    .iter()
                    .map(|n| Expr::Const(self.ctx.intern_term(&Term::NamedNode(n.clone()))))
                    .collect();
                let f = Expr::Not(Box::new(Expr::In(Box::new(Expr::Var(pv)), list)));
                let mut n = filter(scan, vec![f], self.ctx);
                n = project(
                    n.clone(),
                    n.vars.iter().copied().filter(|v| *v != pv).collect(),
                    self.ctx,
                );
                return Ok(join(left, n, self.ctx));
            }
            _ => {
                let mut items = Vec::new();
                self.collect_path(p.s, &p.path, p.o, &p.graph, &mut items)?;
                let n = self.plan_group(items, Vec::new())?;
                return Ok(join(left, n, self.ctx));
            }
        };
        let Some((gf, gvar)) = self.graph_filter(&p.graph) else {
            return Ok(Node::empty(left.vars.clone()));
        };
        let to_end = |x: PT| match x {
            PT::C(id) => PathEnd::Const(id),
            PT::V(v) => PathEnd::Var(v),
        };
        let simple = match inner {
            PP::NamedNode(n) => Some((self.ctx.intern_term(&Term::NamedNode(n.clone())), false)),
            PP::Reverse(r) => match &**r {
                PP::NamedNode(n) => Some((self.ctx.intern_term(&Term::NamedNode(n.clone())), true)),
                _ => None,
            },
            _ => None,
        };
        let mut children = Vec::new();
        let mut edge_vars = None;
        let simple = match simple {
            Some((id, _)) if id.tag() == Tag::Local && min == 1 => {
                return Ok(Node::empty(left.vars.clone()));
            }
            // a predicate that is not in the store: only the zero-length path matches
            Some((id, _)) if id.tag() == Tag::Local && count.is_some() => {
                count = Some((0, Some(0)));
                Some((0, false))
            }
            Some((id, rev)) if id.tag() != Tag::Local => Some((id.0, rev)),
            _ => {
                let (a, b) = (self.ctx.fresh_var(), self.ctx.fresh_var());
                let mut items = Vec::new();
                self.collect_path(PT::V(a), inner, PT::V(b), &p.graph, &mut items)?;
                let edges = self.plan_group(items, Vec::new())?;
                let mut keep = vec![a, b];
                if let Some(g) = gvar {
                    keep.push(g);
                }
                children.push(project(edges, keep, self.ctx));
                edge_vars = Some((a, b));
                None
            }
        };
        let spec = PathSpec {
            subj: to_end(p.s),
            obj: to_end(p.o),
            min,
            max_one,
            simple,
            graph: gf,
            graph_var: gvar,
            edge_vars,
            count,
        };
        let mut vars = Vec::new();
        for e in [&spec.subj, &spec.obj] {
            if let PathEnd::Var(v) = e
                && !vars.contains(v)
            {
                vars.push(*v);
            }
        }
        if let Some(g) = gvar {
            vars.push(g);
        }
        let bound_var = [&spec.subj, &spec.obj].into_iter().find_map(|e| match e {
            PathEnd::Var(v) if left.vars.contains(v) && left.certain.contains(v) => Some(*v),
            _ => None,
        });
        let desc = format!("{} {} {}", self.pt_str(&p.s), p.path, self.pt_str(&p.o));
        let fanout = 10.0;
        if let Some(bv) = bound_var
            && !left.vars.is_empty()
            && gvar.is_none()
        {
            // bound-side traversal from the left input (QLever TransitivePath with bound side)
            let est = left.est * fanout;
            let mut out_vars = left.vars.clone();
            for v in &vars {
                if !out_vars.contains(v) {
                    out_vars.push(*v);
                }
            }
            let mut certain = left.certain.clone();
            certain.extend(vars.iter().copied());
            let dist = out_vars.iter().map(|&v| (v, est)).collect();
            let cost = left.cost + est;
            children.push(left);
            let _ = bv;
            return Ok(Node {
                kind: Kind::Path {
                    spec: Box::new(spec),
                    bound_from_left: true,
                },
                children,
                vars: out_vars,
                certain,
                sorted: Vec::new(),
                est,
                cost,
                dist,
                desc,
            });
        }
        let est = match (&spec.subj, &spec.obj) {
            (PathEnd::Const(_), PathEnd::Const(_)) => 1.0,
            (PathEnd::Const(_), _) | (_, PathEnd::Const(_)) => 100.0,
            _ => (self.ctx.snap.len() as f64).max(1.0),
        };
        let mut n = Node::leaf(
            Kind::Path {
                spec: Box::new(spec),
                bound_from_left: false,
            },
            vars,
            est,
            desc,
        );
        n.children = children;
        n.cost = est * 2.0 + n.children.iter().map(|c| c.cost).sum::<f64>();
        if left.vars.is_empty() && matches!(left.kind, Kind::Values(_)) {
            return Ok(n);
        }
        Ok(join(left, n, self.ctx))
    }
}

// ------------------------------------------------------------------------------
// node constructors (with estimates)
// ------------------------------------------------------------------------------

/// How a join on one variable was estimated, when not from the distinct values on
/// either side.
#[derive(Clone, Copy, Debug)]
pub(super) enum JoinModel {
    /// from characteristic sets: the shares of the product of the inputs' rows and of the
    /// product of their distinct values that the join keeps (see [`super::charsets`])
    Star { rr: f64, rd: f64 },
    /// from probing the values of a small input; `a_src` tells whether the first input
    /// holds it (see [`super::keyprobe`])
    Probe {
        m: super::keyprobe::Measure,
        a_src: bool,
    },
}

impl JoinModel {
    /// The estimated rows of the join of inputs of `a_est` and `b_est` rows.
    pub(super) fn est(&self, a_est: f64, b_est: f64) -> f64 {
        match self {
            JoinModel::Star { rr, .. } => star_est(a_est, b_est, *rr),
            JoinModel::Probe { m, a_src: true } => super::keyprobe::est(a_est, b_est, m),
            JoinModel::Probe { m, a_src: false } => super::keyprobe::est(b_est, a_est, m),
        }
    }

    /// The distinct values of the join variable after the join, from the inputs' `da`
    /// and `db`.
    pub(super) fn distinct(&self, da: f64, db: f64) -> f64 {
        match self {
            JoinModel::Star { rd, .. } => super::charsets::distinct(da, db, *rd),
            JoinModel::Probe { m, a_src: true } => super::keyprobe::distinct(da, db, m),
            JoinModel::Probe { m, a_src: false } => super::keyprobe::distinct(db, da, m),
        }
    }
}

/// The distinct-value estimates of the join of `a` and `b` into `est` rows; `model` is
/// the join variable with how the join was estimated, when not from distinct values.
pub(super) fn merge_dist(
    a: &Node,
    b: &Node,
    est: f64,
    model: Option<(VarId, JoinModel)>,
) -> FxHashMap<VarId, f64> {
    let mut d = FxHashMap::default();
    for v in a.vars.iter().chain(b.vars.iter()) {
        let x = match (a.vars.contains(v), b.vars.contains(v)) {
            (true, true) => match model {
                Some((s, m)) if s == *v => m.distinct(a.d(*v), b.d(*v)),
                _ => a.d(*v).min(b.d(*v)),
            },
            (true, false) => a.d(*v),
            _ => b.d(*v),
        };
        d.insert(*v, x.min(est).max(1.0));
    }
    d
}

pub(super) fn join_est(a: &Node, b: &Node, keys: &[VarId]) -> f64 {
    if keys.is_empty() {
        return a.est * b.est;
    }
    let denom = keys
        .iter()
        .map(|v| a.d(*v).max(b.d(*v)))
        .fold(1.0f64, f64::max);
    join_est_from(a.est, b.est, denom)
}

/// The estimated rows of joining `a` and `b` on `keys`, and, when the join is on one
/// variable and estimated from characteristic sets or probed values, the variable with
/// how it was estimated. The sets come first, since they hold how the predicates of a
/// star occur together where probes see only the small input and one pattern, unless the
/// small input is a subset of values the sets know nothing of (see
/// [`super::keyprobe::first`]).
pub(super) fn join_est_with(
    a: &Node,
    b: &Node,
    keys: &[VarId],
    ctx: &Ctx,
) -> (f64, Option<(VarId, JoinModel)>) {
    let [v] = keys else {
        return (join_est(a, b, keys), None);
    };
    let probe = || {
        if !ctx.opt.probed_keys {
            return None;
        }
        let (sa, sb) = (
            super::keyprobe::side(ctx, a, *v)?,
            super::keyprobe::side(ctx, b, *v)?,
        );
        super::keyprobe::applies(ctx, *v, sa, sb).map(|(m, a_src)| JoinModel::Probe { m, a_src })
    };
    let star = || {
        if !ctx.opt.characteristic_sets {
            return None;
        }
        let (mut qa, mut qb) = (Vec::new(), Vec::new());
        super::charsets::of_node(ctx, a, &mut qa);
        super::charsets::of_node(ctx, b, &mut qb);
        let (qa, qb) = (
            super::charsets::mask_of(&qa, *v),
            super::charsets::mask_of(&qb, *v),
        );
        super::charsets::factor(ctx, *v, qa, qb).map(|(rr, rd)| JoinModel::Star { rr, rd })
    };
    let model = if ctx.opt.probed_keys && super::keyprobe::first(ctx, *v) {
        probe().or_else(star)
    } else {
        star().or_else(probe)
    };
    match model {
        Some(m) => (m.est(a.est, b.est), Some((*v, m))),
        None => (join_est(a, b, keys), None),
    }
}

/// The estimated rows of joining `a_est` and `b_est` rows that keeps the share `rr` of
/// their product (see [`super::charsets`]).
pub(super) fn star_est(a_est: f64, b_est: f64, rr: f64) -> f64 {
    (a_est * b_est * rr).max(if a_est > 0.0 && b_est > 0.0 { 1.0 } else { 0.0 })
}

/// The estimated rows of joining `a_est` and `b_est` rows where the join variables take
/// at most `denom` distinct values on either side.
pub(super) fn join_est_from(a_est: f64, b_est: f64, denom: f64) -> f64 {
    // QLever correction factor
    (a_est * b_est / denom * 0.7).max(if a_est > 0.0 && b_est > 0.0 { 1.0 } else { 0.0 })
}

/// A hash join builds a table of the smaller input, keyed on the join variables, and
/// probes it with each row of the larger one. With a list allocated per key, inserting a
/// row takes 40 to 100 ns, about as long as scanning 48 rows.
const HASH_BUILD_COST: f64 = 48.0;
/// Inserting a row into the flat table takes 10 to 15 ns on one thread while the table
/// fits in the CPU caches, and 3 to 7 ns for a million rows built in partitions in
/// parallel, where a scan reads a row in 2.2 ns and the lists took 18 to 62 ns (measured
/// on generated tables by `costcal_tests::cal_tables`). A probe takes about 1.2 ns, in
/// parallel pieces of the larger input, and is counted as one row.
const FLAT_HASH_BUILD_COST: f64 = 8.0;

/// A merge join's cost per input row, besides its output rows. Timed alone on generated
/// tables (`costcal_tests::cal_tables`), a merge join takes 0.6 to 8 ns per input row
/// where a scan reads a row in 0.8 to 2.2 ns: little for rows it skips, several times
/// more for rows that match, most of it building the output. Hash joins of the same
/// inputs are underpriced as much (at 100k rows a side both took 3 to 5 times their
/// cost), so raising this alone turns merges into hash joins that run slower: across
/// the bench queries at 1.05M and 10.5M triples and WatDiv, run alternately pinned to
/// the performance cores, 2 made the plans 8% slower on geometric mean (32 queries more
/// than 10% slower than their fastest plan, against 17 with 1), 1.5 2%, and 0.75 was
/// within noise of 1.
const MERGE_ROW_COST: f64 = 1.0;

/// [`MERGE_ROW_COST`], which the calibration tests can override with
/// `SPARKLES_CAL_MERGE_ROW_COST`.
#[inline]
pub(super) fn merge_row_cost() -> f64 {
    #[cfg(test)]
    {
        static COST: std::sync::OnceLock<f64> = std::sync::OnceLock::new();
        *COST.get_or_init(|| {
            std::env::var("SPARKLES_CAL_MERGE_ROW_COST")
                .ok()
                .and_then(|s| s.parse().ok())
                .unwrap_or(MERGE_ROW_COST)
        })
    }
    #[cfg(not(test))]
    MERGE_ROW_COST
}

/// What the join costs depend on besides the inputs: whether index joins are offered and
/// what inserting a row into a hash table costs.
#[derive(Clone, Copy, Debug)]
pub(super) struct Costing {
    pub index_joins: bool,
    pub hash_build: f64,
}

impl Costing {
    pub fn of(ctx: &Ctx) -> Costing {
        Costing {
            index_joins: ctx.opt.batched_join,
            hash_build: if ctx.opt.flat_hash_join {
                FLAT_HASH_BUILD_COST
            } else {
                HASH_BUILD_COST
            },
        }
    }
}

/// The cost of hash joining inputs of `a_est` and `b_est` rows, at `build` per row of the
/// smaller one, besides the inputs and the output rows.
pub(super) fn hash_base(build: f64, a_est: f64, b_est: f64) -> f64 {
    build * a_est.min(b_est) + a_est.max(b_est)
}

pub(super) fn sort_cost(n: f64) -> f64 {
    n * n.max(2.0).log2() * 0.25
}

fn mk_join(a: Node, b: Node, algo: JoinAlgo, keys: Vec<VarId>, ctx: &Ctx) -> Node {
    let (est, star) = join_est_with(&a, &b, &keys, ctx);
    let mut vars = a.vars.clone();
    for v in &b.vars {
        if !vars.contains(v) {
            vars.push(*v);
        }
    }
    let mut certain = a.certain.clone();
    for v in &b.certain {
        if !certain.contains(v) {
            certain.push(*v);
        }
    }
    let sorted = match algo {
        JoinAlgo::Merge => vec![keys[0]],
        JoinAlgo::Hash => {
            // probe side (larger) order is preserved
            if a.est >= b.est {
                a.sorted.clone()
            } else {
                b.sorted.clone()
            }
        }
        JoinAlgo::Cross => a.sorted.clone(),
    };
    let base = match algo {
        JoinAlgo::Merge => (a.est + b.est) * merge_row_cost(),
        JoinAlgo::Hash => hash_base(Costing::of(ctx).hash_build, a.est, b.est),
        JoinAlgo::Cross => a.est * b.est,
    };
    let cost = a.cost + b.cost + base + est;
    let dist = merge_dist(&a, &b, est, star);
    let desc = format!(
        "on {}",
        if keys.is_empty() {
            "()".to_string()
        } else {
            format!("{} var(s)", keys.len())
        }
    );
    Node {
        kind: Kind::Join { algo, keys },
        children: vec![a, b],
        vars,
        certain,
        sorted,
        est,
        cost,
        dist,
        desc,
    }
}

fn sort_node(n: Node, v: VarId) -> Node {
    if n.sorted.first() == Some(&v) {
        return n;
    }
    let c = sort_cost(n.est);
    let mut s = Node::unary(Kind::Sort(vec![v]), n, String::new());
    s.sorted = vec![v];
    s.cost += c;
    s
}

/// Join alternatives for the DP (merge join on each shared certain var, hash join).
fn join_candidates(a: &Node, b: &Node, ctx: &Ctx) -> Vec<Node> {
    let keys: Vec<VarId> = a
        .vars
        .iter()
        .filter(|v| b.vars.contains(v))
        .copied()
        .collect();
    let mut out = Vec::new();
    let certain_both: Vec<VarId> = keys
        .iter()
        .filter(|v| a.certain.contains(v) && b.certain.contains(v))
        .copied()
        .collect();
    for &v in &certain_both {
        let (sa, sb) = (a.sorted.first() == Some(&v), b.sorted.first() == Some(&v));
        if merge_offered(a.est, sa, b.est, sb) {
            out.push(merge_join(a.clone(), b.clone(), v, &keys, ctx));
        }
    }
    out.push(hash_join(a.clone(), b.clone(), keys, ctx));
    out.extend(super::indexjoin::candidates(a, b, ctx));
    out
}

/// Whether a merge join is worth offering for inputs of `a_est` and `b_est` rows, which
/// are sorted on the key or not.
pub(super) fn merge_offered(a_est: f64, a_sorted: bool, b_est: f64, b_sorted: bool) -> bool {
    let extra = if a_sorted { 0.0 } else { sort_cost(a_est) }
        + if b_sorted { 0.0 } else { sort_cost(b_est) };
    merge_worth(extra, a_est, b_est, a_sorted && b_sorted)
}

/// Whether a merge join is worth offering when sorting its inputs costs `extra`.
pub(super) fn merge_worth(extra: f64, a_est: f64, b_est: f64, both_sorted: bool) -> bool {
    both_sorted || extra < (a_est + b_est) * 4.0
}

/// The merge join of `a` and `b` on `v`, sorting either input that is not sorted on it;
/// `keys` are all the variables they share.
pub(super) fn merge_join(a: Node, b: Node, v: VarId, keys: &[VarId], ctx: &Ctx) -> Node {
    let (x, y) = (sort_node(a, v), sort_node(b, v));
    let mut k = vec![v];
    k.extend(keys.iter().filter(|x| **x != v));
    let mut j = mk_join(x, y, JoinAlgo::Merge, k, ctx);
    j.desc = format!("on ?{}", ctx.var_name(v));
    j
}

/// The hash join of `a` and `b` on the variables they share (`keys`).
pub(super) fn hash_join(a: Node, b: Node, keys: Vec<VarId>, ctx: &Ctx) -> Node {
    let desc = format!(
        "on {}",
        keys.iter()
            .map(|v| format!("?{}", ctx.var_name(*v)))
            .collect::<Vec<_>>()
            .join(" ")
    );
    let mut h = mk_join(a, b, JoinAlgo::Hash, keys, ctx);
    h.desc = desc;
    h
}

/// Best single join of two plans (used outside the DP).
pub fn join(a: Node, b: Node, ctx: &Ctx) -> Node {
    if a.is_empty() || b.is_empty() {
        let mut vars = a.vars.clone();
        vars.extend(b.vars.iter().filter(|v| !a.vars.contains(v)));
        return Node::empty(vars);
    }
    if a.vars.is_empty() && matches!(&a.kind, Kind::Values(t) if t.len() == 1) {
        return b;
    }
    if b.vars.is_empty() && matches!(&b.kind, Kind::Values(t) if t.len() == 1) {
        return a;
    }
    if !a.vars.iter().any(|v| b.vars.contains(v)) {
        let mut j = mk_join(a, b, JoinAlgo::Cross, Vec::new(), ctx);
        j.desc = "cross product".into();
        return j;
    }
    join_candidates(&a, &b, ctx)
        .into_iter()
        .min_by(|x, y| x.cost.total_cmp(&y.cost))
        .unwrap()
}

fn left_join(l: Node, r: Node, expr: Option<Expr>, ctx: &Ctx) -> Node {
    if l.is_empty() {
        return l;
    }
    let keys: Vec<VarId> = l
        .vars
        .iter()
        .filter(|v| r.vars.contains(v))
        .copied()
        .collect();
    let est = join_est(&l, &r, &keys).max(l.est);
    let mut vars = l.vars.clone();
    vars.extend(r.vars.iter().filter(|v| !l.vars.contains(v)));
    let desc = match &expr {
        Some(e) => format!("filter {}", e.display(ctx)),
        None => format!(
            "on {}",
            keys.iter()
                .map(|v| format!("?{}", ctx.var_name(*v)))
                .collect::<Vec<_>>()
                .join(" ")
        ),
    };
    let dist = merge_dist(&l, &r, est, None);
    // both sides sorted on the one variable they share, which both always bind: a merge
    // that keeps the left side's order (the executor checks the order of the rows)
    let merge = ctx.opt.merge_left_join
        && matches!(&keys[..], [k] if l.sorted.first() == Some(k)
            && r.sorted.first() == Some(k)
            && l.certain.contains(k)
            && r.certain.contains(k));
    let (sorted, base) = if merge {
        (l.sorted.clone(), (l.est + r.est) * merge_row_cost())
    } else if ctx.opt.merge_left_join {
        (
            Vec::new(),
            hash_base(Costing::of(ctx).hash_build, l.est, r.est),
        )
    } else {
        (Vec::new(), 2.0 * r.est + l.est)
    };
    Node {
        kind: Kind::LeftJoin { expr },
        vars,
        certain: l.certain.clone(),
        sorted,
        est,
        cost: l.cost + r.cost + base + est,
        dist,
        desc,
        children: vec![l, r],
    }
}

fn union(children: Vec<Node>) -> Node {
    match children.len() {
        0 => return Node::empty(Vec::new()),
        1 => return children.into_iter().next().unwrap(),
        _ => {}
    }
    let mut vars: Vec<VarId> = Vec::new();
    for c in &children {
        for v in &c.vars {
            if !vars.contains(v) {
                vars.push(*v);
            }
        }
    }
    let certain: Vec<VarId> = vars
        .iter()
        .filter(|v| children.iter().all(|c| c.certain.contains(v)))
        .copied()
        .collect();
    let est: f64 = children.iter().map(|c| c.est).sum();
    let cost: f64 = children.iter().map(|c| c.cost).sum::<f64>() + est;
    let dist = vars.iter().map(|&v| (v, est.max(1.0))).collect();
    Node {
        kind: Kind::Union,
        vars,
        certain,
        sorted: Vec::new(),
        est,
        cost,
        dist,
        desc: format!("{} branches", children.len()),
        children,
    }
}

pub fn filter(n: Node, exprs: Vec<Expr>, ctx: &Ctx) -> Node {
    let (n, exprs) = push_range(n, exprs, ctx);
    let (n, exprs) = super::geopf::push_spatial(n, exprs, ctx);
    if exprs.is_empty() {
        return n;
    }
    let texts: Vec<String> = exprs.iter().map(|e| e.display(ctx)).collect();
    let sampled: Vec<Option<super::sample::Sampled>> =
        texts.iter().map(|t| ctx.sampled(t)).collect();
    let sel = super::sample::combine(sampled.iter().map(|s| s.map(|s| s.sel)));
    let mut desc = texts.join(" && ");
    if let Some(note) = super::sample::note(&sampled) {
        desc = format!("{desc} {note}");
    }
    // a conjunct evaluated once per distinct value sorts its input column, unless the
    // input is sorted on it already (an index scan can be read in that order instead).
    // Costed whether or not the expression cache is on, so that switching it off does
    // not change the plan.
    let unsorted = exprs.iter().any(|e| {
        super::exprcache::eligible(&[e])
            .is_ok_and(|v| v.is_some_and(|v| n.vars.contains(&v) && n.sorted.first() != Some(&v)))
    });
    let sort_cost = if unsorted { n.est * 0.5 } else { 0.0 };
    let in_ranges = prefix_rows(&n, &exprs, ctx);
    let mut f = Node::unary(Kind::Filter(exprs), n, desc);
    f.cost += sort_cost;
    f.est = (f.est * sel).max(if f.est > 0.0 { 1.0 } else { 0.0 });
    if let Some((rows, ranges)) = in_ranges {
        // the scan reads only the key ranges of the start, whose rows mostly pass
        f.cost = rows + 4.0 * ranges as f64;
        f.est = f.est.min(rows.max(1.0));
    }
    for d in f.dist.values_mut() {
        *d = d.min(f.est.max(1.0));
    }
    f
}

/// The rows a FILTER over scan `n` reads, and the key ranges it reads them in, when a
/// conjunct fixes the start of the string of the scan's sorted column (`STRSTARTS`, a
/// start-anchored `REGEX`): the filtered scan then reads only the rows of the base
/// vocabulary's ids with that start and of the ids outside the base vocabulary (see
/// `key_ranges` in the executor). `None` when the scan reads every row.
fn prefix_rows(n: &Node, exprs: &[Expr], ctx: &Ctx) -> Option<(f64, usize)> {
    let o = &ctx.opt;
    if !(o.filter_id_ranges && o.filter_scan_runs && o.filter_key_ranges) {
        return None;
    }
    let Kind::Scan(spec) = &n.kind else {
        return None;
    };
    let &(col, v) = spec.cols.first()?;
    if col != spec.prefix.len() || col >= 4 || spec.dedup {
        return None;
    }
    // the conjuncts over the column alone, as the executor tests them
    let on_key: Vec<Expr> = exprs
        .iter()
        .filter(|e| super::exprcache::input(&[e]) == Ok(Some(v)))
        .cloned()
        .collect();
    let prefixes = super::keyfilter::KeyFilter::new(&on_key, v)?.key_prefixes()?;
    let vocab = &ctx.snap.generation.vocab;
    let mut ids = vec![(0, Id::vocab(0).0 - 1)];
    for p in prefixes {
        let (a, b) = vocab.prefix_range(&p);
        if a < b {
            ids.push((Id::vocab(a).0, Id::vocab(b - 1).0));
        }
    }
    ids.push((Id::vocab(vocab.len()).0, u64::MAX));
    let bound = |v: u64, fill: u64| {
        let mut k = crate::index::pad(&spec.prefix, fill);
        k[col] = v;
        k
    };
    let mut rows = 0.0;
    for &(lo, hi) in &ids {
        rows += ctx
            .snap
            .count_between(spec.perm, bound(lo, 0), bound(hi, u64::MAX))
            .ok()? as f64;
    }
    Some((rows.min(n.est), ids.len()))
}

/// Comparison of the range variable with a numeric constant: `?v op c`.
#[derive(Clone, Copy, PartialEq)]
enum RangeOp {
    Lt,
    Le,
    Gt,
    Ge,
    Eq,
}

/// `?v op c` for a filter conjunct comparing variable `v` with a numeric literal.
fn range_atom(e: &Expr, v: VarId, ctx: &Ctx) -> Option<(RangeOp, super::value::Value)> {
    use super::expr::CmpOp;
    let (a, b, op) = match e {
        Expr::Cmp(a, b, op) => (a, b, Some(*op)),
        Expr::Eq(a, b) => (a, b, None),
        _ => return None,
    };
    fn constant(e: &Expr, ctx: &Ctx) -> Option<super::value::Value> {
        use super::value::{NumOp, Value, arith};
        match e {
            Expr::Lit(_, c) => Some(c.clone()),
            Expr::Const(id) => ctx.value(*id),
            Expr::Neg(x) => arith(NumOp::Sub, &Value::Integer(0.into()), &constant(x, ctx)?).ok(),
            Expr::Pos(x) => constant(x, ctx).filter(|c| c.is_numeric()),
            _ => None,
        }
    }
    let constant = |e: &Expr| constant(e, ctx);
    let (c, flip) = match (&**a, &**b) {
        (Expr::Var(x), c) if *x == v => (constant(c)?, false),
        (c, Expr::Var(x)) if *x == v => (constant(c)?, true),
        _ => return None,
    };
    if !c.is_numeric() {
        return None;
    }
    let op = match (op, flip) {
        (None, _) => RangeOp::Eq,
        (Some(CmpOp::Lt), false) | (Some(CmpOp::Gt), true) => RangeOp::Lt,
        (Some(CmpOp::Le), false) | (Some(CmpOp::Ge), true) => RangeOp::Le,
        (Some(CmpOp::Gt), false) | (Some(CmpOp::Lt), true) => RangeOp::Gt,
        (Some(CmpOp::Ge), false) | (Some(CmpOp::Le), true) => RangeOp::Ge,
    };
    Some((op, c))
}

/// Id ranges whose values satisfy every atom (see [`RangeSpec`]).
fn numeric_ranges(atoms: &[(RangeOp, super::value::Value)], ctx: &Ctx) -> Vec<IdRange> {
    use super::value::compare;
    use std::cmp::Ordering;
    // the atoms split into lower bounds (false, then true along a segment in value order)
    // and upper bounds (true, then false); `=` is both
    let cmp = |id: u64| {
        ctx.value(Id(id)).map(|x| {
            atoms
                .iter()
                .map(move |(op, c)| (*op, compare(&x, c).ok().flatten()))
        })
    };
    let lower_ok = |id: u64| {
        cmp(id).is_some_and(|mut it| {
            it.all(|(op, o)| match op {
                RangeOp::Gt => o == Some(Ordering::Greater),
                RangeOp::Ge | RangeOp::Eq => matches!(o, Some(Ordering::Greater | Ordering::Equal)),
                RangeOp::Lt | RangeOp::Le => true,
            })
        })
    };
    let upper_ok = |id: u64| {
        cmp(id).is_some_and(|mut it| {
            it.all(|(op, o)| match op {
                RangeOp::Lt => o == Some(Ordering::Less),
                RangeOp::Le | RangeOp::Eq => matches!(o, Some(Ordering::Less | Ordering::Equal)),
                RangeOp::Gt | RangeOp::Ge => true,
            })
        })
    };
    // first id in [lo, hi] for which `pred` holds (`pred` monotone false → true)
    let first = |lo: u64, hi: u64, pred: &dyn Fn(u64) -> bool| {
        let (mut a, mut b) = (lo, hi + 1);
        while a < b {
            let m = a + (b - a) / 2;
            if pred(m) { b = m } else { a = m + 1 }
        }
        a
    };
    let mut out = Vec::new();
    let segment = |tag: Tag, lo: u64, hi: u64, out: &mut Vec<IdRange>| {
        let (lo, hi) = (Id::new(tag, lo).0, Id::new(tag, hi).0);
        let s = first(lo, hi, &lower_ok);
        let e = first(lo, hi, &|id| !upper_ok(id));
        if s < e {
            out.push(IdRange {
                lo: s,
                hi: e - 1,
                exact: true,
            });
        }
    };
    let residual = |tag: Tag, out: &mut Vec<IdRange>| {
        out.push(IdRange {
            lo: Id::new(tag, 0).0,
            hi: Id::new(tag, crate::id::PAYLOAD_MASK).0,
            exact: false,
        })
    };
    // tags in id order: Int < Double < Vocab < Delta < Decimal
    let half = 1u64 << (crate::id::PAYLOAD_BITS - 1);
    segment(Tag::Int, 0, half - 1, &mut out);
    segment(Tag::Int, half, 2 * half - 1, &mut out);
    residual(Tag::Double, &mut out);
    residual(Tag::Vocab, &mut out);
    residual(Tag::Delta, &mut out);
    const MANTISSA: u32 = 56;
    for scale in 0..16u64 {
        let base = scale << MANTISSA;
        let m = 1u64 << (MANTISSA - 1);
        segment(Tag::Decimal, base, base + m - 1, &mut out);
        segment(Tag::Decimal, base + m, base + 2 * m - 1, &mut out);
    }
    debug_assert!(out.windows(2).all(|w| w[0].hi < w[1].lo));
    out
}

/// Move numeric range conjuncts on a scan's sorted column into an [`Kind::RangeScan`].
fn push_range(n: Node, exprs: Vec<Expr>, ctx: &Ctx) -> (Node, Vec<Expr>) {
    let Kind::Scan(spec) = &n.kind else {
        return (n, exprs);
    };
    let Some(&(col, v)) = spec.cols.first() else {
        return (n, exprs);
    };
    if !ctx.opt.range_pushdown || col != spec.prefix.len() || spec.graph_col == col {
        return (n, exprs);
    }
    let (pushed, rest): (Vec<Expr>, Vec<Expr>) = exprs
        .into_iter()
        .flat_map(Expr::conjuncts)
        .partition(|e| range_atom(e, v, ctx).is_some());
    if pushed.is_empty() {
        return (n, rest);
    }
    let atoms: Vec<_> = pushed
        .iter()
        .filter_map(|e| range_atom(e, v, ctx))
        .collect();
    let ranges = numeric_ranges(&atoms, ctx);
    let exact = ranges.iter().filter(|r| r.exact).count();
    let desc = format!(
        "{} | {} [{exact} exact + {} tested id ranges]",
        n.desc,
        pushed
            .iter()
            .map(|e| e.display(ctx))
            .collect::<Vec<_>>()
            .join(" && "),
        ranges.len() - exact
    );
    // rows read, counted exactly (the boundary blocks are the ones the scan reads first):
    // exact ranges all qualify, the others are filtered
    let (mut exact_rows, mut tested_rows) = (0.0, 0.0);
    for r in &ranges {
        let bound = |v: u64, fill: u64| {
            let mut k = crate::index::pad(&spec.prefix, fill);
            k[spec.prefix.len()] = v;
            k
        };
        let Ok(rows) = ctx
            .snap
            .count_between(spec.perm, bound(r.lo, 0), bound(r.hi, u64::MAX))
        else {
            return (n, rest.into_iter().chain(pushed).collect());
        };
        let rows = rows as f64;
        if r.exact {
            exact_rows += rows;
        } else {
            tested_rows += rows;
        }
    }
    let read = (exact_rows + tested_rows).min(n.est);
    let est = (exact_rows + tested_rows * FILTER_SELECTIVITY.powi(pushed.len() as i32)).min(n.est);
    let seeks = 4.0 * ranges.len() as f64;
    let spec = spec.clone();
    let mut r = Node {
        kind: Kind::RangeScan(
            spec,
            RangeSpec {
                var: v,
                ranges,
                filter: pushed,
            },
        ),
        desc,
        ..n
    };
    r.cost = read + seeks;
    r.est = est.max(if r.est > 0.0 { 1.0 } else { 0.0 });
    for d in r.dist.values_mut() {
        *d = d.min(r.est.max(1.0));
    }
    (r, rest)
}

/// Id segments of a key column in id order, covering every id, with how ids order the
/// values within each: `Some(true)` when larger ids are larger values, `Some(false)` when
/// they are smaller (inline negative doubles: sign and magnitude), `None` when id order
/// says nothing about value order (vocabulary and delta terms, NaN payloads, booleans,
/// dates, blank nodes). Within a `Some` segment distinct ids are distinct numbers.
fn value_order_segments() -> Vec<(u64, u64, Option<bool>)> {
    use crate::id::{PAYLOAD_BITS, PAYLOAD_MASK, TAG_BITS};
    let id = |tag: Tag, payload: u64| Id::new(tag, payload).0;
    let half = 1u64 << (PAYLOAD_BITS - 1);
    // inline doubles are the IEEE bits without the 4 lowest: positive values, then NaN
    // payloads, then the negative values from -0 to -INF, then negative NaN payloads
    let inf = f64::INFINITY.to_bits() >> TAG_BITS;
    let neg_inf = f64::NEG_INFINITY.to_bits() >> TAG_BITS;
    // tags in id order: (Undef, Special, Bool) < Int < Double < (BNode, Vocab, Delta,
    // Local) < Decimal < (DateTime, Date)
    let mut out = vec![
        (0, id(Tag::Int, 0) - 1, None),
        (id(Tag::Int, 0), id(Tag::Int, half - 1), Some(true)),
        (id(Tag::Int, half), id(Tag::Int, PAYLOAD_MASK), Some(true)),
        (id(Tag::Double, 0), id(Tag::Double, inf), Some(true)),
        (id(Tag::Double, inf + 1), id(Tag::Double, half - 1), None),
        (id(Tag::Double, half), id(Tag::Double, neg_inf), Some(false)),
        (
            id(Tag::Double, neg_inf + 1),
            id(Tag::Double, PAYLOAD_MASK),
            None,
        ),
        (id(Tag::BNode, 0), id(Tag::Decimal, 0) - 1, None),
    ];
    // inline decimals: 4-bit scale, then a 56-bit two's complement mantissa
    const MANTISSA: u32 = 56;
    for scale in 0..16u64 {
        let base = scale << MANTISSA;
        let m = 1u64 << (MANTISSA - 1);
        out.push((
            id(Tag::Decimal, base),
            id(Tag::Decimal, base + m - 1),
            Some(true),
        ));
        out.push((
            id(Tag::Decimal, base + m),
            id(Tag::Decimal, base + 2 * m - 1),
            Some(true),
        ));
    }
    out.push((id(Tag::Decimal, PAYLOAD_MASK) + 1, u64::MAX, None));
    debug_assert!(out.windows(2).all(|w| w[0].1 + 1 == w[1].0));
    out
}

#[cfg(test)]
thread_local! {
    /// tests: choose [`OrderedTopK`] whenever it applies, whatever it costs
    pub(crate) static FORCE_ORDERED_TOPK: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

fn force_ordered_topk() -> bool {
    #[cfg(test)]
    return FORCE_ORDERED_TOPK.with(|f| f.get());
    #[cfg(not(test))]
    false
}

/// `ORDER BY ?v LIMIT k` over a single scan (and FILTERs over it), with `?v` one of the
/// scan's variables: an [`OrderedTopK`] when the scan can be read in the order of `?v`
/// and that reads at most half of its rows (counted exactly per id piece).
fn ordered_topk(n: Node, ctx: &Ctx) -> Node {
    let Kind::OrderBy {
        keys,
        limit: Some(k),
    } = &n.kind
    else {
        return n;
    };
    if !ctx.opt.ordered_topk || *k == 0 {
        return n;
    }
    let [(Expr::Var(v), asc)] = keys.as_slice() else {
        return n;
    };
    let (v, asc, k) = (*v, *asc, *k);
    let child = &n.children[0];
    let (leaf, mut filter) = match &child.kind {
        Kind::Filter(es) => (&child.children[0], es.clone()),
        _ => (child, Vec::new()),
    };
    if filter
        .iter()
        .any(|e| e.has_exists() || !super::cache::deterministic(e))
    {
        return n;
    }
    let (spec, range) = match &leaf.kind {
        Kind::Scan(spec) => (spec, None),
        Kind::RangeScan(spec, range) => (spec, Some(range)),
        _ => return n,
    };
    let Some(scan) = reorder_scan(spec, v) else {
        return n;
    };
    let col = scan.prefix.len();
    if scan.graph_col == col || scan.cols.first().map(|c| c.1) != Some(v) {
        return n;
    }
    // the generic plan's rows come in its scan's key order
    let mut tie: Vec<(usize, VarId)> = spec.cols.clone();
    tie.sort_by_key(|c| c.0);
    let tie_order = tie.into_iter().map(|c| c.1).collect();
    // a range filter on `?v` keeps its id ranges; on another variable it is tested
    let mut desc = retarget_desc(&leaf.desc, &scan);
    let (domain, range_filter) = match range {
        Some(r) if r.var == v => (r.ranges.clone(), r.filter.clone()),
        other => {
            if let Some(r) = other {
                filter.extend(r.filter.iter().cloned());
                desc = format!(
                    "{} | {}",
                    desc.split(" | ").next().unwrap_or_default(),
                    r.filter
                        .iter()
                        .map(|e| e.display(ctx))
                        .collect::<Vec<_>>()
                        .join(" && ")
                );
            }
            let all = IdRange {
                lo: 0,
                hi: u64::MAX,
                exact: true,
            };
            (vec![all], Vec::new())
        }
    };
    if matches!(child.kind, Kind::Filter(_)) {
        desc += &format!(" | {}", child.desc);
    }
    let bound = |id: u64, fill: u64| {
        let mut key = crate::index::pad(&scan.prefix, fill);
        key[col] = id;
        key
    };
    let sel = FILTER_SELECTIVITY.powi(filter.len() as i32);
    let (mut pieces, mut total, mut read) = (Vec::new(), 0.0, 0.0);
    for d in &domain {
        for (lo, hi, mono) in value_order_segments() {
            let (lo, hi) = (lo.max(d.lo), hi.min(d.hi));
            if lo > hi {
                continue;
            }
            let Ok(rows) = ctx
                .snap
                .count_between(scan.perm, bound(lo, 0), bound(hi, u64::MAX))
            else {
                return n;
            };
            if rows == 0 {
                continue;
            }
            let rows = rows as f64;
            total += rows;
            // a monotone piece is read from its best end: the leading columns of about
            // one block are decoded to place the steps, and more rows when filters drop
            // rows
            let sel = if d.exact {
                sel
            } else {
                sel * FILTER_SELECTIVITY
            };
            read += match mono {
                Some(_) => rows.min(crate::index::BLOCK_ROWS as f64 + k as f64 / sel),
                None => rows,
            };
            pieces.push(TopKPiece {
                lo,
                hi,
                exact: d.exact,
                mono,
            });
        }
    }
    if pieces.is_empty() || read * 2.0 > total && !force_ordered_topk() {
        return n;
    }
    let desc = format!(
        "{desc} | {} limit {k} [{} of {} id pieces in value order]",
        n.desc,
        pieces.iter().filter(|p| p.mono.is_some()).count(),
        pieces.len(),
    );
    let seeks = 4.0 * pieces.len() as f64;
    let est = n.est.min(k as f64);
    let mut dist = n.dist.clone();
    for d in dist.values_mut() {
        *d = d.min(est.max(1.0));
    }
    Node {
        vars: n.vars.clone(),
        certain: n.certain.clone(),
        kind: Kind::OrderedTopK(Box::new(OrderedTopK {
            scan,
            var: v,
            asc,
            k,
            pieces,
            filter,
            range_filter,
            tie_order,
            fallback: Box::new(n),
        })),
        children: Vec::new(),
        sorted: Vec::new(),
        est,
        cost: read + seeks + est,
        dist,
        desc,
    }
}

pub fn project(n: Node, vars: Vec<VarId>, ctx: &Ctx) -> Node {
    let desc = vars
        .iter()
        .map(|v| format!("?{}", ctx.var_name(*v)))
        .collect::<Vec<_>>()
        .join(" ");
    let mut p = Node::unary(Kind::Project(vars.clone()), n, desc);
    p.cost = p.children[0].cost;
    p.certain.retain(|v| vars.contains(v));
    p.sorted = p
        .sorted
        .iter()
        .take_while(|v| vars.contains(v))
        .copied()
        .collect();
    p.vars = vars;
    p
}

fn slice(child: Node, start: usize, length: Option<usize>, ctx: &Ctx) -> Node {
    // ORDER BY + LIMIT → top-k (also through a projection), over a single scan read in
    // value order when that is cheaper
    let limit = length.map(|l| l.saturating_add(start));
    let child = match (child, limit) {
        (mut n, Some(k)) if matches!(n.kind, Kind::OrderBy { limit: None, .. }) => {
            if let Kind::OrderBy { limit, .. } = &mut n.kind {
                *limit = Some(k);
            }
            ordered_topk(
                super::geojoin::spatial_knn(super::vectortopk::vector_topk(n, ctx), ctx),
                ctx,
            )
        }
        (mut n, Some(k))
            if matches!(n.kind, Kind::Project(_))
                && matches!(n.children[0].kind, Kind::OrderBy { limit: None, .. }) =>
        {
            if let Kind::OrderBy { limit, .. } = &mut n.children[0].kind {
                *limit = Some(k);
            }
            let order = n.children.pop().unwrap();
            n.children.push(ordered_topk(
                super::geojoin::spatial_knn(super::vectortopk::vector_topk(order, ctx), ctx),
                ctx,
            ));
            n
        }
        (n, _) => n,
    };
    let desc = format!(
        "offset {start} limit {}",
        length.map_or("∞".to_string(), |l| l.to_string())
    );
    let mut n = Node::unary(
        Kind::Slice {
            offset: start,
            limit: length,
        },
        child,
        desc,
    );
    n.est = (n.est - start as f64).max(0.0);
    if let Some(l) = length {
        n.est = n.est.min(l as f64);
    }
    n
}

fn group(child: Node, keys: Vec<VarId>, aggs: Vec<(VarId, Agg)>, ctx: &Ctx) -> Node {
    // COUNT(*) over a single scan without grouping → index metadata (QLever)
    if keys.is_empty()
        && aggs.len() == 1
        && matches!(aggs[0].1.func, AggregateFunction::Count)
        && aggs[0].1.expr.is_none()
        && !aggs[0].1.distinct
        && let Kind::Scan(spec) = &child.kind
        && spec.eqs.is_empty()
        && !spec.dedup
    {
        let direct = match &spec.graph {
            GraphFilter::All => Some((spec.perm, spec.prefix.clone())),
            GraphFilter::Default | GraphFilter::One(_) if spec.prefix.is_empty() => {
                let g = match &spec.graph {
                    GraphFilter::One(g) => *g,
                    _ => Id::DEFAULT_GRAPH.0,
                };
                Some((Perm::Gspo, vec![g]))
            }
            _ => None,
        };
        if let Some((perm, prefix)) = direct {
            let var = aggs[0].0;
            let mut spec = spec.clone();
            spec.perm = perm;
            spec.prefix = prefix;
            let mut n = Node::leaf(
                Kind::CountScan { spec, var },
                vec![var],
                1.0,
                child.desc.clone(),
            );
            n.cost = 1.0;
            return n;
        }
    }
    // COUNT(DISTINCT ?k) over a single scan: in a permutation sorted on ?k the distinct
    // values are the runs of equal ids, counted without materializing or hashing rows
    if keys.is_empty()
        && aggs.len() == 1
        && matches!(aggs[0].1.func, AggregateFunction::Count)
        && aggs[0].1.distinct
        && let Some(Expr::Var(k)) = &aggs[0].1.expr
        && let Kind::Scan(spec) = &child.kind
        && let Some(spec) = reorder_scan(spec, *k)
    {
        let var = aggs[0].0;
        let counted = distinct_count_exact(&spec, child.est, ctx);
        let desc = format!(
            "{} distinct ?{}{}",
            retarget_desc(&child.desc, &spec),
            ctx.var_name(*k),
            counted.as_ref().map_or(String::new(), |c| c.note())
        );
        let cost = if counted.is_some() { 1.0 } else { child.est };
        let mut n = Node::leaf(
            Kind::CountDistinctScan {
                spec,
                var,
                metadata: counted.map(|c| c.total()),
            },
            vec![var],
            1.0,
            desc,
        );
        n.cost = cost;
        return n;
    }
    // COUNT(*) over a FILTER that reads one variable of a single scan: the filter is
    // tested once per run of the variable's values, without materializing the rows
    if keys.is_empty()
        && ctx.opt.count_filter_runs
        && aggs.len() == 1
        && matches!(aggs[0].1.func, AggregateFunction::Count)
        && let Kind::Filter(filter) = &child.kind
        && let Kind::Scan(spec) = &child.children[0].kind
        && let Ok(Some(k)) = super::exprcache::input(&filter.iter().collect::<Vec<_>>())
        && child.children[0].vars.contains(&k)
        && match (&aggs[0].1.expr, aggs[0].1.distinct) {
            (None, false) => true,
            // every variable of a scan is bound on every row
            (Some(Expr::Var(x)), false) => child.children[0].vars.contains(x),
            (Some(Expr::Var(x)), true) => *x == k,
            _ => false,
        }
        && let Some(spec) = reorder_scan(spec, k)
    {
        let scan = &child.children[0];
        let var = aggs[0].0;
        let desc = format!(
            "{} filter {}{}",
            retarget_desc(&scan.desc, &spec),
            child.desc,
            if aggs[0].1.distinct {
                format!(" distinct ?{}", ctx.var_name(k))
            } else {
                String::new()
            }
        );
        let mut n = Node::leaf(
            Kind::CountFilterScan {
                spec,
                key: k,
                filter: filter.clone(),
                var,
                distinct: aggs[0].1.distinct,
            },
            vec![var],
            1.0,
            desc,
        );
        n.cost = scan.est;
        return n;
    }
    // COUNT(*) over a join of two scans on one variable: per-key run lengths
    if keys.is_empty()
        && ctx.opt.count_join_runs
        && aggs.len() == 1
        && matches!(aggs[0].1.func, AggregateFunction::Count)
        && aggs[0].1.expr.is_none()
        && !aggs[0].1.distinct
        && let Kind::Join { algo, keys: jk } = &child.kind
        && *algo != JoinAlgo::Cross
        && let [k] = jk.as_slice()
        && let Some(runs) = key_run_scans(&child, *k, ctx)
    {
        let var = aggs[0].0;
        return Node {
            kind: Kind::CountJoinRuns { var },
            vars: vec![var],
            certain: vec![var],
            sorted: Vec::new(),
            est: 1.0,
            cost: runs.iter().map(|c| c.est).sum(),
            dist: [(var, 1.0)].into_iter().collect(),
            desc: child.desc.clone(),
            children: runs,
        };
    }
    // COUNT(*) over a plain join: count pairs
    if keys.is_empty()
        && aggs.len() == 1
        && matches!(aggs[0].1.func, AggregateFunction::Count)
        && aggs[0].1.expr.is_none()
        && !aggs[0].1.distinct
        && let Kind::Join { algo, .. } = &child.kind
        && *algo != JoinAlgo::Cross
    {
        let var = aggs[0].0;
        return Node {
            kind: Kind::CountJoin {
                algo: algo.clone(),
                var,
            },
            vars: vec![var],
            certain: vec![var],
            sorted: Vec::new(),
            est: 1.0,
            cost: child.cost,
            dist: [(var, 1.0)].into_iter().collect(),
            desc: child.desc.clone(),
            children: child.children,
        };
    }
    // GROUP BY ?k with only COUNT(*) / COUNT(?v) over a single scan
    if keys.len() == 1
        && !aggs.is_empty()
        && let Kind::Scan(spec) = &child.kind
        && aggs.iter().all(|(_, a)| {
            matches!(a.func, AggregateFunction::Count)
                && !a.distinct
                && match &a.expr {
                    None => true,
                    Some(Expr::Var(v)) => child.vars.contains(v),
                    _ => false,
                }
        })
        && let Some(spec) = reorder_scan(spec, keys[0])
    {
        let key = keys[0];
        let counts: Vec<VarId> = aggs.iter().map(|(v, _)| *v).collect();
        let est = child.d(key).min(child.est).max(1.0);
        let mut vars = vec![key];
        vars.extend(&counts);
        let desc = format!(
            "{} by ?{}",
            retarget_desc(&child.desc, &spec),
            ctx.var_name(key)
        );
        let metadata = group_counts_exact(&spec, key, child.est, ctx);
        let desc = match &metadata {
            Some(c) => format!("{desc}{}", c.note()),
            None => desc,
        };
        let n_metadata = metadata.is_some();
        let mut n = Node::leaf(
            Kind::GroupCountScan {
                spec,
                key,
                counts,
                metadata,
            },
            vars,
            est,
            desc,
        );
        n.cost = if n_metadata { 1.0 } else { child.est };
        n.sorted = vec![key];
        return n;
    }
    let est = if keys.is_empty() {
        1.0
    } else {
        keys.iter()
            .map(|k| child.d(*k))
            .fold(1.0, f64::max)
            .min(child.est)
            .max(1.0)
    };
    let mut vars = keys.clone();
    vars.extend(aggs.iter().map(|(v, _)| *v));
    let desc = format!(
        "by {} aggs {}",
        keys.iter()
            .map(|v| format!("?{}", ctx.var_name(*v)))
            .collect::<Vec<_>>()
            .join(" "),
        aggs.iter()
            .map(|(v, a)| format!("?{}={:?}", ctx.var_name(*v), a.func))
            .collect::<Vec<_>>()
            .join(" ")
    );
    let desc = if ctx.opt.incremental_group
        && super::exec::incremental_group_ok(&keys, &aggs, &child.vars)
    {
        format!("{desc} [incremental]")
    } else {
        desc
    };
    let dist = vars.iter().map(|&v| (v, est)).collect();
    let certain = keys
        .iter()
        .filter(|k| child.certain.contains(k))
        .copied()
        .collect();
    Node {
        kind: Kind::Group { keys, aggs },
        vars,
        certain,
        sorted: Vec::new(),
        est,
        cost: child.cost + child.est * 1.5,
        dist,
        desc,
        children: vec![child],
    }
}

// ------------------------------------------------------------------------------
// helpers
// ------------------------------------------------------------------------------

/// The two children of a join, as scans sorted on the join variable `k`, when both are
/// plain scans sharing only `k` (so the count needs nothing but each side's run lengths).
fn key_run_scans(join: &Node, k: VarId, ctx: &Ctx) -> Option<Vec<Node>> {
    let [l, r] = join.children.as_slice() else {
        return None;
    };
    let shared: Vec<VarId> = l
        .vars
        .iter()
        .filter(|v| r.vars.contains(v))
        .copied()
        .collect();
    if shared != [k] {
        return None;
    }
    [l, r]
        .into_iter()
        .map(|c| {
            let Kind::Scan(spec) = &c.kind else {
                return None;
            };
            let spec = reorder_scan(spec, k)?;
            let desc = format!(
                "{} [runs of ?{}]",
                retarget_desc(&c.desc, &spec),
                ctx.var_name(k)
            );
            Some(Node {
                kind: Kind::Scan(spec),
                desc,
                sorted: vec![k],
                ..c.clone()
            })
        })
        .collect()
}

/// The answer of `GROUP BY ?k` with counts over a single scan from the index
/// statistics, corrected for the snapshot's delta and for graphs the scan does not read:
/// the instances of each class (`?s rdf:type ?k`, distinct subjects per class when the
/// scan reads one graph or drops a triple's repeats across graphs), or the quads of each
/// predicate (`?s ?k ?o` over one graph). `est` is the scan's size, which the correction
/// must not exceed in work.
fn group_counts_exact(
    spec: &ScanSpec,
    key: VarId,
    est: f64,
    ctx: &Ctx,
) -> Option<Arc<super::stats::Counts>> {
    use super::stats::{CountKey, Measure};
    if !ctx.opt.metadata_counts
        || !spec.eqs.is_empty()
        || spec.cols.iter().any(|&(c, _)| c == spec.graph_col)
    {
        return None;
    }
    let rdf_type = ctx
        .snap
        .lookup_iri(oxrdf::vocab::rdf::TYPE.as_str())
        .map(|t| t.0);
    let classes = spec.perm == Perm::Pos
        && spec.cols.first() == Some(&(1, key))
        && spec.cols[1..].iter().all(|&(c, _)| c == 2)
        && rdf_type.is_some_and(|t| spec.prefix == [t])
        && (spec.dedup || !spec.graph.multi());
    let predicates = matches!(spec.perm, Perm::Pso | Perm::Pos)
        && spec.prefix.is_empty()
        && spec.cols.first() == Some(&(0, key))
        && !spec.dedup
        && !spec.graph.multi();
    let measure = match (classes, predicates) {
        (true, _) => Measure::Distinct(3),
        (_, true) => Measure::Rows,
        _ => return None,
    };
    let k = CountKey {
        perm: spec.perm,
        prefix: spec.prefix.clone(),
        grouped: true,
        measure,
        graphs: spec.graph.clone(),
    };
    statistics(&k, rdf_type, est, ctx)
}

/// The number of distinct values of a scan's first free column from the index
/// statistics, corrected for the snapshot's delta and for graphs the scan does not read:
/// a scan of the whole index or of one predicate without repeated variables. The
/// statistics hold the distinct subjects, predicates and objects of the index, and the
/// distinct subjects and objects of each predicate. `est` is the scan's size, which the
/// correction must not exceed in work.
fn distinct_count_exact(spec: &ScanSpec, est: f64, ctx: &Ctx) -> Option<Arc<super::stats::Counts>> {
    if !ctx.opt.metadata_counts
        || !spec.eqs.is_empty()
        || spec.prefix.len() > 1
        || spec.cols.first()?.0 != spec.prefix.len()
    {
        return None;
    }
    let k = super::stats::CountKey {
        perm: spec.perm,
        prefix: spec.prefix.clone(),
        grouped: false,
        measure: super::stats::Measure::Distinct(spec.prefix.len() + 1),
        graphs: spec.graph.clone(),
    };
    statistics(&k, None, est, ctx)
}

/// [`super::stats::exact_counts`] for the planner: errors (cancellation, a block that
/// cannot be read) leave the scan to the generic operators, which report them.
fn statistics(
    k: &super::stats::CountKey,
    rdf_type: Option<u64>,
    est: f64,
    ctx: &Ctx,
) -> Option<Arc<super::stats::Counts>> {
    super::stats::exact_counts(
        &ctx.snap,
        k,
        rdf_type,
        est.max(0.0) as u64,
        ctx.opt.delta_statistics,
        &|| ctx.check(),
    )
    .ok()
    .flatten()
}

/// A scan description (`PSO ?s <p> ?o`) naming the permutation of a re-targeted scan.
fn retarget_desc(desc: &str, spec: &ScanSpec) -> String {
    match desc.split_once(' ') {
        Some((_, rest)) => format!("{} {rest}", spec.perm.name().to_uppercase()),
        None => desc.to_string(),
    }
}

/// Re-target a scan to a permutation whose first free column holds `first` (same bound
/// prefix, same variables), if one exists.
fn reorder_scan(spec: &ScanSpec, first: VarId) -> Option<ScanSpec> {
    let order = spec.perm.order();
    if spec.perm == Perm::Gspo {
        return (spec.cols.first().map(|c| c.1) == Some(first)).then(|| spec.clone());
    }
    // component (S/P/O/G) of each bound prefix entry and of each variable column
    let bound: Vec<(usize, u64)> = spec
        .prefix
        .iter()
        .enumerate()
        .map(|(i, v)| (order[i], *v))
        .collect();
    let comp_of = |kc: usize| order[kc];
    let key_comp = comp_of(spec.cols.iter().find(|(_, v)| *v == first)?.0);
    if key_comp == G {
        return None;
    }
    let mut bound_comps: Vec<usize> = bound.iter().map(|b| b.0).collect();
    bound_comps.sort_unstable();
    let perm = [
        Perm::Spo,
        Perm::Sop,
        Perm::Pso,
        Perm::Pos,
        Perm::Osp,
        Perm::Ops,
    ]
    .into_iter()
    .find(|p| {
        let o = p.order();
        let mut f: Vec<usize> = o[..bound.len()].to_vec();
        f.sort_unstable();
        f == bound_comps && o[bound.len()] == key_comp
    })?;
    if perm == spec.perm {
        return Some(spec.clone());
    }
    let po = perm.order();
    let prefix: Vec<u64> = po[..bound.len()]
        .iter()
        .map(|c| bound.iter().find(|b| b.0 == *c).unwrap().1)
        .collect();
    let remap = |kc: usize| perm.col_of(comp_of(kc));
    let mut cols: Vec<(usize, VarId)> = spec.cols.iter().map(|&(kc, v)| (remap(kc), v)).collect();
    cols.sort_by_key(|c| c.0);
    let eqs = spec
        .eqs
        .iter()
        .map(|&(a, b)| (remap(a), remap(b)))
        .collect();
    Some(ScanSpec {
        perm,
        prefix,
        cols,
        eqs,
        graph: spec.graph.clone(),
        graph_col: perm.col_of(G),
        dedup: spec.dedup,
    })
}

/// The longest prefix of an ARQ path range the planner unrolls into joined steps (see
/// [`Planner::collect_path`]); a longer range runs whole in the path operator.
const RANGE_UNROLL: u64 = 32;

fn flatten_union<'g>(gp: &'g GraphPattern, out: &mut Vec<&'g GraphPattern>) {
    if let GraphPattern::Union { left, right } = gp {
        flatten_union(left, out);
        flatten_union(right, out);
    } else {
        out.push(gp);
    }
}

fn flatten_alt<'g>(p: &'g PropertyPathExpression, out: &mut Vec<&'g PropertyPathExpression>) {
    if let PropertyPathExpression::Alternative(a, b) = p {
        flatten_alt(a, out);
        flatten_alt(b, out);
    } else {
        out.push(p);
    }
}

fn has_exists(e: &Expression) -> bool {
    let mut found = false;
    fn walk(e: &Expression, f: &mut bool) {
        use Expression as E;
        match e {
            E::Exists(_) => *f = true,
            E::Or(a, b)
            | E::And(a, b)
            | E::Equal(a, b)
            | E::SameTerm(a, b)
            | E::Greater(a, b)
            | E::GreaterOrEqual(a, b)
            | E::Less(a, b)
            | E::LessOrEqual(a, b)
            | E::Add(a, b)
            | E::Subtract(a, b)
            | E::Multiply(a, b)
            | E::Divide(a, b) => {
                walk(a, f);
                walk(b, f);
            }
            E::UnaryPlus(a) | E::UnaryMinus(a) | E::Not(a) => walk(a, f),
            E::In(a, l) => {
                walk(a, f);
                l.iter().for_each(|x| walk(x, f));
            }
            E::If(a, b, c) => {
                walk(a, f);
                walk(b, f);
                walk(c, f);
            }
            E::Coalesce(l) | E::FunctionCall(_, l) => l.iter().for_each(|x| walk(x, f)),
            _ => {}
        }
    }
    walk(e, &mut found);
    found
}

/// `?x = <iri>` / `sameTerm(?x, c)` where the constant is an IRI (value = term equality).
fn equality_constant(e: &Expr, ctx: &Ctx) -> Option<(VarId, Id)> {
    let (a, b) = match e {
        Expr::Eq(a, b) => (a, b),
        Expr::SameTerm(a, b) => {
            return match (&**a, &**b) {
                (Expr::Var(v), c) | (c, Expr::Var(v)) => c
                    .const_id()
                    .filter(|c| c.tag() != Tag::Local)
                    .map(|c| (*v, c)),
                _ => None,
            };
        }
        _ => return None,
    };
    match (&**a, &**b) {
        (Expr::Var(v), c) | (c, Expr::Var(v)) => c
            .const_id()
            .filter(|c| ctx.kind(*c) == super::ctx::TermKind::Iri)
            .map(|c| (*v, c)),
        _ => None,
    }
}

fn substitute(e: &mut Expr, v: VarId, c: Id) {
    match e {
        Expr::Var(x) if *x == v => *e = Expr::Const(c),
        Expr::Bound(x) if *x == v => *e = Expr::Const(Id::from_bool(true)),
        Expr::Or(a, b)
        | Expr::And(a, b)
        | Expr::Eq(a, b)
        | Expr::SameTerm(a, b)
        | Expr::Cmp(a, b, _)
        | Expr::Arith(a, b, _) => {
            substitute(a, v, c);
            substitute(b, v, c);
        }
        Expr::Not(a) | Expr::Neg(a) | Expr::Pos(a) => substitute(a, v, c),
        Expr::In(a, l) => {
            substitute(a, v, c);
            l.iter_mut().for_each(|x| substitute(x, v, c));
        }
        Expr::If(a, b, d) => {
            substitute(a, v, c);
            substitute(b, v, c);
            substitute(d, v, c);
        }
        Expr::Coalesce(l) | Expr::Call(_, l) => l.iter_mut().for_each(|x| substitute(x, v, c)),
        // the EXISTS pattern is substituted when it is evaluated: it keeps the constant
        Expr::Exists(spec) if spec.vars.contains(&v) => *spec = Arc::new(spec.with_bound(v, c)),
        _ => {}
    }
}

/// Variables certainly bound by a pattern (used for filter placement).
pub fn certain_vars(gp: &GraphPattern, ctx: &Ctx) -> FxHashSet<VarId> {
    let mut names = Vec::new();
    certain_names(gp, &mut names);
    names.iter().map(|n| ctx.var(n)).collect()
}

fn certain_names(gp: &GraphPattern, out: &mut Vec<String>) {
    use GraphPattern as GP;
    match gp {
        GP::Bgp { patterns } => {
            for t in patterns {
                for x in [&t.subject, &t.object] {
                    if let TermPattern::Variable(v) = x {
                        out.push(v.as_str().to_string());
                    }
                }
                if let NamedNodePattern::Variable(v) = &t.predicate {
                    out.push(v.as_str().to_string());
                }
            }
        }
        GP::Path {
            subject, object, ..
        } => {
            for x in [subject, object] {
                if let TermPattern::Variable(v) = x {
                    out.push(v.as_str().to_string());
                }
            }
        }
        GP::Join { left, right } | GP::Lateral { left, right } => {
            certain_names(left, out);
            certain_names(right, out);
        }
        GP::LeftJoin { left, .. }
        | GP::Minus { left, .. }
        | GP::SemiJoin { left, .. }
        | GP::AntiJoin { left, .. } => certain_names(left, out),
        GP::Filter { inner, .. }
        | GP::Distinct { inner }
        | GP::Reduced { inner }
        | GP::Slice { inner, .. }
        | GP::OrderBy { inner, .. } => certain_names(inner, out),
        GP::Graph { name, inner } => {
            if let NamedNodePattern::Variable(v) = name {
                out.push(v.as_str().to_string());
            }
            certain_names(inner, out);
        }
        // a failed expression or a null element leaves the variable unbound
        GP::Extend { inner, .. } | GP::Assign { inner, .. } | GP::Unfold { inner, .. } => {
            certain_names(inner, out)
        }
        GP::Project { inner, variables } => {
            let mut inner_names = Vec::new();
            certain_names(inner, &mut inner_names);
            out.extend(
                inner_names
                    .into_iter()
                    .filter(|n| variables.iter().any(|v| v.as_str() == n)),
            );
        }
        GP::Union { left, right } => {
            let (mut a, mut b) = (Vec::new(), Vec::new());
            certain_names(left, &mut a);
            certain_names(right, &mut b);
            out.extend(a.into_iter().filter(|n| b.contains(n)));
        }
        GP::Group { variables, .. } => {
            let _ = variables;
        }
        _ => {}
    }
}

/// ARQ's `multi(path)`: `path` with each `*`, `+` and `?` as the counted range `{*}`,
/// `{+}` or `{0,1}`. A `distinct(…)` inside keeps its set semantics.
pub(super) fn counted(p: &PropertyPathExpression) -> PropertyPathExpression {
    use PropertyPathExpression as PP;
    let b = |x: &PropertyPathExpression| Box::new(counted(x));
    match p {
        PP::ZeroOrMore(x) => PP::Range {
            path: b(x),
            min: 0,
            max: None,
        },
        PP::OneOrMore(x) => PP::Range {
            path: b(x),
            min: 1,
            max: None,
        },
        PP::ZeroOrOne(x) => PP::Range {
            path: b(x),
            min: 0,
            max: Some(1),
        },
        PP::Range { path, min, max } => PP::Range {
            path: b(path),
            min: *min,
            max: *max,
        },
        PP::Reverse(x) => PP::Reverse(b(x)),
        PP::Sequence(x, y) => PP::Sequence(b(x), b(y)),
        PP::Alternative(x, y) => PP::Alternative(b(x), b(y)),
        PP::Multi(x) => counted(x),
        PP::NamedNode(_) | PP::NegatedPropertySet(_) | PP::Distinct(_) | PP::Shortest(_) => {
            p.clone()
        }
    }
}

/// The variables the items of a group bind.
fn item_vars(items: &[Item]) -> FxHashSet<VarId> {
    let mut out = FxHashSet::default();
    for it in items {
        match it {
            Item::Triple(t) => {
                out.extend(t.t.iter().filter_map(|x| match x {
                    PT::V(v) => Some(*v),
                    PT::C(_) => None,
                }));
                if let ActiveGraph::Var(v) = t.graph {
                    out.insert(v);
                }
            }
            Item::Path(p) => {
                for x in [p.s, p.o] {
                    if let PT::V(v) = x {
                        out.insert(v);
                    }
                }
            }
            Item::Node(n) => out.extend(n.vars.iter().copied()),
            Item::Unpack(u) => {
                out.insert(u.t);
                out.extend(u.parts.iter().filter_map(|x| match x {
                    PT::V(v) => Some(*v),
                    PT::C(_) => None,
                }));
            }
        }
    }
    out
}

/// All variable names mentioned in a pattern (for EXISTS / SERVICE).
pub fn collect_pattern_vars(gp: &GraphPattern, out: &mut Vec<String>) {
    gp.on_in_scope_variable(|v| out.push(v.as_str().to_string()));
    // variables used only in filters also matter for substitution
    fn walk(gp: &GraphPattern, out: &mut Vec<String>) {
        use GraphPattern as GP;
        match gp {
            GP::Filter { expr, inner } => {
                expr_vars(expr, out);
                walk(inner, out);
            }
            GP::LeftJoin {
                left,
                right,
                expression,
            } => {
                if let Some(e) = expression {
                    expr_vars(e, out);
                }
                walk(left, out);
                walk(right, out);
            }
            GP::Join { left, right }
            | GP::Lateral { left, right }
            | GP::Union { left, right }
            | GP::Minus { left, right }
            | GP::SemiJoin { left, right }
            | GP::AntiJoin { left, right } => {
                walk(left, out);
                walk(right, out);
            }
            GP::Extend {
                inner, expression, ..
            }
            | GP::Assign {
                inner, expression, ..
            }
            | GP::Unfold {
                inner, expression, ..
            } => {
                expr_vars(expression, out);
                walk(inner, out);
            }
            GP::Graph { inner, .. }
            | GP::Distinct { inner }
            | GP::Reduced { inner }
            | GP::Slice { inner, .. }
            | GP::OrderBy { inner, .. } => walk(inner, out),
            _ => {}
        }
    }
    walk(gp, out);
}

pub(super) fn expr_vars(e: &Expression, out: &mut Vec<String>) {
    use Expression as E;
    match e {
        E::Variable(v) | E::Bound(v) => out.push(v.as_str().to_string()),
        E::Or(a, b)
        | E::And(a, b)
        | E::Equal(a, b)
        | E::SameTerm(a, b)
        | E::Greater(a, b)
        | E::GreaterOrEqual(a, b)
        | E::Less(a, b)
        | E::LessOrEqual(a, b)
        | E::Add(a, b)
        | E::Subtract(a, b)
        | E::Multiply(a, b)
        | E::Divide(a, b) => {
            expr_vars(a, out);
            expr_vars(b, out);
        }
        E::UnaryPlus(a) | E::UnaryMinus(a) | E::Not(a) => expr_vars(a, out),
        E::In(a, l) => {
            expr_vars(a, out);
            l.iter().for_each(|x| expr_vars(x, out));
        }
        E::If(a, b, c) => {
            expr_vars(a, out);
            expr_vars(b, out);
            expr_vars(c, out);
        }
        E::Coalesce(l) | E::FunctionCall(_, l) => l.iter().for_each(|x| expr_vars(x, out)),
        E::Exists(p) => collect_pattern_vars(p, out),
        _ => {}
    }
}

/// A triple pattern without variables / blank nodes as an RDF 1.2 triple term.
pub fn ground_triple(tp: &TriplePattern) -> Option<oxrdf::Triple> {
    let term = |t: &TermPattern| -> Option<Term> {
        Some(match t {
            TermPattern::NamedNode(n) => Term::NamedNode(n.clone()),
            TermPattern::Literal(l) => Term::Literal(l.clone()),
            TermPattern::Triple(t) => Term::Triple(Box::new(ground_triple(t)?)),
            _ => return None,
        })
    };
    let s = match term(&tp.subject)? {
        Term::NamedNode(n) => oxrdf::NamedOrBlankNode::NamedNode(n),
        _ => return None,
    };
    let NamedNodePattern::NamedNode(p) = &tp.predicate else {
        return None;
    };
    Some(oxrdf::Triple::new(s, p.clone(), term(&tp.object)?))
}

/// A ground term (VALUES) as an RDF term.
pub fn ground_term(t: &GroundTerm) -> Term {
    match t {
        GroundTerm::NamedNode(n) => Term::NamedNode(n.clone()),
        GroundTerm::Literal(l) => Term::Literal(l.clone()),
        GroundTerm::Triple(t) => Term::Triple(Box::new(oxrdf::Triple::new(
            t.subject.clone(),
            t.predicate.clone(),
            ground_term(&t.object),
        ))),
    }
}

/// Compact term rendering for plan descriptions.
pub fn short(t: &Term) -> String {
    match t {
        Term::NamedNode(n) => {
            let s = n.as_str();
            for (p, ns) in [
                ("rdf:", "http://www.w3.org/1999/02/22-rdf-syntax-ns#"),
                ("rdfs:", "http://www.w3.org/2000/01/rdf-schema#"),
                ("owl:", "http://www.w3.org/2002/07/owl#"),
                ("xsd:", "http://www.w3.org/2001/XMLSchema#"),
            ] {
                if let Some(l) = s.strip_prefix(ns) {
                    return format!("{p}{l}");
                }
            }
            format!("<{s}>")
        }
        Term::Literal(l) if l.datatype() == xsd::INTEGER => l.value().to_string(),
        t => {
            let s = t.to_string();
            if s.len() > 60 {
                format!(
                    "{}…",
                    &s[..s.char_indices().nth(57).map_or(s.len(), |x| x.0)]
                )
            } else {
                s
            }
        }
    }
}

/// Evaluate `EXISTS { pattern }` for one outer binding (substitution semantics).
pub fn eval_exists(ctx: &Ctx, spec: &ExistsSpec, key: &[Id]) -> Result<bool> {
    let mut p = Planner::new(ctx);
    for (v, id) in spec.vars.iter().zip(key) {
        if !id.is_undef() {
            p.subst.insert(*v, *id);
        }
    }
    let graph = match &spec.graph {
        ActiveGraph::Var(v) => match p.subst.get(v) {
            Some(id) => ActiveGraph::Named(*id),
            None => spec.graph.clone(),
        },
        g => g.clone(),
    };
    let node = p.plan(&spec.pattern, &graph, Vec::new())?;
    let node = slice(node, 0, Some(1), ctx);
    let (t, _) = super::exec::execute(ctx, &node)?;
    Ok(!t.is_empty())
}

/// Literal helper for tests.
pub fn lit_int(i: i64) -> Term {
    Term::Literal(Literal::new_typed_literal(i.to_string(), xsd::INTEGER))
}
