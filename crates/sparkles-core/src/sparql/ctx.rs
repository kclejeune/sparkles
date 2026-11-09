//! Per-query execution context: snapshot, local vocabulary, decode cache,
//! cancellation and dataset description.

use super::pool::PoolGrant;
use super::query_memory::{PRIMARY, QueryMemoryLease, QueryMemoryLedger, same};
use super::table::VarId;
use super::value::Value;
use crate::error::{Budget, BudgetKind, Error, Result};
use crate::id::{self, Id, Tag};
use crate::store::{Snapshot, bnode_for, parse_bnode_label};
use crate::vocab::AppendVocab;
use oxrdf::Term;
use parking_lot::RwLock;
use rustc_hash::FxHashMap;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Weak};
use std::time::Instant;

/// Jena's IRI for the default graph (`Quad.defaultGraphIRI`).
pub const DEFAULT_GRAPH_IRI: &str = "urn:x-arq:DefaultGraph";
/// Jena's name for the default graph in quads it builds, such as a CONSTRUCT template's
/// triples outside `GRAPH` (`Quad.defaultGraphNodeGenerated`).
pub const DEFAULT_GRAPH_NODE_IRI: &str = "urn:x-arq:DefaultGraphNode";
/// Jena's IRI for the union of all named graphs (`Quad.unionGraph`).
pub const UNION_GRAPH_IRI: &str = "urn:x-arq:UnionGraph";

/// RDF dataset for a query (`FROM` / `FROM NAMED` or protocol parameters).
#[derive(Clone, Debug, Default)]
pub struct DatasetSpec {
    /// Graphs merged into the default graph; `None` = the store's default graph
    /// (or union of named graphs if the store uses a union default graph).
    pub default: Option<Vec<Id>>,
    /// Named graphs; `None` = all named graphs in the store.
    pub named: Option<Vec<Id>>,
    /// default graph is the union of all named graphs
    pub union_default: bool,
}

impl DatasetSpec {
    /// Limit the dataset of a query on `snap` to the graphs `access` may read: hidden
    /// graphs leave `FROM` and `FROM NAMED` (as if they did not exist), the default graph
    /// is empty when it is hidden, and the union of named graphs (the store's union
    /// default graph, or `urn:x-arq:UnionGraph`) becomes the visible named graphs.
    pub fn restrict(
        &mut self,
        snap: &Snapshot,
        access: &crate::access::GraphAccess,
        term: &dyn Fn(Id) -> Option<Term>,
    ) -> Result<()> {
        if access.reads_all() {
            return Ok(());
        }
        let (visible, every) = access.visible_named_all(snap)?;
        let allowed = |g: &Id| {
            if *g == Id::DEFAULT_GRAPH {
                access.read.default_graph()
            } else if visible.binary_search(g).is_ok() {
                true
            } else {
                // a graph without quads (or a query-local term): by its name
                match g.tag() {
                    id::Tag::BNode | id::Tag::Undef | id::Tag::Special => false,
                    _ => access.readable(term(*g).as_ref()),
                }
            }
        };
        let union = || visible.to_vec();
        self.default = match self.default.take() {
            // every named graph is visible: the union of them all, with its fast paths
            None if every && (self.union_default || snap.union_default_graph) => None,
            _ if self.union_default => Some(union()),
            None if snap.union_default_graph => Some(union()),
            // the store's default graph, as before (which keeps its fast paths)
            None if access.read.default_graph() => None,
            None => Some(Vec::new()),
            Some(l) => Some(l.into_iter().filter(allowed).collect()),
        };
        if self.default.is_some() {
            self.union_default = false;
        }
        self.named = match self.named.take() {
            None if every => None,
            None => Some(union()),
            Some(l) => Some(l.into_iter().filter(allowed).collect()),
        };
        Ok(())
    }
}

const VALUE_SHARDS: usize = 64;
/// decoded values kept per shard before the shard is cleared
const VALUE_SHARD_CAP: usize = 1 << 16;

/// Bytes of query state that a context charges in steps and keeps charged until
/// it drops: its decoded value cache, its query-local terms and memos and its parsed
/// geometries. The state counts its bytes itself, and [`Ctx::grow_reservation`] grows
/// the reservation to cover them, so that most additions take one atomic load.
#[derive(Default)]
pub(crate) struct StepReservation {
    /// the bytes charged
    reserved: AtomicU64,
    /// serializes growth, with the owner the charges went to
    grow: parking_lot::Mutex<Option<u64>>,
}

impl StepReservation {
    /// Whether the reservation covers `bytes` already.
    #[inline]
    pub(crate) fn covers(&self, bytes: u64) -> bool {
        bytes <= self.reserved.load(Ordering::Acquire)
    }

    /// The bytes charged.
    #[cfg(test)]
    pub(crate) fn reserved(&self) -> u64 {
        self.reserved.load(Ordering::Acquire)
    }
}

/// The query-local terms and memos of a context: the terms its expressions compute and
/// the blank nodes it mints, charged in both accounting modes (see [`Ctx::retain_bytes`]).
#[derive(Default)]
struct RetainedTerms {
    /// the estimated bytes retained
    used: AtomicU64,
    charge: StepReservation,
}

/// The most the decoded value cache of one query holds, before the memory budget's
/// share (see [`ValueCache::cap`]).
const VALUE_CACHE_BYTES: u64 = 64 << 20;

/// The query's cache of decoded dictionary values, in shards that a hash of the id
/// picks. It is charged to the query in both accounting modes: the cache reserves
/// [`EAGER_STEP`]s from the query's account as its entries grow, and a value whose
/// reservation is refused is returned without being cached. The cache holds at most
/// [`VALUE_CACHE_BYTES`] or an eighth of the memory budget, whichever is smaller, and a
/// shard that reaches its share of that is cleared and freed. The reservation is kept
/// until the context drops.
pub(super) struct ValueCache {
    /// Made on the first value cached or looked up. Most short queries decode nothing
    /// through the cache and skip the 64 shards' allocation, setup and drop.
    shards: std::sync::OnceLock<Box<[RwLock<ValueShard>]>>,
    /// the estimated bytes of the entries held now
    used: AtomicU64,
    /// the bytes charged for the cache, never fewer than `used`
    charge: StepReservation,
}

#[derive(Default)]
struct ValueShard {
    map: FxHashMap<Id, Value>,
    /// the estimated bytes of `map`'s entries
    bytes: u64,
}

#[cfg(test)]
thread_local! {
    /// growths of a step reservation on this thread
    static VALUE_GROWS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

impl Default for ValueCache {
    fn default() -> Self {
        Self {
            shards: std::sync::OnceLock::new(),
            used: AtomicU64::new(0),
            charge: Default::default(),
        }
    }
}

impl ValueCache {
    /// The estimated bytes one cached entry keeps: its slot, counted twice since a map
    /// grows by doubling, and the heap storage of its text.
    pub(super) fn entry_bytes(v: &Value) -> u64 {
        fn text(s: &str) -> u64 {
            s.len() as u64 + 16
        }
        let heap = match v {
            Value::Iri(a) | Value::BNode(a) | Value::Str(a) => text(a),
            Value::Lang(a, b) | Value::LangDir(a, b, _) => text(a) + text(b),
            Value::Other { lex, dt } => text(lex) + text(dt),
            Value::Triple(t) => 64 + t.to_string().len() as u64,
            _ => 0,
        };
        2 * (std::mem::size_of::<(Id, Value)>() as u64 + 1) + heap
    }

    /// The most the cache holds under a memory budget of `mem_limit` bytes.
    fn cap(mem_limit: u64) -> u64 {
        VALUE_CACHE_BYTES.min(mem_limit / 8)
    }

    #[inline]
    fn index(id: Id) -> usize {
        (id.0.wrapping_mul(0x9E37_79B9_7F4A_7C15) >> 58) as usize % VALUE_SHARDS
    }

    #[inline]
    fn shard(&self, id: Id) -> &RwLock<ValueShard> {
        let shards = self
            .shards
            .get_or_init(|| (0..VALUE_SHARDS).map(|_| Default::default()).collect());
        &shards[Self::index(id)]
    }

    #[inline]
    fn get(&self, id: Id) -> Option<Value> {
        let shards = self.shards.get()?;
        shards[Self::index(id)].read().map.get(&id).cloned()
    }

    /// The bytes charged for the cache.
    #[cfg(test)]
    fn reserved(&self) -> u64 {
        self.charge.reserved()
    }
}

/// Executor optimizations that can be switched off for diagnosis (every one of them has
/// a generic fallback with the same results). `SPARKLES_DISABLE_OPTIMIZATIONS` holds a
/// comma-separated list of names that are off by default in a process.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Optimizations {
    /// numeric range FILTERs on a scan's sorted column read only the matching id ranges
    pub range_pushdown: bool,
    /// GROUP BY on one variable with plain-variable aggregates keeps one state per group
    pub incremental_group: bool,
    /// COUNT(*) over a join of two scans counts per-key runs instead of reading rows
    pub count_join_runs: bool,
    /// per-class counts from index statistics when they are exact
    pub metadata_counts: bool,
    /// property paths expand a whole frontier per index sweep
    pub batched_paths: bool,
    /// ORDER BY one numeric variable with LIMIT ranks rounded keys first
    pub topk_prefilter: bool,
    /// ORDER BY a scan's variable with LIMIT reads the scan in value order and stops once
    /// the first rows are proven
    pub ordered_topk: bool,
    /// spatial FILTERs on an indexed predicate's object search the spatial index
    pub spatial_pushdown: bool,
    /// `FILTER (NOT) EXISTS` over a join group probes a key set built once from the
    /// pattern, instead of evaluating the substituted pattern per outer row
    pub decorrelate_exists: bool,
    /// a join with a selective input reads a triple pattern only for the input's distinct
    /// keys, by clustered seeks over a permutation sorted on the key
    pub batched_join: bool,
    /// index joins on one subject over constant predicates are read together, walking
    /// each subject's run once when that touches fewer blocks
    pub star_fusion: bool,
    /// a spatial FILTER between the geometries of two join components joins them
    pub spatial_join: bool,
    /// ORDER BY a distance to a constant with LIMIT searches the nearest geometries
    pub spatial_knn: bool,
    /// pure expressions over one variable (FILTER, BIND, ORDER BY keys, aggregate
    /// arguments) are evaluated once per distinct value
    pub expr_cache: bool,
    /// MINUS on one variable that both sides always bind removes rows by a merge or a
    /// probe of single ids instead of hashing a key per row
    pub anti_join: bool,
    /// ORDER BY several keys with LIMIT evaluates the later keys only on the rows that
    /// the first key does not rule out
    pub topk_first_key: bool,
    /// COUNT(*) over a FILTER on one variable of a single scan tests the filter once per
    /// run of the variable in a permutation sorted on it and sums the run lengths
    pub count_filter_runs: bool,
    /// a FILTER over a scan sorted on a variable it tests reads the runs of that
    /// variable, tests each value once and copies only the rows of the values that pass
    pub filter_scan_runs: bool,
    /// index joins find each key's rows by galloping from the previous key's position
    /// and keep the current block, instead of a binary search and a scan per cluster
    pub gallop_index_join: bool,
    /// OPTIONAL on one variable, with both sides sorted on it and always binding it,
    /// runs as a merge in the left side's order
    pub merge_left_join: bool,
    /// OPTIONAL whose right side is a triple pattern sorted on a variable that every row
    /// of the evaluated left side binds reads the pattern only for the left side's
    /// distinct values, when probing them costs less than reading the pattern
    pub probed_optional: bool,
    /// an OPTIONAL whose sides share no variable and whose condition equates a variable
    /// of each side pairs the rows by that equality instead of forming their product
    pub keyed_left_join: bool,
    /// two parts of a group that share no variable, linked by a FILTER conjunct that
    /// equates a variable of each, are joined by that equality instead of their product
    pub keyed_join: bool,
    /// a triple pattern with one variable of which the store holds one row binds the
    /// variable to that value for the rest of its group at planning time
    pub point_patterns: bool,
    /// a FILTER conjunct over the variables of one triple pattern is tested on a sample
    /// of the pattern's rows, whose share that passes is the planner's estimate of the
    /// share of its input it keeps (instead of 30%)
    pub sampled_filters: bool,
    /// joins on a subject variable of patterns with constant predicates are estimated
    /// from the characteristic sets in the statistics
    pub characteristic_sets: bool,
    /// a join of a small input (VALUES, or a pattern of few rows) with a pattern is
    /// estimated by counting the pattern's rows for a sample of the input's values
    pub probed_keys: bool,
    /// the characteristic sets estimate stars over the subjects of the sets the
    /// statistics do not keep from their remainder, instead of using only the predicates
    /// that the kept sets cover
    pub set_remainders: bool,
    /// joins on a variable that is the object of a pattern of its group are estimated
    /// from the node sets of the statistics (incoming and outgoing predicates per node)
    pub node_sets: bool,
    /// a join on several keys combines their distinct counts by exponential back-off
    /// instead of dividing by the largest alone
    pub key_backoff: bool,
    /// index joins that a fused star reads together are costed for the keys of the
    /// star's input, which a fused star probes in every pattern
    pub fused_star_costs: bool,
    /// an index join with few keys asks the kernel to read every block it will visit
    /// before it decodes the first, so a cold server reads them in parallel
    pub prefetch_blocks: bool,
    /// a hash join whose right side is a `text:query` call without a limit searches only
    /// the subjects of a small left side
    pub text_subject_pushdown: bool,
    /// ORDER BY DESC(spk:cosine(?v, C)) LIMIT k (or spk:dot) over one pattern reads the
    /// k best rows by an exact vector search
    pub vector_topk: bool,
    /// ORDER BY with LIMIT keeps the best rows in a bounded heap of offset + limit rows,
    /// evaluating the later keys only for rows whose first key can still enter
    pub topk_heap: bool,
    /// an ORDER BY key equal to the expression of a BIND or SELECT expression directly
    /// below it orders by that expression's variable instead of evaluating it again
    pub bound_order_keys: bool,
    /// the vocabulary groups that hold a query's constants are requested together before
    /// the planner looks the constants up one after the other
    pub prefetch_constants: bool,
    /// a FILTER, BIND or ORDER BY over a table of fewer than 4,096 rows asks for the
    /// vocabulary pages of the terms it evaluates before the first row
    pub prefetch_terms: bool,
    /// the blocks of a plan's scans with few blocks are requested before any operator runs
    pub prefetch_first_scan: bool,
    /// the columns of the blocks that an index join has asked the kernel for are decoded
    /// in parallel on a pool of the cache's own, before the reader gets to them
    pub parallel_decode: bool,
    /// a streamed result writer decodes the terms of the next batch on the Rayon pool
    /// while it writes the current one
    pub decode_ahead: bool,
    /// cursor operators that read their whole input before output, and filters over a
    /// scan, read larger inputs as a long input goes on
    pub grow_input: bool,
    /// the predicates of a set of subjects, with the subjects having each, and
    /// `spk:hasPredicate` are read from the generation's predicate patterns
    pub predicate_patterns: bool,
}

impl Optimizations {
    pub const NAMES: [&str; 43] = [
        "range_pushdown",
        "incremental_group",
        "count_join_runs",
        "metadata_counts",
        "batched_paths",
        "topk_prefilter",
        "ordered_topk",
        "spatial_pushdown",
        "decorrelate_exists",
        "batched_join",
        "star_fusion",
        "spatial_join",
        "spatial_knn",
        "expr_cache",
        "anti_join",
        "topk_first_key",
        "count_filter_runs",
        "filter_scan_runs",
        "gallop_index_join",
        "merge_left_join",
        "probed_optional",
        "keyed_left_join",
        "keyed_join",
        "point_patterns",
        "sampled_filters",
        "characteristic_sets",
        "probed_keys",
        "set_remainders",
        "node_sets",
        "key_backoff",
        "fused_star_costs",
        "prefetch_blocks",
        "text_subject_pushdown",
        "vector_topk",
        "topk_heap",
        "bound_order_keys",
        "prefetch_constants",
        "prefetch_terms",
        "prefetch_first_scan",
        "parallel_decode",
        "decode_ahead",
        "grow_input",
        "predicate_patterns",
    ];

    /// Everything on.
    pub const ALL: Optimizations = Optimizations {
        range_pushdown: true,
        incremental_group: true,
        count_join_runs: true,
        metadata_counts: true,
        batched_paths: true,
        topk_prefilter: true,
        ordered_topk: true,
        spatial_pushdown: true,
        decorrelate_exists: true,
        batched_join: true,
        star_fusion: true,
        spatial_join: true,
        spatial_knn: true,
        expr_cache: true,
        anti_join: true,
        topk_first_key: true,
        count_filter_runs: true,
        filter_scan_runs: true,
        gallop_index_join: true,
        merge_left_join: true,
        probed_optional: true,
        keyed_left_join: true,
        keyed_join: true,
        point_patterns: true,
        sampled_filters: true,
        characteristic_sets: true,
        probed_keys: true,
        set_remainders: true,
        node_sets: true,
        key_backoff: true,
        fused_star_costs: true,
        prefetch_blocks: true,
        text_subject_pushdown: true,
        vector_topk: true,
        topk_heap: true,
        bound_order_keys: true,
        prefetch_constants: true,
        prefetch_terms: true,
        prefetch_first_scan: true,
        parallel_decode: true,
        decode_ahead: true,
        grow_input: true,
        predicate_patterns: true,
    };

    /// Everything off: the generic operators only.
    pub const NONE: Optimizations = Optimizations {
        range_pushdown: false,
        incremental_group: false,
        count_join_runs: false,
        metadata_counts: false,
        batched_paths: false,
        topk_prefilter: false,
        ordered_topk: false,
        spatial_pushdown: false,
        decorrelate_exists: false,
        batched_join: false,
        star_fusion: false,
        spatial_join: false,
        spatial_knn: false,
        expr_cache: false,
        anti_join: false,
        topk_first_key: false,
        count_filter_runs: false,
        filter_scan_runs: false,
        gallop_index_join: false,
        merge_left_join: false,
        probed_optional: false,
        keyed_left_join: false,
        keyed_join: false,
        point_patterns: false,
        sampled_filters: false,
        characteristic_sets: false,
        probed_keys: false,
        set_remainders: false,
        node_sets: false,
        key_backoff: false,
        fused_star_costs: false,
        prefetch_blocks: false,
        text_subject_pushdown: false,
        vector_topk: false,
        topk_heap: false,
        bound_order_keys: false,
        prefetch_constants: false,
        prefetch_terms: false,
        prefetch_first_scan: false,
        parallel_decode: false,
        decode_ahead: false,
        grow_input: false,
        predicate_patterns: false,
    };

    fn flag(&mut self, name: &str) -> Option<&mut bool> {
        Some(match name {
            "range_pushdown" => &mut self.range_pushdown,
            "incremental_group" => &mut self.incremental_group,
            "count_join_runs" => &mut self.count_join_runs,
            "metadata_counts" => &mut self.metadata_counts,
            "batched_paths" => &mut self.batched_paths,
            "topk_prefilter" => &mut self.topk_prefilter,
            "ordered_topk" => &mut self.ordered_topk,
            "spatial_pushdown" => &mut self.spatial_pushdown,
            "decorrelate_exists" => &mut self.decorrelate_exists,
            "batched_join" => &mut self.batched_join,
            "star_fusion" => &mut self.star_fusion,
            "spatial_join" => &mut self.spatial_join,
            "spatial_knn" => &mut self.spatial_knn,
            "expr_cache" => &mut self.expr_cache,
            "anti_join" => &mut self.anti_join,
            "topk_first_key" => &mut self.topk_first_key,
            "count_filter_runs" => &mut self.count_filter_runs,
            "filter_scan_runs" => &mut self.filter_scan_runs,
            "gallop_index_join" => &mut self.gallop_index_join,
            "merge_left_join" => &mut self.merge_left_join,
            "probed_optional" => &mut self.probed_optional,
            "keyed_left_join" => &mut self.keyed_left_join,
            "keyed_join" => &mut self.keyed_join,
            "point_patterns" => &mut self.point_patterns,
            "sampled_filters" => &mut self.sampled_filters,
            "characteristic_sets" => &mut self.characteristic_sets,
            "probed_keys" => &mut self.probed_keys,
            "set_remainders" => &mut self.set_remainders,
            "node_sets" => &mut self.node_sets,
            "key_backoff" => &mut self.key_backoff,
            "fused_star_costs" => &mut self.fused_star_costs,
            "prefetch_blocks" => &mut self.prefetch_blocks,
            "text_subject_pushdown" => &mut self.text_subject_pushdown,
            "vector_topk" => &mut self.vector_topk,
            "topk_heap" => &mut self.topk_heap,
            "bound_order_keys" => &mut self.bound_order_keys,
            "prefetch_constants" => &mut self.prefetch_constants,
            "prefetch_terms" => &mut self.prefetch_terms,
            "prefetch_first_scan" => &mut self.prefetch_first_scan,
            "parallel_decode" => &mut self.parallel_decode,
            "decode_ahead" => &mut self.decode_ahead,
            "grow_input" => &mut self.grow_input,
            "predicate_patterns" => &mut self.predicate_patterns,
            _ => return None,
        })
    }

