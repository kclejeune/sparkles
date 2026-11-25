//! Query planner: spargebra algebra → physical operator tree.
//!
//! Join groups (BGPs, joins, `GRAPH` blocks, filters) are flattened into items and
//! ordered by a QLever-style dynamic program over connected components that keeps the
//! cheapest plan per (subset, sort order) ("interesting orders"), switching to a greedy
//! strategy for large groups. Filters are placed as soon as all their variables are
//! bound. Other operators follow the algebra, with Jena-style rewrites (filter
//! placement through OPTIONAL / UNION / MINUS / BIND, filter-equality substitution,
//! top-k for ORDER BY + LIMIT).

use super::ctx::{Charge, Ctx};
use super::exec::PlanPattern;
use super::expr::{ExistsSpec, Expr};
use super::table::{Table, VarId};
use crate::access::{
    Preparation, PreparationBuffer, PreparationMap, PreparationText, Reservation, sort_by_prepared,
};
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
use std::cell::OnceCell;
use std::hash::{Hash, Hasher};
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

#[derive(Clone, Debug)]
pub enum GraphFilter {
    /// no restriction (graph is an output column or the prefix)
    All,
    Default,
    /// any named graph (not the default graph)
    Named,
    One(u64),
    Set(Vec<u64>),
    PreparedSet(Arc<PreparedGraphSet>),
}

/// Canonical graph IDs retained by checked plan filters and their clones.
pub struct PreparedGraphSet {
    ids: Vec<u64>,
    _held: Reservation,
}

impl PreparedGraphSet {
    fn new(preparation: &Preparation, set: &[Id]) -> Result<Arc<Self>> {
        let bytes = (set.len() as u64)
            .checked_mul(size_of::<u64>() as u64)
            .and_then(|bytes| {
                bytes.checked_add((size_of::<Self>() + 2 * size_of::<usize>()) as u64)
            })
            .ok_or_else(|| preparation.exhausted())?;
        let held = preparation.reserve(bytes)?;
        let mut ids = Vec::new();
        ids.try_reserve_exact(set.len())
            .map_err(|_| preparation.exhausted())?;
        if ids.capacity() != set.len() {
            return Err(preparation.exhausted());
        }
        let mut ordered = true;
        for &id in set {
            preparation.check()?;
            if let Some(&last) = ids.last() {
                ordered &= last <= id.0;
                if last == id.0 {
                    continue;
                }
            }
            ids.push(id.0);
        }
        if !ordered {
            sort_by_prepared(&mut ids, preparation, |a, b| Ok(a.cmp(b)))?;
            let mut unique = 1;
            for at in 1..ids.len() {
                preparation.check()?;
                if ids[at] != ids[unique - 1] {
                    ids[unique] = ids[at];
                    unique += 1;
                }
            }
            ids.truncate(unique);
        }
        preparation.check()?;
        Ok(Arc::new(Self { ids, _held: held }))
    }
}

impl std::fmt::Debug for PreparedGraphSet {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        std::fmt::Debug::fmt(&self.ids, f)
    }
}

impl PartialEq for GraphFilter {
    fn eq(&self, other: &Self) -> bool {
        use GraphFilter::*;
        match (self, other) {
            (All, All) | (Default, Default) | (Named, Named) => true,
            (One(a), One(b)) => a == b,
            (Set(a), Set(b)) => a == b,
            (PreparedSet(a), PreparedSet(b)) => Arc::ptr_eq(a, b) || a.ids == b.ids,
            (Set(a), PreparedSet(b)) | (PreparedSet(b), Set(a)) => *a == b.ids,
            _ => false,
        }
    }
}

impl Eq for GraphFilter {}