    /// Switch off the named optimizations (`all` switches off every one). Unknown names
    /// are returned as an error.
    pub fn disable(mut self, names: &str) -> std::result::Result<Optimizations, String> {
        for n in names.split(',').map(str::trim).filter(|n| !n.is_empty()) {
            if n == "all" {
                self = Optimizations::NONE;
                continue;
            }
            *self.flag(n).ok_or_else(|| {
                format!(
                    "unknown optimization {n:?} (known: {})",
                    Self::NAMES.join(", ")
                )
            })? = false;
        }
        Ok(self)
    }
}

impl Default for Optimizations {
    /// Everything on, except what `SPARKLES_DISABLE_OPTIMIZATIONS` lists.
    fn default() -> Optimizations {
        static DEFAULT: std::sync::OnceLock<Optimizations> = std::sync::OnceLock::new();
        *DEFAULT.get_or_init(|| match std::env::var("SPARKLES_DISABLE_OPTIMIZATIONS") {
            Ok(v) => Optimizations::ALL.disable(&v).unwrap_or_else(|e| {
                tracing::warn!(target: "sparkles::sparql::ctx", "SPARKLES_DISABLE_OPTIMIZATIONS: {e}");
                Optimizations::ALL
            }),
            Err(_) => Optimizations::ALL,
        })
    }
}

/// A note about a plan for its reader (explain output, MCP): something did not run the
/// way the query suggests, though the answer is the same.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize)]
pub struct PlanWarning {
    /// stable identifier (`geo-not-pushed`, `geo-index-building`, `geo-not-built`, …)
    pub code: &'static str,
    pub message: String,
}

pub struct Ctx {
    pub extensions: Option<Arc<super::extensions::ExtensionRegistry>>,
    /// what query-time reasoning derived for this query (see [`super::reasoning`])
    pub reasoning: Option<Arc<super::reasoning::Applied>>,
    /// The snapshot holds facts with query-local terms, which query-time reasoning
    /// derived (spec F17). A pattern constant the store does not have may then match.
    pub local_facts: bool,
    /// The memory the derived facts of query-time reasoning hold in the snapshot's
    /// overlay, charged to the query until the context drops.
    pub(super) reasoning_memory: Vec<QueryMemoryLease>,
    pub snap: Arc<Snapshot>,
    local: RwLock<AppendVocab>,
    values: ValueCache,
    next_bnode: AtomicU64,
    bnode_memo: parking_lot::Mutex<FxHashMap<(Vec<Id>, String), Id>>,
    /// blank nodes given to the query from outside (initial bindings) that are not
    /// stored ones: a fresh blank node per label
    outside_bnodes: parking_lot::Mutex<FxHashMap<String, Id>>,
    /// the blank nodes that labels inside the composite literals of the query text name
    /// (see [`Ctx::query_literal`])
    literal_bnodes: parking_lot::Mutex<FxHashMap<String, Id>>,
    /// The query's deadline, without the time the query waited for its consumer. Code
    /// that hands a deadline to other work reads [`Ctx::current_deadline`], which adds
    /// [`Ctx::deadline_pause`].
    pub deadline: Option<Instant>,
    /// Nanoseconds the query spent waiting for its consumer, which the deadline does not
    /// count. The thread that drives the cursor adds to it, after each stretch of waiting
    /// for its consumer, and the same thread runs every check, so a check never misses a
    /// wait that is still in progress. A background reader of the deadline would need to
    /// credit a wait before it starts.
    pub deadline_pause: Arc<AtomicU64>,
    pub cancel: Arc<AtomicBool>,
    pub cancellation: Option<crate::task::Cancellation>,
    pub dataset: DatasetSpec,
    /// the request's graph view when it does not read every graph: `dataset` is already
    /// limited to it (see [`DatasetSpec::restrict`]), and plans are redacted
    pub graphs: Option<Arc<crate::access::GraphAccess>>,
    /// The query's `NOW()`, read from the clock on first use so that queries that never
    /// call it skip the clock and the calendar conversion. One value serves the whole
    /// query, as SPARQL requires (see [`Ctx::now`]).
    now: std::sync::OnceLock<oxsdatatypes::DateTime>,
    pub base_iri: Option<oxiri::Iri<String>>,
    pub var_names: RwLock<Vec<String>>,
    /// Maximum number of rows any intermediate result may have (memory guard).
    pub max_rows: usize,
    /// Budget for the estimated bytes of intermediate results alive at once
    /// (`u64::MAX`: unlimited). See [`Ctx::charge`] and [`Ctx::check_output`].
    pub mem_limit: u64,
    /// The grant the query charges, fixed before its first charge (see [`Ctx::set_grant`]).
    /// Contributions retain the grant they were admitted under.
    grant: Option<Arc<super::pool::PoolGrant>>,
    /// The query's disk reservation for spill files, when spill is on.
    pub(crate) spill: Option<Arc<crate::spill::QuerySpill>>,
    /// Budget for the rows all operators produce together (`u64::MAX`: unlimited). See
    /// [`Ctx::produced`].
    pub max_rows_produced: u64,
    /// rows produced so far, summed over operators (shared by the WHERE clauses of one
    /// update)
    pub rows_produced: Arc<AtomicU64>,
    pub allow_service: bool,
    /// SERVICE fails with [`crate::Error::NotPermitted`] (see
    /// [`QueryOptions::forbid_service`](super::QueryOptions::forbid_service))
    pub forbid_service: bool,
    /// network policy of SERVICE
    pub outbound: crate::outbound::OutboundPolicy,
    /// what the SERVICE calls (and, in an update, the LOADs) of the request have spent
    /// of the policy's totals
    pub outbound_budget: Arc<crate::outbound::RequestBudget>,
    /// consult / fill the store's result cache
    pub use_cache: bool,
    /// name the query's patterns and variables in the plan (see
    /// [`QueryOptions::no_plan_patterns`](super::QueryOptions::no_plan_patterns))
    pub plan_patterns: bool,
    /// the caller's scope in the cache of remote SERVICE results (see
    /// [`QueryOptions::service_scope`](super::QueryOptions::service_scope))
    pub service_scope: Arc<str>,
    /// the process's own query endpoints (see [`super::LocalEndpoints`])
    pub local_endpoints: Option<Arc<dyn super::LocalEndpoints>>,
    /// how many in-process SERVICE calls this query runs inside
    pub service_depth: u32,
    /// the dataset's named projections (see [`crate::algo::catalog`])
    pub algo_projections: Option<Arc<crate::algo::catalog::Catalog>>,
    /// the dataset's views, for `SERVICE view:read`
    pub views: Option<Arc<dyn super::viewsvc::ViewSource>>,
    /// RDFS on read (see [`super::rdfs`])
    pub rdfs: Option<Arc<super::rdfs::RdfsSchema>>,
    pub opt: Optimizations,
    /// the I/O counters when the query began, taken only under a profile (see
    /// [`Ctx::io_stats`]); boxed, so that a context without a profile stays small
    io_start: Option<Box<crate::ioacct::IoSnapshot>>,
    /// parsed geometries of this query
    pub geo: crate::geo::memo::GeoMemo,
    /// notes for the plan's reader, without duplicates (see [`Ctx::warn`])
    warnings: parking_lot::Mutex<Vec<PlanWarning>>,
    /// the planner met a `SERVICE` of the graph-algorithm namespace (see
    /// [`Ctx::calls_algorithm`])
    algorithmic: AtomicBool,
    /// The whole eager ASK was admitted as a bounded pure UNION. Only that
    /// consumer may reduce pure input demand below the ordinary prefix policy.
    pub(super) eager_union_demand: AtomicBool,
    /// The plan of the operator that failed, as far as it ran, keyed by the address of
    /// its node, until its parent takes it (see [`Ctx::take_failed`]). Execution state:
    /// a plan shared between executions must not keep it.
    failed: parking_lot::Mutex<Option<(usize, super::exec::PlanInfo)>>,
    /// FILTER selectivities measured on samples for this query, by conjunct text
    sampled: parking_lot::Mutex<FxHashMap<String, super::sample::Sampled>>,
    /// the star predicates registered per subject variable (see [`super::charsets`])
    pub(super) stars: parking_lot::Mutex<FxHashMap<VarId, super::charsets::StarVar>>,
    /// the small input of a variable and the patterns probed with its values (see
    /// [`super::keyprobe`])
    pub(super) probes: parking_lot::Mutex<FxHashMap<VarId, super::keyprobe::VarProbe>>,
    // Keep callback bookkeeping after the ordinary execution fields. The registry
    // stays first so its user-owned callbacks keep their existing drop order.
    pub(crate) extension_failure: Option<super::extensions::Failure>,
    pub(crate) calls_extensions: bool,
    pub(crate) extension_families: Vec<uuid::Uuid>,
    pub(crate) extension_ancestors: Vec<super::extensions::Failure>,
    pub(crate) extension_writers: Vec<(uuid::Uuid, super::extensions::Failure)>,
    callback_bnodes: parking_lot::Mutex<FxHashMap<String, Id>>,
    callback_terms: parking_lot::Mutex<FxHashMap<Id, u64>>,
    /// A budget refusal in infallible code, which fails the next check (see
    /// [`Ctx::fail_memory`]).
    memory_failure: MemoryFailure,
    /// the envelope of transient dictionary decoding
    decode: DecodeEnvelope,
    /// the charge of the query-local terms and memos
    retained: RetainedTerms,
    /// the context itself, once it is shared, which retained charges release through
    /// (see [`Ctx::attach_owner`])
    owner: parking_lot::Mutex<Weak<Ctx>>,
    /// the query's compiled regular expressions, charged in both accounting modes
    regex: super::query_regex::RegexCache,
    // Opt-in cursor accounting stays after the ordinary execution fields.
    /// whether the query streams through pull cursors, whose grant follows their live
    /// bytes (see [`Ctx::streams`])
    streams: bool,
    // Data and weak retained caches drop before final ledger ownership.
    fast: FastMemory,
    memory: std::sync::OnceLock<Arc<QueryMemoryLedger>>,
    // All ordinary payloads, including graphs and ledger callbacks, retire before
    // their actual sources. A new mask can share its original source's backing.
    pub(super) source_view: Option<crate::access::PreparedView>,
    pub(super) read_source: Option<crate::store::BoundReadSource>,
    pub(super) source_preparation: Option<crate::access::Preparation>,
    pub(super) source_setup: Option<crate::access::Reservation>,
    #[cfg(test)]
    pub(super) chunk_hook: parking_lot::Mutex<Option<ChunkHook>>,
}

/// A test observer of the chunks a retained output takes.
#[cfg(test)]
pub(super) type ChunkHook = Arc<dyn Fn(&'static str, usize) + Send + Sync>;

/// Set in [`FastMemory::live`] once the ledger has taken the account over.
const PROMOTED: u64 = 1 << 63;

/// The step in which the fast account admits its eager contribution to the grant. A
/// plain query's estimate grows by a few bytes at a time, and each growth past the
/// contribution takes the account's lock and the grant's counters, so the account
/// admits up to the next step at once. The contribution may then exceed the highest
/// estimate by less than one step.
const EAGER_STEP: u64 = 64 << 10;

/// How far above what a finished eager result keeps its pool contribution may stay
/// before [`Ctx::shrink_retained`] lowers it. Below this the ledger's lock costs more
/// than the memory it would return.
const RETAIN_SLACK: u64 = 1 << 20;

/// The memory account of an eager context until something needs the query ledger: a
/// graph algorithm call, a lease that outlives an operator, cursor accounting or a
/// change of the grant. Plain queries charge and check it with a few atomic operations
/// and never create the ledger. [`Ctx::memory`] hands the account to the ledger as its
/// [`PRIMARY`] owner, after which every charge goes through the ledger.
#[derive(Default)]
struct FastMemory {
    /// live bytes, with [`PROMOTED`] set once the ledger took the account over
    live: AtomicU64,
    /// the highest estimate seen while the account was fast
    peak: AtomicU64,
    /// the account's admitted contribution to `grant`, kept until the context drops
    eager: AtomicU64,
    /// the grant the account charges, fixed at its first use
    grant: std::sync::OnceLock<Option<Arc<PoolGrant>>>,
    /// serializes growth of `eager` with the hand-over to the ledger
    resize: parking_lot::Mutex<()>,
}

impl FastMemory {
    #[inline]
    fn note_peak(&self, bytes: u64) {
        if bytes > self.peak.load(Ordering::Relaxed) {
            self.peak.fetch_max(bytes, Ordering::Relaxed);
        }
    }

    /// Give back `bytes` of the account's live estimate. `false` once the ledger owns
    /// the account.
    #[inline]
    fn release(&self, bytes: u64) -> bool {
        let mut cur = self.live.load(Ordering::Acquire);
        loop {
            if cur & PROMOTED != 0 {
                return false;
            }
            let next = cur.checked_sub(bytes).expect("query live bytes");
            match self
                .live
                .compare_exchange_weak(cur, next, Ordering::AcqRel, Ordering::Acquire)
            {
                Ok(_) => return true,
                Err(c) => cur = c,
            }
        }
    }
}

impl Drop for FastMemory {
    /// An account the ledger never took over returns its eager contribution here.
    fn drop(&mut self) {
        if *self.live.get_mut() & PROMOTED == 0
            && let Some(Some(grant)) = self.grant.get()
        {
            grant.release(*self.eager.get_mut());
        }
    }
}

/// How a context accounts its memory, the one place that reads the mode (see
/// [`Ctx::accounting`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Accounting {
    /// Eager execution, which collects whole tables. Its pool contribution stays at
    /// the admitted high point until the result is dropped.
    Collect,
    /// Pull cursors, whose grant follows their live bytes.
    Cursor,
}

/// The first supported resource refusal that infallible code met. Scalar evaluation cannot return
/// a resource error, so it records the refusal here and the next check fails with it,
/// which keeps a refused allocation from becoming an unbound value.
#[derive(Default)]
struct MemoryFailure {
    failure: parking_lot::Mutex<Option<ResourceRefusal>>,
    failed: AtomicBool,
}

/// Only the supported resource outcomes become sticky. Arbitrary scalar/service
/// errors keep their existing semantics; typed ownership errors never become UNDEF.
#[derive(Clone)]
enum ResourceRefusal {
    Budget(Budget),
    Parent {
        component: &'static str,
        code: String,
        message: String,
        source: sparkles_budget::ParentRefused,
    },
    Cancelled,
    Timeout,
}

impl ResourceRefusal {
    fn from_error(error: &Error) -> Option<Self> {
        match error {
            Error::BudgetExceeded(budget) => Some(Self::Budget(*budget)),
            Error::Cancelled => Some(Self::Cancelled),
            Error::Timeout => Some(Self::Timeout),
            Error::Component(component) => component
                .source
                .as_ref()
                .and_then(|source| source.downcast_ref::<sparkles_budget::ParentRefused>())
                .map(|source| Self::Parent {
                    component: component.component,
                    code: component.code.clone(),
                    message: component.message.clone(),
                    source: *source,
                }),
            _ => None,
        }
    }

    fn into_error(self) -> Error {
        match self {
            Self::Budget(budget) => Error::BudgetExceeded(budget),
            Self::Cancelled => Error::Cancelled,
            Self::Timeout => Error::Timeout,
            Self::Parent {
                component,
                code,
                message,
                source,
            } => Error::Component(Box::new(
                crate::error::ComponentError::new(component, code, message).with_source(source),
            )),
        }
    }
}

/// The high-water envelope that transient dictionary decoding reserves in both
/// accounting modes: a key of the largest size seen so far on every worker, charged once
/// when it grows and released when the work ends (see [`Ctx::reserve_decode_checked`]).
#[derive(Default)]
struct DecodeEnvelope {
    /// the reserved bytes, with the owner of an eager reservation
    state: parking_lot::Mutex<(u64, Option<u64>)>,
    /// the reserved bytes, read without the lock
    reserved: AtomicU64,
}

#[cfg(test)]
thread_local! {
    /// [`Ctx::check`] calls on this thread (for tests of per-query overhead)
    pub(crate) static CHECKS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    /// growths of a fast account's grant contribution on this thread
    static FAST_GROWS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

impl Ctx {
    /// The query's memory ledger, made on first use from the fast account (see
    /// [`FastMemory`]).
    pub(crate) fn memory(&self) -> &Arc<QueryMemoryLedger> {
        self.memory.get_or_init(|| self.promote())
    }

    #[cold]
    #[inline(never)]
    fn promote(&self) -> Arc<QueryMemoryLedger> {
        let fast = &self.fast;
        // No eager growth is in flight, and none starts on the fast account after this.
        let _resize = fast.resize.lock();
        let live = fast.live.fetch_or(PROMOTED, Ordering::AcqRel);
        debug_assert_eq!(live & PROMOTED, 0, "one hand-over");
        let grant = match fast.grant.get() {
            Some(grant) => grant.clone(),
            None => self.grant.clone(),
        };
        QueryMemoryLedger::seeded(
            grant,
            live,
            fast.eager.load(Ordering::Acquire),
            fast.peak.load(Ordering::Acquire),
        )
    }

    pub(super) fn owns_ledger(&self, ledger: &Arc<QueryMemoryLedger>) -> bool {
        self.memory
            .get()
            .is_some_and(|own| Arc::ptr_eq(own, ledger))
    }

    /// A distinct reasoning child sharing this query's live total and original
    /// backing. Construct it only when reasoning starts; ordinary execution keeps
    /// its existing lazy fast account. The child retains accounting, not this Ctx.
    pub fn memory_parent(&self) -> Result<sparkles_budget::ParentHandle> {
        self.memory()
            .child_parent(self.grant.clone(), self.mem_limit)
    }

    /// Attach a seed subcontext to its outer query before any charge. It retains
    /// its own ledger/data lifetime and never reuses the outer primary owner id.
    pub(crate) fn set_memory_parent(
        &mut self,
        parent: sparkles_budget::ParentHandle,
    ) -> Result<()> {
        if self.memory.get().is_some()
            || self.fast.live.load(Ordering::Relaxed) != 0
            || self.fast.eager.load(Ordering::Relaxed) != 0
            || self.fast.peak.load(Ordering::Relaxed) != 0
        {
            return Err(Error::invalid(
                "upstream query memory is set before the first charge",
            ));
        }
        let factory = self
            .grant
            .as_ref()
            .map_or(sparkles_budget::MemoryDomain::unpooled(), |g| {
                g.memory_domain()
            });
        if factory.id() != parent.domain() {
            return Err(Error::Component(Box::new(
                crate::error::ComponentError::new(
                    "query-memory",
                    "foreign-domain",
                    "upstream memory belongs to another accounting domain",
                ),
            )));
        }
        let ledger = QueryMemoryLedger::nested(self.grant.clone(), parent);
        self.memory
            .set(ledger)
            .map_err(|_| Error::invalid("query memory already initialized"))?;
        self.fast.live.store(PROMOTED, Ordering::Release);
        Ok(())
    }

    /// The grant the query charges, if it has one.
    pub fn grant(&self) -> Option<&Arc<PoolGrant>> {
        self.grant.as_ref()
    }

    /// Set the grant before any charge, and fix it as the fast account's grant without
    /// a lazy initialization on the first charge. A grant that changes after a charge
    /// would leave charges on the old one, so a call after the first charge is a bug
    /// that debug builds refuse.
    pub(crate) fn set_grant(&mut self, grant: Option<Arc<PoolGrant>>) {
        debug_assert!(
            self.memory.get().is_none()
                && self.fast.live.load(Ordering::Relaxed) == 0
                && self.fast.eager.load(Ordering::Relaxed) == 0
                && self.fast.peak.load(Ordering::Relaxed) == 0,
            "the grant is set before the first charge"
        );
        self.fast.grant = std::sync::OnceLock::from(grant.clone());
        self.grant = grant;
    }

    /// Change the grant after charges were made, to test that the ledger keeps each
    /// contribution on the grant it was admitted under.
    #[cfg(test)]
    pub(crate) fn reconfigure_grant(&mut self, grant: Option<Arc<PoolGrant>>) {
        self.grant = grant;
    }

    /// The fast account's grant, or `None` when the ledger must take the request: it
    /// exists already, or the public grant changed since the fast account began.
    #[inline]
    fn fast_grant(&self) -> Option<Option<&Arc<PoolGrant>>> {
        if self.memory.get().is_some() {
            return None;
        }
        let grant = self.fast.grant.get_or_init(|| self.grant.clone()).as_ref();
        same(grant, self.grant.as_ref()).then_some(grant)
    }

    /// Admit `bytes` more live estimate (`add`) or check a transient output of
    /// `bytes` on the fast account, with the ledger's semantics for a single eager
    /// owner. `None` when the ledger must take the request.
    #[inline]
    fn fast_admit(&self, bytes: u64, add: bool) -> Option<Result<()>> {
        let grant = self.fast_grant()?;
        let fast = &self.fast;
        let mut cur = fast.live.load(Ordering::Acquire);
        loop {
            if cur & PROMOTED != 0 {
                return None;
            }
            let need = cur.checked_add(bytes).filter(|n| *n < PROMOTED)?;
            if !add {
                // Like the ledger, an output check records the attempted estimate.
                fast.note_peak(need);
            }
            if need > self.mem_limit {
                return Some(Err(self.memory_exceeded(need)));
            }
            if let Some(grant) = grant
                && need > fast.eager.load(Ordering::Acquire)
            {
                return self.fast_grow(grant, bytes, add);
            }
            if !add {
                return Some(Ok(()));
            }
            match fast
                .live
                .compare_exchange_weak(cur, need, Ordering::AcqRel, Ordering::Acquire)
            {
                Ok(_) => {
                    fast.note_peak(need);
                    return Some(Ok(()));
                }
                Err(c) => cur = c,
            }
        }
    }

    /// [`Ctx::fast_admit`] when the estimate passes the eager contribution: grow it
    /// from the grant first, so a refused grant changes nothing.
    #[cold]
    #[inline(never)]
    fn fast_grow(&self, grant: &Arc<PoolGrant>, bytes: u64, add: bool) -> Option<Result<()>> {
        #[cfg(test)]
        FAST_GROWS.with(|n| n.set(n.get() + 1));
        let fast = &self.fast;
        let _resize = fast.resize.lock();
        let mut cur = fast.live.load(Ordering::Acquire);
        loop {
            if cur & PROMOTED != 0 {
                return None;
            }
            let need = cur.checked_add(bytes).filter(|n| *n < PROMOTED)?;
            if need > self.mem_limit {
                return Some(Err(self.memory_exceeded(need)));
            }
            let eager = fast.eager.load(Ordering::Acquire);
            if need > eager {
                // Admit a step ahead, within the local budget, so that the next small
                // increases stay on the lock-free path. When the pool cannot give the
                // step, admit exactly what the estimate needs.
                let ahead = need
                    .next_multiple_of(EAGER_STEP)
                    .min(self.mem_limit)
                    .max(need);
                let admitted = match grant.charge(ahead - eager) {
                    Ok(()) => ahead,
                    Err(_) if ahead > need => match grant.charge(need - eager) {
                        Ok(()) => need,
                        Err(error) => return Some(Err(error)),
                    },
                    Err(error) => return Some(Err(error)),
                };
                fast.eager.store(admitted, Ordering::Release);
            }
            if !add {
                return Some(Ok(()));
            }
            // Lock-free admissions in between stayed within the old contribution.
            match fast
                .live
                .compare_exchange(cur, need, Ordering::AcqRel, Ordering::Acquire)
            {
                Ok(_) => {
                    fast.note_peak(need);
                    return Some(Ok(()));
                }
                Err(c) => cur = c,
            }
        }
    }

    /// [`Ctx::live_bytes`] for the tests of other modules.
    #[cfg(all(test, feature = "geo"))]
    pub(crate) fn live_bytes_for_tests(&self) -> u64 {
        self.live_bytes()
    }

    /// The query's live estimate, from the ledger once it exists.
    #[inline]
    pub(super) fn live_bytes(&self) -> u64 {
        match self.memory.get() {
            Some(ledger) => ledger.live.load(Ordering::Relaxed),
            None => self.fast.live.load(Ordering::Relaxed) & !PROMOTED,
        }
    }

    pub fn new(snap: Arc<Snapshot>) -> Ctx {
        Ctx {
            reasoning: None,
            local_facts: false,
            reasoning_memory: Vec::new(),
            extension_families: Vec::new(),
            extension_writers: Vec::new(),
            extension_ancestors: Vec::new(),
            extensions: None,
            extension_failure: Default::default(),
            calls_extensions: false,
            callback_bnodes: Default::default(),
            callback_terms: Default::default(),
            memory_failure: Default::default(),
            decode: Default::default(),
            retained: Default::default(),
            owner: Default::default(),
            regex: Default::default(),
            streams: false,
            fast: Default::default(),
            memory: Default::default(),
            snap,
            read_source: None,
            source_view: None,
            source_preparation: None,
            source_setup: None,
            #[cfg(test)]
            chunk_hook: parking_lot::Mutex::new(None),
            local: RwLock::new(AppendVocab::default()),
            values: Default::default(),
            next_bnode: AtomicU64::new(0),
            bnode_memo: Default::default(),
            outside_bnodes: Default::default(),
            literal_bnodes: Default::default(),
            deadline: None,
            deadline_pause: Arc::new(AtomicU64::new(0)),
            cancel: Arc::new(AtomicBool::new(false)),
            cancellation: None,
            dataset: DatasetSpec::default(),
            graphs: None,
            now: std::sync::OnceLock::new(),
            base_iri: None,
            var_names: RwLock::new(Vec::new()),
            max_rows: 200_000_000,
            mem_limit: u64::MAX,
            grant: None,
            spill: None,
            max_rows_produced: u64::MAX,
            rows_produced: Arc::new(AtomicU64::new(0)),
            allow_service: true,
            forbid_service: false,
            outbound: Default::default(),
            outbound_budget: crate::outbound::RequestBudget::new(&Default::default()),
            use_cache: true,
            plan_patterns: true,
            service_scope: Arc::from(""),
            local_endpoints: None,
            service_depth: 0,
            algo_projections: None,
            views: None,
            rdfs: None,
            opt: Optimizations::default(),
            io_start: crate::ioacct::profiling()
                .then(|| Box::new(crate::ioacct::IoSnapshot::now())),
            geo: Default::default(),
            warnings: Default::default(),
            algorithmic: AtomicBool::new(false),
            eager_union_demand: AtomicBool::new(false),
            failed: Default::default(),
            sampled: Default::default(),
            stars: Default::default(),
            probes: Default::default(),
        }
    }

    /// What the read path counted since this context was made: index columns decoded,
    /// bytes asked of the kernel and vocabulary lookups (see [`crate::ioacct`]). The
    /// counters are process-wide, so reads of queries that run at the same time are
    /// included. The starting counters are taken only while a [`Profile`](crate::ioacct::Profile)
    /// is alive, so the result is meaningful only for a query made under one: otherwise it
    /// holds everything the process has counted.
    pub fn io_stats(&self) -> crate::ioacct::IoSnapshot {
        let now = crate::ioacct::IoSnapshot::now();
        match &self.io_start {
            Some(start) => now.since(start),
            None => now.since(&Default::default()),
        }
    }

    /// The query's `NOW()`: the clock at its first call, the same value at every later
    /// call of this context.
    pub fn now(&self) -> oxsdatatypes::DateTime {
        *self.now.get_or_init(oxsdatatypes::DateTime::now)
    }

    pub(crate) fn fail_extension(&self, error: super::extensions::ScalarError) {
        let error = error.bounded();
        if !matches!(error, super::extensions::ScalarError::Expression) {
            if let Some(failure) = &self.extension_failure {
                failure.lock().get_or_insert(error.clone());
            }
            for ancestor in &self.extension_ancestors {
                ancestor.lock().get_or_insert(error.clone());
            }
            for (_, writer) in self
                .extension_writers
                .iter()
                .filter(|(id, _)| *id == self.snap.dataset_id)
            {
                writer.lock().get_or_insert(error.clone());
            }
        }
    }

    pub(crate) fn configure_extensions(&mut self, pattern: &spargebra::algebra::GraphPattern) {
        self.calls_extensions = self
            .extensions
            .as_ref()
            .is_some_and(|r| !r.is_empty() && r.references(pattern));
        if self.calls_extensions {
            self.extension_failure = Some(Default::default());
            self.extension_families = super::extensions::captured_families(self.snap.dataset_id);
            self.extension_writers = super::extensions::captured_writers();
            self.extension_ancestors = super::extensions::captured_ancestors();
            self.use_cache = false;
            self.opt.expr_cache = false;
            self.opt.sampled_filters = false;
            self.opt.decorrelate_exists = false;
            self.opt.count_filter_runs = false;
            self.opt.filter_scan_runs = false;
            // Ranking the first key separately can evaluate it twice and omit calls
            // from later keys on discarded rows.
            self.opt.topk_first_key = false;
        }
    }

    pub(crate) fn retain_callback_bytes(&self, id: Id, bytes: u64) -> Result<()> {
        let mut retained = self.callback_terms.lock();
        if retained.contains_key(&id) {
            return Ok(());
        }
        let charge = self.charge(bytes)?;
        retained.insert(id, bytes);
        // This vocabulary belongs to Ctx and is released with it; unlike temporary
        // callback batches it must remain charged for the rest of query execution.
        std::mem::forget(charge);
        Ok(())
    }

    /// Callback blank nodes may refer back to nodes it received, but arbitrary labels
    /// cannot forge stored identities. Newly returned labels live in this query only.
    pub(crate) fn intern_callback_term(&self, term: &Term, arguments: &[Term]) -> Result<Id> {
        self.check()?;
        fn permit(label: &str, out: &mut FxHashMap<String, Id>, charge: &Charge<'_>) -> Result<()> {
            if let Some(payload) = id::parse_bnode_payload(label)
                && !out.contains_key(label)
            {
                charge.add(label.len() as u64 + 96)?;
                out.insert(label.to_owned(), Id::bnode(payload));
            }
            Ok(())
        }
        fn collect(
            term: &Term,
            out: &mut FxHashMap<String, Id>,
            charge: &Charge<'_>,
        ) -> Result<()> {
            match term {
                Term::BlankNode(b) => permit(b.as_str(), out, charge)?,
                Term::Triple(t) => {
                    collect(&t.subject.clone().into(), out, charge)?;
                    collect(&t.object, out, charge)?;
                }
                _ => {}
            }
            Ok(())
        }
        let allowed_charge = self.charge(0)?;
        let mut allowed = FxHashMap::default();
        for term in arguments {
            collect(term, &mut allowed, &allowed_charge)?;
            let mut failure = None;
            let _parsed = super::cdt::callback_relabel_term(self, term, &mut |b| {
                if failure.is_none() {
                    failure = permit(b, &mut allowed, &allowed_charge).err();
                }
                b.to_owned()
            })?;
            if let Some(error) = failure {
                return Err(error);
            }
        }
        let mut failure = None;
        let mut choose = |b: &str| {
            if let Some(id) = allowed.get(b) {
                return *id;
            }
            let mut nodes = self.callback_bnodes.lock();
            if let Some(id) = nodes.get(b) {
                return *id;
            }
            // Keep label-map ownership charged independently of returned values.
            // A failed conversion can never allocate new unaccounted identities.
            if failure.is_some() {
                return Id::bnode(0);
            }
            match self.charge(b.len() as u64 + 96) {
                Ok(charge) => {
                    let id = self.fresh_bnode();
                    nodes.insert(b.to_owned(), id);
                    std::mem::forget(charge);
                    id
                }
                Err(error) => {
                    failure = Some(error);
                    Id::bnode(0)
                }
            }
        };
        let _mapped_charge =
            self.charge(super::extensions::term_bytes(term).map_err(|e| e.engine())?)?;
        let term = self.map_bnodes(term, &mut choose);
        let relabeled = super::cdt::callback_relabel_term(self, &term, &mut |b| {
            id::bnode_label(choose(b).payload())
        })?;
        if let Some(error) = failure {
            return Err(error);
        }
        let mapped = relabeled.term.as_ref().unwrap_or(&term);
        let bytes = super::extensions::term_bytes(mapped).map_err(|e| e.engine())?;
        self.check()?;
        let _output = self.charge(bytes)?;
        let id = self.intern_term(mapped);
        if id.tag() == Tag::Local
            || (id.tag() == Tag::BNode && id.payload() & Id::LOCAL_BNODE_BIT != 0)
        {
            self.retain_callback_bytes(id, bytes)?;
        }
        Ok(id)
    }

    /// Record a warning for the plan (once, however often it is raised).
    pub fn warn(&self, w: PlanWarning) {
        let mut ws = self.warnings.lock();
        if !ws.contains(&w) && self.retain_bytes(w.message.len() as u64 + 128) {
            ws.push(w);
        }
    }

    /// Keep `info` as the plan of the failed node `node` (see [`Ctx::take_failed`]).
    pub(super) fn set_failed(&self, node: &super::plan::Node, info: super::exec::PlanInfo) {
        *self.failed.lock() = Some((node as *const _ as usize, info));
    }

    /// The plan of the failed node `node`, when the failure came from it.
    pub(super) fn take_failed(&self, node: &super::plan::Node) -> Option<super::exec::PlanInfo> {
        let mut f = self.failed.lock();
        match &*f {
            Some((at, _)) if *at == node as *const _ as usize => f.take().map(|(_, i)| i),
            _ => None,
        }
    }

    /// The plan of the operator tree that failed, as far as it ran, when a failure
    /// recorded one.
    pub fn failed_plan(&self) -> Option<super::exec::PlanInfo> {
        self.failed.lock().take().map(|(_, i)| i)
    }

    /// Whether the parsed query calls a graph algorithm, that is, has a `SERVICE` whose
    /// IRI is in the algorithm namespace once prefixes and the base are resolved. The
    /// planner decides it from the algebra, so a prefixed name counts like the full IRI.
    pub fn calls_algorithm(&self) -> bool {
        self.algorithmic.load(Ordering::Relaxed)
    }

    pub(super) fn note_algorithm_call(&self) {
        self.algorithmic.store(true, Ordering::Relaxed);
    }

    /// The warnings recorded so far.
    pub fn warnings(&self) -> Vec<PlanWarning> {
        self.warnings.lock().clone()
    }

    /// The selectivity measured on a sample for the FILTER conjunct shown as `text`, if
    /// one was measured while planning this query.
    pub(super) fn sampled(&self, text: &str) -> Option<super::sample::Sampled> {
        let m = self.sampled.lock();
        if m.is_empty() {
            return None;
        }
        m.get(text).copied()
    }

    /// Keep the selectivity measured for the conjunct shown as `text` (the first one
    /// measured stays, so that every plan compared sees the same).
    pub(super) fn set_sampled(&self, text: String, s: super::sample::Sampled) {
        self.sampled.lock().entry(text).or_insert(s);
    }

    #[inline]
    pub fn check(&self) -> Result<()> {
        #[cfg(test)]
        CHECKS.with(|n| n.set(n.get() + 1));
        self.check_memory_failure()?;
        if let Some(preparation) = &self.source_preparation {
            // Both source and query absolute controls apply during preparation
            // and execution, without cursor wait extensions or renewed deadlines.
            preparation.check()?;
        } else if let Some(source) = &self.read_source {
            // Source preparation has its original absolute deadline. Cursor wait
            // pauses never extend it, and no cancellation watcher is required.
            source.query_input().2.check()?;
        }
        if self.calls_extensions
            && let Some(state) = &self.extension_failure
            && let Some(failure) = state.lock().as_ref()
        {
            return Err(failure.engine());
        }
        if self.cancel.load(Ordering::Relaxed) {
            return Err(Error::Cancelled);
        }
        if let Some(cancellation) = &self.cancellation {
            cancellation.check()?;
        }
        if self.past_deadline() {
            return Err(Error::Timeout);
        }
        Ok(())
    }

    /// Whether the deadline has passed, not counting the time the query spent waiting
    /// for its consumer. The counter is read only once the raw deadline has passed, which
    /// keeps the atomic load off the common path.
    pub fn past_deadline(&self) -> bool {
        let Some(d) = self.deadline else {
            return false;
        };
        let now = Instant::now();
        if now < d {
            return false;
        }
        let paused = std::time::Duration::from_nanos(self.deadline_pause.load(Ordering::Relaxed));
        d.checked_add(paused).is_some_and(|end| now >= end)
    }

    /// The deadline as it stands now, moved back by the time the query has waited for
    /// its consumer so far. Work that copies the deadline, such as a SERVICE call or an
    /// algorithm run, takes this value when it starts. A deadline too far out to
    /// represent is none.
    pub fn current_deadline(&self) -> Option<Instant> {
        let d = self.deadline?;
        let paused = std::time::Duration::from_nanos(self.deadline_pause.load(Ordering::Relaxed));
        d.checked_add(paused)
    }

    pub(super) fn enable_cursor_accounting(&mut self) -> Result<()> {
        self.memory().check_cursor_transition()?;
        // A cursor reserves its decoding on the ledger, so the eager envelope goes.
        self.release_decode_envelope();
        // The decoded value cache, the query-local terms and the parsed geometries are
        // charged already, and the ledger carries their reservations over, so the
        // cursor keeps them.
        self.memory().cursor_mode()?;
        self.streams = true;
        // The result cache and sampled filters are planning choices that stage 6 of
        // item 453 makes independent of the execution mode.
        self.use_cache = false;
        self.opt.sampled_filters = false;
        Ok(())
    }

    /// How this context accounts its memory.
    #[inline]
    pub(super) fn accounting(&self) -> Accounting {
        if self.streams {
            Accounting::Cursor
        } else {
            Accounting::Collect
        }
    }

    /// Whether the query streams through pull cursors. Both modes charge the same state,
    /// so only planning and routing choices, the accumulation of an EXISTS key set across
    /// batches and whether a decoding envelope can refuse read it.
    #[inline]
    pub(super) fn streams(&self) -> bool {
        self.accounting() == Accounting::Cursor
    }

    /// Let retained charges release through this shared context, in both accounting
    /// modes. Query preparation attaches every context it shares.
    ///
    /// Regular expressions compiled before, such as those of sampled filters in eager
    /// planning, are uncharged, so the first attachment drops them and execution
    /// compiles them again with their charges.
    pub(super) fn attach_owner(self: &Arc<Self>) {
        let mut owner = self.owner.lock();
        let first = owner.strong_count() == 0;
        *owner = Arc::downgrade(self);
        drop(owner);
        if first {
            self.regex.clear();
        }
    }

    /// Drop the context's weak reference to itself, which lets a test take the prepared
    /// context back with `Arc::get_mut`.
    #[cfg(test)]
    pub(super) fn detach_owner_for_tests(&self) {
        *self.owner.lock() = Weak::new();
    }

    /// The cursor's own context, for state that runs a cursor operator tree of its own
    /// (the key set of an EXISTS pattern). `None` in eager execution.
    pub(super) fn cursor_owner(&self) -> Option<Arc<Ctx>> {
        if !self.streams {
            return None;
        }
        self.owner.lock().upgrade()
    }

    /// Charge `bytes` of state that outlives the call that builds it, such as a cache
    /// that belongs to the plan, in both accounting modes. The charge holds a weak owner,
    /// which avoids a plan and context reference cycle. A context that was never shared
    /// has no owner to release through, so an eager one keeps such state uncharged, as
    /// `None` says, while a cursor always has one.
    pub(super) fn retained_charge(&self, bytes: u64) -> Result<Option<RetainedCharge>> {
        let owner = self.owner.lock().clone();
        if owner.strong_count() == 0 {
            if !self.streams {
                return Ok(None);
            }
            return Err(Error::invalid("cursor context owner has not been attached"));
        }
        let charge = self.charge(bytes)?;
        let id = charge.disarm();
        Ok(Some(RetainedCharge { owner, id, bytes }))
    }

    /// The query's program of a regular expression, compiled and charged on first use.
    /// `Ok(None)` for a pattern or flags that do not compile.
    pub(super) fn regex(
        &self,
        pattern: &str,
        flags: &str,
    ) -> Result<Option<Arc<super::query_regex::RegexState>>> {
        self.regex.get(self, pattern, flags)
    }

    /// Run `f` on the query's program of a regular expression without taking a
    /// reference to it, which is the per-row path of `REGEX` and `REPLACE`.
    #[inline]
    pub(super) fn with_regex<R>(
        &self,
        pattern: &str,
        flags: &str,
        f: impl FnOnce(&super::query_regex::RegexState) -> R,
    ) -> Result<Option<R>> {
        self.regex.with(self, pattern, flags, f)
    }

    /// Give back `bytes` of estimated memory, and the pool's granules the query no
    /// longer needs.
    #[inline]
    fn release_live(&self, owner: Option<u64>, bytes: u64) {
        match owner {
            Some(PRIMARY) if self.fast.release(bytes) => {}
            Some(id) => self.memory().release(id, bytes),
            None => {
                debug_assert_eq!(bytes, 0);
                // Only cursor accounting trims, and it always has a ledger.
                if let Some(ledger) = self.memory.get() {
                    ledger.trim_zero(self.grant.as_ref());
                }
            }
        }
    }

    /// The bytes this query holds in the server's query memory pool (0 without a pool).
    pub fn granted_bytes(&self) -> u64 {
        self.grant.as_ref().map_or(0, |g| g.granted())
    }

    pub(super) fn memory_remaining(&self) -> u64 {
        self.mem_limit.saturating_sub(self.live_bytes())
    }

    /// The bytes this query could still charge: what its own budget leaves, capped by
    /// what the server pool has free plus the unused part of the query's grant.
    pub(super) fn memory_room(&self) -> u64 {
        let own = self.memory_remaining();
        match &self.grant {
            Some(grant) => {
                let pool = grant.pool();
                let free = pool.capacity().saturating_sub(pool.granted());
                let spare = grant.granted().saturating_sub(grant.used());
                own.min(free.saturating_add(spare))
            }
            None => own,
        }
    }

    /// Release the state that only running work needs: the decode envelope and the
    /// query's regular expressions. Execution ends with it in both accounting modes.
    pub(super) fn release_work(&self) {
        self.regex.clear();
        self.release_decode_envelope();
    }

    /// [`Ctx::release_work`] under the name the cursor executor calls.
    pub(super) fn release_cursor_work(&self) {
        self.release_work();
    }

    fn release_decode_envelope(&self) {
        let envelope = &self.decode;
        let mut state = envelope.state.lock();
        let (bytes, owner) = std::mem::take(&mut *state);
        envelope.reserved.store(0, Ordering::Release);
        drop(state);
        if bytes == 0 {
            return;
        }
        match owner {
            // An eager envelope is a charge of its own.
            Some(owner) => self.release_live(Some(owner), bytes),
            // A cursor's is decode bytes on the ledger.
            None => self.memory().release_decode(),
        }
    }

    /// Fail with the budget refusal that infallible code recorded (see
    /// [`Ctx::fail_memory`]).
    #[inline]
    pub(super) fn check_memory_failure(&self) -> Result<()> {
        let failure = &self.memory_failure;
        if failure.failed.load(Ordering::Acquire) {
            return self.recorded_failure();
        }
        Ok(())
    }

    #[cold]
    #[inline(never)]
    fn recorded_failure(&self) -> Result<()> {
        let failure = self.memory_failure.failure.lock().clone();
        match failure {
            Some(failure) => Err(failure.into_error()),
            None => Ok(()),
        }
    }

    /// Record the first supported resource refusal met by infallible code, so
    /// the next check preserves its original typed outcome in both modes.
    pub(super) fn fail_memory(&self, error: &Error) {
        if self.memory_failure.failed.load(Ordering::Acquire) {
            return;
        }
        if let Some(refusal) = ResourceRefusal::from_error(error) {
            let failure = &self.memory_failure;
            failure.failure.lock().get_or_insert(refusal);
            failure.failed.store(true, Ordering::Release);
        }
    }

    /// Charge `bytes` more of query-local terms and memos, which stay until the context
    /// drops, in both accounting modes. Infallible expression interning must not turn a
    /// resource failure into an ordinary unbound BIND, so a refusal is recorded and fails
    /// the next check, and `false` tells the caller not to allocate.
    pub(crate) fn retain_bytes(&self, bytes: u64) -> bool {
        if self.memory_failure.failed.load(Ordering::Acquire) {
            return false;
        }
        let retained = &self.retained;
        let used = retained.used.fetch_add(bytes, Ordering::AcqRel) + bytes;
        if retained.charge.covers(used) {
            return true;
        }
        match self.grow_reservation(&retained.charge, used, u64::MAX, true) {
            Ok(()) => true,
            Err(error) => {
                retained.used.fetch_sub(bytes, Ordering::AcqRel);
                self.fail_memory(&error);
                false
            }
        }
    }

    /// Admit transient decoding of a key of `bytes` bytes for an explicitly fallible
    /// operation. A refused sort trial can release its storage and try a smaller piece,
    /// so it must not poison the context as an infallible scalar evaluation would. The
    /// envelope covers a key of the largest size seen so far on every worker, so the
    /// common path is one atomic load.
    #[inline]
    pub(super) fn reserve_decode_checked(&self, bytes: usize) -> Result<()> {
        self.check_memory_failure()?;
        let workers = if rayon::current_thread_index().is_some() {
            rayon::current_num_threads() as u64 + 1
        } else {
            1
        };
        let need = (bytes as u64)
            .saturating_mul(8)
            .saturating_add(1024)
            .saturating_mul(workers);
        if self.decode.reserved.load(Ordering::Acquire) >= need {
            return Ok(());
        }
        self.grow_decode(need)
    }

    /// Whether the decode envelope can refuse a key before it is rebuilt. A cursor's
    /// callers report a refusal through the next check, and eager callers cannot.
    #[inline]
    fn envelope_refuses(&self) -> bool {
        self.streams()
    }

    #[cold]
    #[inline(never)]
    fn grow_decode(&self, need: u64) -> Result<()> {
        let envelope = &self.decode;
        let mut state = envelope.state.lock();
        let (peak, owner) = &mut *state;
        if need > *peak {
            // A cursor's callers of `term` and `value` report a refusal through the
            // next check, so its envelope reserves on the ledger and can fail.
            if self.envelope_refuses() {
                let ledger = self.memory();
                let account = ledger.owner(self.grant.as_ref())?;
                ledger.reserve(&account, self.mem_limit, need - *peak, true)?;
            } else {
                // Eager callers of `term` and `value`, the planner's plan text and the
                // readers of a finished result among them, cannot report a refusal. An
                // eager envelope is therefore charged when the budget and the pool allow
                // it, and otherwise the key is decoded without it, as eager decoding
                // always was. The next growth tries the charge again.
                let Ok(charge) = self.charge(need - *peak) else {
                    return Ok(());
                };
                let id = charge.disarm();
                debug_assert!(owner.is_none() || id.is_none() || *owner == id);
                if owner.is_none() {
                    *owner = id;
                }
            }
            *peak = need;
            envelope.reserved.store(need, Ordering::Release);
        }
        drop(state);
        self.check_memory_failure()
    }

    /// Reserve the same envelope for an infallible scalar caller. Its failure stays
    /// sticky so an expression cannot become unbound.
    #[inline]
    pub(super) fn reserve_decode(&self, bytes: usize) -> bool {
        match self.reserve_decode_checked(bytes) {
            Ok(()) => true,
            Err(error) => {
                self.fail_memory(&error);
                false
            }
        }
    }

    /// `f` of the key of base vocabulary id `id`, with its reconstruction and its
    /// decoding inside the envelope. A refusal records a failure and gives `None`.
    #[inline]
    fn with_vocab_key<T>(&self, id: u64, mut f: impl FnMut(&[u8]) -> T) -> Option<T> {
        if !self.envelope_refuses() {
            // An envelope that never refuses is charged after the key is rebuilt, on the
            // vocabulary's unchecked path. Checking it before each step of the rebuild
            // made an eager `term` about a quarter slower, which writing a large answer
            // term by term pays for every cell.
            return self
                .snap
                .generation
                .vocab
                .get_with(id, |key| self.reserve_decode(key.len()).then(|| f(key)))
                .flatten();
        }
        match self.snap.generation.vocab.get_with_checked(
            id,
            |need| self.reserve_decode_checked(need),
            |key| self.reserve_decode(key.len()).then(|| f(key)),
        ) {
            Ok(found) => found.flatten(),
            Err(error) => {
                self.fail_memory(&error);
                None
            }
        }
    }

    /// The row limit of intermediate results.
    #[inline]
    pub fn check_rows(&self, n: usize) -> Result<()> {
        if n > self.max_rows {
            return Err(Error::BudgetExceeded(Budget {
                kind: BudgetKind::Rows,
                limit: self.max_rows as u64,
                requested: n as u64,
            }));
        }
        Ok(())
    }

    /// An operator finished with `rows` rows: count them as produced, and fail once the
    /// total passes [`Ctx::max_rows_produced`]. One atomic update per operator, in both
    /// accounting modes. The counter never wraps: a total past `u64::MAX` fails the
    /// query and leaves the counter as it was.
    #[inline]
    pub fn produced(&self, rows: usize) -> Result<()> {
        let rows = rows as u64;
        let prior = self
            .rows_produced
            .try_update(Ordering::Relaxed, Ordering::Relaxed, |n| {
                n.checked_add(rows)
            })
            .map_err(|_| self.produced_exceeded(u64::MAX))?;
        let total = prior + rows;
        if total > self.max_rows_produced {
            return Err(self.produced_exceeded(total));
        }
        Ok(())
    }

    /// The rows produced by all operators so far.
    pub fn rows_produced(&self) -> u64 {
        self.rows_produced.load(Ordering::Relaxed)
    }

    fn produced_exceeded(&self, requested: u64) -> Error {
        Error::BudgetExceeded(Budget {
            kind: BudgetKind::RowsProduced,
            limit: self.max_rows_produced,
            requested,
        })
    }

    // --------------------------------------------------------------- memory ------

    /// Before an operator produces (or grows its output to) `rows` rows of `width`
    /// columns: the row limit, and the memory budget for the tables held now plus that
    /// output. Runs before the output is allocated where the size is known up front, so
    /// a query over budget fails fast. Records the peak estimate.
    #[inline]
    pub fn check_output(&self, rows: usize, width: usize) -> Result<()> {
        self.check_output_rows(rows)?;
        self.check_output_bytes(rows, width)
    }

    /// The memory half of [`Ctx::check_output`]: admit the bytes of `rows` rows of
    /// `width` columns, such as capacity reserved ahead of the rows, without counting
    /// them against the row limits.
    pub(super) fn check_output_bytes(&self, rows: usize, width: usize) -> Result<()> {
        let bytes = table_bytes(rows, width);
        if let Some(checked) = self.fast_admit(bytes, false) {
            return checked;
        }
        let owner = self.memory().owner(self.grant.as_ref())?;
        self.memory().check_output(&owner, self.mem_limit, bytes)
    }

    pub(super) fn check_output_rows(&self, rows: usize) -> Result<()> {
        self.check_rows(rows)?;
        if self.max_rows_produced != u64::MAX {
            let total = self
                .rows_produced
                .load(Ordering::Relaxed)
                .saturating_add(rows as u64);
            if total > self.max_rows_produced {
                return Err(self.produced_exceeded(total));
            }
        }
        Ok(())
    }

    /// Hold `bytes` against the local budget until the guard drops. An eager
    /// context also retains its admitted pool peak until the context drops.
    pub(super) fn output_lease(&self, bytes: u64) -> Result<QueryMemoryLease> {
        let ledger = self.memory();
        let owner = ledger.owner(self.grant.as_ref())?;
        ledger.reserve(&owner, self.mem_limit, bytes, false)?;
        Ok(QueryMemoryLease {
            ledger: ledger.clone(),
            owner,
            bytes,
            service: None,
        })
    }

    pub fn charge(&self, bytes: u64) -> Result<Charge<'_>> {
        let c = Charge {
            ctx: self,
            bytes: std::cell::Cell::new(0),
            owner: std::cell::Cell::new(None),
        };
        c.add(bytes)?;
        Ok(c)
    }

    /// Lower a finished eager query's retained pool contribution to what its result
    /// keeps: `result` bytes of allocated output columns and its live charges, which
    /// include the state the context keeps, its value cache, terms, memos and
    /// geometries (X08-P1-retained-results). The contribution otherwise stays at the
    /// query's highest admitted estimate, including build and transient state, until the
    /// context drops. A fast account within [`RETAIN_SLACK`] of what the result keeps is
    /// left alone, so that a small query never takes the ledger.
    pub(super) fn shrink_retained(&self, result: u64) -> Result<()> {
        if self.streams {
            return Ok(());
        }
        let fast = match self.memory.get() {
            Some(_) => None,
            None => match self.fast.grant.get() {
                Some(Some(_)) => Some(self.fast.eager.load(Ordering::Acquire)),
                _ => return Ok(()),
            },
        };
        // The output columns hold no lease, so the result keeps them on top of the
        // live charges.
        let keep = result.saturating_add(self.live_bytes());
        // A contribution within the slack of what the result keeps is left alone.
        if fast.is_some_and(|admitted| admitted <= keep.saturating_add(RETAIN_SLACK)) {
            return Ok(());
        }
        self.memory().shrink_eager(result)
    }

    pub fn mem_peak(&self) -> u64 {
        let fast = self.fast.peak.load(Ordering::Relaxed);
        self.memory
            .get()
            .map_or(fast, |ledger| fast.max(ledger.peak.load(Ordering::Relaxed)))
    }

    /// How many rows of `width` columns fit in what is left of the memory budget (for
    /// capping up-front reservations; the output is still checked as it grows).
    pub fn rows_within_budget(&self, width: usize) -> usize {
        if self.mem_limit == u64::MAX {
            return usize::MAX;
        }
        let left = self.mem_limit.saturating_sub(self.live_bytes());
        usize::try_from(left / table_bytes(1, width)).unwrap_or(usize::MAX)
    }

    pub(super) fn memory_exceeded(&self, requested: u64) -> Error {
        Error::BudgetExceeded(Budget {
            kind: BudgetKind::Memory,
            limit: self.mem_limit,
            requested,
        })
    }

    // ------------------------------------------------------------ variables ------

    pub fn var(&self, name: &str) -> VarId {
        if let Some(i) = self.var_names.read().iter().position(|n| n == name) {
            return i as VarId;
        }
        let mut w = self.var_names.write();
        if let Some(i) = w.iter().position(|n| n == name) {
            return i as VarId;
        }
        w.push(name.to_string());
        (w.len() - 1) as VarId
    }

    /// A fresh variable that can never clash with user variables.
    pub fn fresh_var(&self) -> VarId {
        let mut w = self.var_names.write();
        let n = w.len();
        w.push(format!(" _{n}"));
        n as VarId
    }

    /// Is `name` a variable of this query (without creating it)?
    pub fn has_var(&self, name: &str) -> bool {
        self.var_names.read().iter().any(|n| n == name)
    }

    pub fn var_name(&self, v: VarId) -> String {
        self.var_names.read()[v as usize].clone()
    }

    pub fn nvars(&self) -> usize {
        self.var_names.read().len()
    }

    // ---------------------------------------------------------------- terms ------

    /// The id of a term this query decoded from one of its own ids: stored id if the
    /// term exists in the store, else a local id. A blank node label written by
    /// [`bnode_for`] maps back to its id, a stored node's or one this query minted.
    pub fn intern_term(&self, t: &Term) -> Id {
        if let Term::BlankNode(b) = t {
            return match id::parse_bnode_payload(b.as_str()) {
                Some(p) => Id::bnode(p),
                None => self.outside_bnode(b.as_str()),
            };
        }
        if let Some(id) = id::inline_id(t) {
            return id;
        }
        let key = id::term_key(t);
        self.intern_key(&key)
    }

    /// A literal written in the query text. A blank node label inside a composite literal
    /// (`cdt:List`, `cdt:Map`) names a blank node of this query, the same one for the
    /// same label in all the query's literals, as Jena has it. It never names a stored
    /// node, also when it looks like a stored node's label. The literal is written again
    /// with the labels of those nodes, so the functions that read it find them.
    pub fn query_literal<'l>(&self, l: &'l oxrdf::Literal) -> std::borrow::Cow<'l, oxrdf::Literal> {
        match super::cdt::relabel_literal(l, &mut |b| {
            let mut memo = self.literal_bnodes.lock();
            let id = match memo.get(b) {
                Some(id) => *id,
                None if self.retain_bytes(b.len() as u64 + 128) => {
                    let id = self.fresh_bnode();
                    memo.insert(b.to_string(), id);
                    id
                }
                None => Id::UNDEF,
            };
            id::bnode_label(id.payload())
        }) {
            Some(l) => std::borrow::Cow::Owned(l),
            None => std::borrow::Cow::Borrowed(l),
        }
    }

    /// [`Ctx::intern_term`] for a literal written in the query text (see
    /// [`Ctx::query_literal`]).
    pub fn intern_query_literal(&self, l: &oxrdf::Literal) -> Id {
        self.intern_term(&Term::Literal(self.query_literal(l).into_owned()))
    }

    /// The id of a term given to the query from outside, such as an initial binding. A
    /// blank node, also inside a triple term, names a stored node only by that node's
    /// label (see [`parse_bnode_label`]). Any other label, a minted node's label from
    /// an earlier result included, is a new blank node of this query, the same one for
    /// the same label.
    pub fn intern_outside_term(&self, t: &Term) -> Id {
        self.intern_term(&self.map_bnodes(t, &mut |b| {
            parse_bnode_label(b).unwrap_or_else(|| self.outside_bnode(b))
        }))
    }

    /// The id of a term from a SERVICE result. Its blank nodes are the remote endpoint's,
    /// so every label is a new blank node of this query, the same one for the same label
    /// within `scope` (one result set).
    pub fn intern_remote_term(&self, t: &Term, scope: &mut FxHashMap<String, Id>) -> Id {
        self.intern_term(&self.map_bnodes(t, &mut |b| {
            if let Some(&id) = scope.get(b) {
                return id;
            }
            if !self.retain_bytes(b.len() as u64 + 128) {
                return Id::UNDEF;
            }
            let id = self.fresh_bnode();
            scope.insert(b.to_string(), id);
            id
        }))
    }

    /// `t` with each blank node, also inside triple terms, replaced by the node `f` picks
    /// for its label.
    fn map_bnodes(&self, t: &Term, f: &mut dyn FnMut(&str) -> Id) -> Term {
        match t {
            Term::BlankNode(b) => Term::BlankNode(bnode_for(f(b.as_str()))),
            Term::Triple(tr) => {
                let s = match self.map_bnodes(&tr.subject.clone().into(), f) {
                    Term::NamedNode(n) => oxrdf::NamedOrBlankNode::NamedNode(n),
                    Term::BlankNode(b) => oxrdf::NamedOrBlankNode::BlankNode(b),
                    _ => unreachable!("a subject stays a subject"),
                };
                let o = self.map_bnodes(&tr.object, f);
                Term::Triple(Box::new(oxrdf::Triple::new(s, tr.predicate.clone(), o)))
            }
            t => t.clone(),
        }
    }

    /// A blank node of this query for a label from outside it.
    fn outside_bnode(&self, label: &str) -> Id {
        let mut memo = self.outside_bnodes.lock();
        if let Some(&id) = memo.get(label) {
            return id;
        }
        if !self.retain_bytes(label.len() as u64 + 128) {
            return Id::UNDEF;
        }
        let id = self.fresh_bnode();
        memo.insert(label.to_string(), id);
        id
    }

    /// Id for a graph name, mapping Jena's special default-graph IRI.
    pub fn graph_id(&self, iri: &str) -> Id {
        if iri == DEFAULT_GRAPH_IRI {
            Id::DEFAULT_GRAPH
        } else {
            self.intern_term(&Term::NamedNode(oxrdf::NamedNode::new_unchecked(iri)))
        }
    }

    /// Whether `id` is a constant no fact of the snapshot holds: a query-local term,
    /// unless query-time reasoning derived facts with such terms.
    pub fn unknown_const(&self, id: Id) -> bool {
        id.tag() == Tag::Local && !self.local_facts
    }

    pub fn intern_key(&self, key: &[u8]) -> Id {
        if let Some(id) = self.snap.lookup_key(key) {
            return id;
        }
        if let Some(i) = self.local.read().find(key) {
            return Id::local(i);
        }
        let mut local = self.local.write();
        if let Some(i) = local.find(key) {
            return Id::local(i);
        }
        if !self.retain_bytes(key.len() as u64 + 96) {
            return Id::UNDEF;
        }
        Id::local(local.insert(key).0)
    }

    /// SERVICE-created encoded terms retain the existing key-byte estimate in eager
    /// and cursor execution. This is not an allocator/RSS bound for AppendVocab.
    pub(super) fn intern_algorithm_key(
        &self,
        key: &[u8],
        ctl: &crate::algo::Control,
    ) -> Result<Id> {
        let mut scratch = crate::algo::memory::Reservation::new(ctl, 0)?;
        if let Ok(i) = self.snap.generation.vocab.find_with_checked(key, |need| {
            scratch.grow_to(
                crate::algo::memory::bytes::<u8>(need)?
                    .checked_mul(2)
                    .ok_or_else(|| self.memory_exceeded(u64::MAX))?,
            )
        })? {
            return Ok(Id::vocab(i));
        }
        if let Some(i) = self
            .snap
            .generation
            .dvocab
            .find(key)
            .filter(|&i| i < self.snap.dvocab_len)
        {
            return Ok(Id::delta(i));
        }
        if let Some(i) = self.local.read().find(key) {
            return Ok(Id::local(i));
        }
        let estimate = u64::try_from(key.len())
            .ok()
            .and_then(|n| n.checked_add(96))
            .ok_or_else(|| self.memory_exceeded(u64::MAX))?;
        let charge = self.charge(estimate)?;
        self.check()?;
        let mut local = self.local.write();
        if let Some(i) = local.find(key) {
            return Ok(Id::local(i));
        }
        let id = Id::local(local.insert(key).0);
        std::mem::forget(charge);
        Ok(id)
    }

    pub fn intern_value(&self, v: &Value) -> Id {
        match v {
            Value::Bool(b) => Id::from_bool(*b),
            Value::Integer(i) => match Id::from_i64(i64::from(*i)) {
                Some(id) => id,
                None => self.intern_term(&v.to_term()),
            },
            // The canonical form of a decimal value parses back to the same value, so
            // its inline id needs neither the text nor the parse of `intern_term`.
            Value::Decimal(d) => match id::pack_decimal(*d) {
                Some(p) => Id::new(Tag::Decimal, p),
                None => self.intern_term(&v.to_term()),
            },
            _ => self.intern_term(&v.to_term()),
        }
    }

    /// Take the local terms and minted blank nodes of `other`, so that this context reads
    /// the ids `other` gave them (a query that reads a view with derived facts).
    pub(crate) fn share_local(&self, other: &Ctx) {
        let local = other.local.read().clone();
        // The copy is this context's own, so it is charged here, and a refusal fails the
        // next check.
        self.retain_bytes(local.bytes() as u64);
        *self.local.write() = local;
        self.next_bnode
            .store(other.next_bnode.load(Ordering::Relaxed), Ordering::Relaxed);
    }

    pub fn fresh_bnode(&self) -> Id {
        Id::bnode(Id::LOCAL_BNODE_BIT | self.next_bnode.fetch_add(1, Ordering::Relaxed))
    }

    /// `BNODE(str)`: one blank node per (solution, string).
    pub fn bnode_for_row(&self, row: Vec<Id>, s: &str) -> Id {
        let mut memo = self.bnode_memo.lock();
        let key = (row, s.to_string());
        if let Some(id) = memo.get(&key) {
            return *id;
        }
        if !self.retain_bytes(key.0.len() as u64 * 8 + s.len() as u64 + 128) {
            return Id::UNDEF;
        }
        let id = self.fresh_bnode();
        memo.insert(key, id);
        id
    }

    /// A term for plan text, which no answer depends on. It is decoded without the
    /// decode envelope in both accounting modes, as eager decoding always was, so that
    /// describing a plan, such as a constant the planner substituted for a variable,
    /// never fails the query it describes.
    pub(super) fn plan_term(&self, id: Id) -> Option<Term> {
        match id.tag() {
            Tag::Local => self.local.read().get(id.payload()).map(id::key_to_term),
            Tag::BNode => Some(Term::BlankNode(bnode_for(id))),
            _ => self.snap.term(id),
        }
    }

    pub fn term(&self, id: Id) -> Option<Term> {
        match id.tag() {
            Tag::Local => {
                let local = self.local.read();
                let key = local.get(id.payload())?;
                self.reserve_decode(key.len()).then(|| id::key_to_term(key))
            }
            Tag::Vocab => self.with_vocab_key(id.payload(), id::key_to_term),
            Tag::Delta => self.snap.generation.dvocab.with(|vocab| {
                vocab
                    .get(id.payload())
                    .and_then(|key| self.reserve_decode(key.len()).then(|| id::key_to_term(key)))
            }),
            Tag::BNode => Some(Term::BlankNode(bnode_for(id))),
            _ => self.snap.term(id),
        }
    }

    /// Decode one retained output cell after admitting its owned payload. Unlike the
    /// public compatibility decoder, this route cannot turn a refusal into UNDEF.
    /// The charge outlives every returned term; no owning value escapes it. Retained
    /// pages and single terms use this route, so a simple literal takes over its
    /// reconstructed vocabulary key's allocation. The streaming writer reuses one
    /// scratch row through [`Ctx::output_term_into`] instead.
    pub(super) fn output_term(&self, id: Id, row: &Charge<'_>) -> Result<Option<Term>> {
        assert!(
            std::ptr::eq(self, row.ctx),
            "output credit belongs to its query"
        );
        self.check_memory_failure()?;
        let term = match id.tag() {
            Tag::Undef => None,
            Tag::Local => {
                // Keys are append-only. Read the estimate, leave the vocabulary lock,
                // then admit before taking the lock again to allocate the term.
                let bytes = self
                    .local
                    .read()
                    .get(id.payload())
                    .map(|key| output_key_bytes(self, key, 0))
                    .transpose()?;
                match bytes {
                    None => None,
                    Some(bytes) => {
                        row.add(bytes)?;
                        self.local.read().get(id.payload()).map(copied_output_key)
                    }
                }
            }
            Tag::Delta => {
                let bytes = self
                    .snap
                    .generation
                    .dvocab
                    .with(|vocab| {
                        vocab
                            .get(id.payload())
                            .map(|key| output_key_bytes(self, key, 0))
                    })
                    .transpose()?;
                match bytes {
                    None => None,
                    Some(bytes) => {
                        row.add(bytes)?;
                        self.snap
                            .generation
                            .dvocab
                            .with(|vocab| vocab.get(id.payload()).map(copied_output_key))
                    }
                }
            }
            Tag::Vocab => self.output_vocab_term(id.payload(), row)?,
            // Inline numbers/calendar values and blank-node labels have bounded
            // formatted text. This covers their strings and construction overlap.
            _ => {
                row.add(256)?;
                self.snap.term(id)
            }
        };
        self.check_memory_failure()?;
        Ok(term)
    }

    /// Decode one streaming writer cell into the writer's reused scratch, for tests.
    #[cfg(test)]
    pub(super) fn output_scratch_term(
        &self,
        id: Id,
        row: &RowCredit<'_>,
        keys: &RowCredit<'_>,
        key: &mut Vec<u8>,
    ) -> Result<Option<Term>> {
        let mut out = Vec::with_capacity(1);
        self.output_term_into(id, row, keys, key, &mut out)?;
        Ok(out.pop().flatten())
    }

    /// Decode one streaming writer cell after admitting its owned payload, pushing it
    /// onto `out`. Unlike the public compatibility decoder, this route cannot turn a
    /// refusal into UNDEF. The row's credit outlives every pushed term, and the
    /// writer drops the row's terms before it decodes the next row. `key` is the
    /// writer's reused reconstruction buffer, and `keys` admits its growth before it
    /// is allocated and holds that credit until the buffer is freed. A decoded term
    /// is built in place in the row rather than returned through the vocabulary's
    /// callback and its nested results, whose copies of each term cost a large
    /// answer about a tenth of its writing time.
    ///
    /// No term escapes the scratch row, so this route copies each key out of the
    /// reused buffer instead of moving the key's allocation into the term as
    /// [`Ctx::output_term`] does. Both physical allocations stay admitted: the
    /// buffer by `keys` and the copy by `row`.
    #[inline]
    pub(super) fn output_term_into(
        &self,
        id: Id,
        row: &RowCredit<'_>,
        keys: &RowCredit<'_>,
        key: &mut Vec<u8>,
        out: &mut Vec<Option<Term>>,
    ) -> Result<()> {
        assert!(
            std::ptr::eq(self, row.charge.ctx) && std::ptr::eq(self, keys.charge.ctx),
            "output credit belongs to its query"
        );
        match id.tag() {
            Tag::Undef => out.push(None),
            Tag::Local => {
                // Keys are append-only. Read the estimate, leave the vocabulary lock,
                // then admit before taking the lock again to allocate the term.
                let bytes = self
                    .local
                    .read()
                    .get(id.payload())
                    .map(|key| output_key_bytes(self, key, 0))
                    .transpose()?;
                if let Some(bytes) = bytes {
                    row.add(bytes)?;
                }
                out.push(
                    bytes.and_then(|_| self.local.read().get(id.payload()).map(copied_output_key)),
                );
            }
            Tag::Delta => {
                let bytes = self
                    .snap
                    .generation
                    .dvocab
                    .with(|vocab| {
                        vocab
                            .get(id.payload())
                            .map(|key| output_key_bytes(self, key, 0))
                    })
                    .transpose()?;
                if let Some(bytes) = bytes {
                    row.add(bytes)?;
                }
                out.push(bytes.and_then(|_| {
                    self.snap
                        .generation
                        .dvocab
                        .with(|vocab| vocab.get(id.payload()).map(copied_output_key))
                }));
            }
            // Reconstruction scratch is query-owned even when the vocabulary is mapped.
            // Twice the requested size covers old and new buffers during reallocation.
            // The writer reuses one buffer for every cell, and its high-water credit
            // admits growth without an account update per cell. A fresh buffer per
            // cell reallocated while rebuilding nearly every key and made large
            // answers about a third slower to write.
            Tag::Vocab => {
                let found = self.snap.generation.vocab.get_with_checked_in(
                    id.payload(),
                    key,
                    |need| keys.reserve((need as u64).saturating_mul(2)),
                    |key| -> Result<()> {
                        out.push(Some(output_key_term(self, key, row)?));
                        Ok(())
                    },
                )?;
                match found {
                    Some(pushed) => pushed?,
                    None => out.push(None),
                }
            }
            // Inline numbers/calendar values and blank-node labels have bounded
            // formatted text. This covers their strings and construction overlap.
            _ => {
                row.add(256)?;
                out.push(self.snap.term(id));
            }
        }
        Ok(())
    }

    /// A vocabulary key and its final simple literal share one physical allocation.
    /// Every capacity change stays on the actual output account, including realloc
    /// overlap. Other key kinds keep the admitted copy path while scratch is alive.
    fn output_vocab_term(&self, id: u64, row: &Charge<'_>) -> Result<Option<Term>> {
        let base = row.bytes();
        let total = |old: usize, new: usize| {
            base.checked_add(old as u64)
                .and_then(|n| n.checked_add(new as u64))
                .ok_or_else(|| self.memory_exceeded(u64::MAX))
        };
        self.snap.generation.vocab.get_owned_with_controlled(
            id,
            |old, new| row.resize(total(old, new)?),
            |capacity| row.resize(total(0, capacity)?),
            |_| self.check(),
            |key| self.output_owned_key(key, row),
        )
    }

    fn output_owned_key(&self, mut key: Vec<u8>, row: &Charge<'_>) -> Result<Term> {
        self.check()?;
        let capacity = key.capacity() as u64;
        #[cfg(test)]
        {
            self.chunk_event("owned-key-pointer", key.as_ptr() as usize);
            self.chunk_event("owned-key-capacity", key.capacity());
        }
        self.check()?;
        let term = if key.first() == Some(&b'"')
            && key.last() == Some(&id::KEY_SEP)
            && std::str::from_utf8(&key[1..key.len() - 1]).is_ok()
        {
            #[cfg(test)]
            {
                use crate::algo::memory::probe;
                probe::at(probe::Point::Allocation("chunk-term", key.len()));
                if probe::fails("chunk-term") {
                    return Err(Error::invalid("injected chunk term allocation failure"));
                }
            }
            // Move lexical bytes over the marker without allocating or shrinking
            // capacity. Both markers' capacity stays paid until the literal drops.
            let end = key.len() - 1;
            key.copy_within(1..end, 0);
            key.truncate(end - 1);
            let value = String::from_utf8(key)
                .map_err(|_| Error::Corrupt("invalid simple literal key".into()))?;
            Term::Literal(oxrdf::Literal::new_simple_literal(value))
        } else {
            let term = self.output_key(&key, row)?;
            drop(key);
            row.resize(
                row.bytes()
                    .checked_sub(capacity)
                    .ok_or_else(|| Error::invalid("missing output vocabulary scratch credit"))?,
            )?;
            term
        };
        if let Err(error) = self.check() {
            drop(term);
            return Err(error);
        }
        Ok(term)
    }

    #[cfg(test)]
    pub(super) fn chunk_event(&self, stage: &'static str, count: usize) {
        let hook = self.chunk_hook.lock().clone();
        if let Some(hook) = hook {
            hook(stage, count);
        }
    }

    /// The sorted bulk decoder already borrows a reconstructed key. Admit its
    /// actual owned representation before copying, under the same real context.
    pub(super) fn output_key(&self, key: &[u8], storage: &Charge<'_>) -> Result<Term> {
        if !std::ptr::eq(self, storage.ctx) {
            return Err(Error::invalid(
                "bulk output belongs to another query account",
            ));
        }
        let decode = || {
            self.check()?;
            storage.add(output_key_bytes(self, key, 0)?)?;
            #[cfg(test)]
            {
                use crate::algo::memory::probe;
                probe::at(probe::Point::Allocation("chunk-term", key.len()));
                if probe::fails("chunk-term") {
                    return Err(Error::invalid("injected chunk term allocation failure"));
                }
            }
            self.check()?;
            let term = id::key_to_term(key);
            #[cfg(test)]
            self.chunk_event("term-copied", key.len());
            if let Err(error) = self.check() {
                drop(term);
                return Err(error);
            }
            Ok(term)
        };
        if key.first() == Some(&b'(') {
            super::depth::with_stack(crate::nesting::MAX_TRIPLE_TERMS, decode)
        } else {
            decode()
        }
    }

    /// Reconstruction scratch is query-owned even when the vocabulary is mapped.
    /// Twice the requested size covers old and new buffers during reallocation.
    pub(super) fn with_cursor_vocab_key<T>(
        &self,
        id: u64,
        f: impl FnMut(&[u8]) -> T,
    ) -> Result<Option<T>> {
        let scratch = self.charge(0)?;
        let mut reserved = 0;
        self.snap.generation.vocab.get_with_checked(
            id,
            |need| {
                let bytes = (need as u64).saturating_mul(2);
                scratch.add(bytes.saturating_sub(reserved))?;
                reserved = bytes;
                Ok(())
            },
            f,
        )
    }

    /// Algorithm-owned key reconstruction never populates the eager value cache.
    pub(super) fn with_algo_key<T>(
        &self,
        id: Id,
        ctl: &crate::algo::Control,
        mut f: impl FnMut(&[u8]) -> Result<T>,
    ) -> Result<Option<T>> {
        use crate::algo::memory::{Reservation, bytes};
        ctl.check()?;
        match id.tag() {
            Tag::Vocab => {
                let mut scratch = Reservation::new(ctl, 0)?;
                self.snap
                    .generation
                    .vocab
                    .get_with_fallible(
                        id.payload(),
                        |need| {
                            let bound = bytes::<u8>(need)?
                                .checked_mul(2)
                                .ok_or_else(|| Error::invalid("algorithm key scratch overflows"))?;
                            scratch.grow_to(bound)
                        },
                        &mut f,
                    )?
                    .transpose()
            }
            Tag::Delta => self.snap.generation.dvocab.with(|v| {
                if id.payload() >= self.snap.dvocab_len {
                    return Ok(None);
                }
                v.get(id.payload()).map(&mut f).transpose()
            }),
            Tag::Local => self.with_local_key(id.payload(), &mut f).transpose(),
            Tag::BNode => {
                let mut key = [b'_'; 9];
                key[1..].copy_from_slice(&id.payload().to_be_bytes());
                f(&key).map(Some)
            }
            _ => Ok(None),
        }
    }

    pub(super) fn with_local_key<T>(&self, id: u64, f: impl FnOnce(&[u8]) -> T) -> Option<T> {
        self.local.read().get(id).map(f)
    }

    /// The term of an id as eager execution decodes it, without charging the scratch of
    /// a vocabulary block or the term, for a caller that drops the term at once.
    pub(super) fn transient_batch_term(&self, id: Id) -> Option<Term> {
        match id.tag() {
            Tag::Local => self.local.read().get(id.payload()).map(id::key_to_term),
            Tag::Undef | Tag::Special => None,
            _ => self.snap.term(id),
        }
    }

    /// The term of an id as a cursor batch resolves it, for a caller that has already
    /// reserved [`Ctx::decoded_bytes`] for the term, as the result writers do for a
    /// row's cells. Batch resolution is independent of a producer's sticky failure, so
    /// previously returned ids remain valid.
    /// It takes no reservation of its own, which cost an inline number one pass through
    /// the memory ledger per cell.
    pub(super) fn reserved_batch_term(&self, id: Id) -> Result<Option<Term>> {
        let decode = |key: &[u8]| Some(id::key_to_term(key));
        Ok(match id.tag() {
            Tag::Local => self.local.read().get(id.payload()).and_then(decode),
            Tag::Vocab => self.with_cursor_vocab_key(id.payload(), decode)?.flatten(),
            Tag::Delta => self
                .snap
                .generation
                .dvocab
                .with(|v| v.get(id.payload()).and_then(decode)),
            Tag::Undef | Tag::Special => None,
            _ => self.snap.term(id),
        })
    }

    /// The term of an id as a cursor batch resolves it. It asks `cover` for the
    /// [`Ctx::decoded_bytes`] of the term before it builds it, so that a result writer can hold one reservation for its row
    /// and decode each key once.
    pub(super) fn covered_batch_term(
        &self,
        id: Id,
        mut cover: impl FnMut(u64) -> Result<()>,
    ) -> Result<Option<Term>> {
        let mut decode = |key: &[u8]| -> Result<Option<Term>> {
            cover((key.len() as u64).saturating_mul(8).saturating_add(128))?;
            Ok(Some(id::key_to_term(key)))
        };
        match id.tag() {
            Tag::Local => self.local.read().get(id.payload()).map_or(Ok(None), decode),
            Tag::Vocab => self
                .with_cursor_vocab_key(id.payload(), decode)?
                .unwrap_or(Ok(None)),
            Tag::Delta => self
                .snap
                .generation
                .dvocab
                .with(|v| v.get(id.payload()).map_or(Ok(None), &mut decode)),
            Tag::Undef | Tag::Special => Ok(None),
            _ => {
                cover(128)?;
                Ok(self.snap.term(id))
            }
        }
    }

    pub(super) fn decoded_bytes(&self, id: Id) -> Result<u64> {
        let size = match id.tag() {
            Tag::Local => self.local.read().get(id.payload()).map_or(0, <[u8]>::len),
            Tag::Vocab => self
                .with_cursor_vocab_key(id.payload(), <[u8]>::len)?
                .unwrap_or(0),
            Tag::Delta => self
                .snap
                .generation
                .dvocab
                .with(|v| v.get(id.payload()).map_or(0, <[u8]>::len)),
            _ => 0,
        };
        Ok((size as u64).saturating_mul(8).saturating_add(128))
    }

    /// The value of a base vocabulary id from the numeric column, when it holds one.
    /// Kept out of line so that [`Ctx::value`] stays small for the ids it decodes.
    #[inline(never)]
    fn column_value(&self, payload: u64) -> Option<Value> {
        self.snap.generation.vocab.numeric()?.get(payload).value()
    }

    /// Decode an ORDER value without latching a recoverable trial refusal.
    /// Inline and numeric-column values keep their existing paths. Dictionary
    /// reconstruction and admitted decoding retain the same owner and charges.
    pub(super) fn cursor_value_checked(&self, id: Id) -> Result<Option<Value>> {
        self.check_memory_failure()?;
        let decode = |key: &[u8]| -> Result<Value> {
            self.reserve_decode_checked(key.len())?;
            Ok(Value::from_key(key))
        };
        match id.tag() {
            Tag::Vocab => {
                if let Some(value) = self.column_value(id.payload()) {
                    return Ok(Some(value));
                }
                self.with_cursor_vocab_key(id.payload(), decode)?
                    .transpose()
            }
            Tag::Delta => self
                .snap
                .generation
                .dvocab
                .with(|v| v.get(id.payload()).map(decode).transpose()),
            Tag::Local => self.with_local_key(id.payload(), decode).transpose(),
            _ => Ok(self.value(id)),
        }
    }

    /// Decode an id into a value, with a per-query cache.
    pub fn value(&self, id: Id) -> Option<Value> {
        match id.tag() {
            Tag::Undef | Tag::Special => None,
            Tag::Int => Some(Value::Integer(id.as_i64().into())),
            Tag::Bool => Some(Value::Bool(id.as_bool())),
            Tag::Double => Some(Value::Double(id.as_f64().into())),
            Tag::Decimal => Some(Value::Decimal(id::unpack_decimal(id.payload()))),
            Tag::DateTime | Tag::Date => id::inline_to_literal(id).map(|l| Value::from_literal(&l)),
            Tag::BNode => Some(Value::BNode(bnode_for(id).as_str().into())),
            _ => {
                // a number of the base vocabulary from its numeric column, without
                // decoding the key (the same value, see `crate::vocab::numeric`)
                if id.tag() == Tag::Vocab
                    && let Some(v) = self.column_value(id.payload())
                {
                    return Some(v);
                }
                if let Some(v) = self.values.get(id) {
                    return Some(v);
                }
                let decode =
                    |key: &[u8]| self.reserve_decode(key.len()).then(|| Value::from_key(key));
                let v = match id.tag() {
                    Tag::Vocab => self.with_vocab_key(id.payload(), Value::from_key),
                    Tag::Delta => self
                        .snap
                        .generation
                        .dvocab
                        .with(|v| v.get(id.payload()).and_then(decode)),
                    Tag::Local => self.local.read().get(id.payload()).and_then(decode),
                    _ => unreachable!("dictionary value tag"),
                }?;
                self.cache_value(id, &v);
                Some(v)
            }
        }
    }

    /// Keep `v` as the decoded value of `id`, when the cache's reservation can grow to
    /// hold it (see [`ValueCache`]). A refused reservation leaves the value uncached and
    /// records no failure, since the cache is optional.
    fn cache_value(&self, id: Id, v: &Value) {
        let cache = &self.values;
        let cap = ValueCache::cap(self.mem_limit);
        let share = cap / VALUE_SHARDS as u64;
        let entry = ValueCache::entry_bytes(v);
        if entry > share {
            return;
        }
        let mut shard = cache.shard(id).write();
        if shard.map.contains_key(&id) {
            return;
        }
        if shard.bytes + entry > share || shard.map.len() > VALUE_SHARD_CAP {
            // The map is freed with its slots, so that the entries' estimate, slots
            // included, leaves the cache with them.
            cache.used.fetch_sub(shard.bytes, Ordering::AcqRel);
            shard.bytes = 0;
            shard.map = FxHashMap::default();
        }
        let used = cache.used.fetch_add(entry, Ordering::AcqRel) + entry;
        if !cache.charge.covers(used)
            && self
                .grow_reservation(&cache.charge, used, cap, false)
                .is_err()
        {
            cache.used.fetch_sub(entry, Ordering::AcqRel);
            return;
        }
        shard.bytes += entry;
        shard.map.insert(id, v.clone());
    }

    /// Grow `reservation` to cover `used` bytes, a step ahead within `cap`. The step is
    /// [`EAGER_STEP`], or a sixty-fourth of a smaller memory budget. When the
    /// budget or the pool cannot give the step, state that the query needs (`exact`)
    /// takes exactly what it needs, while an optional cache declines, so that it never
    /// takes the last room an operator needs. The charge stays with the context until it
    /// drops. A refusal changes nothing.
    /// The step in which state charged a value at a time grows its reservation:
    /// [`EAGER_STEP`], or a sixty-fourth of a smaller memory budget and at least 256
    /// bytes. A small budget takes small steps, so that the slack of the steps never
    /// holds more than a sixty-fourth of it.
    #[inline]
    pub(super) fn reservation_step(&self) -> u64 {
        (self.mem_limit / 64).clamp(256, EAGER_STEP)
    }

    #[cold]
    #[inline(never)]
    pub(crate) fn grow_reservation(
        &self,
        reservation: &StepReservation,
        used: u64,
        cap: u64,
        exact: bool,
    ) -> Result<()> {
        #[cfg(test)]
        VALUE_GROWS.with(|n| n.set(n.get() + 1));
        let mut owner = reservation.grow.lock();
        let reserved = reservation.reserved.load(Ordering::Acquire);
        if used <= reserved {
            return Ok(());
        }
        let step = self.reservation_step();
        let ahead = used.next_multiple_of(step).min(cap).max(used);
        let (charge, admitted) = match self.charge(ahead - reserved) {
            Ok(charge) => (charge, ahead),
            Err(_) if exact && ahead > used => (self.charge(used - reserved)?, used),
            Err(error) => return Err(error),
        };
        let id = charge.disarm();
        debug_assert!(
            owner.is_none() || id.is_none() || *owner == id,
            "one owner of a reservation"
        );
        if owner.is_none() {
            *owner = id;
        }
        reservation.reserved.store(admitted, Ordering::Release);
        Ok(())
    }

    /// Is the id an IRI / blank node / literal (without full decoding where possible)?
    pub fn kind(&self, id: Id) -> TermKind {
        match id.tag() {
            Tag::Undef | Tag::Special => TermKind::None,
            Tag::Bool | Tag::Int | Tag::Double | Tag::Decimal | Tag::DateTime | Tag::Date => {
                TermKind::Literal
            }
            Tag::BNode => TermKind::BNode,
            Tag::Vocab => {
                let v = &self.snap.generation.vocab;
                if v.is_iri(id.payload()) {
                    TermKind::Iri
                } else if v.is_triple(id.payload()) {
                    TermKind::Triple
                } else {
                    TermKind::Literal
                }
            }
            Tag::Delta => self.snap.key(id).map_or(TermKind::None, |k| key_kind(&k)),
            Tag::Local => self
                .local
                .read()
                .get(id.payload())
                .map_or(TermKind::None, key_kind),
        }
    }

    /// The order of two base-vocabulary simple literals (`xsd:string`) by their lexical
    /// forms, compared on their vocabulary keys without decoding either term. A simple
    /// literal's key is `"` lexical 0xFF, and UTF-8 byte order is code point order.
    /// `None` when either id is another kind of term.
    pub(super) fn simple_literal_cmp(&self, a: Id, b: Id) -> Option<std::cmp::Ordering> {
        let v = &self.snap.generation.vocab;
        let (x, y) = (a.payload(), b.payload());
        // the literals come before the triple terms and the IRIs
        if x >= v.first_triple() || y >= v.first_triple() {
            return None;
        }
        let lexical = |k: &[u8]| -> Option<std::ops::Range<usize>> {
            (k.len() >= 2 && k[0] == b'"' && k[k.len() - 1] == id::KEY_SEP).then(|| 1..k.len() - 1)
        };
        // The reconstruction of both keys is reserved before it grows them, within the
        // high-water envelope of transient decoding ([`Ctx::reserve_decode`]). That
        // envelope covers a key of the largest size seen so far on every worker, both
        // keys of a comparison included, is charged once when it grows and is released
        // when the work ends, so a comparison of keys within it takes no lock. A
        // reservation that fails records the failure, and the generic comparison
        // follows.
        let reserve = |need: usize| {
            if self.reserve_decode(need) {
                Ok(())
            } else {
                Err(())
            }
        };
        let order = v.get_with_checked(x, reserve, |kx| {
            let rx = lexical(kx)?;
            v.get_with_checked(y, reserve, |ky| {
                let ry = lexical(ky)?;
                Some(kx[rx.clone()].cmp(&ky[ry]))
            })
            .ok()
            .flatten()
            .flatten()
        });
        order.ok().flatten().flatten()
    }

    /// For a base-vocabulary simple literal, the first base id whose key starts with its
    /// lexical form, which is the first of the strings it is a proper prefix of, or its
    /// own id when it is a prefix of none. `None` for any other id.
    ///
    /// The vocabulary sorts a string's key `"` lexical 0xFF after the keys of its
    /// extensions, so id order is lexical order except for those. For simple literals
    /// `a < b` by id, the lexical form of `a` is less than that of `b` exactly when `a`
    /// is below `b`'s floor, since the keys between `b`'s floor and `b` all start with
    /// `b`'s lexical form.
    pub(super) fn string_floor(&self, id: Id) -> Option<u64> {
        if id.tag() != Tag::Vocab {
            return None;
        }
        let v = &self.snap.generation.vocab;
        let p = id.payload();
        if p >= v.first_triple() {
            return None;
        }
        let key = v.get(p)?;
        if key.len() < 2 || key[0] != b'"' || key[key.len() - 1] != id::KEY_SEP {
            return None;
        }
        let prefix = &key[..key.len() - 1];
        // most strings extend none: the key before them does not start with them
        if p == 0 || !v.get_with(p - 1, |k| k.starts_with(prefix))? {
            return Some(p);
        }
        Some(v.prefix_range(prefix).0)
    }

    pub fn local_len(&self) -> u64 {
        self.local.read().len()
    }
}