impl Hash for GraphFilter {
    fn hash<H: Hasher>(&self, state: &mut H) {
        match self {
            Self::All => 0u8.hash(state),
            Self::Default => 1u8.hash(state),
            Self::Named => 2u8.hash(state),
            Self::One(id) => {
                3u8.hash(state);
                id.hash(state);
            }
            Self::Set(_) | Self::PreparedSet(_) => {
                4u8.hash(state);
                self.set_ids().hash(state);
            }
        }
    }
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
            GraphFilter::PreparedSet(s) => s.ids.binary_search(&g).is_ok(),
        }
    }

    pub(crate) fn set_ids(&self) -> Option<&[u64]> {
        match self {
            Self::Set(ids) => Some(ids),
            Self::PreparedSet(set) => Some(&set.ids),
            _ => None,
        }
    }

    /// Whether rows of several graphs can pass (so equal triples may repeat).
    pub fn multi(&self) -> bool {
        matches!(
            self,
            GraphFilter::All
                | GraphFilter::Named
                | GraphFilter::Set(_)
                | GraphFilter::PreparedSet(_)
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
    /// the triple pattern of the query the scan reads, for the plan's `patterns`
    pub src: Option<Arc<PlanPattern>>,
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
/// The column's ids are split into pieces. In a strictly monotone piece (inline integers,
/// inline decimals of one scale, inline doubles of one sign, the IRIs of the base
/// vocabulary) id order is value order, so the piece is read from its best end in
/// geometrically growing steps, and only its best `k` rows (with their ties) are kept.
/// In ascending order, the literals of the base vocabulary are read from their low end
/// too, until the prefixes of the last one read can no longer win (see
/// [`TopKPiece::lexical`]). Every other piece (delta terms, blank nodes, NaN, dates…) is
/// read whole. The candidates are ranked by
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
    /// The piece holds literals of the base vocabulary, read in ascending order from its
    /// low end. Their keys are `"`, the lexical form and a 0xFF byte, then any language
    /// tag or datatype, so ids follow the lexical forms except that a lexical form sorts
    /// after the forms it is a proper prefix of. A plain string read after the k-th
    /// candidate, a plain string itself, is therefore either greater than every string
    /// read or a proper prefix of the last one, and those prefixes are looked up in the
    /// vocabulary. A literal of another kind ranks after every plain string. When the
    /// k-th candidate is not a plain string, the generic plan answers.
    pub lexical: bool,
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
    pub registered: Option<std::sync::Arc<super::extensions::AggregateDescriptor>>,
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
    /// for index-based traversal of an alternative of predicates from a bound or
    /// constant end: each predicate with its direction, in place of an edge plan
    pub preds: Option<Vec<(u64, bool)>>,
    pub graph: GraphFilter,
    pub graph_var: Option<VarId>,
    /// edge variables of the child plan (src, dst) when not simple
    pub edge_vars: Option<(VarId, VarId)>,
    /// an ARQ path range `{min,max}` (`max` absent: unbounded), whose solutions are
    /// counted: one per walk, and per simple path after `min` steps when unbounded (see
    /// [`PropertyPathExpression::Range`]). `min` above then tells only whether the
    /// range can match a zero-length path.
    pub count: Option<(u64, Option<u64>)>,
    /// the path pattern of the query, for the plan's `patterns`
    pub src: Option<Arc<PlanPattern>>,
}

/// The admitted backing table for checked GRAPH name enumeration.
pub struct PreparedGraphNames {
    pub(super) table: Table,
    _held: Reservation,
}

/// A checked per-graph group. Planner alternatives share the immutable branches.
pub struct PreparedGraphBranches {
    pub(super) inputs: Vec<Node>,
    pub(super) contains_algorithm: bool,
    max_depth: usize,
    _held: Reservation,
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
    GraphNames(Arc<PreparedGraphNames>),
    GraphBranches(Arc<PreparedGraphBranches>),
    Empty,
    Join {
        algo: JoinAlgo,
        keys: Vec<VarId>,
    },
    LeftJoin {
        expr: Option<Expr>,
    },
    /// An OPTIONAL whose sides share no variable and whose condition has a conjunct
    /// `?left = ?right` over a variable of each side. The candidate pairs are the rows
    /// with the same id where either value is an IRI or a blank node, which no other
    /// value equals, and the pairs of other values. The whole condition is then tested
    /// on the candidates, so the result is the generic left join's.
    KeyedLeftJoin {
        expr: Expr,
        left: VarId,
        right: VarId,
    },
    /// The inner join of two parts of a group that share no variable, linked by a FILTER
    /// conjunct `?left = ?right` over a variable of each. The candidate pairs are those of
    /// [`Kind::KeyedLeftJoin`], and `expr`, the conjunction of the FILTERs that need both
    /// sides, is tested on them, so the result is the filtered product's. With `ripple`,
    /// set for an ASK that a cursor answers, both inputs are read alternately so that the
    /// first matching pair ends the query early (see `cursor::ripple`).
    KeyedJoin {
        expr: Expr,
        left: VarId,
        right: VarId,
        ripple: bool,
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
        /// The input runs whole, as the planner chose for a LIMIT without ORDER BY whose
        /// input is small ([`super::demand::runs_whole`]), rather than as cursor
        /// operators that stop at the rows the LIMIT takes. A cursor pulls it either way.
        whole: bool,
    },
    Group {
        keys: Vec<VarId>,
        aggs: Vec<(VarId, Agg)>,
    },
    /// `COUNT(*)` over a single scan answered from index metadata
    /// `COUNT(*)` over a join: counts matching pairs without materializing them. `keys`
    /// are the join's, so that a cursor can run the join itself and count its rows.
    CountJoin {
        algo: JoinAlgo,
        var: VarId,
        keys: Vec<VarId>,
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
    RegisteredProperty(Box<super::propertyext::Spec>),
    /// paths as solutions (`SERVICE path:search`); child 0, if any, is the rest of the
    /// group, which binds the source or the target
    PathSearch(Box<super::pathsearch::PathSearchSpec>),
    /// the recorded changes of the dataset (`SERVICE hist:changes`)
    HistoryChanges(Box<super::history_svc::HistorySpec>),
    /// the rows of a `table` view (`SERVICE view:read`)
    ViewRead(Box<super::viewsvc::ViewSpec>),
    /// a graph algorithm on a projection of the dataset (`SERVICE algo:pageRank`, …);
    /// child 0, if the call has a variable source, is the rest of the group, and the last
    /// child is the nested pattern of edges, if any
    AlgoCall(Box<super::algosvc::AlgoSpec>),
    /// `GROUP BY ?p` with subject counts over `?s ?p ?o`, read from the predicate
    /// patterns; child 0, if any, is the input that binds `?s` (see
    /// [`super::patternplan`])
    PatternCount(Box<super::patternplan::PatternCountSpec>),
    /// `spk:hasPredicate` read from the predicate patterns
    PatternScan(Box<super::patternplan::PatternScanSpec>),
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

    /// How many levels the plan nests, counted without recursion: 1 for a leaf. The code
    /// that plans, runs, clones and drops a plan recurses that deep.
    pub(super) fn depth(&self) -> usize {
        let mut deepest = 0;
        let mut todo = vec![(self, 1)];
        while let Some((n, d)) = todo.pop() {
            deepest = deepest.max(d);
            if let Kind::GraphBranches(branches) = &n.kind {
                let depth = if branches.inputs.len() == 1 { d - 1 } else { d };
                deepest = deepest.max(depth + branches.max_depth);
            } else {
                todo.extend(n.children.iter().map(|c| (c, d + 1)));
            }
        }
        deepest
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

    pub(super) fn graph_diagnostic_children(&self) -> &[Node] {
        match &self.kind {
            Kind::GraphBranches(branches) => &branches.inputs,
            Kind::OrderedTopK(top) => std::slice::from_ref(top.fallback.as_ref()),
            _ => &self.children,
        }
    }

    pub(super) fn graph_singleton(&self) -> Option<&Node> {
        if let Kind::GraphBranches(branches) = &self.kind
            && let [branch] = branches.inputs.as_slice()
        {
            Some(branch)
        } else {
            None
        }
    }

    pub fn operator(&self) -> &'static str {
        if let Some(branch) = self.graph_singleton() {
            return branch.operator();
        }
        match &self.kind {
            Kind::Scan(_) => "IndexScan",
            Kind::RangeScan(..) => "IndexRangeScan",
            Kind::OrderedTopK(_) => "IndexTopK",
            Kind::Values(_) => "Values",
            Kind::GraphNames(_) => "GraphNames",
            Kind::GraphBranches(_) => "GraphBranches",
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
            Kind::LeftJoin { .. } | Kind::KeyedLeftJoin { .. } => "OptionalJoin",
            Kind::KeyedJoin { .. } => "KeyedJoin",
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
            Kind::RegisteredProperty(_) => "RegisteredPropertyFunction",
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
            Kind::ViewRead(_) => "ViewRead",
            Kind::AlgoCall(_) => "AlgoCall",
            Kind::PatternCount(_) => "PatternCount",
            Kind::PatternScan(_) => "PatternScan",
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
    /// the pattern of the query it comes from, if any
    src: Option<Arc<PlanPattern>>,
}

// a Node is much larger than a triple pattern, but items are short-lived and few
#[allow(clippy::large_enum_variant)]
enum Item {
    Unpack(UnpackItem),
    Triple(Triple),
    Path(PathItem),
    Node(Node),
}

#[cfg(test)]
pub(super) fn graph_item_slots_bytes(slots: usize) -> usize {
    slots * size_of::<Item>()
}

/// A vector whose backing stays admitted until its slots are actually dropped.
enum PlanningVec<T> {
    Ordinary(Vec<T>),
    Checked(PreparationBuffer<T>),
}

impl<T> PlanningVec<T> {
    fn push(&mut self, value: T) -> Result<()> {
        match self {
            Self::Ordinary(values) => {
                values.push(value);
                Ok(())
            }
            Self::Checked(values) => {
                values.push_staged(value, Some("growing checked graph planning buffer"))
            }
        }
    }

    fn into_parts(self) -> (Vec<T>, Option<Reservation>) {
        match self {
            Self::Ordinary(values) => (values, None),
            Self::Checked(values) => {
                let (values, held) = values.into_parts();
                (values, Some(held))
            }
        }
    }

    fn remove(&mut self, index: usize) -> T {
        match self {
            Self::Ordinary(values) => values.remove(index),
            Self::Checked(values) => values.remove(index),
        }
    }
}

impl<T> std::ops::Deref for PlanningVec<T> {
    type Target = [T];

    fn deref(&self) -> &[T] {
        match self {
            Self::Ordinary(values) => values,
            Self::Checked(values) => values,
        }
    }
}

impl<T> std::ops::DerefMut for PlanningVec<T> {
    fn deref_mut(&mut self) -> &mut [T] {
        match self {
            Self::Ordinary(values) => values,
            Self::Checked(values) => values,
        }
    }
}

struct PlanningItems<T> {
    values: std::vec::IntoIter<T>,
    _held: Option<Reservation>,
}

impl<T> Iterator for PlanningItems<T> {
    type Item = T;

    fn next(&mut self) -> Option<T> {
        self.values.next()
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        self.values.size_hint()
    }
}

impl<T> ExactSizeIterator for PlanningItems<T> {}

impl<T> IntoIterator for PlanningVec<T> {
    type Item = T;
    type IntoIter = PlanningItems<T>;

    fn into_iter(self) -> Self::IntoIter {
        let (values, held) = self.into_parts();
        PlanningItems {
            values: values.into_iter(),
            _held: held,
        }
    }
}

struct PathItem {
    s: PT,
    path: PropertyPathExpression,
    o: PT,
    graph: ActiveGraph,
    src: Option<Arc<PlanPattern>>,
    _held: Option<Reservation>,
}

pub(super) const FILTER_SELECTIVITY: f64 = 0.3;
/// A pattern with one variable estimated at this many rows or fewer is read at planning
/// time, and bound to its value when it has one row (see `Planner::point_value`).
const POINT_ROWS: f64 = 4.0;

pub struct Planner<'a> {
    pub ctx: &'a Ctx,
    default_filter: OnceCell<Arc<PreparedGraphSet>>,
    named_filter: OnceCell<Arc<PreparedGraphSet>>,
    checked_exists: std::cell::RefCell<Option<super::checked_syntax::Sources>>,
    branch_payload: std::cell::RefCell<Option<Reservation>>,
    branch_shared_payload: std::cell::RefCell<Option<Reservation>>,
    pub subst: FxHashMap<VarId, Id>,
    /// The variables of `subst` that a sub-select hides unless it projects them: a
    /// `LATERAL`'s substitutions, where a variable a sub-select does not project is a
    /// different variable. Initial bindings and EXISTS substitute everywhere.
    pub scoped: FxHashSet<VarId>,
    pub(super) property_inputs: FxHashSet<VarId>,
    bnode_scope: u32,
    pub(super) view_patterns: Option<&'a super::viewsvc::ViewPatterns>,
    /// RDF 1.2 triple-term patterns created while translating triple patterns
    unpacks: std::cell::RefCell<Vec<UnpackItem>>,
    /// the whole query being planned, if known: a search output it mentions nowhere else
    /// is left out (see [`Planner::used_elsewhere`])
    pub source: Option<&'a spargebra::Query>,
    /// the source's SSE form, or `None` when every variable counts as used
    source_sse: std::cell::OnceCell<Option<String>>,
    /// how many times each pattern of the query has been planned, for its occurrence,
    /// keyed by the first pattern planned so that a lookup copies no text
    seen: std::cell::RefCell<FxHashMap<SeenPattern, u32>>,
    /// how many times each pattern occurs in the source
    source_counts: std::cell::OnceCell<FxHashMap<SeenPattern, u32>>,
    /// Live planning charge for speculative EXISTS planning so ordinary VALUES tables and
    /// unattached nested EXISTS syntax reserve memory before allocating.
    plan_charge: Option<&'a super::ctx::Charge<'a>>,
    pub(super) sample_filters: bool,
}

#[cfg(test)]
thread_local! {
    /// How many patterns the planners of this thread have named, for tests of when a plan
    /// names none.
    pub(super) static PATTERNS_NAMED: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// A pattern as a key of [`Planner::seen`] and [`Planner::source_counts`]: equal when
/// the subject, predicate or path, and object are, as [`PlanPattern`] writes them,
/// whatever the graph and occurrence.
struct SeenPattern(Arc<PlanPattern>);

impl SeenPattern {
    fn new(s: String, p: String, o: String) -> SeenPattern {
        SeenPattern(Arc::new(PlanPattern {
            s,
            p,
            o,
            graph: None,
            occurrence: 0,
        }))
    }
}

impl PartialEq for SeenPattern {
    fn eq(&self, other: &Self) -> bool {
        (&self.0.s, &self.0.p, &self.0.o) == (&other.0.s, &other.0.p, &other.0.o)
    }
}

impl Eq for SeenPattern {}

impl std::hash::Hash for SeenPattern {
    fn hash<H: std::hash::Hasher>(&self, h: &mut H) {
        (&self.0.s, &self.0.p, &self.0.o).hash(h);
    }
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
            default_filter: OnceCell::new(),
            named_filter: OnceCell::new(),
            checked_exists: Default::default(),
            branch_payload: Default::default(),
            branch_shared_payload: Default::default(),
            subst: FxHashMap::default(),
            scoped: FxHashSet::default(),
            property_inputs: FxHashSet::default(),
            bnode_scope: 0,
            view_patterns: None,
            unpacks: Default::default(),
            source: None,
            source_sse: Default::default(),
            seen: Default::default(),
            source_counts: Default::default(),
            plan_charge: None,
            sample_filters: ctx.opt.sampled_filters,
        }
    }

    pub(super) fn for_exists(ctx: &'a Ctx, spec: &'a ExistsSpec) -> Self {
        Self::for_exists_with_charge(ctx, spec, None)
    }

    pub(super) fn for_exists_with_charge(
        ctx: &'a Ctx,
        spec: &'a ExistsSpec,
        plan_charge: Option<&'a super::ctx::Charge<'a>>,
    ) -> Self {
        let mut planner = Self::new(ctx);
        planner.view_patterns = spec.view_patterns();
        planner.plan_charge = plan_charge;
        planner
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

    pub fn compile(&self, e: &Expression, graph: &ActiveGraph) -> Result<Expr> {
        let ctx = self.ctx;
        let subst = &self.subst;
        let exists = |p: &GraphPattern| -> Result<Arc<ExistsSpec>> {
            Ok(Arc::new(ExistsSpec::compile_ordinary_with_admission(
                ctx,
                p,
                self.view_patterns,
                graph.clone(),
                subst,
                self.plan_charge,
            )?))
        };
        let admit = |expr: &Expression| -> Result<()> {
            if let Some(held) = self.plan_charge {
                ctx.check()?;
                held.add(super::checked_layout::expression_compile_bytes(expr))?;
            }
            Ok(())
        };
        super::expr::compile_expression(ctx, subst, &exists, &admit, &|name| Ok(ctx.var(name)), e)
    }

    fn compile_in_branch(&self, e: &Expression, graph: &ActiveGraph) -> Result<Expr> {
        if self.checked_exists.borrow().is_none() {
            return self.compile(e, graph);
        }
        let ctx = self.ctx;
        let subst = &self.subst;
        let preparation = ctx
            .source_preparation
            .as_ref()
            .expect("checked graph planning");
        let exists = |p: &GraphPattern| -> Result<Arc<ExistsSpec>> {
            let pattern = self
                .checked_exists
                .borrow_mut()
                .as_mut()
                .expect("graph syntax registry")
                .get(p, preparation)?;
            let mut ids = PreparationBuffer::new(preparation)?;
            let mut status = Ok(());
            visit_pattern_vars(p, &mut |name| {
                if status.is_ok() {
                    status = ctx
                        .var_prepared(name, preparation)
                        .and_then(|id| ids.push(id));
                }
            });
            status?;
            sort_by_prepared(&mut ids, preparation, |a, b| Ok(a.cmp(b)))?;
            ids.dedup();
            let bound_len = ids.iter().filter(|id| subst.contains_key(id)).count();
            let held = ExistsSpec::reserve_checked(preparation, ids.len(), bound_len)?;
            let view_patterns = match self.view_patterns {
                Some(vp) => vp.project_checked(p, &pattern.syntax, preparation)?.map(
                    |(projected, charge)| {
                        Arc::new(super::expr::RetainedViewPatterns::from_reservation(
                            projected, charge,
                        ))
                    },
                ),
                None => None,
            };
            let mut vars = Vec::with_capacity(ids.len());
            let mut bound = Vec::with_capacity(bound_len);
            for &id in &*ids {
                preparation.check()?;
                vars.push(id);
                if let Some(value) = subst.get(&id) {
                    bound.push((id, *value));
                }
            }
            Ok(Arc::new(ExistsSpec::new_checked(
                pattern,
                graph.clone(),
                vars,
                bound,
                view_patterns,
                held,
            )))
        };
        super::expr::compile_expression(
            ctx,
            subst,
            &exists,
            &|expr| self.admit_expression(expr),
            &|name| ctx.var_prepared(name, preparation),
            e,
        )
    }

    pub(super) fn planning_scratch(
        &self,
        measure: impl FnOnce(&mut super::checked_layout::Layout<'_>) -> Result<()>,
    ) -> Result<Option<Reservation>> {
        let active = self.branch_payload.borrow();
        let Some(held) = active.as_ref() else {
            return Ok(None);
        };
        let preparation = held.preparation();
        let mut layout = super::checked_layout::Layout::new(preparation);
        measure(&mut layout)?;
        preparation.reserve(layout.bytes()).map(Some)
    }

    /// Keep `held`, the credit for storage that the plan being made keeps, for as long
    /// as the checked branch's other plan storage.
    pub(super) fn retain_planning(&self, held: Reservation) {
        match self.branch_payload.borrow_mut().as_mut() {
            Some(credit) => credit.merge(held),
            None => drop(held),
        }
    }

    /// Plan `gp` on the default graph as a checked graph branch is planned, with fresh
    /// credit from `preparation`, for the tests that compare the two routes.
    #[cfg(test)]
    pub(super) fn plan_admitted_for_test(
        &mut self,
        gp: &GraphPattern,
        preparation: &Preparation,
    ) -> Result<Node> {
        let previous = self.branch_payload.replace(Some(preparation.reserve(0)?));
        let previous_shared = self
            .branch_shared_payload
            .replace(Some(preparation.reserve(0)?));
        let node = self.plan(gp, &ActiveGraph::Default, Vec::new());
        self.branch_payload.replace(previous);
        self.branch_shared_payload.replace(previous_shared);
        node
    }

    /// Report a checked planning phase, which rechecks the request's controls. The
    /// ordinary route reports nothing.
    pub(super) fn planning_stage(&self, phase: &str) -> Result<()> {
        let preparation = match self.branch_payload.borrow().as_ref() {
            Some(held) => held.preparation().clone(),
            None => return Ok(()),
        };
        preparation.stage(phase)
    }

    fn planning_vec<T>(&self) -> Result<PlanningVec<T>> {
        if let Some(held) = self.branch_payload.borrow().as_ref() {
            Ok(PlanningVec::Checked(PreparationBuffer::new(
                held.preparation(),
            )?))
        } else {
            Ok(PlanningVec::Ordinary(Vec::new()))
        }
    }

    fn admit_payload(
        &self,
        measure: impl FnOnce(&mut super::checked_layout::Layout<'_>) -> Result<()>,
    ) -> Result<()> {
        if let Some(credit) = self.branch_payload.borrow_mut().as_mut() {
            let mut layout = super::checked_layout::Layout::new(credit.preparation());
            measure(&mut layout)?;
            let bytes = layout.bytes();
            credit.grow_by(bytes)?;
        }
        Ok(())
    }

    pub(super) fn plan_scratch(
        &self,
        measure: impl FnOnce(&mut super::checked_layout::Layout<'_>) -> Result<()>,
    ) -> Result<Option<Charge<'a>>> {
        let Some(held) = self.plan_charge else {
            return Ok(None);
        };
        self.ctx.check()?;
        let mut layout = super::checked_layout::Layout::unchecked(false);
        measure(&mut layout)?;
        let part = held.part();
        part.add(layout.bytes())?;
        Ok(Some(part))
    }

    pub(super) fn retain_plan_scratch(&self, mut scratch: Option<Charge<'_>>) -> Result<()> {
        if let Some(held) = self.plan_charge
            && scratch.is_some()
        {
            held.adopt_charge(&mut scratch)?;
        }
        Ok(())
    }

    fn plan_filter_copy(&self, filters: &[Expr]) -> Result<Option<Charge<'a>>> {
        if filters.is_empty() {
            return Ok(None);
        }
        self.plan_scratch(|layout| {
            layout.slice(filters)?;
            for filter in filters {
                layout.expression(filter)?;
            }
            Ok(())
        })
    }

    fn admit_expression(&self, expr: &Expression) -> Result<()> {
        use Expression as E;
        if let E::Literal(literal) = expr
            && let Some(credit) = self.branch_shared_payload.borrow_mut().as_mut()
        {
            // The Value built for a literal holds its lexeme and optional type
            // or language in Arcs. Join alternatives share these payloads.
            let mut layout = super::checked_layout::Layout::new(credit.preparation());
            layout.add(literal.value().len())?;
            layout.add(literal.datatype().as_str().len())?;
            layout.add(4 * size_of::<usize>())?;
            if let Some(lang) = literal.language() {
                layout.add(lang.len())?;
            }
            let bytes = layout.bytes();
            credit.grow_by(bytes)?;
        }
        self.admit_payload(|layout| {
            let boxed = match expr {
                E::Or(..)
                | E::And(..)
                | E::Equal(..)
                | E::SameTerm(..)
                | E::Greater(..)
                | E::GreaterOrEqual(..)
                | E::Less(..)
                | E::LessOrEqual(..)
                | E::Add(..)
                | E::Subtract(..)
                | E::Multiply(..)
                | E::Divide(..) => 2,
                E::UnaryPlus(_) | E::UnaryMinus(_) | E::Not(_) | E::In(..) => 1,
                E::If(..) => 3,
                E::NamedNode(_)
                | E::Literal(_)
                | E::Variable(_)
                | E::Exists(_)
                | E::Bound(_)
                | E::Coalesce(_)
                | E::FunctionCall(..) => 0,
            };
            layout.slots::<Expr>(boxed)?;
            let args = match expr {
                E::In(_, args) | E::Coalesce(args) | E::FunctionCall(_, args) => args.len(),
                _ => 0,
            };
            if args > 0 {
                // Fallible collection grows geometrically. At growth the old
                // allocation can coexist with its replacement.
                let slots = args
                    .checked_next_power_of_two()
                    .and_then(|size| size.max(4).checked_mul(2))
                    .ok_or_else(|| self.ctx.memory_exceeded(u64::MAX))?;
                layout.slots::<Expr>(slots)?;
            }
            match expr {
                E::NamedNode(node) => layout.add(node.as_str().len()),
                E::Literal(literal) => {
                    layout.add(literal.value().len())?;
                    layout.add(literal.datatype().as_str().len())?;
                    if let Some(lang) = literal.language() {
                        layout.add(lang.len())?;
                    }
                    Ok(())
                }
                E::FunctionCall(spargebra::algebra::Function::Custom(iri), _) => {
                    layout.add(iri.as_str().len())
                }
                _ => Ok(()),
            }
        })
    }

    fn branch_description(
        &self,
        write: impl FnOnce(&mut dyn std::fmt::Write) -> std::fmt::Result,
    ) -> Result<String> {
        if let Some(credit) = self.branch_payload.borrow_mut().as_mut() {
            let mut text = PreparationText::new(credit.preparation())?;
            let written = write(&mut text);
            self.ctx.check()?;
            let (desc, held) = text.finish()?;
            if written.is_err() {
                drop(desc);
                return Err(Error::invalid("could not write a graph branch description"));
            }
            credit.merge(held);
            return Ok(desc);
        }
        let mut text = String::new();
        write(&mut text).expect("writing to String");
        Ok(text)
    }

    fn branch_var(&self, name: &str) -> Result<VarId> {
        if self.branch_payload.borrow().is_some() {
            let preparation = self.ctx.source_preparation.as_ref().expect("checked GRAPH");
            self.ctx.var_prepared(name, preparation)
        } else {
            Ok(self.ctx.var(name))
        }
    }

    fn union_for_branch(&self, children: PlanningVec<Node>) -> Result<Node> {
        let (children, storage) = children.into_parts();
        let Some(storage) = storage else {
            return Ok(union(children));
        };
        if children.len() < 2 {
            let result = union(children);
            drop(storage);
            return Ok(result);
        }
        let checked = checked_union_shape(&children, storage.preparation());
        let (mut out, shape) = match checked {
            Ok(value) => value,
            Err(error) => {
                drop(children);
                return Err(error);
            }
        };
        let mut active = self.branch_payload.borrow_mut();
        let held = active.as_mut().expect("checked graph union");
        held.merge(storage);
        held.merge(shape);
        out.children = children;
        Ok(out)
    }

    fn project_for_branch(&self, child: Node, vars: Vec<VarId>) -> Result<Node> {
        if self.branch_payload.borrow().is_none() {
            return Ok(project(child, vars, self.ctx));
        }
        self.admit_payload(|layout| {
            layout.metadata(&child)?;
            layout.slots::<Node>(1)?;
            layout.slots::<VarId>(vars.len())?;
            layout.slots::<VarId>(child.sorted.len())
        })?;
        let desc = self.branch_description(|text| {
            let names = self.ctx.var_names.read();
            for (index, var) in vars.iter().enumerate() {
                if index != 0 {
                    text.write_char(' ')?;
                }
                write!(text, "?{}", names[*var as usize])?;
            }
            Ok(())
        })?;
        Ok(project_described(child, vars, desc))
    }

    fn admit_binding_shape(&self, child: &Node, new_vars: usize) -> Result<()> {
        self.admit_payload(|layout| {
            layout.metadata(child)?;
            layout.slots::<Node>(1)?;
            let max = child
                .vars
                .len()
                .checked_add(new_vars)
                .and_then(|len| len.max(4).checked_mul(2))
                .ok_or_else(|| self.ctx.memory_exceeded(u64::MAX))?;
            layout.slots::<VarId>(max)?;
            layout.map::<VarId, f64>(max)
        })
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
                let (push, top, _push_list) =
                    self.split_filters(filters, |v| certain.contains(v))?;
                let l = self.plan(left, g, push)?;
                let n = super::enhancer::plan_loop(self, l, right, g, None)?;
                self.apply_filters(n, top)
            }
            GP::LeftJoin {
                left,
                right,
                expression,
            } if super::enhancer::is_loop(right) => {
                let certain = certain_vars(left, self.ctx);
                let (push, top, _push_list) =
                    self.split_filters(filters, |v| certain.contains(v))?;
                let l = self.plan(left, g, push)?;
                let n = super::enhancer::plan_loop(self, l, right, g, Some(expression.clone()))?;
                self.apply_filters(n, top)
            }
            GP::Bgp { .. } | GP::Path { .. } | GP::Join { .. } | GP::Graph { .. } => {
                // a constant table joined with a UNION goes into each branch, where it
                // binds the branch's lookups instead of meeting their whole result
                let calls = |ctx: &Ctx, patterns: &[spargebra::term::TriplePattern]| {
                    super::propertyext::has_calls(ctx, patterns)
                        || patterns.iter().any(|t| match &t.predicate {
                            spargebra::term::NamedNodePattern::NamedNode(p) => {
                                super::catalog::property_functions()
                                    .iter()
                                    .any(|f| f == p.as_str())
                            }
                            _ => false,
                        })
                };
                // the named graphs a table moving into `GRAPH ?g` is copied into, one
                // plan of the inner pattern for each
                let graphs = || {
                    if self.ctx.selector_default_only {
                        return 0;
                    }
                    if self.ctx.source_preparation.is_some() {
                        // Suppress this speculative rewrite without enumerating named graphs.
                        return usize::MAX;
                    }
                    self.ctx.snap.graph_ids().map_or(usize::MAX, |ids| {
                        ids.iter()
                            .filter(|gid| {
                                self.ctx
                                    .dataset
                                    .named
                                    .as_ref()
                                    .is_none_or(|s| s.contains(gid))
                            })
                            .count()
                    })
                };
                // These rewrites clone algebra. View rebuilds plan the saved occurrences
                // whose semantics the incremental compiler recorded.
                if self.view_patterns.is_none()
                    && let Some(pushed) = super::valuespush::rewrite(
                        gp,
                        &|patterns| calls(self.ctx, patterns),
                        &graphs,
                    )
                {
                    return self.plan(&pushed, g, filters);
                }
                let consider_union = self.view_patterns.is_none() && !self.ctx.calls_extensions
                    // Preserve outer filters' ordinary evaluation boundary, including
                    // extension aliases and dynamic calls that might be volatile.
                    && filters.is_empty();
                let mut items = self.planning_vec()?;
                self.collect(gp, g, &mut items)?;
                let original = self.plan_group(items, filters)?;
                if consider_union
                    && super::unionpush::screen(gp, &original)
                    && let Some(candidate) =
                        super::unionpush::candidate(gp, &|patterns| calls(self.ctx, patterns))
                    && candidate.worth_planning(&original)
                {
                    // Planning a small pure anchor does not execute it. Keep broad
                    // inputs outside the UNION instead of repeating their reads.
                    #[cfg(test)]
                    super::unionpush::alternate_planned();
                    let anchor = self.plan(candidate.anchor, g, Vec::new())?;
                    if anchor.est.is_finite() && anchor.est <= super::unionpush::MAX_ANCHOR_ROWS {
                        let distributed = self.plan(&candidate.expand(), g, Vec::new())?;
                        // Few output rows may still require expensive intersections.
                        // Include those repeated reads, not just the anchor's size.
                        return Ok(
                            if distributed.cost.is_finite() && distributed.cost < original.cost {
                                distributed
                            } else {
                                original
                            },
                        );
                    }
                }
                Ok(original)
            }
            GP::Lateral { left, right } if super::lateral::join_equivalent(self, left, right) => {
                let mut items = self.planning_vec()?;
                self.collect(gp, g, &mut items)?;
                self.plan_group(items, filters)
            }
            GP::Lateral { left, right } => {
                // filters on variables every left row binds keep their value in the
                // merged rows, as the right side cannot bind them differently
                let certain = certain_vars(left, self.ctx);
                let (push, top, _push_list) =
                    self.split_filters(filters, |v| certain.contains(v))?;
                let l = self.plan(left, g, push)?;
                let n = super::lateral::plan(self, l, right, g)?;
                self.apply_filters(n, top)
            }
            GP::Filter { expr, inner } if self.ctx.calls_extensions => {
                // Keep application calls at their algebra operator, including AND
                // short-circuiting. No sampling, conjunct reordering or pushdown.
                let child = self.plan(inner, g, Vec::new())?;
                let expression = self.compile_in_branch(expr, g)?;
                // On the checked route the list of one is admitted before it is made.
                let (single, _single) = if self.branch_payload.borrow().is_none() {
                    (vec![expression], list_credit(None))
                } else {
                    let mut single = Vec::new();
                    let credit = self.grow_filters(&mut single, 1, 6, |s| s.push(expression))?;
                    (single, credit)
                };
                let node = self.apply_filters(child, single)?;
                self.apply_filters(node, filters)
            }
            GP::Filter { expr, inner } => {
                let mut fs = filters;
                let compiled = self.compile_in_branch(expr, g)?;
                if self.branch_payload.borrow().is_none() {
                    fs.extend(compiled.conjuncts());
                    return self.plan(inner, g, fs);
                }
                // The conjuncts join the list in place, which grows to its size first.
                let more = compiled.conjunct_count();
                let _grown =
                    self.grow_filters(&mut fs, more, 0, |fs| compiled.conjuncts_into(fs))?;
                self.plan(inner, g, fs)
            }
            GP::LeftJoin {
                left,
                right,
                expression,
            } if super::arqpf::reads_left(self, left, right)
                || super::propertyext::reads_left(self, left, right) =>
            {
                // ARQ evaluates the OPTIONAL per left row, so that the property functions
                // of the right side read the left side's values
                let certain = certain_vars(left, self.ctx);
                let (push, top, _push_list) =
                    self.split_filters(filters, |v| certain.contains(v))?;
                let l = self.plan(left, g, push)?;
                let n = super::lateral::plan_optional(self, l, right, expression.as_ref(), g)?;
                self.apply_filters(n, top)
            }
            GP::LeftJoin {
                left,
                right,
                expression,
            } => {
                let certain = certain_vars(left, self.ctx);
                let (push, top, _push_list) =
                    self.split_filters(filters, |v| certain.contains(v))?;
                let l = self.plan(left, g, push)?;
                let r = self.plan(right, g, Vec::new())?;
                let expr = expression
                    .as_ref()
                    .map(|e| self.compile_in_branch(e, g))
                    .transpose()?;
                let n = left_join(l, r, expr, self.ctx);
                self.apply_filters(n, top)
            }
            GP::Union { .. } => {
                let mut branches = self.planning_vec()?;
                flatten_union(gp, &mut branches)?;
                let mut children = self.planning_vec()?;
                let mut filters = filters;
                let checked = self.branch_payload.borrow().is_some();
                let mut iter = branches.into_iter().peekable();
                while let Some(branch) = iter.next() {
                    let copy;
                    let branch_filters = if !checked && iter.peek().is_none() {
                        copy = None;
                        std::mem::take(&mut filters)
                    } else {
                        self.admit_payload(|layout| layout.expressions(&filters))?;
                        copy = self.plan_filter_copy(&filters)?;
                        filters.clone()
                    };
                    let child = self.plan(branch, g, branch_filters)?;
                    if !child.is_empty() {
                        children.push(child)?;
                        self.retain_plan_scratch(copy)?;
                    }
                }
                self.union_for_branch(children)
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
                let v = self.branch_var(variable.as_str())?;
                let (push, top, _push_list) = self.split_filters(filters, |x| *x != v)?;
                let child = self.plan(inner, g, push)?;
                let e = self.compile_in_branch(expression, g)?;
                self.admit_binding_shape(&child, 1)?;
                let desc = self.branch_description(|text| {
                    write!(text, "?{} := ", variable.as_str())?;
                    e.write_description(self.ctx, text)
                })?;
                let mut n = Node::unary(Kind::Extend(v, e), child, desc);
                n.vars.push(v);
                n.dist.insert(v, n.est);
                self.apply_filters(n, top)
            }
            GP::Assign {
                inner,
                variable,
                expression,
            } => {
                let v = self.branch_var(variable.as_str())?;
                let (push, top, _push_list) = self.split_filters(filters, |x| *x != v)?;
                let child = self.plan(inner, g, push)?;
                let e = self.compile_in_branch(expression, g)?;
                self.admit_binding_shape(&child, 1)?;
                let desc = self.branch_description(|text| {
                    write!(text, "?{} := ", variable.as_str())?;
                    e.write_description(self.ctx, text)
                })?;
                if let Some(&c) = self.subst.get(&v) {
                    // the outer solution binds ?v (a LATERAL or EXISTS substitution): a
                    // different value drops the solution, and an error keeps it
                    let n = {
                        #[cfg(test)]
                        let _stage = crate::test_alloc::Stage::within(6);
                        // the test's list of two and its two boxes, and the FILTER's list
                        let built = self.planning_scratch(|layout| layout.slots::<Expr>(5))?;
                        let keep = Expr::Coalesce(vec![
                            Expr::Eq(Box::new(e), Box::new(Expr::Const(c))),
                            Expr::Const(Id::from_bool(true)),
                        ]);
                        let filters = vec![keep];
                        if let Some(built) = built {
                            self.retain_planning(built);
                        }
                        self.filter_admitted(child, filters)?
                    };
                    return self.apply_filters(n, top);
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
                self.apply_filters(n, top)
            }
            GP::Unfold {
                inner,
                expression,
                variable,
                second,
            } => {
                let v = self.branch_var(variable.as_str())?;
                let w = second
                    .as_ref()
                    .map(|s| self.branch_var(s.as_str()))
                    .transpose()?;
                let (push, top, _push_list) =
                    self.split_filters(filters, |x| *x != v && Some(*x) != w)?;
                let child = self.plan(inner, g, push)?;
                let e = self.compile_in_branch(expression, g)?;
                self.admit_binding_shape(&child, 1 + usize::from(second.is_some()))?;
                let desc = self.branch_description(|text| {
                    e.write_description(self.ctx, text)?;
                    write!(text, " AS ?{}", variable.as_str())?;
                    if let Some(second) = second {
                        write!(text, ", ?{}", second.as_str())?;
                    }
                    Ok(())
                })?;
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
                self.apply_filters(n, top)
            }
            GP::Values {
                variables,
                bindings,
            } => {
                let native = self
                    .view_patterns
                    .is_some_and(|patterns| patterns.contains(gp));
                if self.branch_payload.borrow().is_some() {
                    let values = self.checked_values(variables, bindings, native)?;
                    return self.apply_filters(values, filters);
                }
                let vars: Vec<VarId> = variables.iter().map(|v| self.ctx.var(v.as_str())).collect();
                let width = vars.len();
                let rows = bindings.len();
                let table_bytes = (rows as u64)
                    .saturating_mul((width as u64).saturating_mul(std::mem::size_of::<Id>() as u64))
                    .saturating_add(
                        (width as u64).saturating_mul(std::mem::size_of::<Vec<Id>>() as u64),
                    )
                    .saturating_add(
                        (width as u64).saturating_mul(std::mem::size_of::<VarId>() as u64),
                    );
                if let Some(held) = self.plan_charge {
                    held.add(table_bytes)?;
                }
                let mut t = Table::new(vars.clone());
                if self.plan_charge.is_some() {
                    for col in &mut t.cols {
                        col.try_reserve_exact(rows)
                            .map_err(|_| self.ctx.memory_exceeded(table_bytes))?;
                    }
                }
                // a term repeated in the table is looked up once
                let mut seen: FxHashMap<&GroundTerm, Id> = FxHashMap::default();
                let seen_charge = self.plan_charge.map(|_| self.ctx.charge(0)).transpose()?;
                let mut seen_full = false;
                let mut row_ids = Vec::with_capacity(width);
                for (row_idx, row) in bindings.iter().enumerate() {
                    if row_idx % 1024 == 0 {
                        self.ctx.check()?;
                    }
                    row_ids.clear();
                    for term in row {
                        let id = match term {
                            None => Id::UNDEF,
                            Some(term) => {
                                if let Some(&id) = seen.get(term) {
                                    id
                                } else {
                                    let id = match term {
                                        GroundTerm::NamedNode(n) => {
                                            self.ctx.intern_term(&Term::NamedNode(n.clone()))
                                        }
                                        GroundTerm::Literal(l) if !native => {
                                            self.ctx.intern_query_literal(l)
                                        }
                                        _ => self.ctx.intern_term(&ground_term(term)),
                                    };
                                    if !seen_full {
                                        if let Some(charge) = &seen_charge {
                                            let cap = seen.capacity();
                                            let grown = sparkles_budget::sizes::table_grown(
                                                cap,
                                                seen.len(),
                                                1,
                                            );
                                            if grown > cap {
                                                let slot = std::mem::size_of::<(&GroundTerm, Id)>();
                                                let old_bytes = if cap == 0 {
                                                    0
                                                } else {
                                                    sparkles_budget::sizes::table_bytes(cap, slot)
                                                        as u64
                                                };
                                                let new_bytes = sparkles_budget::sizes::table_bytes(
                                                    grown, slot,
                                                )
                                                    as u64;
                                                let peak = old_bytes.saturating_add(new_bytes);
                                                if charge.resize(peak).is_ok() {
                                                    seen.insert(term, id);
                                                    let _ = charge.resize(new_bytes);
                                                } else {
                                                    seen_full = true;
                                                }
                                            } else {
                                                seen.insert(term, id);
                                            }
                                        } else {
                                            seen.insert(term, id);
                                        }
                                    }
                                    id
                                }
                            }
                        };
                        row_ids.push(id);
                    }
                    t.push_row(&row_ids);
                }
                drop(seen);
                drop(seen_charge);
                self.ctx.check()?;
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
                self.apply_filters(n, filters)
            }
            GP::OrderBy { inner, expression } => {
                let child = self.plan(inner, g, Vec::new())?;
                let mut bound = self.planning_vec()?;
                if self.ctx.opt.bound_order_keys {
                    for (var, rhs) in bound_expressions(self, inner)? {
                        if !self.subst.contains_key(&self.branch_var(var.as_str())?) {
                            bound.push((var, rhs))?;
                        }
                    }
                }
                self.admit_payload(|layout| {
                    layout.slots::<(Expr, bool)>(expression.len())?;
                    layout.metadata(&child)?;
                    layout.slots::<Node>(1)
                })?;
                let mut keys = Vec::with_capacity(expression.len());
                for order in expression {
                    let (expr, asc) = match order {
                        OrderExpression::Asc(e) => (e, true),
                        OrderExpression::Desc(e) => (e, false),
                    };
                    let e = match bound.iter().find(|(_, rhs)| *rhs == expr) {
                        Some((var, _)) => {
                            let var = self.branch_var(var.as_str())?;
                            self.subst
                                .get(&var)
                                .map_or(Expr::Var(var), |id| Expr::Const(*id))
                        }
                        None => self.compile_in_branch(expr, g)?,
                    };
                    keys.push((e, asc));
                }
                let desc = self.branch_description(|text| {
                    for (index, (key, asc)) in keys.iter().enumerate() {
                        if index != 0 {
                            text.write_char(' ')?;
                        }
                        text.write_str(if *asc { "ASC(" } else { "DESC(" })?;
                        key.write_description(self.ctx, text)?;
                        text.write_char(')')?;
                    }
                    Ok(())
                })?;
                let mut n = Node::unary(Kind::OrderBy { keys, limit: None }, child, desc);
                n.sorted.clear();
                n.cost += n.est * n.est.max(2.0).log2() * 0.5;
                self.apply_filters(n, filters)
            }
            GP::Project { inner, variables } => {
                // sub-select: fresh scope for blank node labels
                self.bnode_scope += 1;
                self.admit_payload(|layout| layout.slots::<VarId>(variables.len()))?;
                let mut vars = Vec::with_capacity(variables.len());
                for variable in variables {
                    vars.push(self.branch_var(variable.as_str())?);
                }
                // A LATERAL's substitution does not reach variables hidden by the sub-select.
                let mut hidden = self.planning_vec()?;
                for v in self.scoped.iter().filter(|v| !vars.contains(v)) {
                    if let Some(id) = self.subst.get(v) {
                        hidden.push((*v, *id))?;
                    }
                }
                let mut hidden_inputs = self.planning_vec()?;
                for v in self
                    .property_inputs
                    .iter()
                    .filter(|v| self.scoped.contains(v) && !vars.contains(v))
                {
                    hidden_inputs.push(*v)?;
                }
                for (var, _) in hidden.iter() {
                    self.subst.remove(var);
                }
                for var in hidden_inputs.iter() {
                    self.property_inputs.remove(var);
                }
                let child = self.plan(inner, g, Vec::new());
                self.property_inputs.extend(hidden_inputs);
                self.subst.extend(hidden);
                let n = self.project_for_branch(child?, vars)?;
                self.apply_filters(n, filters)
            }
            GP::Reduced { inner }
                if self
                    .view_patterns
                    .is_none_or(|patterns| !patterns.contains(gp)) =>
            {
                let child = self.plan(inner, g, Vec::new())?;
                self.apply_filters(child, filters)
            }
            GP::Distinct { inner } | GP::Reduced { inner } => {
                // SELECT DISTINCT ?p over ?s ?p ?o: the predicates the patterns list
                if let GP::Project {
                    inner: pinner,
                    variables,
                } = &**inner
                    && variables.len() == 1
                    && self.subst.is_empty()
                {
                    self.bnode_scope += 1;
                    if let Some(n) = self.pattern_count(pinner, variables, &[], g)? {
                        return self.apply_filters(n, filters);
                    }
                }
                let child = self.plan(inner, g, Vec::new())?;
                let mut n = Node::unary(Kind::Distinct, child, String::new());
                n.est = n.children[0].est; // unknown reduction
                if self.ctx.spill.is_some() {
                    n = super::demand::distinct_below_order(self.ctx, n);
                }
                self.apply_filters(n, filters)
            }
            GP::Slice {
                inner,
                start,
                length,
            } => {
                let mut child = self.plan(inner, g, Vec::new())?;
                super::demand::plan_slice(self.ctx, &mut child, *start, *length);
                let n = slice(child, *start, *length, self.ctx);
                self.apply_filters(n, filters)
            }
            GP::Group {
                inner,
                variables,
                aggregates,
            } => {
                if let Some(n) = self.pattern_count(inner, variables, aggregates, g)? {
                    return self.apply_filters(n, filters);
                }
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
                                registered: None,
                            },
                            AggregateExpression::FunctionCall {
                                name,
                                expr,
                                distinct,
                            } => Agg {
                                func: name.clone(),
                                expr: Some(self.compile_in_branch(expr, g)?),
                                distinct: *distinct,
                                fold: None,
                                registered: match name {
                                    AggregateFunction::Custom(iri) => self
                                        .ctx
                                        .extensions
                                        .as_ref()
                                        .and_then(|r| r.aggregate(iri.as_str()))
                                        .cloned(),
                                    _ => None,
                                },
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
                                expr: Some(self.compile_in_branch(expr, g)?),
                                distinct: *distinct,
                                fold: Some(Box::new(Fold {
                                    value: value
                                        .as_ref()
                                        .map(|e| self.compile_in_branch(e, g))
                                        .transpose()?,
                                    order: order
                                        .iter()
                                        .map(|o| match o {
                                            OrderExpression::Asc(e) => {
                                                Ok((self.compile_in_branch(e, g)?, true))
                                            }
                                            OrderExpression::Desc(e) => {
                                                Ok((self.compile_in_branch(e, g)?, false))
                                            }
                                        })
                                        .collect::<Result<_>>()?,
                                })),
                                registered: None,
                            },
                        };
                        Ok((self.ctx.var(v.as_str()), agg))
                    })
                    .collect::<Result<_>>()?;
                let n = group(child, keys, aggs, self.ctx);
                self.apply_filters(n, filters)
            }
            GP::Service { inner, name, .. } if super::viewsvc::is_read(name) => {
                let n = super::viewsvc::view_leaf(self, inner)?;
                self.apply_filters(n, filters)
            }
            GP::Service { inner, name, .. } if super::history_svc::is_changes(name) => {
                let n = super::history_svc::history_leaf(self, inner)?;
                self.apply_filters(n, filters)
            }
            GP::Service { name, inner, .. } if super::algosvc::is_algo(name) => {
                let n = super::algosvc::algo_leaf(self, name, inner, g)?;
                self.apply_filters(n, filters)
            }
            GP::Service { name, inner, .. } if super::pathsearch::is_search(name) => {
                let n = super::pathsearch::path_search_leaf(self, inner, g)?;
                self.apply_filters(n, filters)
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
                self.apply_filters(n, filters)
            }
        }
    }

    /// The filters without EXISTS whose variables all pass `ok`, which move below an
    /// operator, and the rest, which stay above it. On the checked route the first list
    /// is admitted before it is made, to its size, the rest stay in `filters`' own list,
    /// and the credit returned holds the first list while the caller plans with it.
    fn split_filters(
        &self,
        filters: Vec<Expr>,
        ok: impl Fn(&VarId) -> bool,
    ) -> Result<(Vec<Expr>, Vec<Expr>, ListCredit)> {
        if self.branch_payload.borrow().is_none() || filters.is_empty() {
            let (push, top) = filters
                .into_iter()
                .partition(|f| filter_binds(f, true, |v| ok(&v)));
            return Ok((push, top, list_credit(None)));
        }
        let mut top = filters;
        // The same test without allocating: every occurrence of a variable passes `ok`.
        let (push, credit) = self.extract_filters(&mut top, |f| {
            !f.has_exists()
                && f.visit_vars(&mut |v| if ok(&v) { Ok(()) } else { Err(()) })
                    .is_ok()
        })?;
        #[cfg(test)]
        if credit.is_some() {
            note_list(1, push.len());
        }
        Ok((push, top, list_credit(credit)))
    }

    /// [`defer_open_filters`] on the checked route: the deferred filters move into a list
    /// made to their number and admitted before it is made, and the rest stay in
    /// `filters`' own list. The credit returned holds the deferred list.
    fn defer_open_filters_checked(
        &self,
        filters: &mut Vec<Expr>,
        triples: &[Triple],
        paths: &[PathItem],
        nodes: &[Node],
        unpacks: &[UnpackItem],
    ) -> Result<(Vec<Expr>, ListCredit)> {
        if filters.is_empty() || !may_defer(nodes) {
            return Ok((Vec::new(), list_credit(None)));
        }
        // The same test without allocating: a variable of the filter is open.
        let (deferred, credit) = self.extract_filters(filters, |f| {
            f.visit_vars(&mut |v| {
                if open_filter_var(v, triples, paths, nodes, unpacks) {
                    Err(())
                } else {
                    Ok(())
                }
            })
            .is_err()
        })?;
        #[cfg(test)]
        if let Some(credit) = &credit {
            note_list(2, deferred.len());
            watch_list(2, &deferred, credit);
        }
        Ok((deferred, list_credit(credit)))
    }

    /// On the checked route, the credit for a list of `slots` filters, taken before the
    /// list is made or grown. `None` on the ordinary route.
    fn filter_list_scratch(&self, slots: usize) -> Result<Option<Reservation>> {
        if self.branch_payload.borrow().is_none() {
            return Ok(None);
        }
        self.planning_stage(FILTER_LIST_PHASE)?;
        self.planning_scratch(|layout| layout.slots::<Expr>(slots))
    }

    /// Move the filters that `take` selects out of `filters` into a list made to their
    /// number and admitted before it is made, keeping the order of both. The rest stay
    /// in `filters`' own list. The credit returned holds the new list.
    fn extract_filters(
        &self,
        filters: &mut Vec<Expr>,
        take: impl Fn(&Expr) -> bool,
    ) -> Result<(Vec<Expr>, Option<Reservation>)> {
        #[cfg(test)]
        let _stage = crate::test_alloc::Stage::enter(10);
        let count = filters.iter().filter(|f| take(f)).count();
        let credit = self.filter_list_scratch(count)?;
        let mut out = Vec::new();
        if let Some(credit) = &credit {
            out.try_reserve_exact(count)
                .map_err(|_| credit.preparation().exhausted())?;
        }
        out.extend(filters.extract_if(.., |f| take(f)));
        debug_assert_eq!(out.len(), count);
        Ok((out, credit))
    }

    /// Add `more` filters to `filters` with `fill`. On the checked route, when the list
    /// must grow, the whole grown list is admitted before it is made, since the old list
    /// and its replacement can be live at once, and `fill` runs under the same admission.
    /// The credit returned holds the grown list.
    fn grow_filters(
        &self,
        filters: &mut Vec<Expr>,
        more: usize,
        #[allow(unused_variables)] site: usize,
        fill: impl FnOnce(&mut Vec<Expr>),
    ) -> Result<ListCredit> {
        let need = filters
            .len()
            .checked_add(more)
            .ok_or_else(|| self.ctx.memory_exceeded(u64::MAX))?;
        if self.branch_payload.borrow().is_none() || filters.capacity() >= need {
            fill(filters);
            debug_assert_eq!(filters.len(), need);
            return Ok(list_credit(None));
        }
        let credit = {
            #[cfg(test)]
            let _stage = crate::test_alloc::Stage::enter(10);
            let credit = self
                .filter_list_scratch(need)?
                .expect("checked filter list");
            filters
                .try_reserve_exact(more)
                .map_err(|_| credit.preparation().exhausted())?;
            fill(filters);
            debug_assert_eq!(filters.len(), need);
            #[cfg(test)]
            {
                note_list(site, need);
                watch_list(site, filters, &credit);
            }
            credit
        };
        Ok(list_credit(Some(credit)))
    }

    fn apply_filters(&self, n: Node, filters: Vec<Expr>) -> Result<Node> {
        if filters.is_empty() || n.is_empty() {
            return Ok(n);
        }
        self.filter_admitted(n, filters)
    }

    /// The checked collector can borrow the triples when all extractors would
    /// leave them unchanged. Keep each extractor's exact recognition test.
    fn plain_checked_bgp(&self, patterns: &[TriplePattern]) -> Result<bool> {
        let Some(preparation) = &self.ctx.source_preparation else {
            return Ok(false);
        };
        if self.branch_payload.borrow().is_none() {
            return Ok(false);
        }
        let rewrite = super::georewrite::rewrite_enabled(self.ctx);
        for pattern in patterns {
            preparation.check()?;
            let one = std::slice::from_ref(pattern);
            if super::arqpf::has_calls(one) || super::propertyext::has_calls(self.ctx, one) {
                return Ok(false);
            }
            let NamedNodePattern::NamedNode(predicate) = &pattern.predicate else {
                continue;
            };
            let iri = predicate.as_str();
            if matches!(
                iri,
                super::arqpf::RDFS_MEMBER
                    | super::textpf::TEXT_QUERY
                    | crate::vector::VECTOR_SEARCH
                    | super::hybrid::HYBRID_SEARCH
                    | super::georewrite::SPATIAL_EQUALS
            ) || crate::geo::SpatialPfKind::from_iri(iri).is_some()
                || rewrite && crate::geo::Relation::from_property(iri).is_some()
            {
                return Ok(false);
            }
        }
        Ok(true)
    }

    /// Flatten a join group into items + filters.
    fn collect(
        &mut self,
        gp: &GraphPattern,
        g: &ActiveGraph,
        items: &mut PlanningVec<Item>,
    ) -> Result<()> {
        use GraphPattern as GP;
        match gp {
            GP::Bgp { patterns }
                if patterns
                    .iter()
                    .any(|t| super::patternplan::is_has_predicate(&t.predicate)) =>
            {
                let (calls, rest): (Vec<_>, Vec<_>) = patterns
                    .iter()
                    .cloned()
                    .partition(|t| super::patternplan::is_has_predicate(&t.predicate));
                self.collect(&GP::Bgp { patterns: rest }, g, items)?;
                for t in &calls {
                    let n = self.has_predicate(t, g)?;
                    items.push(Item::Node(n))?;
                }
            }
            GP::Bgp { patterns } => {
                if self.plain_checked_bgp(patterns)? {
                    for tp in patterns {
                        self.ctx.check()?;
                        items.push(Item::Triple(self.triple(tp, g)))?;
                    }
                    for unpack in self.unpacks.borrow_mut().drain(..) {
                        items.push(Item::Unpack(unpack))?;
                    }
                    return Ok(());
                }
                if super::propertyext::has_calls(self.ctx, patterns) {
                    // a context configured on a pattern without the call never runs it
                    if !self.ctx.calls_extensions {
                        return Err(super::extensions::unconfigured());
                    }
                    let (calls, rest) = super::propertyext::take_calls(self.ctx, patterns)?;
                    self.collect(&GP::Bgp { patterns: rest }, g, items)?;
                    let input = self
                        .plan_group(std::mem::replace(items, self.planning_vec()?), Vec::new())?;
                    items.push(Item::Node(super::propertyext::attach(
                        self, input, calls, g,
                    )?))?;
                    return Ok(());
                }
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
                        items.push(Item::Node(n))?;
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
                    items.push(Item::Triple(self.triple(tp, g)))?;
                }
                for c in calls {
                    items.push(Item::Node(self.text_leaf(c, g)?))?;
                }
                for (subjects, args) in vcalls {
                    items.push(Item::Node(self.vector_leaf(subjects, args, g)?))?;
                }
                for (subjects, args) in hcalls {
                    items.push(Item::Node(super::hybrid::hybrid_leaf(
                        self, subjects, args, g,
                    )?))?;
                }
                for c in scalls {
                    items.push(Item::Node(super::geopf::spatial_leaf(self, c, g)?))?;
                }
                for c in rcalls {
                    items.push(Item::Node(super::georewrite::rewrite_leaf(self, c, g)?))?;
                }
                for unpack in self.unpacks.borrow_mut().drain(..) {
                    items.push(Item::Unpack(unpack))?;
                }
            }
            GP::Path {
                subject,
                path,
                object,
            } => {
                let s = self.term_pattern(subject);
                let o = self.term_pattern(object);
                let src = self.source_pattern(g, || {
                    (
                        source_term(subject),
                        super::exec::path_text(path),
                        source_term(object),
                    )
                });
                self.collect_path(s, path, o, g, items, &src)?;
                for unpack in self.unpacks.borrow_mut().drain(..) {
                    items.push(Item::Unpack(unpack))?;
                }
            }
            GP::Join { left, right } if !super::enhancer::is_loop(right) => {
                self.collect(left, g, items)?;
                self.collect(right, g, items)?;
            }
            GP::Lateral { left, right } if super::lateral::join_equivalent(self, left, right) => {
                self.collect(left, g, items)?;
                self.collect(right, g, items)?;
            }
            GP::Graph { .. } if self.ctx.selector_default_only => {
                // The selector has no named graphs, including the ARQ aliases.
                // Preserve the algebra's variable scope without planning the inner group.
                let mut vars = Vec::new();
                gp.on_in_scope_variable(|variable| {
                    if let PT::V(var) = self.var(variable.as_str()) {
                        vars.push(var);
                    }
                });
                vars.sort_unstable();
                vars.dedup();
                items.push(Item::Node(Node::empty(vars)))?;
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
                    let checked_names = self
                        .ctx
                        .source_preparation
                        .is_some()
                        .then(|| self.graph_names(&named))
                        .transpose()?;
                    let node = match self.graph_filter(&named)? {
                        Some(_) => self.plan(inner, &named, Vec::new())?,
                        None => Node::empty(Vec::new()),
                    };
                    items.push(Item::Node(node))?;
                    let names = match checked_names {
                        Some(names) => names,
                        None => self.graph_names(&named)?,
                    };
                    items.push(Item::Node(names))?;
                    return Ok(());
                }
                if self.ctx.source_preparation.is_some() {
                    items.push(Item::Node(self.prepare_graph_branches(inner, v)?))?;
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
                items.push(Item::Node(n))?;
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
                let checked_names = (self.ctx.source_preparation.is_some()
                    && matches!(g2, ActiveGraph::Named(_)))
                .then(|| self.graph_names(&g2))
                .transpose()?;
                if let GP::Filter { .. } = &**inner {
                    items.push(Item::Node(self.plan(inner, &g2, Vec::new())?))?;
                } else {
                    self.collect(inner, &g2, items)?;
                }
                // GRAPH ?g / GRAPH <g> iterate over (existing) named graphs even when the
                // inner pattern does not touch the data
                let names = match checked_names {
                    Some(names) => names,
                    None => self.graph_names(&g2)?,
                };
                items.push(Item::Node(names))?;
            }
            other => items.push(Item::Node(self.plan(other, g, Vec::new())?))?,
        }
        Ok(())
    }

    fn collect_path(
        &mut self,
        s: PT,
        path: &PropertyPathExpression,
        o: PT,
        g: &ActiveGraph,
        items: &mut PlanningVec<Item>,
        src: &Option<Arc<PlanPattern>>,
    ) -> Result<()> {
        use PropertyPathExpression as PP;
        match path {
            // ARQ's `multi(path)`: the path with its closures counting walks
            PP::Multi(path) => {
                // Counted closures have the same per-node layout, and removing
                // a nested Multi can only remove a box.
                let _normalized = self.planning_scratch(|layout| layout.path(path))?;
                let normalized = counted(path);
                self.ctx.check()?;
                return self.collect_path(s, &normalized, o, g, items, src);
            }
            // ARQ's `distinct(path)`: the pairs the path connects, each once
            PP::Distinct(p) => {
                let mut sub = self.planning_vec()?;
                self.collect_path(s, p, o, g, &mut sub, src)?;
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
                items.push(Item::Node(d))?;
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
                src: src.clone(),
            }))?,
            PP::Reverse(p) => self.collect_path(o, p, s, g, items, src)?,
            PP::Sequence(a, b) => {
                let mid = PT::V(self.ctx.fresh_var());
                self.collect_path(s, a, mid, g, items, src)?;
                self.collect_path(mid, b, o, g, items, src)?;
            }
            // ARQ's PathCompiler: `p{n,m}` with n ≥ 1 is n steps of p, then p{0,m-n}
            // (p{*} when unbounded); the steps are joined like a sequence's. A change to
            // this arm must change `Walk::path_levels` in depth.rs, which counts the steps.
            PP::Range { path, min, max } if (1..=RANGE_UNROLL).contains(min) => {
                let mut from = s;
                for i in 0..*min {
                    let to = if i + 1 == *min && *max == Some(*min) {
                        o
                    } else {
                        PT::V(self.ctx.fresh_var())
                    };
                    self.collect_path(from, path, to, g, items, src)?;
                    from = to;
                }
                if *max != Some(*min) {
                    let held = self.planning_scratch(|layout| {
                        layout.slots::<PropertyPathExpression>(1)?;
                        layout.path(path)
                    })?;
                    let inner = match &held {
                        Some(held) => {
                            Box::new(super::checked_syntax::copy_path(path, held.preparation())?)
                        }
                        None => path.clone(),
                    };
                    let rest = PP::Range {
                        path: inner,
                        min: 0,
                        max: max.map(|m| m - min),
                    };
                    items.push(Item::Path(PathItem {
                        s: from,
                        path: rest,
                        o,
                        graph: g.clone(),
                        src: src.clone(),
                        _held: held,
                    }))?;
                }
            }
            _ => {
                let held = self.planning_scratch(|layout| layout.path(path))?;
                let path = match &held {
                    Some(held) => super::checked_syntax::copy_path(path, held.preparation())?,
                    None => path.clone(),
                };
                items.push(Item::Path(PathItem {
                    s,
                    path,
                    o,
                    graph: g.clone(),
                    src: src.clone(),
                    _held: held,
                }))?;
            }
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
        let Some((graph, graph_var)) = self.graph_filter(g)? else {
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
        let Some((graph, graph_var)) = self.graph_filter(g)? else {
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

    /// A GROUP BY that the predicate patterns answer (see [`super::patternplan`]), if
    /// this one is such and the patterns give the scan's answer here.
    fn pattern_count(
        &mut self,
        inner: &GraphPattern,
        variables: &[spargebra::term::Variable],
        aggregates: &[(spargebra::term::Variable, AggregateExpression)],
        g: &ActiveGraph,
    ) -> Result<Option<Node>> {
        use super::patternplan::{Measure, PatternCountSpec, count_shape, usable};
        if !self.ctx.opt.predicate_patterns || matches!(g, ActiveGraph::Var(_)) {
            return Ok(None);
        }
        // `GRAPH <urn:x-arq:UnionGraph> { … }` around the pattern reads the named table
        let union;
        let (inner, g) = match inner {
            GraphPattern::Graph {
                name: NamedNodePattern::NamedNode(n),
                inner,
            } if n.as_str() == super::ctx::UNION_GRAPH_IRI => {
                if self.ctx.selector_default_only {
                    return Ok(None);
                }
                union = ActiveGraph::Union;
                (&**inner, &union)
            }
            _ => (inner, g),
        };
        let Some(shape) = count_shape(inner, variables, aggregates) else {
            return Ok(None);
        };
        let (PT::V(s), PT::V(p)) = (self.var(shape.s.as_str()), self.var(variables[0].as_str()))
        else {
            return Ok(None);
        };
        // a substituted object restricts the triples, which the patterns cannot
        if let Some(o) = &shape.o
            && !matches!(self.var(o.as_str()), PT::V(_))
        {
            return Ok(None);
        }
        let Some((gf, None)) = self.graph_filter(g)? else {
            return Ok(None);
        };
        let Some(uses) = usable(self.ctx, &gf) else {
            return Ok(None);
        };
        let input = if shape.rest.is_empty() {
            None
        } else {
            let n = self.plan(
                &GraphPattern::Bgp {
                    patterns: shape.rest,
                },
                g,
                Vec::new(),
            )?;
            if !n.certain.contains(&s) || n.vars.contains(&p) {
                return Ok(None);
            }
            Some(n)
        };
        let counts: Vec<(VarId, Measure)> = shape
            .counts
            .iter()
            .map(|(v, m)| (self.ctx.var(v.as_str()), *m))
            .collect();
        let mut vars = vec![p];
        vars.extend(counts.iter().map(|c| c.0));
        let est = (self.ctx.snap.generation.stats.distinct_predicates.max(1) as f64).min(
            input
                .as_ref()
                .map_or(f64::INFINITY, |i| i.est.max(1.0) * 64.0),
        );
        let desc = format!(
            "?{} by subjects{}{} [{} table]",
            self.ctx.var_name(p),
            if input.is_some() {
                format!(" of ?{}", self.ctx.var_name(s))
            } else {
                String::new()
            },
            // the measures belong in the description, which keys the result cache
            counts
                .iter()
                .map(|(v, m)| format!(
                    " ?{}={}",
                    self.ctx.var_name(*v),
                    match m {
                        Measure::Subjects => "subjects",
                        Measure::Rows => "rows",
                    }
                ))
                .collect::<String>(),
            uses.scope.name()
        );
        let cost = match &input {
            Some(i) => i.cost + i.est,
            None => est + uses.corr.touched.len() as f64,
        };
        let src = self.source_pattern(g, || {
            (
                source_term(&shape.triple.subject),
                source_named(&shape.triple.predicate),
                source_term(&shape.triple.object),
            )
        });
        let spec = PatternCountSpec {
            uses,
            subject: input.as_ref().map(|_| s),
            pred: p,
            counts,
            src,
        };
        let mut n = Node::leaf(Kind::PatternCount(Box::new(spec)), vars, est, desc);
        n.cost = cost;
        n.sorted = vec![p];
        n.children = input.into_iter().collect();
        Ok(Some(n))
    }

    /// `?s spk:hasPredicate ?p` (or QLever's `ql:has-predicate`): from the predicate
    /// patterns when they give the answer here, and otherwise as the distinct subjects
    /// and predicates of `?s ?p []`.
    fn has_predicate(&mut self, t: &TriplePattern, g: &ActiveGraph) -> Result<Node> {
        use super::patternplan::{PatternScanSpec, usable};
        // A triple term is never a subject (RDF 1.2 has them only as objects) nor a
        // predicate, so the relation is empty. Its variables stay unbound.
        if matches!(t.subject, TermPattern::Triple(_)) || matches!(t.object, TermPattern::Triple(_))
        {
            fn walk(t: &TermPattern, out: &mut Vec<String>) {
                match t {
                    TermPattern::Variable(v) => out.push(v.as_str().to_string()),
                    TermPattern::Triple(tp) => {
                        walk(&tp.subject, out);
                        if let NamedNodePattern::Variable(v) = &tp.predicate {
                            out.push(v.as_str().to_string());
                        }
                        walk(&tp.object, out);
                    }
                    _ => {}
                }
            }
            let mut names = Vec::new();
            walk(&t.subject, &mut names);
            walk(&t.object, &mut names);
            let mut vars = Vec::new();
            for n in names {
                if let PT::V(v) = self.var(&n)
                    && !vars.contains(&v)
                {
                    vars.push(v);
                }
            }
            return Ok(Node::empty(vars));
        }
        let src = self.source_pattern(g, || {
            (
                source_term(&t.subject),
                source_named(&t.predicate),
                source_term(&t.object),
            )
        });
        let s = self.term_pattern(&t.subject);
        let p = self.term_pattern(&t.object);
        let mut vars = Vec::new();
        for x in [s, p] {
            if let PT::V(v) = x
                && !vars.contains(&v)
            {
                vars.push(v);
            }
        }
        let end = |x: PT| match x {
            PT::C(id) => PathEnd::Const(id),
            PT::V(v) => PathEnd::Var(v),
        };
        // A constant predicate reads its own PSO range, the subjects that have it, which
        // costs less than walking every subject's pattern.
        if vars.len() == [s, p].iter().filter(|x| matches!(x, PT::V(_))).count()
            && matches!(p, PT::V(_))
            && !matches!(g, ActiveGraph::Var(_))
            && let Some((gf, None)) = self.graph_filter(g)?
            && let Some(uses) = usable(self.ctx, &gf)
        {
            let stats = &self.ctx.snap.generation.stats;
            let est = match s {
                PT::C(_) => (stats.distinct_predicates as f64).clamp(1.0, 16.0),
                PT::V(_) => stats
                    .predicates
                    .iter()
                    .map(|st| st.distinct_subjects as f64)
                    .sum(),
            };
            let desc = format!(
                "{} spk:hasPredicate {} [{} table]",
                self.pt_str(&s),
                self.pt_str(&p),
                uses.scope.name()
            );
            let spec = PatternScanSpec {
                uses,
                s: end(s),
                p: end(p),
                src: src.clone(),
            };
            let mut n = Node::leaf(Kind::PatternScan(Box::new(spec)), vars.clone(), est, desc);
            n.sorted = match (s, p) {
                (PT::V(a), PT::V(b)) => vec![a, b],
                (PT::V(a), _) | (_, PT::V(a)) => vec![a],
                _ => Vec::new(),
            };
            if let PT::V(a) = s {
                n.dist
                    .insert(a, (stats.distinct_subjects as f64).clamp(1.0, est.max(1.0)));
            }
            if let PT::V(b) = p {
                n.dist.insert(
                    b,
                    (stats.distinct_predicates as f64).clamp(1.0, est.max(1.0)),
                );
            }
            return Ok(n);
        }
        let o = PT::V(self.ctx.fresh_var());
        let mut sub = self.planning_vec()?;
        sub.push(Item::Triple(Triple {
            t: [s, p, o],
            graph: g.clone(),
            src,
        }))?;
        let node = self.plan_group(sub, Vec::new())?;
        if let ActiveGraph::Var(gv) = g
            && node.vars.contains(gv)
            && !vars.contains(gv)
        {
            vars.push(*gv);
        }
        let n = project(node, vars, self.ctx);
        let mut d = Node::unary(Kind::Distinct, n, "spk:hasPredicate".into());
        d.est = d.children[0].est;
        Ok(d)
    }

    fn triple(&self, tp: &TriplePattern, g: &ActiveGraph) -> Triple {
        Triple {
            t: [
                self.term_pattern(&tp.subject),
                self.named_pattern(&tp.predicate),
                self.term_pattern(&tp.object),
            ],
            graph: g.clone(),
            src: self.source_pattern(g, || {
                (
                    source_term(&tp.subject),
                    source_named(&tp.predicate),
                    source_term(&tp.object),
                )
            }),
        }
    }

    /// The pattern `s p o` of the query in graph `g` as the plan names it (spec U04
    /// §11.3), with its occurrence among the equal patterns planned so far. A part of
    /// the query that is planned again (the inner pattern of `GRAPH ?g` once per graph)
    /// counts its patterns again, so the count wraps at the number of equal patterns in
    /// the source.
    ///
    /// `None` when the plan does not name its patterns ([`Ctx::plan_patterns`]), and then
    /// `text`, which writes the subject, predicate or path, and object, is not called.
    #[inline]
    fn source_pattern(
        &self,
        g: &ActiveGraph,
        text: impl FnOnce() -> (String, String, String),
    ) -> Option<Arc<PlanPattern>> {
        if !self.ctx.plan_patterns {
            return None;
        }
        let (s, p, o) = text();
        Some(self.record_pattern(s, p, o, g))
    }

    /// [`Self::source_pattern`] of a pattern whose text is written.
    fn record_pattern(&self, s: String, p: String, o: String, g: &ActiveGraph) -> Arc<PlanPattern> {
        #[cfg(test)]
        PATTERNS_NAMED.with(|n| n.set(n.get() + 1));
        let graph = match g {
            ActiveGraph::Default => None,
            ActiveGraph::Union => Some(format!("<{}>", super::ctx::UNION_GRAPH_IRI)),
            ActiveGraph::Named(id) => self.ctx.plan_term(*id).map(|t| t.to_string()),
            ActiveGraph::Var(v) => Some(self.pt_str(&PT::V(*v))),
        };
        let mut pattern = Arc::new(PlanPattern {
            s,
            p,
            o,
            graph,
            occurrence: 0,
        });
        // the pattern itself is the key, so that a first occurrence copies no text
        let key = SeenPattern(pattern.clone());
        let mut seen = self.seen.borrow_mut();
        let mut occurrence = match seen.get_mut(&key) {
            Some(n) => {
                let o = *n;
                *n += 1;
                // a second holder would make `make_mut` below copy the pattern
                drop(key);
                o
            }
            None => {
                seen.insert(key, 1);
                0
            }
        };
        drop(seen);
        // the source is counted only when a pattern comes again, which most queries never do
        if occurrence > 0 {
            let key = SeenPattern(pattern.clone());
            if let Some(total) = self.source_counts().get(&key)
                && *total > 0
            {
                occurrence %= *total;
            }
            drop(key);
            if occurrence > 0 {
                // the sole holder: the map keeps the first equal pattern, not this one
                Arc::make_mut(&mut pattern).occurrence = occurrence;
            }
        }
        pattern
    }

    /// How many times each triple pattern and path occurs in the source query.
    fn source_counts(&self) -> &FxHashMap<SeenPattern, u32> {
        self.source_counts.get_or_init(|| {
            let mut out = FxHashMap::default();
            if let Some(q) = self.source {
                let gp = match q {
                    spargebra::Query::Select { pattern, .. }
                    | spargebra::Query::Construct { pattern, .. }
                    | spargebra::Query::Describe { pattern, .. }
                    | spargebra::Query::Ask { pattern, .. } => pattern,
                };
                count_patterns(gp, &mut out);
            }
            out
        })
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
        fn plain(gp: &GraphPattern, ctx: &Ctx) -> bool {
            match gp {
                // ARQ's property functions read lists per graph
                GraphPattern::Bgp { patterns } => {
                    !super::arqpf::has_calls(patterns)
                        && !super::propertyext::has_calls(ctx, patterns)
                }
                GraphPattern::Path { .. } => true,
                GraphPattern::Join { left, right } => plain(left, ctx) && plain(right, ctx),
                GraphPattern::Filter { inner, expr } => plain(inner, ctx) && !has_exists(expr),
                _ => false,
            }
        }
        if !plain(inner, self.ctx) {
            return false;
        }
        let mut mentions_graph_var = false;
        visit_pattern_vars(inner, &mut |name| mentions_graph_var |= name == v.as_str());
        !mentions_graph_var
    }

    fn named_accepts(&self, id: Id) -> Result<bool> {
        let Some(named) = &self.ctx.dataset.named else {
            return Ok(true);
        };
        if self.ctx.source_preparation.is_some() && named.len() > 1 {
            let filter = self.set_filter(named, &self.named_filter)?;
            Ok(filter.accepts(id.0))
        } else {
            Ok(named.contains(&id))
        }
    }

    fn prepare_graph_names(
        &self,
        var: VarId,
        preparation: &Preparation,
    ) -> Result<Arc<PreparedGraphNames>> {
        let fixed = size_of::<PreparedGraphNames>()
            + 2 * size_of::<usize>()
            + size_of::<Vec<Id>>()
            + size_of::<VarId>();
        let mut held = preparation.reserve(fixed as u64)?;
        let selected = self
            .ctx
            .dataset
            .named
            .as_ref()
            .map(|named| self.set_filter(named, &self.named_filter))
            .transpose()?;
        let mut ids = PreparationBuffer::new(preparation)?;
        self.ctx.snap.scan_graph_ids_prepared(preparation, |id| {
            if id != Id::DEFAULT_GRAPH
                && selected.as_ref().is_none_or(|filter| filter.accepts(id.0))
            {
                ids.push(id)?;
            }
            Ok(())
        })?;
        let (ids, credit) = ids.into_parts();
        held.merge(credit);
        let len = ids.len();
        let table = Table {
            vars: vec![var],
            cols: vec![ids],
            len,
            sorted: Vec::new(),
        };
        Ok(Arc::new(PreparedGraphNames { table, _held: held }))
    }

    fn prepare_graph_branches(&mut self, inner: &GraphPattern, var: VarId) -> Result<Node> {
        let ctx = self.ctx;
        let preparation = ctx
            .source_preparation
            .as_ref()
            .expect("checked graph planning");
        let names = self.prepare_graph_names(var, preparation)?;
        if self.checked_exists.get_mut().is_some() || names.table.len == 0 {
            return self.build_graph_branches(inner, var, &names, preparation);
        }
        let sources = super::checked_syntax::Sources::new(inner, preparation)?;
        *self.checked_exists.get_mut() = Some(sources);
        let result = self.build_graph_branches(inner, var, &names, preparation);
        *self.checked_exists.get_mut() = None;
        result
    }

    fn checked_values(
        &self,
        variables: &[oxrdf::Variable],
        bindings: &[Vec<Option<GroundTerm>>],
        native: bool,
    ) -> Result<Node> {
        let preparation = self.ctx.source_preparation.as_ref().expect("checked GRAPH");
        let mut layout = super::checked_layout::Layout::new(preparation);
        let width = variables.len();
        let rows = bindings.len();
        let cells = width
            .checked_mul(rows)
            .ok_or_else(|| preparation.exhausted())?;
        layout.slots::<Id>(cells)?;
        layout.slots::<Vec<Id>>(width)?;
        // Table, node output schema and certainty each own one vector.
        let schema_slots = width
            .checked_mul(3)
            .ok_or_else(|| preparation.exhausted())?;
        layout.slots::<VarId>(schema_slots)?;
        layout.map::<VarId, f64>(width)?;
        // Any row count as displayed by f64 fits in this decimal description.
        layout.add(64)?;
        self.branch_payload
            .borrow_mut()
            .as_mut()
            .expect("branch construction")
            .grow_by(layout.bytes())?;

        let mut vars = Vec::with_capacity(width);
        for variable in variables {
            vars.push(self.ctx.var_prepared(variable.as_str(), preparation)?);
        }
        let mut table = Table::new(vars.clone());
        for column in &mut table.cols {
            column
                .try_reserve_exact(rows)
                .map_err(|_| preparation.exhausted())?;
        }

        let mut seen = PreparationMap::new(preparation)?;
        for row in bindings {
            preparation.check()?;
            if row.len() != width {
                return Err(Error::invalid("VALUES row does not match its variables"));
            }
            for (term, column) in row.iter().zip(&mut table.cols) {
                preparation.check()?;
                let id = if let Some(term) = term {
                    if let Some(&id) = seen.get(term) {
                        id
                    } else {
                        let mut layout = super::checked_layout::Layout::new(preparation);
                        layout.ground(term)?;
                        let _term = preparation.reserve(layout.bytes())?;
                        let id = match term {
                            GroundTerm::Literal(literal) if !native => {
                                self.ctx.intern_query_literal(literal)
                            }
                            _ => self.ctx.intern_term(&ground_term(term)),
                        };
                        self.ctx.check()?;
                        seen.insert(term, id)?;
                        id
                    }
                } else {
                    Id::UNDEF
                };
                column.push(id);
            }
            table.len += 1;
        }
        drop(seen);
        for (index, var) in vars.iter().enumerate() {
            let Some(&bound) = self.subst.get(var) else {
                continue;
            };
            let mut keep = PreparationBuffer::new(preparation)?;
            for id in &table.cols[index] {
                keep.push(id.is_undef() || *id == bound)?;
            }
            table.filter_rows(&keep);
            table.cols[index].fill(bound);
        }
        let est = table.len() as f64;
        let mut node = Node::leaf(Kind::Values(table), vars, est, format!("{est} rows"));
        let Kind::Values(table) = &node.kind else {
            unreachable!();
        };
        let mut certain = 0;
        for (index, column) in table.cols.iter().enumerate() {
            preparation.check()?;
            let mut bound = true;
            for part in column.chunks(1024) {
                preparation.check()?;
                if part.iter().any(|id| id.is_undef()) {
                    bound = false;
                    break;
                }
            }
            if bound {
                node.certain[certain] = node.vars[index];
                certain += 1;
            }
        }
        node.certain.truncate(certain);
        Ok(node)
    }

    fn build_graph_branches(
        &mut self,
        inner: &GraphPattern,
        var: VarId,
        names: &PreparedGraphNames,
        preparation: &Preparation,
    ) -> Result<Node> {
        let fixed = size_of::<PreparedGraphBranches>() + 2 * size_of::<usize>();
        let mut held = preparation.reserve(fixed as u64)?;
        let mut inputs = PreparationBuffer::new(preparation)?;
        let mut contains_algorithm = false;
        let mut max_depth = 0;
        // Each checked payload and candidate belongs to the graph already being
        // planned. It shares the allowance with the preceding live branches.
        for &id in &names.table.cols[0] {
            preparation.check()?;
            let payload = preparation.reserve(0)?;
            let shared_payload = preparation.reserve(0)?;
            let previous = self.branch_payload.replace(Some(payload));
            let previous_shared = self.branch_shared_payload.replace(Some(shared_payload));
            let node = self.plan(inner, &ActiveGraph::Named(id), Vec::new());
            let mut payload = self
                .branch_payload
                .replace(previous)
                .expect("branch construction");
            let shared_payload = self
                .branch_shared_payload
                .replace(previous_shared)
                .expect("branch shared payload");
            let node = node?;
            if node.is_empty() {
                continue;
            }

            let mut layout = super::checked_layout::Layout::new(preparation);
            layout.slots::<VarId>(3)?;
            layout.slots::<Vec<Id>>(1)?;
            layout.slots::<Id>(4)?;
            layout.map::<VarId, f64>(1)?;
            layout.add("graph".len())?;
            payload.grow_by(layout.bytes())?;
            let mut binding = Table::new(vec![var]);
            binding.push_row(&[id]);
            let bind = Node::leaf(Kind::Values(binding), vec![var], 1.0, "graph".into());
            let branch = checked_graph_binding_join(node, bind, var, self.ctx, &mut payload)?;
            let mut retained = super::checked_layout::Layout::retained(preparation);
            retained.node(&branch)?;
            let retained_bytes = retained.bytes();
            // Producers outside this admission (see the lane notes) still leave the
            // branch short here, so tests read the shortfall rather than assert it.
            #[cfg(test)]
            BRANCH_SHORTFALL.with(|most| {
                most.set(
                    most.get()
                        .max(retained_bytes.saturating_sub(payload.bytes())),
                );
            });
            payload.grow_to(retained_bytes)?;
            // Planning and join selection have dropped their temporary storage.
            // Shared backing has its own credit, independent of node copies.
            payload.release_to(retained_bytes);
            held.merge(payload);
            held.merge(shared_payload);
            contains_algorithm |= super::exec::contains_algorithm(&branch);
            max_depth = max_depth.max(branch.depth());
            inputs.push(branch)?;
        }
        if inputs.is_empty() {
            return Ok(Node::empty(vec![var]));
        }
        let (inputs, credit) = inputs.into_parts();
        held.merge(credit);
        let mut out = if let [branch] = inputs.as_slice() {
            let mut layout = super::checked_layout::Layout::new(preparation);
            layout.metadata(branch)?;
            held.grow_by(layout.bytes())?;
            Node {
                kind: Kind::Empty,
                children: Vec::new(),
                vars: branch.vars.clone(),
                certain: branch.certain.clone(),
                sorted: branch.sorted.clone(),
                est: branch.est,
                cost: branch.cost,
                dist: branch.dist.clone(),
                desc: branch.desc.clone(),
            }
        } else {
            let (shape, credit) = checked_union_shape(&inputs, preparation)?;
            held.merge(credit);
            shape
        };
        out.kind = Kind::GraphBranches(Arc::new(PreparedGraphBranches {
            inputs,
            contains_algorithm,
            max_depth,
            _held: held,
        }));
        Ok(out)
    }

    /// One row per named graph of the active dataset (bound to the graph variable), or
    /// the unit table if a constant graph exists.
    fn graph_names(&self, g: &ActiveGraph) -> Result<Node> {
        let exists = |id: Id| -> Result<bool> {
            if id.tag() == Tag::Local || !self.named_accepts(id)? {
                return Ok(false);
            }
            if let Some(preparation) = &self.ctx.source_preparation {
                return self.ctx.snap.graph_exists_prepared(id, preparation);
            }
            Ok(self.ctx.snap.count(Perm::Gspo, &[id.0])? > 0)
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
                    if let Some(preparation) = &self.ctx.source_preparation {
                        let names = self.prepare_graph_names(*v, preparation)?;
                        let est = names.table.len() as f64;
                        return Ok(Node::leaf(
                            Kind::GraphNames(names),
                            vec![*v],
                            est,
                            format!("{est} named graphs"),
                        ));
                    }
                    let mut t = Table::new(vec![*v]);
                    for gid in self.ctx.snap.graph_ids()? {
                        if self.named_accepts(gid)? {
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

    fn set_filter(
        &self,
        set: &[Id],
        cache: &OnceCell<Arc<PreparedGraphSet>>,
    ) -> Result<GraphFilter> {
        if let Some(preparation) = &self.ctx.source_preparation {
            if let Some(filter) = cache.get() {
                return Ok(GraphFilter::PreparedSet(filter.clone()));
            }
            let filter = PreparedGraphSet::new(preparation, set)?;
            let _ = cache.set(filter.clone());
            return Ok(GraphFilter::PreparedSet(filter));
        }
        let mut ids: Vec<u64> = set.iter().map(|id| id.0).collect();
        ids.sort_unstable();
        ids.dedup();
        Ok(GraphFilter::Set(ids))
    }

    pub(super) fn graph_filter(
        &self,
        g: &ActiveGraph,
    ) -> Result<Option<(GraphFilter, Option<VarId>)>> {
        let ds = &self.ctx.dataset;
        Ok(Some(match g {
            // An empty default graph (a dataset of named graphs only) matches nothing.
            ActiveGraph::Default => match &ds.default {
                Some(set) if set.is_empty() => return Ok(None),
                Some(set) if set.len() == 1 => (GraphFilter::One(set[0].0), None),
                Some(set) => (self.set_filter(set, &self.default_filter)?, None),
                None if ds.union_default || self.ctx.snap.union_default_graph => {
                    (GraphFilter::Named, None)
                }
                None => (GraphFilter::Default, None),
            },
            // The union graph of a graph view is the union of the named graphs it sees.
            ActiveGraph::Union => match (&self.ctx.graphs, &ds.named) {
                (Some(_), Some(set)) if set.is_empty() => return Ok(None),
                (Some(_), Some(set)) => (self.set_filter(set, &self.named_filter)?, None),
                _ => (GraphFilter::Named, None),
            },
            ActiveGraph::Named(id) => {
                if id.tag() == Tag::Local {
                    return Ok(None);
                }
                if !self.named_accepts(*id)? {
                    return Ok(None);
                }
                (GraphFilter::One(id.0), None)
            }
            ActiveGraph::Var(v) => match self.subst.get(v) {
                Some(id) => return self.graph_filter(&ActiveGraph::Named(*id)),
                None => match &ds.named {
                    Some(set) if set.is_empty() => return Ok(None),
                    Some(set) => (self.set_filter(set, &self.named_filter)?, Some(*v)),
                    None => (GraphFilter::Named, Some(*v)),
                },
            },
        }))
    }

    // ----------------------------------------------------------------- scans ------

    /// All useful scan plans for a triple pattern (one per sort order).
    fn scan_options(&self, t: &Triple) -> Result<PlanningVec<Node>> {
        let empty_options = |t: &Triple| -> Result<PlanningVec<Node>> {
            self.admit_payload(|layout| {
                layout.slots::<VarId>(8)?;
                layout.map::<VarId, f64>(4)?;
                layout.add("empty".len())
            })?;
            let mut v = Vec::with_capacity(4);
            v.extend(
                t.t.iter()
                    .filter_map(|x| if let PT::V(v) = x { Some(*v) } else { None }),
            );
            if let ActiveGraph::Var(g) = t.graph {
                v.push(g);
            }
            v.sort_unstable();
            v.dedup();
            let mut options = self.planning_vec()?;
            options.push(Node::empty(v))?;
            Ok(options)
        };
        let Some((gf, gvar)) = self.graph_filter(&t.graph)? else {
            return empty_options(t);
        };
        // unknown constants → empty
        if t.t
            .iter()
            .any(|x| matches!(x, PT::C(id) if self.ctx.unknown_const(*id)))
        {
            return empty_options(t);
        }
        // positions S=0,P=1,O=2,G=3 → PT
        let mut pos: [Option<PT>; 4] = [Some(t.t[0]), Some(t.t[1]), Some(t.t[2]), None];
        if let Some(gv) = gvar {
            pos[3] = Some(PT::V(gv));
        }
        let mut bound = [0; 3];
        let mut bound_len = 0;
        for (i, item) in pos.iter().enumerate().take(3) {
            if matches!(item, Some(PT::C(_))) {
                bound[bound_len] = i;
                bound_len += 1;
            }
        }
        let bound = &bound[..bound_len];
        let mut out = self.planning_vec()?;
        let use_gspo = matches!(gf, GraphFilter::One(_))
            && (bound.is_empty() || bound == [S] || bound == [S, P] || bound == [S, P, O]);
        let perms = [
            Perm::Spo,
            Perm::Sop,
            Perm::Pso,
            Perm::Pos,
            Perm::Osp,
            Perm::Ops,
            Perm::Gspo,
        ]
        .into_iter()
        .filter(|perm| {
            if *perm == Perm::Gspo {
                return use_gspo;
            }
            let mut order = perm.order();
            order[..bound.len()].sort_unstable();
            &order[..bound.len()] == bound
        });
        // An order is distinguished by its first variable, or by the absence of one.
        let mut seen_orders = [None; 7];
        let mut orders = 0;
        let stats = &self.ctx.snap.generation.stats;
        let snap = &self.ctx.snap;
        for perm in perms {
            let order = perm.order();
            let mut prefix = [0; 4];
            let mut first_free = 0;
            if perm == Perm::Gspo {
                let GraphFilter::One(gid) = gf else {
                    unreachable!()
                };
                prefix[0] = gid;
                first_free = 1;
            }
            for &component in &order[first_free..] {
                match pos[component] {
                    Some(PT::C(id)) => {
                        prefix[first_free] = id.0;
                        first_free += 1;
                    }
                    _ => break,
                }
            }
            // Other permutations bind only their leading SPO constants.
            if perm != Perm::Gspo {
                first_free = bound.len();
            }
            let prefix = &prefix[..first_free];
            let mut cols = [(0usize, 0); 4];
            let mut eqs = [(0usize, 0usize); 4];
            let mut variables = 0;
            let mut equalities = 0;
            for (kc, &c) in order.iter().enumerate().skip(first_free) {
                if let Some(PT::V(v)) = pos[c] {
                    match cols[..variables].iter().find(|(_, x)| *x == v) {
                        Some((k0, _)) => {
                            eqs[equalities] = (*k0, kc);
                            equalities += 1;
                        }
                        None => {
                            cols[variables] = (kc, v);
                            variables += 1;
                        }
                    }
                }
            }
            let first = cols[..variables].first().map(|(_, v)| *v);
            if seen_orders[..orders].contains(&first) {
                continue;
            }
            seen_orders[orders] = first;
            orders += 1;
            let cols = &cols[..variables];
            let eqs = &eqs[..equalities];
            self.admit_payload(|layout| {
                layout.slice(prefix)?;
                layout.slice(cols)?;
                layout.slice(eqs)?;
                layout.slots::<VarId>(variables * 3)?;
                // Only this map is newly built, with at most four keys.
                layout.map::<VarId, f64>(variables)?;
                layout.map::<VarId, f64>(variables)?;
                if let GraphFilter::Set(ids) = &gf {
                    layout.slice(ids)?;
                }
                Ok(())
            })?;
            let graph_col = perm.col_of(G);
            let graph = if perm == Perm::Gspo
                || gvar.is_some() && gf.set_ids().is_none() && !matches!(gf, GraphFilter::Named)
            {
                GraphFilter::All
            } else {
                gf.clone()
            };
            let dedup = gvar.is_none() && gf.multi() && perm != Perm::Gspo;
            let mut est = if prefix.is_empty() {
                snap.len() as f64
            } else if let Some(n) = snap.predicate_count(perm, prefix) {
                // the same exact count, without decoding the predicate's end blocks
                n as f64
            } else {
                snap.count(perm, prefix)? as f64
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
            for &(kc, v) in cols {
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
            let desc = self.scan_description(perm, t)?;
            let sorted: Vec<VarId> = cols.iter().map(|(_, v)| *v).collect();
            let n = Node {
                kind: Kind::Scan(ScanSpec {
                    perm,
                    prefix: prefix.to_vec(),
                    cols: cols.to_vec(),
                    eqs: eqs.to_vec(),
                    graph,
                    graph_col,
                    dedup,
                    src: t.src.clone(),
                }),
                vars: sorted.clone(),
                certain: sorted.clone(),
                sorted,
                children: Vec::new(),
                est,
                cost: est,
                dist,
                desc,
            };
            if est == 0.0 {
                return empty_options(t);
            }
            out.push(n)?;
        }
        if out.is_empty() {
            return empty_options(t);
        }
        Ok(out)
    }

    fn scan_description(&self, perm: Perm, triple: &Triple) -> Result<String> {
        use std::fmt::Write;

        let mut active = self.branch_payload.borrow_mut();
        let Some(held) = active.as_mut() else {
            return Ok(format!(
                "{} {}",
                perm.name().to_uppercase(),
                triple
                    .t
                    .iter()
                    .map(|term| self.pt_str(term))
                    .collect::<Vec<_>>()
                    .join(" ")
            ) + &match &triple.graph {
                ActiveGraph::Default => String::new(),
                graph => format!(
                    " GRAPH {}",
                    match graph {
                        ActiveGraph::Named(id) => self.pt_str(&PT::C(*id)),
                        ActiveGraph::Var(var) => self.pt_str(&PT::V(*var)),
                        _ => String::new(),
                    }
                ),
            });
        };

        let preparation = held.preparation().clone();
        let mut text = PreparationText::new(&preparation)?;
        for ch in perm.name().chars().flat_map(char::to_uppercase) {
            let _ = text.write_char(ch);
        }
        for term in &triple.t {
            text.push_str(" ")?;
            self.write_checked_plan_term(&mut text, &preparation, *term)?;
        }
        match triple.graph {
            ActiveGraph::Default => {}
            ActiveGraph::Named(id) => {
                text.push_str(" GRAPH ")?;
                self.write_checked_plan_term(&mut text, &preparation, PT::C(id))?;
            }
            ActiveGraph::Var(var) => {
                text.push_str(" GRAPH ")?;
                self.write_checked_plan_term(&mut text, &preparation, PT::V(var))?;
            }
            ActiveGraph::Union => text.push_str(" GRAPH ")?,
        }
        let (desc, desc_credit) = text.finish()?;
        held.merge(desc_credit);
        Ok(desc)
    }

    fn write_checked_plan_term(
        &self,
        text: &mut PreparationText,
        preparation: &Preparation,
        term: PT,
    ) -> Result<()> {
        use std::fmt::Write;

        if let PT::V(var) = term {
            let names = self.ctx.var_names.read();
            let name = &names[var as usize];
            text.push_str("?")?;
            return text.push_str(if name.starts_with(' ') { "_" } else { name });
        }
        let PT::C(id) = term else { unreachable!() };
        let decoded = self.ctx.charge(0)?;
        let Some(term) = self.ctx.output_term(id, &decoded)? else {
            let _ = write!(text, "{id:?}");
            return Ok(());
        };
        if let Term::NamedNode(iri) = &term {
            if let Some((prefix, suffix)) = short_iri(iri.as_str()) {
                text.push_str(prefix)?;
                return text.push_str(suffix);
            }
            text.push_str("<")?;
            text.push_str(iri.as_str())?;
            return text.push_str(">");
        }
        if let Term::Literal(literal) = &term
            && literal.datatype() == xsd::INTEGER
        {
            return text.push_str(literal.value());
        }

        // Preserve the existing truncation after N-Triples rendering.
        let mut rendered = PreparationText::new(preparation)?;
        let _ = write!(rendered, "{term}");
        let (rendered, credit) = rendered.finish()?;
        let result = if rendered.len() > 60 {
            let end = rendered
                .char_indices()
                .nth(57)
                .map_or(rendered.len(), |(index, _)| index);
            text.push_str(&rendered[..end])
                .and_then(|()| text.push_str("…"))
        } else {
            text.push_str(&rendered)
        };
        drop(rendered);
        drop(credit);
        result
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
                .plan_term(*id)
                .map_or_else(|| format!("{id:?}"), |t| short(&t)),
        }
    }

    // ------------------------------------------------------------- join groups ------

    fn plan_group(&mut self, items: PlanningVec<Item>, filters: Vec<Expr>) -> Result<Node> {
        // The credit for each filter list on the checked route is declared before the
        // list it covers, and the parameter is moved below them, so that every list is
        // freed before its credit is released on each path, refusals included.
        let _deferred_list;
        let _late_list;
        let mut joined_list = list_credit(None);
        let _settled_list;
        let _late_joined;
        let mut filters = filters;
        let mut triples = self.planning_vec()?;
        let mut paths = self.planning_vec()?;
        let mut nodes = self.planning_vec()?;
        let mut unpacks = self.planning_vec()?;
        for it in items {
            match it {
                Item::Unpack(u) => unpacks.push(u)?,
                Item::Triple(t) => triples.push(t)?,
                Item::Path(p) => paths.push(p)?,
                Item::Node(n) => nodes.push(n)?,
            }
        }

        // Filter-equality substitution (Jena TransformFilterEquality): FILTER(?x = <iri>)
        // with ?x only used in triple patterns → substitute the constant.
        let mut binds = self.planning_vec::<(VarId, Id)>()?;
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
                for t in triples.iter_mut() {
                    for x in &mut t.t {
                        if *x == PT::V(v) {
                            *x = PT::C(c);
                        }
                    }
                }
                for f in &mut filters {
                    substitute(f, v, c)?;
                }
                binds.push((v, c))?;
                continue;
            }
            i += 1;
        }

        // Point patterns: a triple pattern with a single variable of which the store holds
        // exactly one row binds that variable to a constant for the rest of the group, as
        // the filter-equality substitution above does. Its FILTERs then test the constant,
        // so they can narrow scans of the group's other patterns.
        if self.ctx.opt.point_patterns && !self.ctx.calls_extensions {
            let mut i = 0;
            while i < triples.len() {
                let point =
                    self.point_value(&triples[i], i, &triples, &filters, &nodes, &paths, &unpacks)?;
                let Some((v, c)) = point else {
                    i += 1;
                    continue;
                };
                triples.remove(i);
                for t in triples.iter_mut() {
                    for x in &mut t.t {
                        if *x == PT::V(v) {
                            *x = PT::C(c);
                        }
                    }
                }
                for f in &mut filters {
                    substitute(f, v, c)?;
                }
                binds.push((v, c))?;
            }
        }

        // A FILTER of the group tests the join of all its elements. It can run earlier, on
        // part of the join, when each of its variables has its final value there. A
        // variable that an element may leave unbound (a UNION branch, an OPTIONAL, a
        // failed BIND, an UNDEF of VALUES) does not, when another element can still bind
        // it. Such a FILTER waits until every element that binds them has joined.
        let deferred;
        (deferred, _deferred_list) = if self.branch_payload.borrow().is_none() {
            let deferred = defer_open_filters(&mut filters, &triples, &paths, &nodes, &unpacks);
            (deferred, list_credit(None))
        } else {
            self.defer_open_filters_checked(&mut filters, &triples, &paths, &nodes, &unpacks)?
        };

        let leaf_slots = triples
            .len()
            .checked_add(nodes.len())
            .ok_or_else(|| self.ctx.memory_exceeded(u64::MAX))?;
        let leaf_storage = self.planning_scratch(|layout| layout.slots::<Vec<Node>>(leaf_slots))?;
        let mut scan_storage = self.planning_scratch(|layout| layout.slots::<Node>(nodes.len()))?;
        let mut leaves = if leaf_storage.is_some() {
            Vec::with_capacity(leaf_slots)
        } else {
            Vec::new()
        };
        for t in triples.iter() {
            let (options, credit) = self.scan_options(t)?.into_parts();
            if let Some(credit) = credit {
                scan_storage
                    .as_mut()
                    .expect("checked scan storage")
                    .merge(credit);
            }
            leaves.push(options);
        }
        if self.sample_filters && !filters.is_empty() {
            super::sample::prepare(self.ctx, &leaves, &filters, self.plan_charge);
        }
        if self.ctx.opt.characteristic_sets {
            use super::charsets::Term;
            let rdf_type = super::charsets::rdf_type(self.ctx);
            let incoming = super::charsets::incoming(self.ctx);
            let term = |x: PT| match x {
                PT::V(v) => Term::V(v),
                PT::C(c) => Term::C(c.0),
            };
            let mut stars: Vec<(VarId, u64)> = Vec::new();
            for t in triples.iter() {
                super::charsets::items_of(
                    term(t.t[0]),
                    term(t.t[1]),
                    term(t.t[2]),
                    rdf_type,
                    incoming,
                    &mut stars,
                );
            }
            super::charsets::register(self.ctx, &stars);
        }
        // searches that read the rest of the group are attached to it at the end
        let mut dependent = self.planning_vec()?;
        for node in nodes {
            let needs_input = match &node.kind {
                Kind::VectorSearch(s) => s.needs_input(),
                Kind::TextSearch(s) => s.needs_input(),
                Kind::HybridSearch(s) => s.needs_input(),
                Kind::SpatialPf(s) => s.needs_input(),
                Kind::PathSearch(s) => s.needs_input(),
                Kind::PropertyFn(s) => s.needs_input(),
                Kind::HistoryChanges(s) => s.needs_input(),
                Kind::AlgoCall(s) => s.needs_input(),
                _ => false,
            };
            if needs_input {
                dependent.push(node)?;
            } else {
                leaves.push(vec![node]);
            }
        }
        // A deferred FILTER whose variables only the join's leaves bind is settled once
        // the components that hold them are planned, since a component holds every leaf
        // that shares a variable with it. The join of the components can then still key
        // on it. One whose variables a path, an unpack or a dependent search also binds
        // waits for those.
        let binds_late = |v: VarId| {
            paths.iter().any(|p| p.s == PT::V(v) || p.o == PT::V(v))
                || unpacks
                    .iter()
                    .any(|u| u.t == v || u.parts.contains(&PT::V(v)))
                || dependent.iter().any(|n| n.vars.contains(&v))
        };
        let mut settled;
        let late;
        (settled, late, _late_list) =
            if self.branch_payload.borrow().is_none() || deferred.is_empty() {
                let (settled, late): (Vec<Expr>, Vec<Expr>) = deferred.into_iter().partition(|f| {
                    f.visit_vars(&mut |v| if binds_late(v) { Err(()) } else { Ok(()) })
                        .is_ok()
                });
                (settled, late, list_credit(None))
            } else {
                // The late filters move out of the deferred list, the settled ones stay.
                let mut settled = deferred;
                let (late, credit) = self.extract_filters(&mut settled, |f| {
                    f.visit_vars(&mut |v| if binds_late(v) { Err(()) } else { Ok(()) })
                        .is_err()
                })?;
                #[cfg(test)]
                if let Some(credit) = &credit {
                    note_list(3, late.len());
                    watch_list(3, &late, credit);
                }
                (settled, late, list_credit(credit))
            };
        if self.ctx.opt.probed_keys && leaves.len() > 1 {
            super::keyprobe::prepare(self.ctx, &leaves);
        }
        let mut result = if leaves.is_empty() {
            drop(leaves);
            Node::unit()
        } else if leaves.iter().any(|l| l[0].is_empty()) {
            let mut vars: Vec<VarId> = leaves.iter().flat_map(|l| l[0].vars.clone()).collect();
            vars.sort_unstable();
            vars.dedup();
            drop(leaves);
            Node::empty(vars)
        } else {
            self.join_order(leaves, &mut filters, &mut settled, &mut joined_list)?
        };
        drop(scan_storage);
        drop(leaf_storage);
        _settled_list = if self.branch_payload.borrow().is_none() {
            filters.append(&mut settled);
            list_credit(None)
        } else {
            let more = settled.len();
            self.grow_filters(&mut filters, more, 7, |f| f.append(&mut settled))?
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
            result = self.place_retained(result, &mut filters, true)?;
        }
        // decompose triple terms once their variable is bound (outer before nested)
        while let Some(i) = unpacks.iter().position(|u| result.vars.contains(&u.t)) {
            let u = unpacks.remove(i);
            result = self.unpack(result, u);
            result = self.place_retained(result, &mut filters, true)?;
        }
        for d in dependent {
            result = match &d.kind {
                Kind::SpatialPf(_) => super::geopf::attach_spatial(self, result, d)?,
                Kind::PathSearch(_) => super::pathsearch::attach(self, result, d)?,
                Kind::HistoryChanges(_) => super::history_svc::attach(self, result, d)?,
                Kind::AlgoCall(_) => super::algosvc::attach(self, result, d)?,
                Kind::PropertyFn(_) => super::arqpf::attach(result, d),
                Kind::TextSearch(_) => self.attach_text(result, d)?,
                Kind::HybridSearch(_) => super::hybrid::attach(self, result, d)?,
                _ => self.attach_vector(result, d)?,
            };
            result = self.place_retained(result, &mut filters, true)?;
        }
        _late_joined = if self.branch_payload.borrow().is_none() {
            filters.extend(late);
            list_credit(None)
        } else {
            let more = late.len();
            self.grow_filters(&mut filters, more, 8, |f| f.extend(late))?
        };
        if !filters.is_empty() {
            result = self.filter_admitted(result, filters)?;
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

    /// The constant that the point pattern `triples[i]` binds its variable to, read from
    /// the snapshot, when the pattern has one variable in one position, the store holds
    /// exactly one row of it, and the variable appears in another pattern or a FILTER of
    /// the group and nowhere a constant cannot take its place.
    #[allow(clippy::too_many_arguments)]
    fn point_value(
        &self,
        t: &Triple,
        i: usize,
        triples: &[Triple],
        filters: &[Expr],
        nodes: &[Node],
        paths: &[PathItem],
        unpacks: &[UnpackItem],
    ) -> Result<Option<(VarId, Id)>> {
        if matches!(t.graph, ActiveGraph::Var(_)) {
            return Ok(None);
        }
        let mut vars = t.t.iter().filter_map(|x| match x {
            PT::V(v) => Some(*v),
            PT::C(_) => None,
        });
        let (Some(v), None) = (vars.next(), vars.next()) else {
            return Ok(None);
        };
        let used = triples
            .iter()
            .enumerate()
            .any(|(j, o)| j != i && o.t.contains(&PT::V(v)))
            || filters.iter().any(|f| {
                f.visit_vars(&mut |var| if var == v { Err(()) } else { Ok(()) })
                    .is_err()
            });
        if !used
            || filters
                .iter()
                .any(|f| super::exprcache::impurity(f).is_some())
            || nodes.iter().any(|n| n.vars.contains(&v))
            || paths.iter().any(|p| p.s == PT::V(v) || p.o == PT::V(v))
            || unpacks
                .iter()
                .any(|u| u.t == v || u.parts.contains(&PT::V(v)))
            || triples
                .iter()
                .any(|o| matches!(o.graph, ActiveGraph::Var(g) if g == v))
        {
            return Ok(None);
        }
        let Some(scan) = self.scan_options(t)?.into_iter().next() else {
            return Ok(None);
        };
        let Kind::Scan(spec) = &scan.kind else {
            return Ok(None);
        };
        // an estimate of a few rows comes before the read, which costs a seek
        if !scan.est.is_finite() || scan.est > POINT_ROWS || !spec.eqs.is_empty() {
            return Ok(None);
        }
        let (rows, _) =
            super::exec::scan_limited(self.ctx, spec, &scan.vars, &scan.sorted, Some(2))?;
        Ok(match (rows.table.len(), rows.table.col_of(v)) {
            (1, Some(c)) => Some((v, rows.table.cols[c][0])).filter(|(_, id)| !id.is_undef()),
            _ => None,
        })
    }

    /// DP / greedy join ordering over connected components (QLever QueryPlanner).
    /// `settled` are FILTERs that may run only on whole components: each is placed on
    /// the component that binds all its variables, and the rest join the `filters` of
    /// the joins between components. On the checked route, when `filters` grow for the
    /// rest of `settled`, the credit for the grown list goes into `append_credit`, the
    /// caller's slot, which the caller declares before its list.
    fn join_order(
        &self,
        leaves: Vec<Vec<Node>>,
        filters: &mut Vec<Expr>,
        settled: &mut Vec<Expr>,
        append_credit: &mut ListCredit,
    ) -> Result<Node> {
        let n = leaves.len();
        let preparation = self
            .branch_payload
            .borrow()
            .as_ref()
            .map(|held| held.preparation().clone());
        // Component parents, the membership map, the ordered groups and the
        // moved leaf slots all coexist until the component join plans are known.
        let _components = self.planning_scratch(|layout| {
            layout.slots::<usize>(n)?;
            layout.map::<usize, PlanningVec<usize>>(n)?;
            layout.slots::<PlanningVec<usize>>(n)?;
            layout.slots::<Option<Vec<Node>>>(n)
        })?;
        // connected components by shared variables
        let mut comp: Vec<usize> = (0..n).collect();
        fn find(c: &mut [usize], x: usize) -> usize {
            let mut r = x;
            while c[r] != r {
                r = c[r];
            }
            let mut y = x;
            while c[y] != r {
                (c[y], y) = (r, c[y]);
            }
            r
        }
        if n <= 32 {
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
        } else {
            // each leaf joins the component of the first leaf with each of its variables,
            // since testing every pair costs the square of the leaves
            let mut first: FxHashMap<VarId, usize> = FxHashMap::default();
            let mut checked_first = preparation.as_ref().map(PreparationMap::new).transpose()?;
            for (i, l) in leaves.iter().enumerate() {
                for &v in &l[0].vars {
                    let j = if let Some(first) = &mut checked_first {
                        if let Some(&previous) = first.get(&v) {
                            previous
                        } else {
                            first.insert(v, i)?;
                            i
                        }
                    } else {
                        *first.entry(v).or_insert(i)
                    };
                    if j != i {
                        let (a, b) = (find(&mut comp, j), find(&mut comp, i));
                        comp[a] = b;
                    }
                }
            }
        }
        let mut groups: FxHashMap<usize, PlanningVec<usize>> = FxHashMap::default();
        if let Some(preparation) = &preparation {
            groups.try_reserve(n).map_err(|_| preparation.exhausted())?;
        }
        for i in 0..n {
            let r = find(&mut comp, i);
            match groups.entry(r) {
                std::collections::hash_map::Entry::Occupied(mut group) => {
                    group.get_mut().push(i)?
                }
                std::collections::hash_map::Entry::Vacant(group) => {
                    let mut members = self.planning_vec()?;
                    members.push(i)?;
                    group.insert(members);
                }
            }
        }
        let mut leaves: Vec<Option<Vec<Node>>> = leaves.into_iter().map(Some).collect();
        let mut group_list: Vec<_> = groups.into_values().collect();
        // Component member lists are disjoint, nonempty and ascending, so ordering them by
        // their first member equals the lexicographic order and they never tie.
        group_list.sort_unstable_by_key(|members| members[0]);
        let _components_out = self.planning_scratch(|layout| {
            layout.slots::<Node>(group_list.len())?;
            // Keep the stable cost sort of components, including equal estimates.
            layout.slots::<Node>(group_list.len())
        })?;
        let mut parts = if preparation.is_some() {
            Vec::with_capacity(group_list.len())
        } else {
            Vec::new()
        };
        for members in group_list {
            self.ctx.check()?;
            let _items =
                self.planning_scratch(|layout| layout.slots::<Vec<Node>>(members.len()))?;
            let items: Vec<Vec<Node>> =
                members.iter().map(|&i| leaves[i].take().unwrap()).collect();
            let plan = if items.len() == 1 {
                // filters can change which access path is cheapest (range pushdown)
                let opts = items.into_iter().next().unwrap();
                if preparation.is_some() {
                    self.choose_checked_scan(opts, filters)?
                } else if opts.len() == 1 {
                    let o = opts.into_iter().next().unwrap();
                    self.place_filters(o, filters)?
                } else {
                    struct Candidate<'a> {
                        node: Node,
                        rest: Vec<Expr>,
                        copy: Option<Charge<'a>>,
                        kept: Option<Charge<'a>>,
                    }
                    // the first of the cheapest, as `min_by` takes it
                    let mut best: Option<Candidate<'_>> = None;
                    let mut iter = opts.into_iter().peekable();
                    while let Some(o) = iter.next() {
                        self.ctx.check()?;
                        let (copy, mut f) = if iter.peek().is_none() {
                            (None, std::mem::take(filters))
                        } else {
                            (self.plan_filter_copy(filters)?, filters.clone())
                        };
                        let (n, kept) = self.place_plan_where(o, &mut f, false)?;
                        let candidate = Candidate {
                            node: n,
                            rest: f,
                            copy,
                            kept,
                        };
                        if best
                            .as_ref()
                            .is_none_or(|b| candidate.node.cost.total_cmp(&b.node.cost).is_lt())
                        {
                            best = Some(candidate);
                        }
                    }
                    let Candidate {
                        node,
                        rest,
                        copy,
                        kept,
                    } = best.unwrap();
                    *filters = rest;
                    drop(copy);
                    self.retain_plan_scratch(kept)?;
                    node
                }
            } else {
                #[cfg(test)]
                if REFERENCE_JOIN_ORDER.with(|r| r.get()) {
                    parts.push(self.place_filters(self.reference_order(items, filters)?, settled)?);
                    continue;
                }
                super::joinorder::order(self, items, filters)?
            };
            parts.push(self.place_filters(plan, settled)?);
        }
        // On the checked route the credit for the grown list goes into the caller's slot,
        // which is declared before the caller's list, before anything else can fail.
        if preparation.is_none() {
            filters.append(settled);
        } else {
            let more = settled.len();
            *append_credit = self.grow_filters(filters, more, 4, |f| f.append(settled))?;
        }
        // components connected by a spatial conjunct are joined on it
        super::geojoin::spatial_joins(&mut parts, filters, self.ctx);
        // cross products between components, smallest first
        parts.sort_by(|a, b| a.est.total_cmp(&b.est));
        let mut it = parts.into_iter();
        let mut acc = it.next().unwrap();
        for p in it {
            acc = self.join_components(acc, p, filters)?;
        }
        Ok(super::indexjoin::fuse_stars(acc, self.ctx))
    }

    fn choose_checked_scan(&self, options: Vec<Node>, filters: &mut Vec<Expr>) -> Result<Node> {
        struct Choice {
            node: Node,
            rest: Vec<Expr>,
            copy: Reservation,
        }

        let mut best: Option<Choice> = None;
        for option in options {
            self.ctx.check()?;
            let mut copy = self
                .planning_scratch(|layout| layout.expressions(filters))?
                .expect("checked scan choice");
            let mut rest = filters.clone();
            let (node, kept) = self.place_filters_held(option, &mut rest)?;
            if let Some(kept) = kept {
                copy.merge(kept);
            }
            let candidate = Choice { node, rest, copy };
            if best
                .as_ref()
                .is_none_or(|best| candidate.node.cost.total_cmp(&best.node.cost).is_lt())
            {
                best = Some(candidate);
            }
        }
        let Choice { node, rest, copy } = best.expect("one scan option");
        let mut active = self.branch_payload.borrow_mut();
        active.as_mut().expect("checked graph branch").merge(copy);
        *filters = rest;
        Ok(node)
    }

    /// The join of `a` and `b`, which share no variable, keyed on a conjunct `?x = ?y` of
    /// a remaining FILTER with one variable on each side (see [`Kind::KeyedJoin`]). Every
    /// FILTER that both sides together bind is tested on the candidates and removed from
    /// `filters`. Without a suitable equality it retains the ordinary product.
    fn join_components(&self, a: Node, b: Node, filters: &mut Vec<Expr>) -> Result<Node> {
        // An application callback in the condition may count or depend on its calls,
        // which the candidates change, so it keeps the product.
        if !self.ctx.opt.keyed_join
            || self.ctx.calls_extensions
            || a.is_empty()
            || b.is_empty()
            || a.vars.iter().any(|v| b.vars.contains(v))
        {
            return self.place_filters(super::joinorder::join_admitted(a, b, self)?, filters);
        }
        let bound =
            |f: &Expr| filter_binds(f, false, |v| a.vars.contains(&v) || b.vars.contains(&v));
        let Some((x, y)) = filters
            .iter()
            .filter(|f| bound(f))
            .find_map(|f| equality_key(f, &a, &b))
        else {
            return self.place_filters(super::joinorder::join_admitted(a, b, self)?, filters);
        };
        let expr = if self.branch_payload.borrow().is_none() {
            let (now, later): (Vec<Expr>, Vec<Expr>) =
                std::mem::take(filters).into_iter().partition(|f| bound(f));
            *filters = later;
            now.into_iter()
                .reduce(|l, r| Expr::And(Box::new(l), Box::new(r)))
                .expect("the linking FILTER")
        } else {
            // The same test without allocating: every occurrence of a variable is bound
            // by one of the sides, and there is one.
            // The credit is declared before the list, which is freed first on a refusal.
            let _now_list;
            let now = {
                let (now, now_list) = self.extract_filters(filters, bound)?;
                #[cfg(test)]
                if let Some(credit) = &now_list {
                    watch_list(5, &now, credit);
                }
                _now_list = list_credit(now_list);
                now
            };
            #[cfg(test)]
            note_list(5, now.len());
            // The conjunction the join keeps boxes both sides of each conjunct it adds,
            // admitted before it is made, with the plan's other storage.
            let (expr, boxes) = {
                #[cfg(test)]
                let _stage = crate::test_alloc::Stage::enter(10);
                let boxes = now.len().saturating_sub(1).saturating_mul(2);
                let boxes = self
                    .filter_list_scratch(boxes)?
                    .expect("checked join condition");
                let expr = now
                    .into_iter()
                    .reduce(|l, r| Expr::And(Box::new(l), Box::new(r)))
                    .expect("the linking FILTER");
                (expr, boxes)
            };
            self.retain_planning(boxes);
            expr
        };
        // The larger input is streamed and the smaller one indexed.
        Ok(if a.est >= b.est {
            keyed_inner_join(a, b, expr, (x, y), self.ctx)
        } else {
            keyed_inner_join(b, a, expr, (y, x), self.ctx)
        })
    }

    /// Apply (and remove) filters whose variables are all bound by `n`. On the checked
    /// route what the plan keeps of the FILTER stays with the branch's plan storage.
    pub(super) fn place_filters(&self, n: Node, filters: &mut Vec<Expr>) -> Result<Node> {
        self.place_retained(n, filters, false)
    }

    /// [`Planner::place_filters`], with the credit for what the plan keeps of the FILTER
    /// returned to the caller instead.
    pub(super) fn place_filters_held(
        &self,
        n: Node,
        filters: &mut Vec<Expr>,
    ) -> Result<(Node, Option<Reservation>)> {
        self.place_where(n, filters, false)
    }

    fn place_retained(&self, n: Node, filters: &mut Vec<Expr>, constant: bool) -> Result<Node> {
        let (n, kept) = self.place_where(n, filters, constant)?;
        if let Some(kept) = kept {
            self.retain_planning(kept);
        }
        Ok(n)
    }

    /// Apply (and remove) the filters without EXISTS whose variables are all bound by
    /// `n`, and those without variables when `constant`. On the checked route the
    /// partition of the filters is admitted before it is made, the filters not placed
    /// stay in `filters`' own list, and the credit returned holds what the plan keeps of
    /// the FILTER.
    fn place_where(
        &self,
        n: Node,
        filters: &mut Vec<Expr>,
        constant: bool,
    ) -> Result<(Node, Option<Reservation>)> {
        #[cfg(test)]
        let _stage = crate::test_alloc::Stage::within(6);
        if self.branch_payload.borrow().is_none() || filters.is_empty() {
            let (n, kept) = self.place_plan_where(n, filters, constant)?;
            self.retain_plan_scratch(kept)?;
            return Ok((n, None));
        }
        let bound = |f: &Expr| filter_binds(f, constant, |v| n.vars.contains(&v));
        self.planning_stage(FILTER_PHASE)?;
        let placed = filters.iter().filter(|f| bound(f)).count();
        #[cfg(test)]
        BOUND_TERMS.with(|terms| {
            let mut t = terms.get();
            t[2] = t[2].max(filters.len() as u64);
            terms.set(t);
        });
        // Placed filters move into a list sized before allocation. The rest stay in
        // the caller's list.
        let mut partition = self
            .planning_scratch(|layout| layout.slots::<Expr>(placed))?
            .expect("checked filter partition");
        let mut now = Vec::new();
        now.try_reserve_exact(placed)
            .map_err(|_| partition.preparation().exhausted())?;
        now.extend(filters.extract_if(.., |f| bound(f)));
        debug_assert_eq!(now.len(), placed);
        #[cfg(test)]
        watch_list(PARTITION_WATCH, &now, &partition);
        let (n, kept) = if now.is_empty() {
            (n, None)
        } else {
            let scratch = self.filter_scratch(&n, now.iter(), false)?;
            let owned = self.filters_owned(&now)?;
            self.filter_held(scratch, n, now, owned)?
        };
        // the FILTER keeps its list of conjuncts, which `kept` holds
        partition.release_to(0);
        #[cfg(test)]
        crate::test_alloc::observe_release();
        if let Some(kept) = kept {
            partition.merge(kept);
        }
        Ok((n, Some(partition)))
    }

    fn place_plan_where(
        &self,
        n: Node,
        filters: &mut Vec<Expr>,
        constant: bool,
    ) -> Result<(Node, Option<Charge<'a>>)> {
        let applies = |f: &Expr| filter_binds(f, constant, |v| n.vars.contains(&v));
        if self.plan_charge.is_none() {
            let (now, later): (Vec<_>, _) = std::mem::take(filters).into_iter().partition(applies);
            *filters = later;
            return Ok((
                if now.is_empty() {
                    n
                } else {
                    filter(n, now, self.ctx)
                },
                None,
            ));
        }
        let placed = filters.iter().filter(|f| applies(f)).count();
        if placed == 0 {
            return Ok((n, None));
        }
        let mut scratch =
            self.plan_filter_scratch(&n, filters.iter().filter(|f| applies(f)), false)?;
        if let Some(held) = &scratch {
            held.add((placed as u64).saturating_mul(size_of::<Expr>() as u64))?;
        }
        let mut now = Vec::with_capacity(placed);
        now.extend(filters.extract_if(.., |f| applies(f)));
        let owned = super::checked_layout::expressions_owned_bytes(now.iter());
        let n = self.filter_plan_held(&mut scratch, n, now, owned)?;
        Ok((n, scratch))
    }

    pub(super) fn plan_filter_scratch<'e>(
        &self,
        n: &Node,
        exprs: impl Iterator<Item = &'e Expr> + Clone,
        copies: bool,
    ) -> Result<Option<Charge<'a>>> {
        self.plan_scratch(|layout| filter_bound(layout, n, exprs, copies, self.ctx))
    }

    pub(super) fn filter_plan_held(
        &self,
        scratch: &mut Option<Charge<'_>>,
        n: Node,
        exprs: Vec<Expr>,
        owned: u64,
    ) -> Result<Node> {
        let Some(held) = scratch else {
            return Ok(filter(n, exprs, self.ctx));
        };
        let shallow = |node: &Node| {
            let mut layout = super::checked_layout::Layout::unchecked(true);
            let _ = layout.shallow(node);
            layout.bytes()
        };
        let before = shallow(&n);
        let f = filter(n, exprs, self.ctx);
        let after = shallow(&f).saturating_add(match f.kind {
            Kind::Filter(_) => shallow(&f.children[0]),
            _ => 0,
        });
        held.resize(after.saturating_sub(before).saturating_sub(owned))?;
        Ok(f)
    }

    /// The credit for [`filter`] of `n` under the filters `exprs` lists, taken before the
    /// caller copies them when `copies`, or `None` on the ordinary route.
    pub(super) fn filter_scratch<'e>(
        &self,
        n: &Node,
        exprs: impl Iterator<Item = &'e Expr> + Clone,
        copies: bool,
    ) -> Result<Option<Reservation>> {
        if self.branch_payload.borrow().is_none() {
            return Ok(None);
        }
        self.planning_stage(FILTER_PHASE)?;
        self.planning_scratch(|layout| filter_bound(layout, n, exprs, copies, self.ctx))
    }

    /// The storage that `exprs` own beyond their list, which a caller's credit holds
    /// when it moves them into a FILTER. Zero on the ordinary route.
    fn filters_owned(&self, exprs: &[Expr]) -> Result<u64> {
        let active = self.branch_payload.borrow();
        let Some(held) = active.as_ref() else {
            return Ok(0);
        };
        let mut layout = super::checked_layout::Layout::retained(held.preparation());
        for e in exprs {
            layout.expression(e)?;
        }
        Ok(layout.bytes())
    }

    /// [`filter`] of `n` under `exprs`, admitted by `scratch` from
    /// [`Planner::filter_scratch`]. `owned` is the storage of `exprs` that the caller's
    /// credit holds already. On the checked route the credit returned holds what the plan
    /// keeps beyond `n` and that storage.
    pub(super) fn filter_held(
        &self,
        scratch: Option<Reservation>,
        n: Node,
        exprs: Vec<Expr>,
        owned: u64,
    ) -> Result<(Node, Option<Reservation>)> {
        let Some(mut held) = scratch else {
            return Ok((filter(n, exprs, self.ctx), None));
        };
        let before = {
            let mut layout = super::checked_layout::Layout::retained(held.preparation());
            layout.shallow(&n)?;
            layout.bytes()
        };
        let f = filter(n, exprs, self.ctx);
        // The FILTER, and the scan under it that range pushdown made in place of `n`,
        // or `n` itself, whose storage `before` measured.
        let (after, spatial) = {
            let mut layout = super::checked_layout::Layout::retained(held.preparation());
            layout.shallow(&f)?;
            let base = match &f.kind {
                Kind::Filter(_) => {
                    layout.shallow(&f.children[0])?;
                    &f.children[0]
                }
                _ => &f,
            };
            (layout.bytes(), matches!(base.kind, Kind::SpatialScan(_)))
        };
        let kept = after.saturating_sub(before).saturating_sub(owned);
        // the spatial index's pushdown is admitted on its own (not yet)
        debug_assert!(
            spatial || kept <= held.bytes(),
            "the filter keeps {kept} bytes of the {} admitted",
            held.bytes()
        );
        if kept <= held.bytes() {
            held.release_to(kept);
        } else {
            held.grow_by(kept - held.bytes())?;
        }
        #[cfg(test)]
        crate::test_alloc::observe_release();
        Ok((f, Some(held)))
    }

    /// [`filter`] of `n` under `exprs`, which the plan's credit holds already, with what
    /// the plan keeps of it admitted on the checked route.
    fn filter_admitted(&self, n: Node, exprs: Vec<Expr>) -> Result<Node> {
        #[cfg(test)]
        let _stage = crate::test_alloc::Stage::within(6);
        if self.plan_charge.is_some() {
            let mut scratch = self.plan_filter_scratch(&n, exprs.iter(), false)?;
            let owned = super::checked_layout::expressions_owned_bytes(exprs.iter())
                .saturating_add((exprs.capacity() as u64).saturating_mul(size_of::<Expr>() as u64));
            let n = self.filter_plan_held(&mut scratch, n, exprs, owned)?;
            self.retain_plan_scratch(scratch)?;
            return Ok(n);
        }
        let mut scratch = self.filter_scratch(&n, exprs.iter(), false)?;
        // A list that filters were moved out of in place keeps its capacity, which the
        // FILTER keeps with it. That storage was made before this stage, so its credit
        // here has no allocation of the stage beside it.
        let spare = exprs.capacity() - exprs.len();
        if let Some(scratch) = scratch.as_mut()
            && spare > 0
        {
            let mut layout = super::checked_layout::Layout::new(scratch.preparation());
            layout.slots::<Expr>(spare)?;
            scratch.grow_by(layout.bytes())?;
        }
        let owned = self.filters_owned(&exprs)?;
        let (n, kept) = self.filter_held(scratch, n, exprs, owned)?;
        if let Some(kept) = kept {
            self.retain_planning(kept);
        }
        Ok(n)
    }

    // --------------------------------------------------------------- paths ------

    /// The predicates of a path that is an alternative of predicates and inverse
    /// predicates, each with whether it is inverted. A predicate that is not in the
    /// store has no edges and is left out. `None` when the path is not such an
    /// alternative, or when a predicate is a term of the query's local facts.
    fn alt_preds(&mut self, inner: &PropertyPathExpression) -> Option<Vec<(u64, bool)>> {
        use PropertyPathExpression as PP;
        if !matches!(inner, PP::Alternative(..)) {
            return None;
        }
        let mut alts = Vec::new();
        flatten_alt(inner, &mut alts);
        let mut preds = Vec::new();
        for a in alts {
            let (n, rev) = match a {
                PP::NamedNode(n) => (n, false),
                PP::Reverse(r) => match &**r {
                    PP::NamedNode(n) => (n, true),
                    _ => return None,
                },
                _ => return None,
            };
            let id = self.ctx.intern_term(&Term::NamedNode(n.clone()));
            if self.ctx.unknown_const(id) {
                continue;
            }
            if id.tag() == Tag::Local {
                return None;
            }
            preds.push((id.0, rev));
        }
        Some(preds)
    }

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
                let mut alts = self.planning_vec()?;
                flatten_alt_into(&p.path, &mut |alt| alts.push(alt))?;
                let mut branches = self.planning_vec()?;
                for a in alts {
                    let mut items = self.planning_vec()?;
                    self.collect_path(p.s, a, p.o, &p.graph, &mut items, &p.src)?;
                    branches.push(self.plan_group(items, Vec::new())?)?;
                }
                return Ok(join(left, self.union_for_branch(branches)?, self.ctx));
            }
            PP::NegatedPropertySet(set) => {
                // ?s ?p ?o FILTER(?p NOT IN set) (forward only; spargebra splits inverses)
                let pv = self.ctx.fresh_var();
                let t = Triple {
                    t: [p.s, PT::V(pv), p.o],
                    graph: p.graph.clone(),
                    src: p.src.clone(),
                };
                let mut opts = self.scan_options(&t)?;
                opts.sort_by(|a, b| a.cost.total_cmp(&b.cost));
                let scan = opts.remove(0);
                let mut n = {
                    #[cfg(test)]
                    let _stage = crate::test_alloc::Stage::within(6);
                    // the list of the set's terms, the test's two boxes and the FILTER's
                    // list, which the plan keeps
                    let built = self.planning_scratch(|layout| {
                        layout.slots::<Expr>(set.len().saturating_add(3))
                    })?;
                    let list: Vec<Expr> = set
                        .iter()
                        .map(|n| Expr::Const(self.ctx.intern_term(&Term::NamedNode(n.clone()))))
                        .collect();
                    let filters =
                        vec![Expr::Not(Box::new(Expr::In(Box::new(Expr::Var(pv)), list)))];
                    if let Some(built) = built {
                        self.retain_planning(built);
                    }
                    self.filter_admitted(scan, filters)?
                };
                n = project(
                    n.clone(),
                    n.vars.iter().copied().filter(|v| *v != pv).collect(),
                    self.ctx,
                );
                return Ok(join(left, n, self.ctx));
            }
            _ => {
                let mut items = self.planning_vec()?;
                self.collect_path(p.s, &p.path, p.o, &p.graph, &mut items, &p.src)?;
                let n = self.plan_group(items, Vec::new())?;
                return Ok(join(left, n, self.ctx));
            }
        };
        let Some((gf, gvar)) = self.graph_filter(&p.graph)? else {
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
        let bound_var = [p.s, p.o].into_iter().find_map(|e| match e {
            PT::V(v) if left.vars.contains(&v) && left.certain.contains(&v) => Some(v),
            _ => None,
        });
        let bound_side = bound_var.is_some() && !left.vars.is_empty() && gvar.is_none();
        let const_end = matches!(p.s, PT::C(_)) || matches!(p.o, PT::C(_));
        // A closure over an alternative of predicates and inverse predicates with a
        // bound or constant end walks from that end by index lookups, rather than
        // reading every edge of the alternative into memory first.
        let preds = match (simple, count) {
            (None, None) if bound_side || const_end => self.alt_preds(inner),
            _ => None,
        };
        let mut children = Vec::new();
        let mut edge_vars = None;
        let simple = match simple {
            None if preds.is_some() => None,
            Some((id, _)) if self.ctx.unknown_const(id) && min == 1 => {
                return Ok(Node::empty(left.vars.clone()));
            }
            // a predicate that is not in the store: only the zero-length path matches
            Some((id, _)) if self.ctx.unknown_const(id) && count.is_some() => {
                count = Some((0, Some(0)));
                Some((0, false))
            }
            Some((id, rev)) if id.tag() != Tag::Local || self.ctx.local_facts => Some((id.0, rev)),
            _ => {
                let (a, b) = (self.ctx.fresh_var(), self.ctx.fresh_var());
                let mut items = self.planning_vec()?;
                self.collect_path(PT::V(a), inner, PT::V(b), &p.graph, &mut items, &None)?;
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
            preds,
            graph: gf,
            graph_var: gvar,
            edge_vars,
            count,
            src: p.src.clone(),
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
        let desc = format!("{} {} {}", self.pt_str(&p.s), p.path, self.pt_str(&p.o));
        let fanout = 10.0;
        if let Some(bv) = bound_var
            && bound_side
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
    let probes = a.vars.len() + b.vars.len();
    let (in_a, in_b) = (VarSet::new(&a.vars, probes), VarSet::new(&b.vars, probes));
    let mut d = FxHashMap::default();
    for v in a.vars.iter().chain(b.vars.iter()) {
        let x = match (in_a.contains(v), in_b.contains(v)) {
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
    join_est_keys(a, b, keys, false)
}

/// [`join_est_keys`], dividing by each key's domain on either side at least (see
/// [`domain`]): the keys of a pattern restricted by a constant are a sample of the
/// predicate's values, not a superset of the other side's.
pub(super) fn join_est_dom(a: &Node, b: &Node, keys: &[VarId], backoff: bool, ctx: &Ctx) -> f64 {
    if keys.is_empty() {
        return a.est * b.est;
    }
    let cap = (backoff && keys.len() > 1 && one_input_binds(a, keys) && one_input_binds(b, keys))
        .then_some((a.est, b.est));
    let denom = keys_denom(
        keys.iter().map(|v| {
            (
                a.d(*v).max(domain(ctx, a, *v)),
                b.d(*v).max(domain(ctx, b, *v)),
            )
        }),
        backoff,
        cap,
    );
    join_est_from(a.est, b.est, denom)
}

/// The domain of `v` in plan `n`: the distinct subjects or objects of a predicate whose
/// pattern in `n` binds `v` and fixes the other column to a constant other than a class,
/// the largest over such patterns, or 0 when `n` has none. `?v p <c>` keeps the subjects
/// of `p` with the value `c`, a sample of all its subjects, so a join on `?v` keeps the
/// share `rows / distinct subjects` of the other side, where dividing by the pattern's own
/// distinct keys would take them for a superset of the other side's. Classes stay with
/// the characteristic sets, which know which predicates their instances have. The walk
/// descends through the joins, index joins, filters, sorts, projections and extends that
/// bind `v`, the operators the join order builds over its inputs, and stops at any other,
/// so that the plan builder, the join-order DP and the greedy path, which carry an
/// input's domain up through the joins by maximum, see the same domains. It is unbounded,
/// since a bound would let a deep subtree hide a scan that a summary remembers.
pub(super) fn domain(ctx: &Ctx, n: &Node, v: VarId) -> f64 {
    if !ctx.opt.domain_estimates || !ctx.snap.delta.is_empty() || !n.vars.contains(&v) {
        return 0.0;
    }
    let rdf_type = super::charsets::rdf_type(ctx);
    let mut best = 0.0f64;
    domain_walk(ctx, n, v, rdf_type, &mut best);
    best
}

fn domain_walk(ctx: &Ctx, n: &Node, v: VarId, rdf_type: Option<u64>, best: &mut f64) {
    if !n.vars.contains(&v) {
        return;
    }
    match &n.kind {
        Kind::Scan(spec) | Kind::RangeScan(spec, _) => {
            *best = best.max(scan_domain(ctx, spec, v, rdf_type));
        }
        Kind::IndexJoin(j) => {
            for p in &j.probes {
                *best = best.max(scan_domain(ctx, &p.scan, v, rdf_type));
            }
            domain_walk(ctx, &n.children[0], v, rdf_type, best);
        }
        Kind::Join { .. }
        | Kind::Filter(_)
        | Kind::Sort(_)
        | Kind::Project(_)
        | Kind::Extend(..) => {
            for c in &n.children {
                domain_walk(ctx, c, v, rdf_type, best);
            }
        }
        _ => {}
    }
}

/// [`domain`] of `v` in the scan `spec`: the predicate's distinct values of the first
/// free column when the prefix fixes the predicate and one of the subject and the object.
fn scan_domain(ctx: &Ctx, spec: &ScanSpec, v: VarId, rdf_type: Option<u64>) -> f64 {
    if spec.prefix.len() != 2 || spec.cols.first() != Some(&(2, v)) || spec.graph_col == 2 {
        return 0.0;
    }
    let order = spec.perm.order();
    let Some(pcol) = order[..2].iter().position(|c| *c == P) else {
        return 0.0;
    };
    let p = spec.prefix[pcol];
    if Some(p) == rdf_type {
        return 0.0;
    }
    let Some(stat) = ctx.snap.generation.stats.predicate(p) else {
        return 0.0;
    };
    let d = match order[2] {
        S => stat.distinct_subjects,
        O => stat.distinct_objects,
        _ => 0,
    };
    d as f64
}

/// [`join_est`], with the keys combined by [`keys_denom`]'s back-off when `backoff`.
pub(super) fn join_est_keys(a: &Node, b: &Node, keys: &[VarId], backoff: bool) -> f64 {
    if keys.is_empty() {
        return a.est * b.est;
    }
    let cap = (backoff && keys.len() > 1 && one_input_binds(a, keys) && one_input_binds(b, keys))
        .then_some((a.est, b.est));
    let denom = keys_denom(keys.iter().map(|v| (a.d(*v), b.d(*v))), backoff, cap);
    join_est_from(a.est, b.est, denom)
}

/// The denominator of a join on keys with the distinct counts `(da, db)` of each key on
/// either side. One key divides by the larger count. Without `backoff`, several keys
/// divide by the largest count alone, as if the other keys added nothing. With it, each
/// side's distinct key tuples are estimated from its counts sorted from the largest with
/// exponents 1, 1/2, 1/4 and 1/8 (exponential back-off, between the largest count and the
/// product that independent keys would give), and the join divides by the larger side's.
/// A cycle closed by a join on two keys, such as a triangle's last edge, keeps few of the
/// pairs its input holds, which the largest count alone misses by orders of magnitude.
///
/// `cap` holds the rows of both sides when each has one input that binds every key, such
/// as two predicates between the same nodes or two subqueries grouped by the keys. The
/// keys then depend on each other, a side has no more tuples than rows, and the tuples of
/// the side with fewer are taken to be among the other's, as for one key.
pub(super) fn keys_denom(
    m: impl Iterator<Item = (f64, f64)>,
    backoff: bool,
    cap: Option<(f64, f64)>,
) -> f64 {
    if !backoff {
        return m.fold(1.0f64, |acc, (da, db)| acc.max(da).max(db));
    }
    // the four largest counts of each side, largest first
    let mut top = [[1.0f64; 4]; 2];
    let mut keys = 0;
    for (da, db) in m {
        keys += 1;
        for (side, x) in top.iter_mut().zip([da, db]) {
            let mut x = x.max(1.0);
            for t in side.iter_mut() {
                if x > *t {
                    std::mem::swap(t, &mut x);
                }
            }
        }
    }
    if keys < 2 {
        return top[0][0].max(top[1][0]);
    }
    // roots by square roots, which the planner calls for every pair it costs
    let tuples = |t: &[f64; 4]| {
        let (r2, r4) = (t[2].sqrt(), t[3].sqrt().sqrt());
        t[0] * t[1].sqrt() * r2.sqrt() * r4.sqrt()
    };
    let (ta, tb) = (tuples(&top[0]), tuples(&top[1]));
    let (ta, tb) = match cap {
        Some((ea, eb)) => (ta.min(ea.max(1.0)), tb.min(eb.max(1.0))),
        None => (ta, tb),
    };
    // never below the largest count of one key
    ta.max(tb).max(top[0][0]).max(top[1][0])
}

/// Whether one input of plan `n` that its joins do not split, a pattern or anything other
/// than a join, binds every one of `keys`: one that [`input_vars`] lists.
pub(super) fn one_input_binds(n: &Node, keys: &[VarId]) -> bool {
    match &n.kind {
        Kind::Join { .. } | Kind::Filter(_) | Kind::Sort(_) => {
            n.children.iter().any(|c| one_input_binds(c, keys))
        }
        Kind::IndexJoin(j) => {
            j.probes
                .iter()
                .any(|p| keys.iter().all(|k| p.scan.cols.iter().any(|c| c.1 == *k)))
                || one_input_binds(&n.children[0], keys)
        }
        _ => keys.iter().all(|k| n.vars.contains(k)),
    }
}

/// Call `f` with the variables of each input of plan `n` that its joins do not split:
/// the patterns an index join probes and whatever is not a join, a filter or a sort.
pub(super) fn input_vars(n: &Node, f: &mut dyn FnMut(&[VarId])) {
    match &n.kind {
        Kind::Join { .. } | Kind::Filter(_) | Kind::Sort(_) => {
            for c in &n.children {
                input_vars(c, f);
            }
        }
        Kind::IndexJoin(j) => {
            for p in &j.probes {
                let vars: Vec<VarId> = p.scan.cols.iter().map(|c| c.1).collect();
                f(&vars);
            }
            input_vars(&n.children[0], f);
        }
        _ => f(&n.vars),
    }
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
        return (join_est_dom(a, b, keys, ctx.opt.key_backoff, ctx), None);
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
        let (qa, qb) = (
            super::charsets::mask_in(ctx, a, *v),
            super::charsets::mask_in(ctx, b, *v),
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
        None => (join_est_dom(a, b, keys, false, ctx), None),
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

/// A hash join builds a flat table of the smaller input, keyed on the join variables,
/// and probes it with each row of the larger one. Inserting a row into the flat table takes 10 to 15 ns on one thread while the table
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
            hash_build: FLAT_HASH_BUILD_COST,
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

/// The variables of `a` followed by those of `b` that `a` does not have, in order.
fn union_vars(a: &[VarId], b: &[VarId]) -> Vec<VarId> {
    let mut out = a.to_vec();
    if b.len() > 32 && a.len() + b.len() > 64 {
        let mut seen: FxHashSet<VarId> = a.iter().copied().collect();
        out.extend(b.iter().filter(|v| seen.insert(**v)));
    } else {
        for v in b {
            if !out.contains(v) {
                out.push(*v);
            }
        }
    }
    out
}

/// Whether variables are among `vars`: by a linear search, or by a hash set when
/// `probes` searches of that many variables would cost more than building it. A join of
/// two large plans otherwise costs the product of their numbers of variables.
pub(super) struct VarSet<'a> {
    vars: &'a [VarId],
    set: Option<FxHashSet<VarId>>,
}

impl<'a> VarSet<'a> {
    pub(super) fn new(vars: &'a [VarId], probes: usize) -> VarSet<'a> {
        let set = (vars.len() > 32 && probes > 32).then(|| vars.iter().copied().collect());
        VarSet { vars, set }
    }

    pub(super) fn contains(&self, v: &VarId) -> bool {
        match &self.set {
            Some(s) => s.contains(v),
            None => self.vars.contains(v),
        }
    }
}

/// The most that [`union_vars`] of `a` and `b` variables allocates at once: the copy of
/// the first list, which grows to hold both while old and new storage coexist, and the
/// set of seen variables of a long union with its growth.
pub(super) fn union_bound(
    layout: &mut super::checked_layout::Layout<'_>,
    a: usize,
    b: usize,
) -> Result<()> {
    let width = a.saturating_add(b);
    layout.slots::<VarId>(width.max(4).saturating_mul(3))?;
    if b > 32 && width > 64 {
        layout.map::<VarId, ()>(width)?;
        layout.map::<VarId, ()>(width)?;
    }
    Ok(())
}

/// The most that [`merge_dist`] of plans with `a` and `b` variables allocates at once:
/// the membership sets of long inputs and the table, with its growth.
pub(super) fn dist_bound(
    layout: &mut super::checked_layout::Layout<'_>,
    a: usize,
    b: usize,
) -> Result<()> {
    let width = a.saturating_add(b);
    for side in [a, b] {
        if side > 32 && width > 32 {
            layout.map::<VarId, ()>(side)?;
        }
    }
    layout.map::<VarId, f64>(width)?;
    layout.map::<VarId, f64>(width)
}

/// The most that [`mk_join`] of `a` and `b` allocates at once, counting what the join
/// keeps: the variable unions, the order, the distinct values, a short description and
/// the children. The keys are the caller's.
pub(super) fn join_bound(
    layout: &mut super::checked_layout::Layout<'_>,
    a: &Node,
    b: &Node,
) -> Result<()> {
    union_bound(layout, a.vars.len(), b.vars.len())?;
    union_bound(layout, a.certain.len(), b.certain.len())?;
    layout.slots::<VarId>(a.sorted.len().max(b.sorted.len()).max(1))?;
    dist_bound(layout, a.vars.len(), b.vars.len())?;
    layout.add(64)?;
    layout.slots::<Node>(2)
}

/// The most that [`merge_join`] of `a` and `b` on `v`, which share `keys` variables,
/// allocates at once: the join, a sort over each input not sorted on `v`, the keys with
/// `v` first, and the description naming `v`.
pub(super) fn merge_join_bound(
    layout: &mut super::checked_layout::Layout<'_>,
    a: &Node,
    b: &Node,
    v: VarId,
    keys: usize,
    ctx: &Ctx,
) -> Result<()> {
    join_bound(layout, a, b)?;
    for input in [a, b] {
        if input.sorted.first() != Some(&v) {
            layout.metadata(input)?;
            layout.slots::<Node>(1)?;
            layout.slots::<VarId>(2)?;
        }
    }
    // the keys grow by pushes from one, and old and new storage can coexist
    layout.slots::<VarId>(keys.saturating_add(1).max(4).saturating_mul(3))?;
    let name = ctx.var_names.read()[v as usize].len();
    layout.add(name.saturating_add(16).saturating_mul(4))
}

/// The most that [`hash_join`] of `a` and `b` on `keys` allocates at once: the join and
/// its description, which spells each key, collects the spellings and joins them.
pub(super) fn hash_join_bound(
    layout: &mut super::checked_layout::Layout<'_>,
    a: &Node,
    b: &Node,
    keys: impl Iterator<Item = VarId>,
    ctx: &Ctx,
) -> Result<()> {
    join_bound(layout, a, b)?;
    let names = ctx.var_names.read();
    let (mut count, mut text) = (0usize, 0usize);
    for v in keys {
        count += 1;
        text = text.saturating_add(names[v as usize].len().saturating_add(2));
    }
    drop(names);
    layout.slots::<String>(count)?;
    layout.add(text.saturating_mul(6))?;
    layout.add(64)
}

pub(super) fn mk_join(a: Node, b: Node, algo: JoinAlgo, keys: Vec<VarId>, ctx: &Ctx) -> Node {
    let (est, star) = {
        // estimates can grow the query's memo of characteristic sets (a stage of its own)
        #[cfg(test)]
        let _stage = crate::test_alloc::Stage::enter(8);
        join_est_with(&a, &b, &keys, ctx)
    };
    let vars = union_vars(&a.vars, &b.vars);
    let certain = union_vars(&a.certain, &b.certain);
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

/// Whether [`join`] of `a` and `b` returns one of them unchanged, since the other is
/// the one empty solution.
pub(super) fn join_keeps_input(a: &Node, b: &Node) -> bool {
    let unit = |n: &Node| n.vars.is_empty() && matches!(&n.kind, Kind::Values(t) if t.len() == 1);
    !a.is_empty() && !b.is_empty() && (unit(a) || unit(b))
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

/// Independent payloads of one candidate and its output join metadata.
struct GraphJoinCandidate {
    node: Node,
    held: Reservation,
}

fn graph_join_metadata(
    layout: &mut super::checked_layout::Layout<'_>,
    branch: &Node,
    binding: &Node,
    var: VarId,
    ctx: &Ctx,
) -> Result<()> {
    let width = branch
        .vars
        .len()
        .checked_add(1)
        .ok_or_else(|| ctx.memory_exceeded(u64::MAX))?;
    // Variables and certainty clone the branch's vector before appending the one
    // variable the binding can add. The old and replacement vectors can coexist.
    let schemas = width
        .max(4)
        .checked_mul(6)
        .ok_or_else(|| ctx.memory_exceeded(u64::MAX))?;
    layout.slots::<VarId>(schemas)?;
    layout.slots::<VarId>(branch.sorted.len().max(binding.sorted.len()).max(1))?;
    layout.slots::<Node>(2)?;
    layout.slots::<VarId>(1)?;
    // Join distributions grow a raw table and may use a temporary variable
    // membership set for the larger side.
    layout.map::<VarId, f64>(width)?;
    layout.map::<VarId, f64>(width)?;
    layout.map::<VarId, ()>(branch.vars.len())?;
    let name_len = ctx.var_names.read()[var as usize].len();
    // A one-key description can hold only the key and fixed framing. Hash join
    // retains the name, its prefixed spelling and the joined text while formatting.
    let text = name_len
        .checked_add(32)
        .and_then(|len| len.checked_mul(8))
        .ok_or_else(|| ctx.memory_exceeded(u64::MAX))?;
    layout.add(text)?;
    layout.slots::<String>(1)
}

#[cfg(test)]
thread_local! {
    /// The most bytes a graph's branch kept beyond the credit admitted while it was
    /// built, on this thread since the last reset.
    pub(super) static BRANCH_SHORTFALL: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
}

/// The event before the index candidate of a graph's binding is admitted.
pub(super) const GRAPH_INDEX_PHASE: &str = "reserving graph binding index join";

#[cfg(test)]
thread_local! {
    /// The index candidates of graph bindings built on this thread since the last reset.
    pub(super) static GRAPH_INDEX_OFFERS: std::cell::Cell<usize> =
        const { std::cell::Cell::new(0) };
}

fn checked_graph_binding_join(
    branch: Node,
    binding: Node,
    var: VarId,
    ctx: &Ctx,
    original: &mut Reservation,
) -> Result<Node> {
    let preparation = original.preparation().clone();
    let mut layout = super::checked_layout::Layout::new(&preparation);
    graph_join_metadata(&mut layout, &branch, &binding, var, ctx)?;

    // The empty and identity cases and the Cartesian path keep their owning
    // inputs without making alternate copies.
    if !branch.vars.contains(&var) {
        original.grow_by(layout.bytes())?;
        return Ok(join(branch, binding, ctx));
    }

    let mut copies = super::checked_layout::Layout::new(&preparation);
    copies.node(&branch)?;
    copies.node(&binding)?;
    let alternative = |merge: bool| -> Result<GraphJoinCandidate> {
        preparation.stage("copying graph join candidate")?;
        let mut held = preparation.reserve(layout.bytes())?;
        held.grow_by(copies.bytes())?;
        if merge {
            for input in [&branch, &binding] {
                if input.sorted.first() != Some(&var) {
                    let mut sort = super::checked_layout::Layout::new(&preparation);
                    sort.metadata(input)?;
                    sort.slots::<Node>(1)?;
                    sort.slots::<VarId>(2)?;
                    held.grow_by(sort.bytes())?;
                }
            }
        }
        let a = branch.clone();
        let b = binding.clone();
        let node = if merge {
            merge_join(a, b, var, &[var], ctx)
        } else {
            hash_join(a, b, vec![var], ctx)
        };
        Ok(GraphJoinCandidate { node, held })
    };
    let merge = (branch.certain.contains(&var)
        && merge_offered(
            branch.est,
            branch.sorted.first() == Some(&var),
            binding.est,
            binding.sorted.first() == Some(&var),
        ))
    .then(|| alternative(true))
    .transpose()?;
    let hash = alternative(false)?;
    let index = if ctx.opt.batched_join {
        #[cfg(test)]
        let _stage = crate::test_alloc::Stage::enter(7);
        // The candidate copies the binding, and offering it copies the branch's filters
        // and builds the operator, whether or not it is then offered.
        preparation.stage(GRAPH_INDEX_PHASE)?;
        let mut bound = super::checked_layout::Layout::new(&preparation);
        bound.node(&binding)?;
        super::indexjoin::index_join_bound(&mut bound, &binding, &branch, ctx)?;
        let mut held = preparation.reserve(bound.bytes())?;
        match super::indexjoin::graph_binding_candidate(&binding, &branch, ctx) {
            Some(node) => {
                #[cfg(test)]
                GRAPH_INDEX_OFFERS.with(|offers| offers.set(offers.get() + 1));
                // The offer's temporary storage is gone, so the candidate keeps only
                // what its plan keeps.
                let mut retained = super::checked_layout::Layout::retained(&preparation);
                retained.node(&node)?;
                let kept = retained.bytes();
                debug_assert!(
                    kept <= held.bytes(),
                    "the index candidate keeps {kept} bytes of the {} admitted",
                    held.bytes()
                );
                held.grow_to(kept)?;
                held.release_to(kept);
                Some(GraphJoinCandidate { node, held })
            }
            None => None,
        }
    } else {
        None
    };
    let winner = [merge, Some(hash), index]
        .into_iter()
        .flatten()
        .min_by(|a, b| a.node.cost.total_cmp(&b.node.cost))
        .expect("a hash candidate is always available");
    drop(branch);
    drop(binding);
    original.release_to(0);
    original.merge(winner.held);
    Ok(winner.node)
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
    // The keyed join tests the condition on fewer pairs, so an application callback in
    // it, which may count or depend on its calls, keeps the generic join.
    if keys.is_empty()
        && ctx.opt.keyed_left_join
        && !ctx.calls_extensions
        && let Some(e) = &expr
        && let Some((a, b)) = equality_key(e, &l, &r)
    {
        return keyed_left_join(l, r, expr.unwrap(), (a, b), ctx);
    }
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

/// The first conjunct `?a = ?b` of an OPTIONAL condition whose variables come one from
/// each side, as (left variable, right variable). The sides must share no variable, so
/// that the pair is the only link between them.
fn equality_key(e: &Expr, l: &Node, r: &Node) -> Option<(VarId, VarId)> {
    fn find(e: &Expr, l: &Node, r: &Node) -> Option<(VarId, VarId)> {
        match e {
            Expr::And(a, b) => find(a, l, r).or_else(|| find(b, l, r)),
            Expr::Eq(a, b) => match (&**a, &**b) {
                (Expr::Var(x), Expr::Var(y)) if l.vars.contains(x) && r.vars.contains(y) => {
                    Some((*x, *y))
                }
                (Expr::Var(x), Expr::Var(y)) if l.vars.contains(y) && r.vars.contains(x) => {
                    Some((*y, *x))
                }
                _ => None,
            },
            _ => None,
        }
    }
    find(e, l, r)
}

/// The OPTIONAL of `l` and `r` keyed on the condition's equality of `a` (left) and `b`
/// (right), estimated as a join on one variable rather than as their product.
fn keyed_left_join(l: Node, r: Node, expr: Expr, (a, b): (VarId, VarId), ctx: &Ctx) -> Node {
    let est = join_est_from(l.est, r.est, l.d(a).max(r.d(b))).max(l.est);
    let mut vars = l.vars.clone();
    vars.extend(r.vars.iter().filter(|v| !l.vars.contains(v)));
    let desc = format!(
        "on ?{} = ?{} filter {}",
        ctx.var_name(a),
        ctx.var_name(b),
        expr.display(ctx)
    );
    let dist = merge_dist(&l, &r, est, None);
    let base = hash_base(Costing::of(ctx).hash_build, l.est, r.est);
    Node {
        kind: Kind::KeyedLeftJoin {
            expr,
            left: a,
            right: b,
        },
        vars,
        certain: l.certain.clone(),
        sorted: Vec::new(),
        est,
        cost: l.cost + r.cost + base + est,
        dist,
        desc,
        children: vec![l, r],
    }
}

/// The inner join of `l` and `r` keyed on the equality of `a` (left) and `b` (right),
/// with the condition `expr` tested on the candidates (see [`Kind::KeyedJoin`]). It is
/// estimated as a join on one variable whose result the rest of the condition filters.
fn keyed_inner_join(l: Node, r: Node, expr: Expr, (a, b): (VarId, VarId), ctx: &Ctx) -> Node {
    let joined = join_est_from(l.est, r.est, l.d(a).max(r.d(b)));
    let conjuncts = expr.conjunct_count().max(1);
    let est = (joined * FILTER_SELECTIVITY.powi(conjuncts as i32 - 1)).max(1.0);
    let mut vars = l.vars.clone();
    vars.extend(r.vars.iter().filter(|v| !l.vars.contains(v)));
    let mut certain = l.certain.clone();
    certain.extend(r.certain.iter().filter(|v| !l.certain.contains(v)));
    let desc = format!(
        "on ?{} = ?{} filter {}",
        ctx.var_name(a),
        ctx.var_name(b),
        expr.display(ctx)
    );
    let dist = merge_dist(&l, &r, est, None);
    let base = hash_base(Costing::of(ctx).hash_build, r.est, l.est);
    Node {
        kind: Kind::KeyedJoin {
            expr,
            left: a,
            right: b,
            ripple: false,
        },
        vars,
        certain,
        sorted: Vec::new(),
        est,
        cost: l.cost + r.cost + base + joined + est,
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
    let mut node = union_shape(&children, || Ok(())).expect("uncontrolled union shape");
    node.children = children;
    node
}

fn union_shape(children: &[Node], check: impl Fn() -> Result<()>) -> Result<Node> {
    let mut vars: Vec<VarId> = Vec::new();
    let mut shared = children[0].certain.clone();
    let mut est = 0.0;
    let mut cost = 0.0;
    for child in children {
        check()?;
        for &var in &child.vars {
            if !vars.contains(&var) {
                vars.push(var);
            }
        }
        shared.retain(|var| child.certain.contains(var));
        est += child.est;
        cost += child.cost;
    }
    let certain = vars
        .iter()
        .copied()
        .filter(|v| shared.contains(v))
        .collect();
    let dist = vars.iter().map(|&v| (v, est.max(1.0))).collect();
    Ok(Node {
        kind: Kind::Union,
        vars,
        certain,
        sorted: Vec::new(),
        est,
        cost: cost + est,
        dist,
        desc: format!("{} branches", children.len()),
        children: Vec::new(),
    })
}

fn checked_union_shape(
    children: &[Node],
    preparation: &Preparation,
) -> Result<(Node, Reservation)> {
    use std::fmt::Write;

    let mut vars = PreparationBuffer::new(preparation)?;
    let mut seen = PreparationMap::<VarId, ()>::new(preparation)?;
    let mut occurrences = PreparationMap::new(preparation)?;
    let mut est = 0.0;
    let mut cost = 0.0;
    for (index, child) in children.iter().enumerate() {
        preparation.check()?;
        for &var in &child.vars {
            if seen.insert(var, ())?.is_none() {
                vars.push(var)?;
            }
        }
        for &var in &child.certain {
            let (count, last) = occurrences.get(&var).copied().unwrap_or((0, usize::MAX));
            if last != index {
                occurrences.insert(var, (count + 1, index))?;
            }
        }
        est += child.est;
        cost += child.cost;
    }
    drop(seen);

    let mut certain = PreparationBuffer::new(preparation)?;
    let mut dist = PreparationMap::new(preparation)?;
    for &var in &*vars {
        if occurrences
            .get(&var)
            .is_some_and(|(count, _)| *count == children.len())
        {
            certain.push(var)?;
        }
        dist.insert(var, est.max(1.0))?;
    }
    drop(occurrences);

    let length = (children.len().ilog10() + 1) as usize + " branches".len();
    let mut held = preparation.reserve(length as u64)?;
    let mut desc = String::new();
    desc.try_reserve_exact(length)
        .map_err(|_| preparation.exhausted())?;
    write!(desc, "{} branches", children.len()).expect("writing to a String");
    let (vars, vars_credit) = vars.into_parts();
    let (certain, certain_credit) = certain.into_parts();
    let (dist, dist_credit) = dist.into_parts();
    held.merge(vars_credit);
    held.merge(certain_credit);
    held.merge(dist_credit);
    Ok((
        Node {
            kind: Kind::Union,
            vars,
            certain,
            sorted: Vec::new(),
            est,
            cost: cost + est,
            dist,
            desc,
            children: Vec::new(),
        },
        held,
    ))
}

/// The phase that admits a FILTER placed on a plan (see [`Planner::filter_scratch`]).
pub(super) const FILTER_PHASE: &str = "reserving filter";

/// The phase that admits a list of filters that planning moves filters into or grows.
pub(super) const FILTER_LIST_PHASE: &str = "reserving filter list";

/// The credit for a list of filters, held while the caller plans with the list. Its
/// binding is declared before the list's binding, or before the parameter that owns the
/// list is bound again, so that on every path the list is freed, or moved into storage
/// admitted on its own, before the credit returns. Unit tests settle its drop with the
/// observer, as credit handed up by the stage that acquired it.
#[cfg(test)]
type ListCredit = crate::test_alloc::Settled;
#[cfg(not(test))]
type ListCredit = Option<Reservation>;

#[cfg(test)]
thread_local! {
    /// The longest admitted filter list per producer: the FILTER's conjuncts, the
    /// filters moved below an operator, the deferred and the late filters, the
    /// remaining filters that the settled ones join after the components, the filters
    /// of a keyed join, the list of one FILTER that calls application extensions, and
    /// the remaining filters that the settled ones join without components and that the
    /// late ones join.
    pub(super) static LIST_REACH: std::cell::Cell<[usize; 9]> =
        const { std::cell::Cell::new([0; 9]) };
}

#[cfg(test)]
type ListWatch = Box<dyn Fn(usize, &[Expr], &Reservation)>;

#[cfg(test)]
thread_local! {
    /// A test's look at each filter list on the checked route right after it is made or
    /// grown: the producer as in [`LIST_REACH`], or [`PARTITION_WATCH`] for the filters
    /// that [`Planner::place_where`] places, then the list and the credit that holds it.
    /// The test may watch the list's storage, fill the budget or cancel there, and then
    /// see whether the list is freed before its credit returns.
    pub(super) static LIST_WATCH: std::cell::RefCell<Option<ListWatch>> =
        const { std::cell::RefCell::new(None) };
}

/// The [`LIST_WATCH`] site of the filters that [`Planner::place_where`] places.
#[cfg(test)]
pub(super) const PARTITION_WATCH: usize = 9;

#[cfg(test)]
fn watch_list(site: usize, list: &[Expr], credit: &Reservation) {
    LIST_WATCH.with(|watch| {
        if let Some(watch) = watch.borrow().as_ref() {
            watch(site, list, credit);
        }
    });
}

#[cfg(test)]
fn note_list(site: usize, len: usize) {
    LIST_REACH.with(|reach| {
        let mut all = reach.get();
        all[site] = all[site].max(len);
        reach.set(all);
    });
}

#[cfg(test)]
fn list_credit(held: Option<Reservation>) -> ListCredit {
    crate::test_alloc::Settled::new(crate::test_alloc::Origin::handed_up(), held)
}
#[cfg(not(test))]
fn list_credit(held: Option<Reservation>) -> ListCredit {
    held
}

fn filter_binds(f: &Expr, constant: bool, contains: impl Fn(VarId) -> bool) -> bool {
    if f.has_exists() {
        return false;
    }
    let mut any = false;
    f.visit_vars(&mut |v| {
        any = true;
        if contains(v) { Ok(()) } else { Err(()) }
    })
    .is_ok()
        && (constant || any)
}

/// The most that [`filter`] of `n` under `exprs` allocates at once, counting what the
/// FILTER keeps but not what it keeps of `n`, and the copies of `exprs` when `copies`.
/// Moving range conjuncts into a scan of `n` splits the filters into their conjuncts,
/// sorts them and renders the moved ones, and copies the scan. A FILTER renders every
/// conjunct for its description and its sampled selectivity, and a FILTER over a
/// scan's sorted column may compile the conjuncts on it to find the key ranges it
/// reads. The spatial index's pushdown is not counted here.
pub(super) fn filter_bound<'e>(
    layout: &mut super::checked_layout::Layout<'_>,
    n: &Node,
    exprs: impl Iterator<Item = &'e Expr> + Clone,
    copies: bool,
    ctx: &Ctx,
) -> Result<()> {
    let (mut count, mut conjuncts, mut text, mut most) = (0usize, 0usize, 0usize, 0usize);
    for e in exprs.clone() {
        count += 1;
        conjuncts = conjuncts.saturating_add(e.conjunct_count());
        text = text
            .saturating_add(8)
            .saturating_add(e.description_bytes(ctx)?);
        let mut occurrences = 0usize;
        let _ = e.visit_vars(&mut |_| {
            occurrences += 1;
            Ok::<(), ()>(())
        });
        most = most.max(occurrences);
    }
    let copy = |layout: &mut super::checked_layout::Layout<'_>| -> Result<()> {
        layout.slots::<Expr>(count.max(4).saturating_mul(2))?;
        for e in exprs.clone() {
            layout.expression(e)?;
        }
        Ok(())
    };
    if copies {
        copy(layout)?;
    }
    let slots = conjuncts.max(4);
    let checked = layout.is_checked();
    // The individual strings can each hold nearly twice their text. Their joined
    // copy costs one more, and a selectivity note can cause a further reformat.
    let text = text
        .saturating_add(conjuncts.saturating_mul(4))
        .saturating_add(64);
    let texts = if checked {
        text.saturating_add(n.desc.len()).saturating_mul(6)
    } else if ctx.has_sampled() {
        text.saturating_mul(3)
            .saturating_add(conjuncts.saturating_mul(512))
    } else {
        text.saturating_mul(3)
    };
    if let Kind::Scan(spec) = &n.kind {
        // Range pushdown splits the filters into their conjuncts and partitions them.
        layout.slots::<Expr>(slots.saturating_mul(8))?;
        if !checked && super::geopf::copies_filters(n, exprs.clone(), ctx) {
            copy(layout)?;
        }
        fn compares(e: &Expr) -> bool {
            match e {
                Expr::And(a, b) => compares(a) || compares(b),
                Expr::Cmp(..) | Expr::Eq(..) => true,
                _ => false,
            }
        }
        let range = checked
            || (ctx.opt.range_pushdown
                && spec
                    .cols
                    .first()
                    .is_some_and(|&(c, _)| c == spec.prefix.len() && c != spec.graph_col)
                && exprs.clone().any(compares));
        #[cfg(test)]
        let sizes = std::cell::Cell::new((0u64, 0u64, 0usize));
        // Range conjuncts can move into a range scan. Otherwise the scan can read
        // key ranges using the same expressions by reference.
        layout.larger(
            |layout| {
                if !range {
                    return Ok(());
                }
                #[cfg(test)]
                let start = layout.bytes();
                // the atoms, the id ranges, the rendered conjuncts and the scan's copy
                layout.slots::<(RangeOp, super::value::Value)>(slots.saturating_mul(2))?;
                layout.slots::<IdRange>(64)?;
                layout.slots::<String>(conjuncts)?;
                layout.add(texts.saturating_add(n.desc.len().saturating_mul(2)))?;
                layout.scan(spec)?;
                #[cfg(test)]
                sizes.set((layout.bytes() - start, sizes.get().1, sizes.get().2));
                Ok(())
            },
            |layout| {
                #[cfg(test)]
                let start = layout.bytes();
                // The key tests, their prefixes, and the resulting ID ranges.
                if checked {
                    layout.slots::<Expr>(slots.saturating_mul(2))?;
                    for e in exprs.clone() {
                        layout.expression(e)?;
                    }
                }
                if let Some(&(_, v)) = spec.cols.first() {
                    let mut tests = 0usize;
                    for e in exprs.clone() {
                        tests = tests
                            .saturating_add(super::keyfilter::KeyFilter::plan_prefixes_bytes(e, v));
                    }
                    layout.add(tests)?;
                    #[cfg(test)]
                    sizes.set((sizes.get().0, sizes.get().1, tests));
                }
                layout.slots::<(u64, u64)>(4)?;
                #[cfg(test)]
                sizes.set((sizes.get().0, layout.bytes() - start, sizes.get().2));
                Ok(())
            },
        )?;
        #[cfg(test)]
        if checked {
            // the terms of the branch that sets the bound
            let (range, keys, tests) = sizes.get();
            BOUND_TERMS.with(|terms| {
                let mut t = terms.get();
                if range >= keys {
                    t[0] = t[0].max(range);
                } else {
                    t[1] = t[1].max(tests as u64);
                }
                terms.set(t);
            });
        }
    }
    // the variables of one filter at a time, listed with every occurrence, and the
    // reason a filter is not evaluated once per value, when it costs the FILTER's sort
    // or tests the scan's key column
    layout.slots::<VarId>(most.max(4).saturating_mul(2))?;
    layout.add(128)?;
    // the rendered conjuncts, their sampled selectivities, the description, and the
    // FILTER's copy of its input's metadata
    layout.slots::<String>(conjuncts)?;
    layout.slots::<Option<super::sample::Sampled>>(conjuncts)?;
    layout.add(texts)?;
    layout.metadata(n)?;
    layout.slots::<Node>(1)
}

pub fn filter(n: Node, exprs: Vec<Expr>, ctx: &Ctx) -> Node {
    #[cfg(test)]
    let _stage = crate::test_alloc::Stage::within(6);
    let (n, exprs) = push_range(n, exprs, ctx);
    let (n, exprs) = {
        #[cfg(test)]
        let _stage = crate::test_alloc::Stage::enter(9);
        super::geopf::push_spatial(n, exprs, ctx)
    };
    if exprs.is_empty() {
        return n;
    }
    let texts: Vec<String> = exprs.iter().map(|e| e.display(ctx)).collect();
    let sampled: Vec<Option<super::sample::Sampled>> =
        texts.iter().map(|t| ctx.sampled(t)).collect();
    let sel = super::sample::combine(sampled.iter().map(|s| s.map(|s| s.sel)));
    let desc = if let Some(note) = super::sample::note(&sampled) {
        let size = texts.iter().map(String::len).sum::<usize>()
            + 4 * texts.len().saturating_sub(1)
            + 1
            + note.len();
        let mut desc = String::with_capacity(size);
        for (i, text) in texts.iter().enumerate() {
            if i > 0 {
                desc.push_str(" && ");
            }
            desc.push_str(text);
        }
        desc.push(' ');
        desc.push_str(&note);
        desc
    } else {
        texts.join(" && ")
    };
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
    if !o.filter_scan_runs {
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
    let on_key = exprs
        .iter()
        .filter(|e| super::exprcache::input(&[e]) == Ok(Some(v)));
    #[cfg(test)]
    let mut key_filters = 0;
    #[cfg(test)]
    let on_key = on_key.inspect(|_| key_filters += 1);
    let prefixes = super::keyfilter::KeyFilter::plan_prefixes(on_key, v)?;
    #[cfg(test)]
    REACHED.with(|r| r.set((r.get().0, r.get().1.max(key_filters))));
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
        // the store's reads, which the planner's credit does not cover (as for join
        // estimates)
        #[cfg(test)]
        let _stage = crate::test_alloc::Stage::enter(8);
        rows += ctx
            .snap
            .count_between(spec.perm, bound(lo, 0), bound(hi, u64::MAX))
            .ok()? as f64;
    }
    Some((rows.min(n.est), ids.len()))
}

/// The variables that the BINDs directly under an ORDER BY (a chain of `Extend`s, as
/// SELECT expressions and BIND produce) bind to an expression whose value depends only
/// on the row. An ORDER BY key equal to such an expression orders by its variable: the
/// variable holds the expression's value, and is unbound where the expression is an
/// error, which orders as the error does. An expression that reads a variable bound by
/// its own or a later BIND of the chain would read another value at the ORDER BY, so it
/// is left out.
fn bound_expressions<'a>(
    planner: &Planner<'_>,
    gp: &'a GraphPattern,
) -> Result<PlanningVec<(&'a oxrdf::Variable, &'a Expression)>> {
    let mut out = planner.planning_vec()?;
    // the variables bound between an Extend's input and the ORDER BY
    let mut later = planner.planning_vec()?;
    let mut gp = gp;
    while let GraphPattern::Extend {
        inner,
        variable,
        expression,
    } = gp
    {
        later.push(variable)?;
        let mut read = planner.planning_vec()?;
        expression_vars(expression, &mut read)?;
        if row_determined(expression) && !read.iter().any(|v| later.contains(v)) {
            out.push((variable, expression))?;
        }
        gp = inner;
    }
    Ok(out)
}

/// The variables an expression reads (outside EXISTS, which [`row_determined`] rejects).
fn expression_vars<'e>(
    e: &'e spargebra::algebra::Expression,
    out: &mut PlanningVec<&'e spargebra::term::Variable>,
) -> Result<()> {
    use spargebra::algebra::Expression as E;
    match e {
        E::Variable(v) | E::Bound(v) => out.push(v)?,
        E::NamedNode(_) | E::Literal(_) | E::Exists(_) => {}
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
            expression_vars(a, out)?;
            expression_vars(b, out)?;
        }
        E::UnaryPlus(a) | E::UnaryMinus(a) | E::Not(a) => expression_vars(a, out)?,
        E::In(a, l) => {
            expression_vars(a, out)?;
            for item in l {
                expression_vars(item, out)?;
            }
        }
        E::If(a, b, c) => {
            expression_vars(a, out)?;
            expression_vars(b, out)?;
            expression_vars(c, out)?;
        }
        E::Coalesce(l) | E::FunctionCall(_, l) => {
            for item in l {
                expression_vars(item, out)?;
            }
        }
    }
    Ok(())
}

/// Whether an expression's value is the same each time it is evaluated on a row: no
/// random or fresh values, no EXISTS, and no function other than SPARQL's and the
/// supported XSD casts.
fn row_determined(e: &spargebra::algebra::Expression) -> bool {
    use spargebra::algebra::{Expression as E, Function as F};
    match e {
        E::NamedNode(_) | E::Literal(_) | E::Variable(_) | E::Bound(_) => true,
        E::Exists(_) => false,
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
        | E::Divide(a, b) => row_determined(a) && row_determined(b),
        E::UnaryPlus(a) | E::UnaryMinus(a) | E::Not(a) => row_determined(a),
        E::In(a, l) => row_determined(a) && l.iter().all(row_determined),
        E::If(a, b, c) => row_determined(a) && row_determined(b) && row_determined(c),
        E::Coalesce(l) => l.iter().all(row_determined),
        E::FunctionCall(f, args) => {
            let fixed = match f {
                F::BNode | F::Rand | F::Uuid | F::StrUuid => false,
                F::Custom(iri) => super::expr::is_cast(iri.as_str()),
                _ => true,
            };
            fixed && args.iter().all(row_determined)
        }
    }
}

/// Comparison of the range variable with a constant: `?v op c`.
#[derive(Clone, Copy, PartialEq, Debug)]
pub(super) enum RangeOp {
    Lt,
    Le,
    Gt,
    Ge,
    Eq,
}

/// `?v op c` for a filter conjunct comparing variable `v` with a numeric literal.
pub(super) fn range_atom(e: &Expr, v: VarId, ctx: &Ctx) -> Option<(RangeOp, super::value::Value)> {
    compare_atom(e, v, ctx, super::value::Value::is_numeric)
}

/// `?v op c` for a filter conjunct comparing variable `v` with a constant `c` that
/// `accept` admits. Arithmetic in the constant is folded only over numbers.
pub(super) fn compare_atom(
    e: &Expr,
    v: VarId,
    ctx: &Ctx,
    accept: fn(&super::value::Value) -> bool,
) -> Option<(RangeOp, super::value::Value)> {
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
            Expr::Arith(x, y, op) => {
                use super::expr::ArithOp;
                let op = match op {
                    ArithOp::Add => NumOp::Add,
                    ArithOp::Sub => NumOp::Sub,
                    ArithOp::Mul => NumOp::Mul,
                    ArithOp::Div => NumOp::Div,
                };
                let (x, y) = (constant(x, ctx)?, constant(y, ctx)?);
                if !(x.is_numeric() && y.is_numeric()) {
                    return None;
                }
                arith(op, &x, &y).ok()
            }
            _ => None,
        }
    }
    let constant = |e: &Expr| constant(e, ctx);
    let (c, flip) = match (&**a, &**b) {
        (Expr::Var(x), c) if *x == v => (constant(c)?, false),
        (c, Expr::Var(x)) if *x == v => (constant(c)?, true),
        _ => return None,
    };
    if !accept(&c) {
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
pub(super) fn numeric_ranges(atoms: &[(RangeOp, super::value::Value)], ctx: &Ctx) -> Vec<IdRange> {
    let residual = |tag: Tag| IdRange {
        lo: Id::new(tag, 0).0,
        hi: Id::new(tag, crate::id::PAYLOAD_MASK).0,
        exact: false,
    };
    // tags in id order: Int < Double < Vocab < Delta < Decimal
    let segments = numeric_segments();
    let (ints, decimals) = segments.split_at(2);
    let segment = |&(tag, lo, hi): &(Tag, u64, u64)| numeric_segment(atoms, ctx, tag, lo, hi);
    let out: Vec<IdRange> = ints
        .iter()
        .filter_map(segment)
        .chain([Tag::Double, Tag::Vocab, Tag::Delta].map(residual))
        .chain(decimals.iter().filter_map(segment))
        .collect();
    debug_assert!(out.windows(2).all(|w| w[0].hi < w[1].lo));
    out
}

/// Bits of an inline decimal's mantissa, above which its payload holds its scale.
pub(super) const DECIMAL_MANTISSA: u32 = 56;

/// The segments of inline numeric ids in which ids sort by value, in id order, as a tag
/// and the first and last payload: the non-negative and the negative integers, then the
/// non-negative and the negative decimals of each scale.
pub(super) fn numeric_segments() -> [(Tag, u64, u64); 34] {
    let half = 1u64 << (crate::id::PAYLOAD_BITS - 1);
    let m = 1u64 << (DECIMAL_MANTISSA - 1);
    std::array::from_fn(|i| match i {
        0 => (Tag::Int, 0, half - 1),
        1 => (Tag::Int, half, 2 * half - 1),
        _ => {
            let (scale, upper) = (((i - 2) / 2) as u64, (i - 2) % 2 == 1);
            let lo = (scale << DECIMAL_MANTISSA) + if upper { m } else { 0 };
            (Tag::Decimal, lo, lo + m - 1)
        }
    })
}

/// The ids of one segment of [`numeric_segments`] whose values satisfy every atom,
/// found by binary search with the ordinary comparison.
pub(super) fn numeric_segment(
    atoms: &[(RangeOp, super::value::Value)],
    ctx: &Ctx,
    tag: Tag,
    lo: u64,
    hi: u64,
) -> Option<IdRange> {
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
    let (lo, hi) = (Id::new(tag, lo).0, Id::new(tag, hi).0);
    let s = first(lo, hi, &lower_ok);
    let e = first(lo, hi, &|id| !upper_ok(id));
    (s < e).then_some(IdRange {
        lo: s,
        hi: e - 1,
        exact: true,
    })
}

#[cfg(test)]
thread_local! {
    /// The most conjuncts moved into one range scan, and the most key tests that set
    /// the rows of one scan, on this thread since the last reset.
    pub(super) static REACHED: std::cell::Cell<(usize, usize)> =
        const { std::cell::Cell::new((0, 0)) };
    /// The terms of [`filter_bound`] and of the checked partition of filters that set a
    /// bound, on this thread since the last reset: the most bytes of the range scan's
    /// branch when it is the larger, the most bytes of key tests in the key ranges'
    /// branch when that is the larger, and the most filters partitioned at once.
    pub(super) static BOUND_TERMS: std::cell::Cell<[u64; 3]> =
        const { std::cell::Cell::new([0; 3]) };
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
        let counted = {
            // the store's reads, which the planner's credit does not cover
            #[cfg(test)]
            let _stage = crate::test_alloc::Stage::enter(8);
            ctx.snap
                .count_between(spec.perm, bound(r.lo, 0), bound(r.hi, u64::MAX))
        };
        let Ok(rows) = counted else {
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
    #[cfg(test)]
    REACHED.with(|r| r.set((r.get().0.max(pushed.len()), r.get().1)));
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
/// says nothing about value order (vocabulary literals, delta terms, NaN payloads,
/// booleans, dates, blank nodes). Within a `Some` segment distinct ids are distinct
/// values.
///
/// `terms` are sorted, disjoint, inclusive id ranges of the base vocabulary that get
/// segments of their own, with their order. The base vocabulary is sorted by key, and an
/// IRI's key is `<` followed by the IRI's UTF-8 bytes, so the ids of its IRIs are in the
/// order of the IRIs' code points, which is the order ORDER BY gives IRIs. Its literals
/// form a segment of their own without value order (see [`TopKPiece::lexical`]). Terms
/// that updates added have delta ids and stay unordered.
fn value_order_segments(terms: &[(u64, u64, Option<bool>)]) -> Vec<(u64, u64, Option<bool>)> {
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
    ];
    // blank nodes, the base vocabulary, delta and local terms
    let (mut at, end) = (id(Tag::BNode, 0), id(Tag::Decimal, 0) - 1);
    for &(lo, hi, order) in terms {
        if lo < at || hi < lo || hi >= end {
            continue;
        }
        if lo > at {
            out.push((at, lo - 1, None));
        }
        out.push((lo, hi, order));
        at = hi + 1;
    }
    out.push((at, end, None));
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

/// Rows of a literal piece whose values decide whether it is read from the low end.
const LEXICAL_PROBE_ROWS: usize = 32;

/// Whether the first rows of `scan` with order-column ids in `[lo, hi]` hold plain strings
/// of the base vocabulary. A piece of base-vocabulary literals read from its low end
/// stops early only when the k-th candidate is a plain string. When it is a literal of
/// another kind, the generic plan answers after the first step, so the planner keeps
/// the plain sort for columns that do not start with plain strings, such
/// as language-tagged labels. Only the plan depends on this check.
fn plain_strings_first(ctx: &Ctx, scan: &ScanSpec, lo: u64, hi: u64) -> bool {
    let col = scan.prefix.len();
    let bound = |id: u64, fill: u64| {
        let mut key = crate::index::pad(&scan.prefix, fill);
        key[col] = id;
        key
    };
    let mut ids = Vec::with_capacity(LEXICAL_PROBE_ROWS);
    let read = ctx
        .snap
        .scan_between(scan.perm, bound(lo, 0), bound(hi, u64::MAX), |chunk| {
            match chunk {
                crate::store::Chunk::Block(b, s, e) => {
                    let e = e.min(s + LEXICAL_PROBE_ROWS - ids.len());
                    ids.extend_from_slice(&b.cols[col][s..e]);
                }
                crate::store::Chunk::Row(k) => ids.push(k[col]),
            }
            Ok(ids.len() < LEXICAL_PROBE_ROWS)
        });
    let vocab = &ctx.snap.generation.vocab;
    read.is_ok()
        && !ids.is_empty()
        && ids.iter().all(|&id| {
            Id(id).tag() == Tag::Vocab
                && vocab.get(Id(id).payload()).is_some_and(|k| {
                    k.first() == Some(&b'"') && k.last() == Some(&crate::id::KEY_SEP)
                })
        })
}

/// What placing the read steps at the best end of a monotone [`OrderedTopK`] piece costs,
/// in rows read (see [`ordered_topk`]).
const TOPK_EDGE_ROWS: f64 = (crate::index::BLOCK_ROWS / 8) as f64;

#[cfg(test)]
thread_local! {
    /// tests: choose [`OrderedTopK`] whenever it applies, whatever it costs, and read
    /// every literal piece in ascending order from its low end
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
    // the base vocabulary's literals, then its IRIs, each an inclusive id range
    let vocab = &ctx.snap.generation.vocab;
    let mut terms = Vec::new();
    for (key, order) in [(&b"\""[..], None), (&b"<"[..], Some(true))] {
        let (lo, hi) = vocab.prefix_range(key);
        if lo < hi {
            terms.push((Id::vocab(lo).0, Id::vocab(hi - 1).0, order));
        }
    }
    let literals = terms.first().filter(|t| t.2.is_none()).map(|t| (t.0, t.1));
    let segments = value_order_segments(&terms);
    let (mut pieces, mut total, mut read) = (Vec::new(), 0.0, 0.0);
    for d in &domain {
        for &(lo, hi, mono) in &segments {
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
            // A monotone piece is read from its best end. Placing the steps decodes the
            // leading key columns of about one block, which the block cache keeps, and
            // the steps read k rows, more when filters drop rows. That costs about an
            // eighth of reading the block's rows: charged a whole block, a 1.05M range
            // TopK kept the full scan and ran 16% slower than the ordered read.
            let sel = if d.exact {
                sel
            } else {
                sel * FILTER_SELECTIVITY
            };
            // ascending, the literals of the base vocabulary are read from the low end
            // when they look like plain strings there
            let lexical = asc
                && literals.is_some_and(|(a, b)| a <= lo && hi <= b)
                && (force_ordered_topk() || plain_strings_first(ctx, &scan, lo, hi));
            read += match mono {
                Some(_) => rows.min(TOPK_EDGE_ROWS + k as f64 / sel),
                None if lexical => rows.min(TOPK_EDGE_ROWS + k as f64 / sel),
                None => rows,
            };
            pieces.push(TopKPiece {
                lo,
                hi,
                exact: d.exact,
                mono,
                lexical,
            });
        }
    }
    if pieces.is_empty() || read * 2.0 > total && !force_ordered_topk() {
        return n;
    }
    let desc = format!(
        "{desc} | {} limit {k} [{} of {} id pieces in value order]",
        n.desc,
        pieces
            .iter()
            .filter(|p| p.mono.is_some() || p.lexical)
            .count(),
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
    project_described(n, vars, desc)
}

fn project_described(n: Node, vars: Vec<VarId>, desc: String) -> Node {
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
    // A DISTINCT that the planner moved below its sort so that it may spill keeps the
    // sort, which spills too, where a top-k heap would hold its `k` rows in memory.
    let spilling = |order: &Node| {
        order
            .children
            .first()
            .is_some_and(super::demand::numbered_distinct)
    };
    let child = match (child, limit) {
        (mut n, Some(k))
            if matches!(n.kind, Kind::OrderBy { limit: None, .. }) && !spilling(&n) =>
        {
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
                && matches!(n.children[0].kind, Kind::OrderBy { limit: None, .. })
                && !spilling(&n.children[0]) =>
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
    let mut child = child;
    if let (0, Some(k)) = (start, length) {
        limit_into(&mut child, k);
    }
    let desc = format!(
        "offset {start} limit {}",
        length.map_or("∞".to_string(), |l| l.to_string())
    );
    let whole = length.is_some()
        && !super::demand::ordered_input(&child)
        && super::demand::runs_whole(ctx, &child, start.saturating_add(length.unwrap_or(0)));
    let mut n = Node::unary(
        Kind::Slice {
            offset: start,
            limit: length,
            whole,
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

/// Pass a LIMIT of `k` rows without OFFSET into an operator below it whose first `k`
/// rows are as good an answer as any `k` of its rows, through the operators that keep
/// every input row as one output row. A SERVICE that reads no input sends its remote
/// query with the limit, and a full-text search keeps its `k` best hits, so neither
/// fetches every solution for a few. A LIMIT without ORDER BY may take any `k`
/// solutions, so both answers are correct. A SERVICE that loops over its input, in
/// bulk or not, and a search with a variable query string read their input and stay
/// as they are. OFFSET would let the remote endpoint skip solutions that a later
/// request returns, so a LIMIT with an OFFSET passes nothing.
fn limit_into(n: &mut Node, k: usize) {
    match &mut n.kind {
        Kind::Project(_) | Kind::Extend(..) if n.children.len() == 1 => {
            limit_into(&mut n.children[0], k)
        }
        Kind::Service {
            endpoint: PathEnd::Const(_),
            query,
            ..
        } if n.children.is_empty() => {
            query.push_str(&format!(" LIMIT {k}"));
            n.desc.clone_from(query);
            n.est = n.est.min(k as f64);
        }
        Kind::TextSearch(spec) if n.children.is_empty() && spec.query_var.is_none() => {
            spec.limit = Some(spec.limit.map_or(k, |l| l.min(k)));
            n.est = n.est.min(k as f64);
        }
        _ => {}
    }
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
        && let Kind::Join {
            algo,
            keys: join_keys,
        } = &child.kind
        && *algo != JoinAlgo::Cross
    {
        let var = aggs[0].0;
        return Node {
            kind: Kind::CountJoin {
                algo: algo.clone(),
                var,
                keys: join_keys.clone(),
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
            .map(|(v, a)| {
                let name = format!("?{}={:?}", ctx.var_name(*v), a.func);
                if let Some(descriptor) = &a.registered {
                    format!(
                        "{name} [aggregate {:?}; singleton batch]",
                        descriptor.volatility
                    )
                } else {
                    name
                }
            })
            .collect::<Vec<_>>()
            .join(" ")
    );
    let desc = if super::group::hashable(ctx, &aggs) {
        format!("{desc} [incremental]")
    } else {
        desc
    };
    // Another reading of errors changes the answer of these aggregates, and the
    // description is part of the result cache's key.
    let desc = if ctx.aggregate_errors != super::AggregateErrors::default()
        && aggs.iter().any(|(_, a)| {
            a.registered.is_none()
                && a.fold.is_none()
                && matches!(
                    a.func,
                    AggregateFunction::Min
                        | AggregateFunction::Max
                        | AggregateFunction::Avg
                        | AggregateFunction::GroupConcat { .. }
                )
        }) {
        format!("{desc} [aggregate errors: {}]", ctx.aggregate_errors)
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
    super::stats::exact_counts(&ctx.snap, k, rdf_type, est.max(0.0) as u64, &|| ctx.check())
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
        src: spec.src.clone(),
    })
}

/// The longest prefix of an ARQ path range the planner unrolls into joined steps (see
/// [`Planner::collect_path`]); a longer range runs whole in the path operator.
const RANGE_UNROLL: u64 = 32;

/// Remove from `filters` and return those that must wait until every element of the join
/// group that binds their variables has joined (see [`Planner::join_order`]). A
/// FILTER waits when one of its variables may be unbound in an element of the group that
/// binds it and another element also binds it, since the join with that element can fill
/// in the value the FILTER would test early. A variable bound in every row of each
/// element that binds it, or bound by one element only, has its final value as soon as
/// that element is joined.
fn defer_open_filters(
    filters: &mut Vec<Expr>,
    triples: &[Triple],
    paths: &[PathItem],
    nodes: &[Node],
    unpacks: &[UnpackItem],
) -> Vec<Expr> {
    if filters.is_empty() || !may_defer(nodes) {
        return Vec::new();
    }
    let open = |v: VarId| open_filter_var(v, triples, paths, nodes, unpacks);
    let (deferred, kept): (Vec<Expr>, Vec<Expr>) =
        std::mem::take(filters).into_iter().partition(|f| {
            f.visit_vars(&mut |v| if open(v) { Err(()) } else { Ok(()) })
                .is_err()
        });
    *filters = kept;
    deferred
}

/// Whether an element of the group may leave a variable unbound, so that a FILTER of the
/// group may wait (see [`defer_open_filters`]).
fn may_defer(nodes: &[Node]) -> bool {
    nodes.iter().any(|n| n.certain.len() < n.vars.len())
}

/// Whether a FILTER on `v` waits until every element of the group that binds `v` has
/// joined (see [`defer_open_filters`]).
fn open_filter_var(
    v: VarId,
    triples: &[Triple],
    paths: &[PathItem],
    nodes: &[Node],
    unpacks: &[UnpackItem],
) -> bool {
    let in_graph = |g: &ActiveGraph, v: VarId| matches!(g, ActiveGraph::Var(x) if *x == v);
    if !nodes
        .iter()
        .any(|n| n.vars.contains(&v) && !n.certain.contains(&v))
    {
        return false;
    }
    let elements = triples
        .iter()
        .filter(|t| t.t.contains(&PT::V(v)) || in_graph(&t.graph, v))
        .count()
        + paths
            .iter()
            .filter(|p| p.s == PT::V(v) || p.o == PT::V(v) || in_graph(&p.graph, v))
            .count()
        + nodes.iter().filter(|n| n.vars.contains(&v)).count()
        + unpacks
            .iter()
            .filter(|u| u.t == v || u.parts.contains(&PT::V(v)))
            .count();
    elements > 1
}

fn flatten_union<'g>(gp: &'g GraphPattern, out: &mut PlanningVec<&'g GraphPattern>) -> Result<()> {
    if let GraphPattern::Union { left, right } = gp {
        flatten_union(left, out)?;
        flatten_union(right, out)?;
    } else {
        out.push(gp)?;
    }
    Ok(())
}

fn flatten_alt_into<'g>(
    path: &'g PropertyPathExpression,
    push: &mut impl FnMut(&'g PropertyPathExpression) -> Result<()>,
) -> Result<()> {
    if let PropertyPathExpression::Alternative(left, right) = path {
        flatten_alt_into(left, push)?;
        flatten_alt_into(right, push)
    } else {
        push(path)
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

fn substitute(e: &mut Expr, v: VarId, c: Id) -> Result<()> {
    match e {
        Expr::Var(x) if *x == v => *e = Expr::Const(c),
        Expr::Bound(x) if *x == v => *e = Expr::Const(Id::from_bool(true)),
        Expr::Or(a, b)
        | Expr::And(a, b)
        | Expr::Eq(a, b)
        | Expr::SameTerm(a, b)
        | Expr::Cmp(a, b, _)
        | Expr::Arith(a, b, _) => {
            substitute(a, v, c)?;
            substitute(b, v, c)?;
        }
        Expr::Not(a) | Expr::Neg(a) | Expr::Pos(a) => substitute(a, v, c)?,
        Expr::In(a, l) => {
            substitute(a, v, c)?;
            for e in l {
                substitute(e, v, c)?;
            }
        }
        Expr::If(a, b, d) => {
            substitute(a, v, c)?;
            substitute(b, v, c)?;
            substitute(d, v, c)?;
        }
        Expr::Coalesce(l) | Expr::Call(_, l) => {
            for e in l {
                substitute(e, v, c)?;
            }
        }
        // the EXISTS pattern is substituted when it is evaluated: it keeps the constant
        Expr::Exists(spec) if spec.vars.contains(&v) => {
            *spec = Arc::new(spec.with_bound_checked(v, c)?)
        }
        _ => {}
    }
    Ok(())
}

/// Variables certainly bound by a pattern (used for filter placement).
pub fn certain_vars(gp: &GraphPattern, ctx: &Ctx) -> FxHashSet<VarId> {
    let mut names = Vec::new();
    certain_names(gp, &mut names);
    names.iter().map(|n| ctx.var(n)).collect()
}

pub(super) fn certain_names(gp: &GraphPattern, out: &mut Vec<String>) {
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
    visit_pattern_vars(gp, &mut |name| out.push(name.to_string()));
}

fn visit_pattern_vars(gp: &GraphPattern, out: &mut impl FnMut(&str)) {
    let _ = try_visit_pattern_vars(gp, &mut |name| {
        out(name);
        Ok::<(), std::convert::Infallible>(())
    });
}

pub(super) fn try_visit_pattern_vars<E>(
    gp: &GraphPattern,
    out: &mut impl FnMut(&str) -> std::result::Result<(), E>,
) -> std::result::Result<(), E> {
    let mut status = Ok(());
    gp.on_in_scope_variable(|v| {
        if status.is_ok() {
            status = out(v.as_str());
        }
    });
    status?;
    // variables used only in filters also matter for substitution
    fn walk<E>(
        gp: &GraphPattern,
        out: &mut impl FnMut(&str) -> std::result::Result<(), E>,
    ) -> std::result::Result<(), E> {
        use GraphPattern as GP;
        match gp {
            GP::Filter { expr, inner } => {
                try_visit_expr_vars(expr, out)?;
                walk(inner, out)
            }
            GP::LeftJoin {
                left,
                right,
                expression,
            } => {
                if let Some(e) = expression {
                    try_visit_expr_vars(e, out)?;
                }
                walk(left, out)?;
                walk(right, out)
            }
            GP::Join { left, right }
            | GP::Lateral { left, right }
            | GP::Union { left, right }
            | GP::Minus { left, right }
            | GP::SemiJoin { left, right }
            | GP::AntiJoin { left, right } => {
                walk(left, out)?;
                walk(right, out)
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
                try_visit_expr_vars(expression, out)?;
                walk(inner, out)
            }
            GP::Graph { inner, .. }
            | GP::Distinct { inner }
            | GP::Reduced { inner }
            | GP::Slice { inner, .. }
            | GP::OrderBy { inner, .. } => walk(inner, out),
            _ => Ok(()),
        }
    }
    walk(gp, out)
}

pub(super) fn try_visit_expr_vars<E>(
    e: &Expression,
    out: &mut impl FnMut(&str) -> std::result::Result<(), E>,
) -> std::result::Result<(), E> {
    use Expression as E;
    match e {
        E::Variable(v) | E::Bound(v) => out(v.as_str()),
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
            try_visit_expr_vars(a, out)?;
            try_visit_expr_vars(b, out)
        }
        E::UnaryPlus(a) | E::UnaryMinus(a) | E::Not(a) => try_visit_expr_vars(a, out),
        E::In(a, l) => {
            try_visit_expr_vars(a, out)?;
            for x in l {
                try_visit_expr_vars(x, out)?;
            }
            Ok(())
        }
        E::If(a, b, c) => {
            try_visit_expr_vars(a, out)?;
            try_visit_expr_vars(b, out)?;
            try_visit_expr_vars(c, out)
        }
        E::Coalesce(l) | E::FunctionCall(_, l) => {
            for x in l {
                try_visit_expr_vars(x, out)?;
            }
            Ok(())
        }
        E::Exists(p) => try_visit_pattern_vars(p, out),
        _ => Ok(()),
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

fn short_iri(iri: &str) -> Option<(&'static str, &str)> {
    for (prefix, namespace) in [
        ("rdf:", "http://www.w3.org/1999/02/22-rdf-syntax-ns#"),
        ("rdfs:", "http://www.w3.org/2000/01/rdf-schema#"),
        ("owl:", "http://www.w3.org/2002/07/owl#"),
        ("xsd:", "http://www.w3.org/2001/XMLSchema#"),
    ] {
        if let Some(suffix) = iri.strip_prefix(namespace) {
            return Some((prefix, suffix));
        }
    }
    None
}

/// Compact term rendering for plan descriptions.
pub fn short(t: &Term) -> String {
    match t {
        Term::NamedNode(n) => {
            let iri = n.as_str();
            match short_iri(iri) {
                Some((prefix, suffix)) => format!("{prefix}{suffix}"),
                None => format!("<{iri}>"),
            }
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
pub fn eval_exists(ctx: &Ctx, spec: &ExistsSpec, key: &[Id], schema: &[VarId]) -> Result<bool> {
    super::depth::guard_exists(|| {
        let mut p = Planner::for_exists(ctx, spec);
        if ctx.calls_extensions {
            p.property_inputs.extend(schema.iter().copied());
        }
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
        let node = p.plan(spec.pattern(), &graph, Vec::new())?;
        // A substituted pattern of one triple or a table of values reads its first row
        // directly, as the per-row evaluation of EXISTS stays eager until item 829. A larger
        // pattern pulls its first solution through cursor operators, as a LIMIT does, or
        // runs whole when it is small enough that a LIMIT would.
        if let Some((rows, _, _)) = super::exec::leaf_rows(ctx, &node, 1)? {
            return Ok(!rows.table.is_empty());
        }
        if super::demand::runs_whole(ctx, &node, 1)
            && let Some((rows, _)) = super::exec::whole_or_refused(ctx, &node)?
        {
            return Ok(!rows.table.is_empty());
        }
        super::cursor::has_solution(ctx, &node)
    })
}

/// Literal helper for tests.
pub fn lit_int(i: i64) -> Term {
    Term::Literal(Literal::new_typed_literal(i.to_string(), xsd::INTEGER))
}

/// A subject or object of the query as [`PlanPattern`] writes it: an N-Triples term, a
/// variable as `?name`, and a blank node or a triple term with variables as `?_`.
fn source_term(t: &TermPattern) -> String {
    match t {
        TermPattern::Variable(v) => var_text(v.as_str()),
        TermPattern::NamedNode(n) => iri_text(n.as_str()),
        TermPattern::Literal(l) => l.to_string(),
        TermPattern::Triple(tp) => match ground_triple(tp) {
            Some(t) => Term::Triple(Box::new(t)).to_string(),
            None => "?_".into(),
        },
        TermPattern::BlankNode(_) => "?_".into(),
    }
}

/// A predicate of the query as [`PlanPattern`] writes it.
fn source_named(n: &NamedNodePattern) -> String {
    match n {
        NamedNodePattern::NamedNode(n) => iri_text(n.as_str()),
        NamedNodePattern::Variable(v) => var_text(v.as_str()),
    }
}

/// `<iri>`, as an IRI's `Display` writes it, without the formatting machinery: the plan
/// names every pattern of every query this way.
fn iri_text(iri: &str) -> String {
    let mut s = String::with_capacity(iri.len() + 2);
    s.push('<');
    s.push_str(iri);
    s.push('>');
    s
}

/// `?name` for a variable of the query.
fn var_text(name: &str) -> String {
    let mut s = String::with_capacity(name.len() + 1);
    s.push('?');
    s.push_str(name);
    s
}

/// Count each triple pattern and path of `gp` into `out`, keyed as the planner keys them.
fn count_patterns(gp: &GraphPattern, out: &mut FxHashMap<SeenPattern, u32>) {
    use GraphPattern as GP;
    let mut todo = vec![gp];
    while let Some(gp) = todo.pop() {
        match gp {
            GP::Bgp { patterns } => {
                for t in patterns {
                    let key = SeenPattern::new(
                        source_term(&t.subject),
                        source_named(&t.predicate),
                        source_term(&t.object),
                    );
                    *out.entry(key).or_insert(0) += 1;
                }
            }
            GP::Path {
                subject,
                path,
                object,
            } => {
                let key = SeenPattern::new(
                    source_term(subject),
                    super::exec::path_text(path),
                    source_term(object),
                );
                *out.entry(key).or_insert(0) += 1;
            }
            GP::Join { left, right }
            | GP::LeftJoin { left, right, .. }
            | GP::Lateral { left, right }
            | GP::Union { left, right }
            | GP::SemiJoin { left, right }
            | GP::AntiJoin { left, right }
            | GP::Minus { left, right } => {
                todo.push(left);
                todo.push(right);
            }
            GP::Filter { inner, .. }
            | GP::Graph { inner, .. }
            | GP::Extend { inner, .. }
            | GP::Assign { inner, .. }
            | GP::Unfold { inner, .. }
            | GP::OrderBy { inner, .. }
            | GP::Project { inner, .. }
            | GP::Distinct { inner }
            | GP::Reduced { inner }
            | GP::Slice { inner, .. }
            | GP::Group { inner, .. }
            | GP::Service { inner, .. } => todo.push(inner),
            GP::Values { .. } => {}
        }
    }
}

// ------------------------------------------------------ reference ordering ------

#[cfg(test)]
thread_local! {
    /// Plan connected groups with the reference join ordering of the tests instead of
    /// the program on cost summaries (see [`Planner::reference_order`]).
    pub(crate) static REFERENCE_JOIN_ORDER: std::cell::Cell<bool> =
        const { std::cell::Cell::new(false) };
}

/// The join ordering the program on cost summaries replaced, kept as the tests' oracle:
/// the exhaustive dynamic program over plan trees for up to 12 inputs, and a greedy
/// ordering beyond.
#[cfg(test)]
impl Planner<'_> {
    fn reference_order(&self, items: Vec<Vec<Node>>, filters: &mut Vec<Expr>) -> Result<Node> {
        if items.len() <= 12 {
            self.reference_dp(items, filters)
        } else {
            self.reference_greedy(items, filters)
        }
    }

    fn reference_dp(&self, items: Vec<Vec<Node>>, filters: &mut Vec<Expr>) -> Result<Node> {
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
                let (o, fm, kept) =
                    super::joinorder::place_masked(self, o, 0, filters, &filter_vars, None)?;
                if let Some(kept) = kept {
                    self.retain_planning(kept);
                }
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
                                    let (c, fm, kept) = super::joinorder::place_masked(
                                        self,
                                        c,
                                        fa | fb,
                                        filters,
                                        &filter_vars,
                                        None,
                                    )?;
                                    if let Some(kept) = kept {
                                        self.retain_planning(kept);
                                    }
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
        // ones `place_masked` places, so any later filter stays for the caller
        let mut i = 0;
        filters.retain(|_| {
            let keep = i >= 64 || fm & (1u64 << i) == 0;
            i += 1;
            keep
        });
        Ok(best)
    }

    fn reference_greedy(&self, items: Vec<Vec<Node>>, filters: &mut Vec<Expr>) -> Result<Node> {
        let mut plans: Vec<Node> = items
            .into_iter()
            .map(|mut o| {
                o.sort_by(|a, b| a.cost.total_cmp(&b.cost));
                o.swap_remove(0)
            })
            .map(|p| self.place_filters(p, filters))
            .collect::<Result<_>>()?;
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
            plans.push(self.place_filters(c, filters)?);
        }
        let mut it = plans.into_iter();
        let mut acc = it.next().unwrap();
        for p in it {
            acc = self.place_filters(join(acc, p, self.ctx), filters)?;
        }
        Ok(acc)
    }
}