/// The term of one decoded vocabulary key, admitted on `row` before it is built.
/// An IRI or literal key is split and its text checked once, and the term is built
/// from the checked parts with the charge [`output_key_bytes`] gives for valid text.
/// Sizing and building it in two passes repeated the separator scan and the UTF-8
/// checks, a measurable share of the time to write a large answer.
#[inline]
fn output_key_term(ctx: &Ctx, key: &[u8], row: &RowCredit<'_>) -> Result<Term> {
    if let Some(text) = id::TextKey::parse(key) {
        let tag = match text {
            id::TextKey::Lang(_, tag) => tag.len() as u64,
            _ => 0,
        };
        row.add(
            (key.len() as u64)
                .checked_add(64 + tag)
                .ok_or_else(|| ctx.memory_exceeded(u64::MAX))?,
        )?;
        return Ok(text.into_term());
    }
    row.add(output_key_bytes(ctx, key, 0)?)?;
    Ok(copied_output_key(key))
}

/// Heap and construction overlap of one decoded vocabulary key, excluding the
/// outer Term slot already covered by its row. Ordinary lexical bytes are charged
/// once, independently of the existing reconstruction envelope's eightfold policy.
/// RDF-star nodes also cover their temporary component Vec and final boxed Triple.
fn copied_output_key(key: &[u8]) -> Term {
    if key.first() == Some(&b'(') {
        super::depth::with_stack(crate::nesting::MAX_TRIPLE_TERMS, || id::key_to_term(key))
    } else {
        id::key_to_term(key)
    }
}

fn output_key_bytes(ctx: &Ctx, key: &[u8], depth: usize) -> Result<u64> {
    if depth == 0 && key.first() == Some(&b'(') {
        return super::depth::with_stack(crate::nesting::MAX_TRIPLE_TERMS, || {
            output_key_bytes_inner(ctx, key, depth)
        });
    }
    output_key_bytes_inner(ctx, key, depth)
}
fn output_key_bytes_inner(ctx: &Ctx, key: &[u8], depth: usize) -> Result<u64> {
    let overflow = || ctx.memory_exceeded(u64::MAX);
    let malformed = || Error::Corrupt("invalid vocabulary triple-term key".into());
    if depth > crate::nesting::MAX_TRIPLE_TERMS {
        return Err(malformed());
    }
    if key.first() != Some(&b'(') {
        // UTF-8 text is copied into String. The fixed allowance covers
        // a datatype/language string, blank-node formatting and string construction.
        // A damaged key's lossy UTF-8 route can expand each byte to three bytes.
        let bytes = (key.len() as u64).checked_mul(3).ok_or_else(overflow)?;
        // Literal separators and blank-node binary IDs are not UTF-8 text.
        let mut overlap = 64u64;
        let valid = match key.first() {
            Some(b'"') => {
                let sep = key
                    .iter()
                    .rposition(|&b| b == id::KEY_SEP)
                    .unwrap_or(key.len());
                let suffix = key.get(sep + 1..).unwrap_or(&[]);
                if suffix.first() == Some(&b'@') {
                    // lang_literal borrows a temporary decoded tag while oxrdf
                    // copies that tag into its final owned String.
                    let tag = &suffix[1..];
                    let copies = if std::str::from_utf8(tag).is_ok() {
                        1
                    } else {
                        3
                    };
                    overlap = overlap
                        .checked_add(
                            (tag.len() as u64)
                                .checked_mul(copies)
                                .ok_or_else(overflow)?,
                        )
                        .ok_or_else(overflow)?;
                }
                std::str::from_utf8(&key[1..sep]).is_ok() && std::str::from_utf8(suffix).is_ok()
            }
            Some(b'_') => true,
            _ => std::str::from_utf8(key).is_ok(),
        };
        return (if valid { key.len() as u64 } else { bytes })
            .checked_add(overlap)
            .ok_or_else(overflow);
    }
    let mut bytes =
        (3 * std::mem::size_of::<Term>()) as u64 + std::mem::size_of::<oxrdf::Triple>() as u64;
    let mut pos = 1usize;
    for _ in 0..3 {
        // Only existing three-component headers are walked, without allocation.
        // Invalid lengths are corruption; they are not relabeled budget failures.
        let mut len = 0u64;
        let mut shift = 0;
        loop {
            let next = *key.get(pos).ok_or_else(malformed)?;
            pos += 1;
            if shift == 63 && next > 1 {
                return Err(malformed());
            }
            len |= u64::from(next & 127)
                .checked_shl(shift)
                .ok_or_else(malformed)?;
            if next & 128 == 0 {
                break;
            }
            shift += 7;
            if shift >= 64 {
                return Err(malformed());
            }
        }
        let len = usize::try_from(len).map_err(|_| malformed())?;
        let end = pos.checked_add(len).ok_or_else(malformed)?;
        let part = key.get(pos..end).ok_or_else(malformed)?;
        bytes = bytes
            .checked_add(output_key_bytes(ctx, part, depth + 1)?)
            .ok_or_else(overflow)?;
        pos = end;
    }
    if pos != key.len() {
        return Err(malformed());
    }
    Ok(bytes)
}

/// Estimated bytes of `rows` rows of `width` ids (a row of a zero-width table still
/// counts as one id).
#[inline]
pub fn table_bytes(rows: usize, width: usize) -> u64 {
    (rows as u64).saturating_mul(width.max(1) as u64 * 8)
}

/// Estimated memory held by a running operator (its input tables), released when the
/// guard drops, also while an error unwinds.
pub struct Charge<'a> {
    ctx: &'a Ctx,
    bytes: std::cell::Cell<u64>,
    owner: std::cell::Cell<Option<u64>>,
}
impl Charge<'_> {
    pub(super) fn bytes(&self) -> u64 {
        self.bytes.get()
    }

    pub(super) fn belongs_to(&self, ctx: &Ctx) -> bool {
        std::ptr::eq(self.ctx, ctx)
    }

    /// Reconcile retained physical storage without moving it to a ledger account.
    /// Callers admit growth before allocating and release a shrink after dropping it.
    pub(super) fn resize(&self, bytes: u64) -> Result<()> {
        let current = self.bytes.get();
        if bytes > current {
            self.add(bytes - current)?;
        } else if bytes < current {
            self.ctx.release_live(self.owner.get(), current - bytes);
            self.bytes.set(bytes);
        }
        Ok(())
    }

    /// Consume another live charge on this exact context without a release interval.
    /// Every check precedes mutation, including for an empty but owned source.
    pub(super) fn adopt_charge(&self, source: &mut Option<Charge<'_>>) -> Result<()> {
        let part = source
            .as_ref()
            .ok_or_else(|| Error::invalid("missing materialized input credit"))?;
        if !std::ptr::eq(self.ctx, part.ctx)
            || matches!((self.owner.get(), part.owner.get()), (Some(a), Some(b)) if a != b)
            || (self.bytes.get() != 0 && self.owner.get().is_none())
            || (part.bytes.get() != 0 && part.owner.get().is_none())
        {
            return Err(Error::invalid(
                "materialized input belongs to another query account",
            ));
        }
        let bytes = self
            .bytes
            .get()
            .checked_add(part.bytes.get())
            .ok_or_else(|| self.ctx.memory_exceeded(u64::MAX))?;
        let owner = self.owner.get().or(part.owner.get());
        let part = source.take().expect("checked live input credit");
        part.disarm();
        self.owner.set(owner);
        self.bytes.set(bytes);
        Ok(())
    }

    pub fn add(&self, bytes: u64) -> Result<()> {
        if bytes == 0 {
            return Ok(());
        }
        let owned = self
            .bytes
            .get()
            .checked_add(bytes)
            .ok_or_else(|| self.ctx.memory_exceeded(u64::MAX))?;
        if self.owner.get().is_none_or(|id| id == PRIMARY)
            && let Some(admitted) = self.ctx.fast_admit(bytes, true)
        {
            admitted?;
            self.owner.set(Some(PRIMARY));
            self.bytes.set(owned);
            return Ok(());
        }
        let ledger = self.ctx.memory();
        let owner = match self.owner.get() {
            Some(id) => ledger.owner_by_id(id),
            None => ledger.owner(self.ctx.grant.as_ref())?,
        };
        ledger.reserve(&owner, self.ctx.mem_limit, bytes, false)?;
        self.owner.set(Some(owner.id));
        self.bytes.set(owned);
        Ok(())
    }
    fn disarm(&self) -> Option<u64> {
        self.bytes.set(0);
        self.owner.take()
    }
    /// Transfer this already admitted physical allocation to an immutable output.
    /// The receiving context is the actual account owner, not a domain token.
    pub(super) fn into_output(self, ctx: &Arc<Ctx>) -> OutputCredit {
        assert!(
            std::ptr::eq(self.ctx, Arc::as_ptr(ctx)),
            "output account owner"
        );
        let bytes = self.bytes.get();
        let owner = self.disarm();
        OutputCredit {
            ctx: ctx.clone(),
            owner,
            bytes,
            exact_receiving: false,
        }
    }

    /// Move query-only input ownership into the returned backing without a release
    /// interval. Existing nonalgorithm kernels retain their estimate policy.
    pub(super) fn output_lease(&self, bytes: u64) -> Result<QueryMemoryLease> {
        if bytes > self.bytes.get() {
            self.add(bytes - self.bytes.get())?;
        } else {
            self.ctx
                .release_live(self.owner.get(), self.bytes.get() - bytes);
            self.bytes.set(bytes);
        }
        let ledger = self.ctx.memory();
        let owner = self
            .owner
            .get()
            .map(|id| ledger.owner_by_id(id))
            .map_or_else(|| ledger.owner(self.ctx.grant.as_ref()), Ok)?;
        self.disarm();
        Ok(QueryMemoryLease {
            ledger: ledger.clone(),
            owner,
            bytes,
            service: None,
        })
    }
    pub(super) fn adopt(&self, slot: &mut Option<QueryMemoryLease>) -> Result<()> {
        let lease = slot.as_ref().expect("admitted input");
        if lease.service.is_some()
            || !Arc::ptr_eq(self.ctx.memory(), &lease.ledger)
            || self.owner.get().is_some_and(|id| id != lease.owner.id)
        {
            return Err(Error::invalid(
                "algorithm input belongs to another query owner",
            ));
        }
        let bytes = self
            .bytes
            .get()
            .checked_add(lease.bytes)
            .ok_or_else(|| self.ctx.memory_exceeded(u64::MAX))?;
        self.owner.set(Some(lease.owner.id));
        self.bytes.set(bytes);
        slot.take().expect("admitted input").bytes = 0;
        Ok(())
    }
}
/// Credit of a reused output row. The writer frees each row's terms before it decodes
/// the next, so live output is at most the largest row so far. The credit keeps that
/// high-water mark admitted and adds to the query's account only when a row exceeds it,
/// which leaves one shared account update per growth instead of one per cell.
pub(super) struct RowCredit<'a> {
    charge: Charge<'a>,
    used: std::cell::Cell<u64>,
}
impl<'a> RowCredit<'a> {
    pub(super) fn new(ctx: &'a Ctx) -> Self {
        Self {
            charge: Charge {
                ctx,
                bytes: std::cell::Cell::new(0),
                owner: std::cell::Cell::new(None),
            },
            used: std::cell::Cell::new(0),
        }
    }
    /// Admit `bytes` more for the current row before they are allocated.
    pub(super) fn add(&self, bytes: u64) -> Result<()> {
        let used = self
            .used
            .get()
            .checked_add(bytes)
            .ok_or_else(|| self.charge.ctx.memory_exceeded(u64::MAX))?;
        let held = self.charge.bytes.get();
        if used > held {
            self.charge.add(used - held)?;
        }
        self.used.set(used);
        Ok(())
    }
    /// Hold at least `bytes` in total, for scratch whose size replaces rather than
    /// adds to what was held before.
    pub(super) fn reserve(&self, bytes: u64) -> Result<()> {
        let held = self.charge.bytes.get();
        if bytes > held {
            self.charge.add(bytes - held)?;
        }
        Ok(())
    }
    /// Start a new row. The caller has already dropped every term of the last one.
    pub(super) fn reset(&self) {
        self.used.set(0);
    }
}

impl Drop for Charge<'_> {
    fn drop(&mut self) {
        self.ctx.release_live(self.owner.get(), self.bytes.get());
    }
}

/// Final physical output credit. Retaining the real context keeps its original
/// fast account or promoted ledger alive. Moving a paid charge here never returns
/// and reacquires its credit and never forces fast eager queries onto the ledger.
pub(super) struct OutputCredit {
    ctx: Arc<Ctx>,
    owner: Option<u64>,
    bytes: u64,
    exact_receiving: bool,
}
impl OutputCredit {
    pub(super) fn receiving(ctx: &Arc<Ctx>, bytes: u64) -> Result<Self> {
        // Ordinary parentless eager outputs keep their original lock-free account.
        if ctx.source_preparation.is_none() {
            return Ok(ctx.charge(bytes)?.into_output(ctx));
        }
        let ledger = ctx.memory();
        let owner = ledger.owner(ctx.grant.as_ref())?;
        ledger.reserve_receiving(&owner, ctx.mem_limit, bytes)?;
        Ok(Self {
            ctx: ctx.clone(),
            owner: Some(owner.id),
            bytes,
            exact_receiving: true,
        })
    }
    /// Move a worker's already paid payload contribution without releasing it.
    /// Metadata buffers remain with their own original guard until they are freed.
    pub(super) fn absorb_charge(&mut self, slot: &mut Option<Charge<'_>>) -> Result<()> {
        let part = slot
            .as_ref()
            .ok_or_else(|| Error::invalid("missing worker credit"))?;
        if self.exact_receiving
            || !std::ptr::eq(Arc::as_ptr(&self.ctx), part.ctx)
            || matches!((self.owner, part.owner.get()), (Some(a), Some(b)) if a != b)
            || (part.bytes.get() != 0 && part.owner.get().is_none())
        {
            return Err(Error::invalid(
                "worker payload belongs to another query account",
            ));
        }
        let bytes = self
            .bytes
            .checked_add(part.bytes.get())
            .ok_or_else(|| self.ctx.memory_exceeded(u64::MAX))?;
        let part = slot.take().expect("validated worker credit");
        let owner = part.disarm();
        self.owner = self.owner.or(owner);
        self.bytes = bytes;
        Ok(())
    }

    pub(super) fn belongs_to(&self, ctx: &Ctx) -> bool {
        std::ptr::eq(Arc::as_ptr(&self.ctx), ctx)
    }
    pub(super) fn bytes(&self) -> u64 {
        self.bytes
    }
    pub(super) fn check(&self) -> Result<()> {
        self.ctx.check()
    }
    pub(super) fn resize(&mut self, bytes: u64) -> Result<()> {
        if self.exact_receiving {
            let ledger = self.ctx.memory();
            let id = self.owner.expect("receiving owner");
            if bytes > self.bytes {
                ledger.reserve_receiving(
                    &ledger.owner_by_id(id),
                    self.ctx.mem_limit,
                    bytes - self.bytes,
                )?;
            } else if bytes < self.bytes {
                ledger.release_receiving(id, self.bytes - bytes);
            }
            self.bytes = bytes;
            return Ok(());
        }
        let held = Charge {
            ctx: &self.ctx,
            bytes: std::cell::Cell::new(self.bytes),
            owner: std::cell::Cell::new(self.owner),
        };
        let result = held.resize(bytes);
        let bytes = held.bytes();
        let owner = held.disarm();
        drop(held);
        self.bytes = bytes;
        self.owner = owner;
        result
    }
}
impl Drop for OutputCredit {
    fn drop(&mut self) {
        if self.exact_receiving {
            self.ctx
                .memory()
                .release_receiving(self.owner.expect("receiving owner"), self.bytes);
        } else {
            self.ctx.release_live(self.owner, self.bytes);
        }
    }
}

/// Weak retained state never owns the context, grant or ledger.
pub(super) struct RetainedCharge {
    owner: Weak<Ctx>,
    id: Option<u64>,
    bytes: u64,
}
impl RetainedCharge {
    pub(super) fn owner(&self) -> Option<Arc<Ctx>> {
        self.owner.upgrade()
    }
    pub(super) fn bytes(&self) -> u64 {
        self.bytes
    }

    /// Transfer a previously validated live physical owner without returning its
    /// bytes. The caller must validate the actual context before consuming it.
    pub(super) fn into_output(mut self, ctx: &Arc<Ctx>) -> OutputCredit {
        assert!(
            std::ptr::eq(self.owner.as_ptr(), Arc::as_ptr(ctx)),
            "retained output account owner"
        );
        let bytes = std::mem::take(&mut self.bytes);
        let owner = self.id.take();
        self.owner = Weak::new();
        OutputCredit {
            ctx: ctx.clone(),
            owner,
            bytes,
            exact_receiving: false,
        }
    }
    pub(super) fn resize(&mut self, bytes: u64) -> Result<()> {
        // A size that does not change takes neither the owner nor an account. A sort key
        // column of numbers resized its charge on every row this way.
        if bytes == self.bytes {
            return Ok(());
        }
        let ctx = self
            .owner
            .upgrade()
            .ok_or_else(|| Error::invalid("cursor context was released"))?;
        if bytes > self.bytes {
            // A charge on the same owner, which an eager context's lock-free account
            // takes without the ledger.
            let charge = Charge {
                ctx: &ctx,
                bytes: std::cell::Cell::new(0),
                owner: std::cell::Cell::new(self.id),
            };
            charge.add(bytes - self.bytes)?;
            self.id = charge.disarm();
        } else {
            ctx.release_live(self.id, self.bytes - bytes);
        }
        self.bytes = bytes;
        if bytes == 0 {
            self.id = None;
        }
        Ok(())
    }

    /// Grow the charge to cover `bytes`, `step` bytes ahead when the budget and the pool
    /// allow it and exactly otherwise, so that state admitted one value at a time takes a
    /// charge once per step rather than once per value. The charge never falls below what
    /// it covers and holds at most `step` bytes more. A refusal changes nothing.
    pub(super) fn cover(&mut self, bytes: u64, step: u64) -> Result<()> {
        if bytes <= self.bytes {
            return Ok(());
        }
        match self.resize(bytes.saturating_add(step)) {
            Err(Error::BudgetExceeded(_)) if step > 0 => self.resize(bytes),
            result => result,
        }
    }

    /// An empty charge on this charge's owner in `ctx`, the context the charge belongs
    /// to, which a parallel piece of work grows on its own thread and
    /// [`RetainedCharge::absorb`] takes back.
    pub(super) fn part<'a>(&self, ctx: &'a Ctx) -> Charge<'a> {
        debug_assert!(
            std::ptr::eq(self.owner.as_ptr(), ctx),
            "the charge's context"
        );
        Charge {
            ctx,
            bytes: std::cell::Cell::new(0),
            owner: std::cell::Cell::new(self.id),
        }
    }

    /// Keep the bytes of `part`, a charge made by [`RetainedCharge::part`], with this
    /// charge, without releasing and charging them again.
    pub(super) fn absorb(&mut self, part: Charge<'_>) {
        let bytes = part.bytes.get();
        let id = part.disarm();
        if bytes == 0 {
            return;
        }
        debug_assert!(self.id.is_none() || self.id == id, "one owner of a charge");
        if self.id.is_none() {
            self.id = id;
        }
        self.bytes = self.bytes.saturating_add(bytes);
    }
}
impl Drop for RetainedCharge {
    fn drop(&mut self) {
        if let Some(ctx) = self.owner.upgrade() {
            ctx.release_live(self.id, self.bytes);
        }
    }
}

/// Cursor ownership can adopt an admitted algorithm lease without release/recharge.
pub(super) struct OwnedCharge {
    ctx: Arc<Ctx>,
    lease: QueryMemoryLease,
}
impl OwnedCharge {
    pub(super) fn bytes(&self) -> u64 {
        self.lease.bytes
    }
    pub(super) fn new(ctx: &Arc<Ctx>, bytes: u64) -> Result<Self> {
        let ledger = ctx.memory();
        let owner = ledger.owner(ctx.grant.as_ref())?;
        ledger.reserve(&owner, ctx.mem_limit, bytes, false)?;
        Ok(Self {
            ctx: ctx.clone(),
            lease: QueryMemoryLease {
                ledger: ledger.clone(),
                owner,
                bytes,
                service: None,
            },
        })
    }
    pub(super) fn adopt(ctx: &Arc<Ctx>, slot: &mut Option<QueryMemoryLease>) -> Result<Self> {
        let lease = slot.as_ref().expect("admitted output");
        if !Arc::ptr_eq(ctx.memory(), &lease.ledger) {
            return Err(Error::invalid("algorithm output belongs to another query"));
        }
        Ok(Self {
            ctx: ctx.clone(),
            lease: slot.take().expect("admitted output"),
        })
    }
    pub(super) fn resize(&mut self, bytes: u64) -> Result<()> {
        self.lease.resize(self.ctx.mem_limit, bytes)
    }
    /// Adopt the actual predecoded payload guard; the existing cursor ledger is
    /// required and checked before the source is consumed.
    pub(super) fn adopt_charge(&mut self, source: &mut Option<Charge<'_>>) -> Result<()> {
        let held = source
            .as_ref()
            .ok_or_else(|| Error::invalid("missing graph payload credit"))?;
        if !held.belongs_to(&self.ctx)
            || held.owner.get().is_some_and(|id| id != self.lease.owner.id)
            || (held.bytes() != 0 && held.owner.get().is_none())
        {
            return Err(Error::invalid(
                "graph payload belongs to another query account",
            ));
        }
        let bytes = self
            .lease
            .bytes
            .checked_add(held.bytes())
            .ok_or_else(|| self.ctx.memory_exceeded(u64::MAX))?;
        let held = source.take().expect("validated graph payload");
        held.disarm();
        self.lease.bytes = bytes;
        Ok(())
    }
    pub(super) fn into_parts(self) -> (Arc<Ctx>, QueryMemoryLease) {
        (self.ctx, self.lease)
    }
}

fn key_kind(k: &[u8]) -> TermKind {
    if id::is_key_iri(k) {
        TermKind::Iri
    } else if id::is_key_triple(k) {
        TermKind::Triple
    } else {
        TermKind::Literal
    }
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum TermKind {
    None,
    Iri,
    BNode,
    Literal,
    /// RDF 1.2 triple term
    Triple,
}

#[cfg(test)]
mod charge_tests {
    use super::*;
    use crate::store::{Store, StoreOptions};

    #[test]
    fn eager_charge_transfer_keeps_the_actual_fast_credit_at_the_limit() {
        let store = Store::in_memory(StoreOptions::default());
        let mut ctx = Ctx::new(store.snapshot());
        ctx.mem_limit = 12_288;
        let output = ctx.charge(4096).unwrap();
        let mut input = Some(ctx.charge(8192).unwrap());
        assert_eq!(ctx.live_bytes(), ctx.mem_limit);
        output.adopt_charge(&mut input).unwrap();
        assert!(input.is_none());
        assert_eq!(output.bytes.get(), 12_288);
        assert_eq!(ctx.live_bytes(), ctx.mem_limit);
        assert!(ctx.memory.get().is_none(), "transfer must not promote");
        output.resize(4096).unwrap();
        assert_eq!(ctx.live_bytes(), 4096);
        output.resize(8192).unwrap();
        assert_eq!(ctx.live_bytes(), 8192);
        drop(output);
        assert_eq!(ctx.live_bytes(), 0);
        assert!(ctx.memory.get().is_none());
    }

    #[test]
    fn eager_charge_transfer_rejects_a_different_context_on_the_same_grant() {
        let store = Store::in_memory(StoreOptions::default());
        let pool = super::super::pool::QueryPool::new(32 << 20);
        let grant = Arc::new(pool.empty_grant());
        let one = pooled(&store, &grant);
        let two = pooled(&store, &grant);
        let output = one.charge(4096).unwrap();
        let mut input = Some(two.charge(8192).unwrap());
        let used = grant.used();
        assert!(output.adopt_charge(&mut input).is_err());
        assert_eq!(output.bytes.get(), 4096);
        assert_eq!(input.as_ref().unwrap().bytes.get(), 8192);
        assert_eq!(one.live_bytes(), 4096);
        assert_eq!(two.live_bytes(), 8192);
        assert_eq!(grant.used(), used);
        assert!(one.memory.get().is_none() && two.memory.get().is_none());
    }

    #[test]
    fn eager_charge_transfer_refuses_empty_foreign_accounts_and_overflow_unchanged() {
        let store = Store::in_memory(StoreOptions::default());
        let ctx = Ctx::new(store.snapshot());
        let output = ctx.charge(4096).unwrap();
        // These states exercise validation only; no fictitious credit is released.
        let foreign = Charge {
            ctx: &ctx,
            bytes: std::cell::Cell::new(0),
            owner: std::cell::Cell::new(Some(PRIMARY + 1)),
        };
        let mut input = Some(foreign);
        let refused = output.adopt_charge(&mut input);
        let foreign_owner = input.as_ref().unwrap().owner.get();
        input.as_ref().unwrap().disarm();
        assert!(refused.is_err());
        assert_eq!(foreign_owner, Some(PRIMARY + 1));
        assert_eq!(output.bytes.get(), 4096);
        let part = input.as_ref().unwrap();
        part.owner.set(Some(PRIMARY));
        part.bytes.set(u64::MAX);
        let refused = output.adopt_charge(&mut input);
        let source_bytes = input.as_ref().unwrap().bytes.get();
        input.as_ref().unwrap().disarm();
        assert!(refused.is_err());
        assert_eq!(source_bytes, u64::MAX);
        assert_eq!(output.bytes.get(), 4096);
        assert_eq!(ctx.live_bytes(), 4096);
        assert!(ctx.memory.get().is_none());
    }

    #[test]
    fn eager_charge_resize_refusal_preserves_the_admitted_storage() {
        let store = Store::in_memory(StoreOptions::default());
        let mut ctx = Ctx::new(store.snapshot());
        ctx.mem_limit = 8192;
        let charge = ctx.charge(4096).unwrap();
        assert!(charge.resize(8193).is_err());
        assert_eq!(charge.bytes.get(), 4096);
        assert_eq!(ctx.live_bytes(), 4096);
        charge.resize(0).unwrap();
        assert_eq!(ctx.live_bytes(), 0);
        let mut empty = Some(ctx.charge(0).unwrap());
        charge.adopt_charge(&mut empty).unwrap();
        assert!(empty.is_none());
        assert!(ctx.memory.get().is_none());
    }

    /// The one-pass output decode builds the same term as `key_to_term` and admits
    /// the same bytes as `output_key_bytes`, for valid, damaged and special keys.
    #[test]
    fn one_pass_output_keys_match_the_two_pass_term_and_charge() {
        let store = Store::in_memory(StoreOptions::default());
        let ctx = Ctx::new(store.snapshot());
        let sep = id::KEY_SEP;
        let mut keys: Vec<Vec<u8>> = vec![
            b"".to_vec(),
            b"<".to_vec(),
            b"<http://example.org/a".to_vec(),
            "<http://example.org/\u{e9}".as_bytes().to_vec(),
            b"<http://example.org/\xff\xfe".to_vec(),
            b"\"".to_vec(),
            b"\"plain".to_vec(),
            b"x-bare".to_vec(),
            b"\xc3".to_vec(),
            b"_short".to_vec(),
        ];
        for lex in [&b"lex"[..], b"", "caf\u{e9}".as_bytes(), b"bad\xc3("] {
            let with = |suffix: &[u8]| {
                let mut key = vec![b'"'];
                key.extend_from_slice(lex);
                key.push(sep);
                key.extend_from_slice(suffix);
                key
            };
            keys.push(with(b""));
            keys.push(with(b"@en"));
            keys.push(with(b"@en--ltr"));
            keys.push(with(b"@ar--rtl"));
            keys.push(with(b"@e\xffn"));
            keys.push(with(b"^http://www.w3.org/2001/XMLSchema#integer"));
            keys.push(with(b"^http://example.org/\xc3"));
            keys.push(with("\u{e9}http://example.org/t".as_bytes()));
            keys.push(with(b"\xe9x"));
        }
        let mut bnode = vec![b'_'];
        bnode.extend_from_slice(&7u64.to_be_bytes());
        keys.push(bnode);
        let mut triple = vec![b'('];
        for part in [
            &b"<http://example.org/s"[..],
            b"<http://example.org/p",
            b"\"o",
        ] {
            triple.push(part.len() as u8);
            triple.extend_from_slice(part);
        }
        keys.push(triple);
        // The one-pass route takes every valid IRI and literal form, so that a fallback
        // cannot hide a disabled fast path, and leaves the others to the two-pass route.
        let one_pass = |key: &[u8]| id::TextKey::parse(key).is_some();
        let lit = |suffix: &[u8]| [&b"\"lex"[..], &[sep], suffix].concat();
        for key in [
            &b""[..],
            b"<http://example.org/a",
            "<http://example.org/\u{e9}".as_bytes(),
            b"x-bare",
            b"\"plain",
            &lit(b""),
            &lit(b"@en"),
            &lit(b"@ar--rtl"),
            &lit(b"^http://www.w3.org/2001/XMLSchema#integer"),
        ] {
            assert!(one_pass(key), "{key:?}");
        }
        for key in [
            &b"<http://example.org/\xff"[..],
            b"\xc3",
            b"_short",
            &keys[keys.len() - 2],
            &keys[keys.len() - 1],
            &lit(b"@e\xffn"),
            &lit(b"^http://example.org/\xc3"),
            &lit("\u{e9}http://example.org/t".as_bytes()),
            &[&b"\"bad\xc3("[..], &[sep]].concat(),
        ] {
            assert!(!one_pass(key), "{key:?}");
        }
        for key in &keys {
            let row = RowCredit::new(&ctx);
            let term = output_key_term(&ctx, key, &row).unwrap();
            assert_eq!(term, id::key_to_term(key), "{key:?}");
            assert_eq!(
                row.used.get(),
                output_key_bytes(&ctx, key, 0).unwrap(),
                "{key:?}"
            );
        }
    }

    /// A decimal value gets the id of its canonical literal, inline or query-local.
    #[test]
    fn decimal_values_intern_as_their_canonical_literal() {
        use oxsdatatypes::Decimal;
        let store = Store::in_memory(StoreOptions::default());
        let context = Ctx::new(store.snapshot());
        let mut bits: Vec<i128> = vec![
            0,
            1,
            -1,
            1_950_000_000_000_000_000_000,
            -5_000_000_000_000_000_000,
            123_456_789,
            i128::MAX,
            i128::MIN,
            1 << 100,
        ];
        for scale in 0..=38 {
            let p = 10i128.pow(scale);
            bits.extend([p, -p, 7 * (p / 10)]);
            bits.extend(
                [1i128 << 55, (1 << 55) - 1, -(1 << 55), -(1 << 55) - 1]
                    .iter()
                    .filter_map(|m| m.checked_mul(p / 10)),
            );
        }
        let mut inline = 0;
        for v in bits {
            let value = Value::Decimal(Decimal::from_be_bytes(v.to_be_bytes()));
            let id = context.intern_value(&value);
            assert_eq!(id, context.intern_term(&value.to_term()), "{value:?}");
            inline += usize::from(id.tag() == Tag::Decimal);
            assert_eq!(context.term(id), Some(value.to_term()));
        }
        assert!(inline > 20, "most of the values are inline: {inline}");
    }

    /// The executors read the accounting mode in at most this many places outside tests.
    /// Stage 4 of item 453 lowers it as each `cursor_*` twin merges, to the planning and
    /// routing sites that stage 6 removes, so that new twins do not come back.
    const MODE_CHECKS: usize = 6;

    /// Counts the calls of `streams()` in the non-test sources of the SPARQL engine,
    /// and fails when they exceed [`MODE_CHECKS`] (decision D9 of item 453).
    #[test]
    fn the_accounting_mode_is_read_in_a_bounded_number_of_places() {
        fn sources(dir: &std::path::Path, out: &mut Vec<std::path::PathBuf>) {
            for entry in std::fs::read_dir(dir).unwrap() {
                let path = entry.unwrap().path();
                if path.is_dir() {
                    sources(&path, out);
                } else if path.extension().is_some_and(|e| e == "rs") {
                    out.push(path);
                }
            }
        }
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/sparql");
        let mut files = Vec::new();
        sources(&root, &mut files);
        let mut sites = Vec::new();
        for file in files {
            let name = file.file_name().unwrap().to_string_lossy().into_owned();
            if name == "tests.rs" || name.ends_with("_tests.rs") {
                continue;
            }
            let text = std::fs::read_to_string(&file).unwrap();
            // a file's unit tests follow its code
            let code = text
                .find("\n#[cfg(test)]\nmod ")
                .map_or(text.as_str(), |end| &text[..end]);
            for (line, content) in code.lines().enumerate() {
                if content.contains(".streams()") {
                    sites.push(format!("{name}:{}", line + 1));
                }
            }
        }
        assert!(
            sites.len() <= MODE_CHECKS,
            "{} reads of the accounting mode, more than {MODE_CHECKS}: {sites:?}",
            sites.len()
        );
    }

    /// A program compiled before the context has an owner, as in eager planning, is
    /// uncharged, so the first attachment drops it and the next use charges it.
    #[test]
    fn regex_programs_compiled_before_the_owner_are_compiled_again_with_charges() {
        let store = Store::in_memory(StoreOptions::default());
        let context = Arc::new(Ctx::new(store.snapshot()));
        let before = context.live_bytes();
        let planned = context.regex("^ab+c$", "").unwrap().unwrap();
        assert_eq!(context.live_bytes(), before, "no owner, no retained charge");
        context.attach_owner();
        let executed = context.regex("^ab+c$", "").unwrap().unwrap();
        assert!(!Arc::ptr_eq(&planned, &executed));
        assert!(context.live_bytes() > before);
        let charged = context.live_bytes();
        // a second attachment keeps the charged programs
        context.attach_owner();
        let again = context.regex("^ab+c$", "").unwrap().unwrap();
        assert!(Arc::ptr_eq(&executed, &again));
        assert_eq!(context.live_bytes(), charged);
        drop((planned, executed, again));
        context.release_work();
        assert_eq!(context.live_bytes(), before);
    }

    #[test]
    fn retained_cursor_charges_release_and_do_not_keep_context_alive() {
        let store = Store::in_memory(StoreOptions::default());
        let mut context = Ctx::new(store.snapshot());
        context.enable_cursor_accounting().unwrap();
        let context = Arc::new(context);
        context.attach_owner();
        let before = context.memory().live.load(Ordering::Relaxed);
        let mut retained = context.retained_charge(4096).unwrap().unwrap();
        assert_eq!(context.memory().live.load(Ordering::Relaxed), before + 4096);
        retained.resize(2048).unwrap();
        assert_eq!(context.memory().live.load(Ordering::Relaxed), before + 2048);
        drop(retained);
        assert_eq!(context.memory().live.load(Ordering::Relaxed), before);
        let retained = context.retained_charge(4096).unwrap();
        let weak = Arc::downgrade(&context);
        drop(context);
        assert!(weak.upgrade().is_none());
        drop(retained);
    }

    /// A retained charge resized to its own size touches neither its owner nor an
    /// account, and one that covers values a step at a time grows a step ahead, takes the
    /// exact bytes when the step does not fit, and changes nothing when even those do not.
    #[test]
    fn retained_charges_cover_values_in_steps() {
        let store = Store::in_memory(StoreOptions::default());
        let mut context = Ctx::new(store.snapshot());
        context.mem_limit = 1 << 20;
        let context = Arc::new(context);
        context.attach_owner();
        let step = context.reservation_step();
        assert_eq!(step, (1 << 20) / 64);
        let base = context.live_bytes();
        let mut retained = context.retained_charge(100).unwrap().unwrap();
        retained.cover(60, step).unwrap();
        assert_eq!(retained.bytes(), 100);
        retained.cover(200, step).unwrap();
        assert_eq!(retained.bytes(), 200 + step);
        assert_eq!(context.live_bytes(), base + 200 + step);
        retained.cover(200 + step, step).unwrap();
        assert_eq!(retained.bytes(), 200 + step);
        // without room for the step, the exact bytes
        let room = context.memory_remaining();
        let storage = context.charge(room - 1000).unwrap();
        let held = retained.bytes();
        retained.cover(held + 500, step).unwrap();
        assert_eq!(retained.bytes(), held + 500);
        // without room for those, nothing changes
        assert!(matches!(
            retained.cover(held + 2000, step),
            Err(Error::BudgetExceeded(_))
        ));
        assert_eq!(retained.bytes(), held + 500);
        drop(storage);
        // a part grown on another charge joins it without a second charge
        let part = retained.part(&context);
        part.add(300).unwrap();
        let live = context.live_bytes();
        retained.absorb(part);
        assert_eq!(retained.bytes(), held + 800);
        assert_eq!(context.live_bytes(), live);
        // the same size needs no owner, even one that is gone
        let bytes = retained.bytes();
        let weak = Arc::downgrade(&context);
        drop(context);
        assert!(weak.upgrade().is_none());
        retained.resize(bytes).unwrap();
        drop(retained);
    }

    #[test]
    fn rejected_finite_and_unlimited_overflow_leave_accounting_reusable() {
        for limit in [100, u64::MAX] {
            let store = Store::in_memory(StoreOptions::default());
            let mut ctx = Ctx::new(store.snapshot());
            ctx.mem_limit = limit;
            let charge = ctx.charge(80).unwrap();
            assert!(charge.add(u64::MAX).is_err());
            assert_eq!(ctx.memory().live.load(Ordering::Relaxed), 80);
            assert_eq!(charge.bytes.get(), 80);
            charge.add(20).unwrap();
            drop(charge);
            assert_eq!(ctx.memory().live.load(Ordering::Relaxed), 0);
            // A different guard can overflow the global count while its own count
            // is representable. Failure must not corrupt either guard's release.
            let first = ctx.charge(limit).unwrap();
            assert!(ctx.charge(1).is_err());
            assert_eq!(ctx.memory().live.load(Ordering::Relaxed), limit);
            drop(first);
            assert_eq!(ctx.memory().live.load(Ordering::Relaxed), 0);
            drop(ctx.charge(1).unwrap());
            assert_eq!(ctx.memory().live.load(Ordering::Relaxed), 0);
        }
    }

    #[test]
    fn concurrent_rejections_do_not_wrap_or_steal_live_charges() {
        for limit in [100, u64::MAX] {
            let store = Store::in_memory(StoreOptions::default());
            let mut ctx = Ctx::new(store.snapshot());
            ctx.mem_limit = limit;
            let owner = ctx.charge(limit - 10).unwrap();
            let barrier = std::sync::Barrier::new(8);
            std::thread::scope(|scope| {
                for _ in 0..8 {
                    let ctx = &ctx;
                    let barrier = &barrier;
                    scope.spawn(move || {
                        barrier.wait();
                        for _ in 0..500 {
                            assert!(ctx.charge(11).is_err());
                            assert!(ctx.charge(u64::MAX).is_err());
                            drop(ctx.charge(1).unwrap());
                        }
                    });
                }
            });
            assert_eq!(ctx.memory().live.load(Ordering::Relaxed), limit - 10);
            drop(owner);
            assert_eq!(ctx.memory().live.load(Ordering::Relaxed), 0);
        }
    }

    const MIB: u64 = 1 << 20;

    fn pooled(store: &Store, grant: &Arc<super::super::pool::PoolGrant>) -> Ctx {
        let mut ctx = Ctx::new(store.snapshot());
        ctx.set_grant(Some(grant.clone()));
        ctx
    }

    /// A plain query's estimate grows a few bytes at a time. The fast account admits a
    /// step from the grant at once, so the next small increases take neither its lock
    /// nor the grant's counters.
    #[test]
    fn small_eager_growth_admits_one_step_from_the_grant() {
        let store = Store::in_memory(StoreOptions::default());
        let pool = super::super::pool::QueryPool::new(128 * MIB);
        let grant = Arc::new(pool.try_grant(16 * MIB).unwrap());
        let ctx = pooled(&store, &grant);
        let before = super::FAST_GROWS.with(|n| n.get());
        let a = ctx.charge(8).unwrap();
        let b = ctx.charge(24).unwrap();
        ctx.check_output(100, 2).unwrap();
        assert_eq!(super::FAST_GROWS.with(|n| n.get()) - before, 1);
        assert_eq!(grant.used(), EAGER_STEP);
        let c = ctx.charge(EAGER_STEP).unwrap();
        assert_eq!(grant.used(), 2 * EAGER_STEP);
        assert_eq!(super::FAST_GROWS.with(|n| n.get()) - before, 2);
        assert_eq!(ctx.mem_peak(), 32 + EAGER_STEP);
        drop((a, b, c));
        drop(ctx);
        assert_eq!(grant.used(), 0);
    }

    /// The step never takes the contribution past the local budget, and a pool that
    /// cannot give the step still admits the exact estimate.
    #[test]
    fn the_eager_step_stays_within_the_budget_and_the_pool() {
        let store = Store::in_memory(StoreOptions::default());
        let pool = super::super::pool::QueryPool::new(128 * MIB);
        let grant = Arc::new(pool.empty_grant());
        let mut ctx = Ctx::new(store.snapshot());
        ctx.mem_limit = 1000;
        ctx.set_grant(Some(grant.clone()));
        drop(ctx.charge(10).unwrap());
        assert_eq!(grant.used(), 1000);
        drop(ctx);
        assert_eq!(grant.used(), 0);

        let small = super::super::pool::QueryPool::new(100_000);
        let grant = Arc::new(small.empty_grant());
        let ctx = pooled(&store, &grant);
        let held = ctx.charge(70_000).unwrap();
        assert_eq!(grant.used(), 70_000);
        assert!(matches!(ctx.charge(40_000), Err(Error::BudgetExceeded(b))
            if b.kind == BudgetKind::ServerMemory));
        assert_eq!(grant.used(), 70_000);
        drop(held);
        drop(ctx);
        assert_eq!(grant.used(), 0);
        drop(grant);
        assert_eq!(small.granted(), 0);
    }

    #[test]
    fn a_fitting_eager_output_remains_owned_until_context_drop() {
        let store = Store::in_memory(StoreOptions::default());
        let pool = super::super::pool::QueryPool::new(128 * MIB);
        let grant = Arc::new(pool.try_grant(100 * MIB).unwrap());
        let ctx = pooled(&store, &grant);
        let held = ctx.charge(80 * MIB).unwrap();
        ctx.check_output((20 * MIB / 8) as usize, 1).unwrap();
        drop(held);
        assert_eq!(grant.used(), 100 * MIB);
        assert!(grant.granted() >= 100 * MIB);
        drop(ctx);
        assert_eq!(grant.used(), 0);
        assert_eq!(grant.granted(), 16 * MIB);
    }

    /// `n` distinct query-local simple literals and their texts.
    fn local_literals(ctx: &Ctx, n: usize) -> Vec<(Id, String)> {
        (0..n)
            .map(|i| {
                let text = format!("literal number {i}");
                let term = Term::Literal(oxrdf::Literal::new_simple_literal(text.clone()));
                let id = ctx.intern_term(&term);
                assert_eq!(id.tag(), Tag::Local);
                (id, text)
            })
            .collect()
    }

    fn assert_text(value: Option<Value>, text: &str) {
        match value {
            Some(Value::Str(s)) => assert_eq!(&*s, text),
            other => panic!("{other:?} for {text:?}"),
        }
    }

    /// Cached values are charged to the query, and a finished eager result keeps
    /// their reservation in its pool contribution.
    #[test]
    fn cached_values_are_charged_and_kept_by_a_finished_result() {
        let store = Store::in_memory(StoreOptions::default());
        let pool = super::super::pool::QueryPool::new(256 * MIB);
        let grant = Arc::new(pool.empty_grant());
        let ctx = pooled(&store, &grant);
        let literals = local_literals(&ctx, 2_000);
        for (id, text) in &literals {
            assert_text(ctx.value(*id), text);
        }
        let reserved = ctx.values.reserved();
        assert!(reserved >= 2_000 * 40, "{reserved} bytes reserved");
        assert!(ctx.live_bytes() >= reserved);
        assert!(grant.used() >= reserved);
        ctx.shrink_retained(0).unwrap();
        assert!(grant.used() >= reserved, "{} of {reserved}", grant.used());
        // a second read hits the cache and reserves nothing more
        for (id, text) in &literals {
            assert_text(ctx.value(*id), text);
        }
        assert_eq!(ctx.values.reserved(), reserved);
        drop(ctx);
        assert_eq!(grant.used(), 0);
    }

    /// A cursor keeps the values cached before its transition and the charge of
    /// them, and caches repeated values with one reservation step per 64 KiB.
    #[test]
    fn a_cursor_caches_repeated_values_in_steps() {
        let store = Store::in_memory(StoreOptions::default());
        let pool = super::super::pool::QueryPool::new(256 * MIB);
        let grant = Arc::new(pool.empty_grant());
        let mut ctx = pooled(&store, &grant);
        ctx.mem_limit = 64 * MIB;
        let literals = local_literals(&ctx, 1_000);
        assert_text(ctx.value(literals[0].0), &literals[0].1);
        let prepared = ctx.values.reserved();
        assert!(prepared > 0);
        ctx.enable_cursor_accounting().unwrap();
        assert_eq!(
            ctx.values.reserved(),
            prepared,
            "the transition keeps the cache"
        );
        assert!(ctx.memory().live.load(Ordering::SeqCst) >= prepared);
        let before = super::VALUE_GROWS.with(|n| n.get());
        for i in 0..100_000 {
            let (id, text) = &literals[i % literals.len()];
            assert_text(ctx.value(*id), text);
        }
        let grows = super::VALUE_GROWS.with(|n| n.get()) - before;
        let reserved = ctx.values.reserved();
        assert!(
            grows as u64 <= reserved / EAGER_STEP + 1,
            "{grows} growths for {reserved} bytes"
        );
        assert!(reserved <= ValueCache::cap(ctx.mem_limit));
        assert!(ctx.live_bytes() <= ctx.mem_limit);
        assert!(ctx.check().is_ok());
        drop(ctx);
        assert_eq!(grant.used(), 0);
    }

    /// A budget that cannot hold the cache's reservation leaves every value bound and
    /// uncached, in both accounting modes, and records no failure.
    #[test]
    fn a_refused_cache_reservation_leaves_values_bound() {
        let store = Store::in_memory(StoreOptions::default());
        for cursor in [false, true] {
            let mut ctx = Ctx::new(store.snapshot());
            ctx.mem_limit = MIB;
            let literals = local_literals(&ctx, 200);
            if cursor {
                ctx.enable_cursor_accounting().unwrap();
            }
            let held = ctx.charge(MIB - ctx.live_bytes() - 10_000).unwrap();
            for (id, text) in &literals {
                assert_text(ctx.value(*id), text);
            }
            assert_eq!(ctx.values.reserved(), 0, "cursor {cursor}");
            assert!(ctx.check().is_ok(), "cursor {cursor}");
            drop(held);
        }
    }

    /// Decoding a term reserves the decode envelope in both accounting modes, and the end
    /// of the work releases it. A cursor refuses a decode that the budget cannot hold,
    /// gives no term and fails the next check, so a refused decode never becomes an
    /// unbound value. Eager callers cannot report a refusal, so an eager context decodes
    /// without the envelope then.
    #[test]
    fn refused_decode_envelopes_fail_the_next_check() {
        let store = Store::in_memory(StoreOptions::default());
        for cursor in [false, true] {
            let mut ctx = Ctx::new(store.snapshot());
            ctx.mem_limit = MIB;
            let literals = local_literals(&ctx, 2);
            if cursor {
                ctx.enable_cursor_accounting().unwrap();
            }
            let live = ctx.live_bytes();
            assert!(ctx.term(literals[0].0).is_some(), "cursor {cursor}");
            assert!(
                ctx.live_bytes() > live,
                "cursor {cursor}: the envelope is charged"
            );
            ctx.release_work();
            assert_eq!(ctx.live_bytes(), live, "cursor {cursor}: and released");
            let held = ctx.charge(MIB - live - 100).unwrap();
            if cursor {
                assert_eq!(ctx.term(literals[1].0), None);
                assert!(
                    matches!(ctx.check(), Err(Error::BudgetExceeded(b)) if b.kind == BudgetKind::Memory)
                );
            } else {
                assert!(ctx.term(literals[1].0).is_some());
                assert!(ctx.check().is_ok());
            }
            drop(held);
        }
    }

    /// A finished result keeps its unleased output columns on top of the charged state
    /// the context retains, such as its value cache.
    #[test]
    fn a_finished_result_keeps_its_output_and_its_charged_state() {
        let store = Store::in_memory(StoreOptions::default());
        let pool = super::super::pool::QueryPool::new(512 * MIB);
        let grant = Arc::new(pool.empty_grant());
        let ctx = pooled(&store, &grant);
        for (id, text) in &local_literals(&ctx, 2_000) {
            assert_text(ctx.value(*id), text);
        }
        let state = ctx.live_bytes();
        assert!(state > 0);
        ctx.check_output((200 * MIB / 8) as usize, 1).unwrap();
        ctx.shrink_retained(32 * MIB).unwrap();
        assert!(
            grant.used() >= state + 32 * MIB,
            "{} of {}",
            grant.used(),
            state + 32 * MIB
        );
        assert!(grant.used() < 200 * MIB);
        drop(ctx);
        assert_eq!(grant.used(), 0);
    }

    /// A shard that reaches its share of the cache is freed with its slots, so that the
    /// slots of cleared shards never pile up outside the reservation. The test watches
    /// one shard through its evictions: a map cleared in place would keep the capacity
    /// it had before the eviction.
    #[test]
    fn a_cleared_value_shard_frees_its_slots() {
        let store = Store::in_memory(StoreOptions::default());
        let mut ctx = Ctx::new(store.snapshot());
        ctx.mem_limit = 8 * MIB;
        let watched = ctx.values.shard(Id(0)) as *const _;
        let literals: Vec<_> = local_literals(&ctx, 40_000)
            .into_iter()
            .filter(|(id, _)| std::ptr::eq(ctx.values.shard(*id), watched))
            .collect();
        let share = ValueCache::cap(ctx.mem_limit) / VALUE_SHARDS as u64;
        let (mut before, mut evictions) = (0, 0);
        for (id, text) in &literals {
            assert_text(ctx.value(*id), text);
            let shard = ctx.values.shard(*id).read();
            if shard.bytes < before {
                evictions += 1;
                assert_eq!(shard.map.len(), 1);
                assert!(
                    shard.map.capacity() <= 4,
                    "a cleared shard keeps {} slots",
                    shard.map.capacity()
                );
            }
            assert!(shard.bytes <= share);
            before = shard.bytes;
        }
        assert!(evictions >= 2, "{evictions} evictions of the watched shard");
        assert!(ctx.values.reserved() <= ValueCache::cap(ctx.mem_limit));
    }

    #[test]
    fn fitting_eager_contexts_own_distinct_contributions() {
        let store = Store::in_memory(StoreOptions::default());
        let pool = super::super::pool::QueryPool::new(128 * MIB);
        let grant = Arc::new(pool.try_grant(100 * MIB).unwrap());
        let one = pooled(&store, &grant);
        let two = pooled(&store, &grant);
        one.check_output((40 * MIB / 8) as usize, 1).unwrap();
        two.check_output((24 * MIB / 8) as usize, 1).unwrap();
        assert_eq!(grant.used(), 64 * MIB);
        drop(one);
        assert_eq!(grant.used(), 24 * MIB);
        drop(two);
        assert_eq!(grant.used(), 0);
    }

    #[test]
    fn eager_live_and_output_estimates_do_not_count_live_bytes_twice() {
        let store = Store::in_memory(StoreOptions::default());
        let pool = super::super::pool::QueryPool::new(64 * MIB);
        let grant = Arc::new(pool.empty_grant());
        let ctx = pooled(&store, &grant);
        let held = ctx.charge(40 * MIB).unwrap();
        ctx.check_output((20 * MIB / 8) as usize, 1).unwrap();
        assert_eq!(grant.used(), 60 * MIB);
        ctx.check_output((10 * MIB / 8) as usize, 1).unwrap();
        assert_eq!(grant.used(), 60 * MIB);
        drop(held);
        assert_eq!(grant.used(), 60 * MIB);
        drop(ctx);
        assert_eq!(grant.used(), 0);
    }

    #[test]
    fn rejected_eager_estimates_preserve_the_successful_contribution() {
        let store = Store::in_memory(StoreOptions::default());
        let pool = super::super::pool::QueryPool::new(32 * MIB);
        let grant = Arc::new(pool.empty_grant());
        let mut ctx = pooled(&store, &grant);
        ctx.mem_limit = 48 * MIB;
        drop(ctx.charge(24 * MIB).unwrap());
        assert!(matches!(ctx.charge(40 * MIB), Err(Error::BudgetExceeded(b))
            if b.kind == BudgetKind::ServerMemory));
        assert!(matches!(ctx.check_output((56 * MIB / 8) as usize, 1),
            Err(Error::BudgetExceeded(b)) if b.kind == BudgetKind::Memory));
        assert_eq!(ctx.memory().live.load(Ordering::SeqCst), 0);
        assert_eq!(grant.used(), 24 * MIB);
        drop(ctx.charge(0).unwrap());
        ctx.check_output(0, 0).unwrap();
        assert_eq!(grant.used(), 24 * MIB);
        drop(ctx);
        assert_eq!(grant.used(), 0);
    }

    #[test]
    fn concurrent_eager_peaks_reserve_only_the_local_maximum() {
        let store = Store::in_memory(StoreOptions::default());
        let pool = super::super::pool::QueryPool::new(64 * MIB);
        let grant = Arc::new(pool.empty_grant());
        let ctx = pooled(&store, &grant);
        let barrier = std::sync::Barrier::new(2);
        std::thread::scope(|scope| {
            for bytes in [24 * MIB, 40 * MIB] {
                let (ctx, barrier) = (&ctx, &barrier);
                scope.spawn(move || {
                    barrier.wait();
                    ctx.check_output((bytes / 8) as usize, 1).unwrap();
                });
            }
        });
        assert_eq!(grant.used(), 40 * MIB);
        drop(ctx);
        assert_eq!(grant.used(), 0);
    }

    #[test]
    fn eager_and_cursor_ownership_share_a_grant_without_pinning_cursor_peaks() {
        let store = Store::in_memory(StoreOptions::default());
        let pool = super::super::pool::QueryPool::new(128 * MIB);
        let grant = Arc::new(pool.empty_grant());
        let eager = pooled(&store, &grant);
        eager.check_output((24 * MIB / 8) as usize, 1).unwrap();
        let mut cursor = pooled(&store, &grant);
        cursor.enable_cursor_accounting().unwrap();
        let held = cursor.charge(48 * MIB).unwrap();
        assert_eq!(grant.used(), 72 * MIB);
        drop(held);
        assert_eq!(grant.used(), 24 * MIB);
        assert!(grant.granted() <= 32 * MIB);
        drop(eager);
        assert_eq!(grant.used(), 0);
        drop(cursor);
        drop(grant);
        assert_eq!(pool.granted(), 0);
    }

    #[test]
    fn cursor_transition_reconciles_prepared_local_terms_and_eager_peaks() {
        let store = Store::in_memory(StoreOptions::default());
        let pool = super::super::pool::QueryPool::new(128 * MIB);
        let grant = Arc::new(pool.empty_grant());
        let mut ctx = pooled(&store, &grant);
        ctx.check_output((64 * MIB / 8) as usize, 1).unwrap();
        let term = Term::Literal(oxrdf::Literal::new_simple_literal("x".repeat(MIB as usize)));
        let id = ctx.intern_term(&term);
        assert!(ctx.local_len() > 0);
        ctx.enable_cursor_accounting().unwrap();
        let live = ctx.memory().live.load(Ordering::SeqCst);
        assert!(live >= MIB);
        assert_eq!(grant.used(), live);
        assert!(grant.granted() <= 16 * MIB);
        assert_eq!(ctx.term(id), Some(term));
        drop(ctx);
        assert_eq!(grant.used(), 0);
        drop(grant);
        assert_eq!(pool.granted(), 0);
    }

    #[test]
    fn a_grant_shared_by_two_contexts_follows_their_sum() {
        let store = Store::in_memory(StoreOptions::default());
        let pool = super::super::pool::QueryPool::new(256 * MIB);
        let grant = Arc::new(pool.empty_grant());
        let one = pooled(&store, &grant);
        let two = pooled(&store, &grant);
        let held = one.charge(50 * MIB).unwrap();
        drop(two.charge(40 * MIB).unwrap());
        assert!(grant.granted() >= 50 * MIB, "{}", grant.granted());
        drop(held);
    }

    #[test]
    fn a_trim_never_unbacks_an_accepted_charge() {
        use std::sync::atomic::AtomicBool;
        let store = Store::in_memory(StoreOptions::default());
        let pool = super::super::pool::QueryPool::new(64 * MIB);
        let g1 = Arc::new(pool.empty_grant());
        let g2 = Arc::new(pool.empty_grant());
        let ctx1 = pooled(&store, &g1);
        let ctx2 = pooled(&store, &g2);
        let accepted = AtomicU64::new(0);
        let stop = AtomicBool::new(false);
        let worker = |ctx: &Ctx, bytes: u64, hold: bool| {
            while !stop.load(Ordering::Relaxed) {
                if let Ok(charge) = ctx.charge(bytes) {
                    let now = accepted.fetch_add(bytes, Ordering::SeqCst) + bytes;
                    assert!(now <= 64 * MIB, "accepted {now} bytes in a 64 MiB pool");
                    if hold {
                        std::thread::yield_now();
                    }
                    accepted.fetch_sub(bytes, Ordering::SeqCst);
                    drop(charge);
                }
            }
        };
        std::thread::scope(|s| {
            s.spawn(|| worker(&ctx1, 48 * MIB, true));
            s.spawn(|| worker(&ctx1, MIB, false));
            s.spawn(|| worker(&ctx2, 48 * MIB, true));
            std::thread::sleep(std::time::Duration::from_secs(2));
            stop.store(true, Ordering::Relaxed);
        });
        drop((ctx1, ctx2));
        assert_eq!(accepted.load(Ordering::SeqCst), 0);
        drop((g1, g2));
        assert_eq!(pool.granted(), 0);
    }
}
