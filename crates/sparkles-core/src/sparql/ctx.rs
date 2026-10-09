//! Per-query execution context: snapshot, local vocabulary, decode cache,
//! cancellation and dataset description.

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
    /// scans decode (and cache) only the key columns they read
    pub selective_columns: bool,
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
    /// counts from the index statistics are corrected for the snapshot's delta and for
    /// quads in graphs the query does not read, instead of being used only when neither
    /// exists
    pub delta_statistics: bool,
    /// join ordering runs the dynamic program on cost summaries, over subsets connected
    /// by shared variables, and drops partial plans dearer than a greedy plan; off: the
    /// program builds every candidate plan tree for every split
    pub pruned_join_order: bool,
    /// COUNT(*) over a FILTER on one variable of a single scan tests the filter once per
    /// run of the variable in a permutation sorted on it and sums the run lengths
    pub count_filter_runs: bool,
    /// a FILTER over a scan sorted on a variable it tests reads the runs of that
    /// variable, tests each value once and copies only the rows of the values that pass
    pub filter_scan_runs: bool,
    /// those two read only the key ranges of the values whose string starts as a
    /// `STRSTARTS` or a `REGEX` anchored on a literal start requires
    pub filter_key_ranges: bool,
    /// hash joins group the build side's rows by key in one flat array instead of a list
    /// per key
    pub flat_hash_join: bool,
    /// index joins find each key's rows by galloping from the previous key's position
    /// and keep the current block, instead of a binary search and a scan per cluster
    pub gallop_index_join: bool,
    /// OPTIONAL on one variable, with both sides sorted on it and always binding it,
    /// runs as a merge in the left side's order
    pub merge_left_join: bool,
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
    /// index joins that a fused star reads together are costed for the keys of the
    /// star's input, which a fused star probes in every pattern
    pub fused_star_costs: bool,
    /// an index join with few keys asks the kernel to read every block it will visit
    /// before it decodes the first, so a cold server reads them in parallel
    pub prefetch_blocks: bool,
    /// a FILTER tested on vocabulary keys whose `STRSTARTS` or start-anchored `REGEX`
    /// fixes the start of the string rejects the base-vocabulary ids outside the id
    /// ranges of the keys with that start, without reading their keys
    pub filter_id_ranges: bool,
    /// a hash join whose right side is a `text:query` call without a limit searches only
    /// the subjects of a small left side
    pub text_subject_pushdown: bool,
    /// ORDER BY DESC(spk:cosine(?v, C)) LIMIT k (or spk:dot) over one pattern reads the
    /// k best rows by an exact vector search
    pub vector_topk: bool,
    /// ORDER BY with LIMIT keeps the best rows in a bounded heap of offset + limit rows,
    /// evaluating the later keys only for rows whose first key can still enter
    pub topk_heap: bool,
}

impl Optimizations {
    pub const NAMES: [&str; 34] = [
        "range_pushdown",
        "incremental_group",
        "count_join_runs",
        "metadata_counts",
        "batched_paths",
        "topk_prefilter",
        "ordered_topk",
        "selective_columns",
        "spatial_pushdown",
        "decorrelate_exists",
        "batched_join",
        "star_fusion",
        "spatial_join",
        "spatial_knn",
        "expr_cache",
        "anti_join",
        "topk_first_key",
        "delta_statistics",
        "pruned_join_order",
        "count_filter_runs",
        "filter_scan_runs",
        "filter_key_ranges",
        "flat_hash_join",
        "gallop_index_join",
        "merge_left_join",
        "sampled_filters",
        "characteristic_sets",
        "probed_keys",
        "fused_star_costs",
        "prefetch_blocks",
        "filter_id_ranges",
        "text_subject_pushdown",
        "vector_topk",
        "topk_heap",
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
        selective_columns: true,
        spatial_pushdown: true,
        decorrelate_exists: true,
        batched_join: true,
        star_fusion: true,
        spatial_join: true,
        spatial_knn: true,
        expr_cache: true,
        anti_join: true,
        topk_first_key: true,
        delta_statistics: true,
        pruned_join_order: true,
        count_filter_runs: true,
        filter_scan_runs: true,
        filter_key_ranges: true,
        flat_hash_join: true,
        gallop_index_join: true,
        merge_left_join: true,
        sampled_filters: true,
        characteristic_sets: true,
        probed_keys: true,
        fused_star_costs: true,
        prefetch_blocks: true,
        filter_id_ranges: true,
        text_subject_pushdown: true,
        vector_topk: true,
        topk_heap: true,
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
        selective_columns: false,
        spatial_pushdown: false,
        decorrelate_exists: false,
        batched_join: false,
        star_fusion: false,
        spatial_join: false,
        spatial_knn: false,
        expr_cache: false,
        anti_join: false,
        topk_first_key: false,
        delta_statistics: false,
        pruned_join_order: false,
        count_filter_runs: false,
        filter_scan_runs: false,
        filter_key_ranges: false,
        flat_hash_join: false,
        gallop_index_join: false,
        merge_left_join: false,
        sampled_filters: false,
        characteristic_sets: false,
        probed_keys: false,
        fused_star_costs: false,
        prefetch_blocks: false,
        filter_id_ranges: false,
        text_subject_pushdown: false,
        vector_topk: false,
        topk_heap: false,
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
            "selective_columns" => &mut self.selective_columns,
            "spatial_pushdown" => &mut self.spatial_pushdown,
            "decorrelate_exists" => &mut self.decorrelate_exists,
            "batched_join" => &mut self.batched_join,
            "star_fusion" => &mut self.star_fusion,
            "spatial_join" => &mut self.spatial_join,
            "spatial_knn" => &mut self.spatial_knn,
            "expr_cache" => &mut self.expr_cache,
            "anti_join" => &mut self.anti_join,
            "topk_first_key" => &mut self.topk_first_key,
            "delta_statistics" => &mut self.delta_statistics,
            "pruned_join_order" => &mut self.pruned_join_order,
            "count_filter_runs" => &mut self.count_filter_runs,
            "filter_scan_runs" => &mut self.filter_scan_runs,
            "filter_key_ranges" => &mut self.filter_key_ranges,
            "flat_hash_join" => &mut self.flat_hash_join,
            "gallop_index_join" => &mut self.gallop_index_join,
            "merge_left_join" => &mut self.merge_left_join,
            "sampled_filters" => &mut self.sampled_filters,
            "characteristic_sets" => &mut self.characteristic_sets,
            "probed_keys" => &mut self.probed_keys,
            "fused_star_costs" => &mut self.fused_star_costs,
            "prefetch_blocks" => &mut self.prefetch_blocks,
            "filter_id_ranges" => &mut self.filter_id_ranges,
            "text_subject_pushdown" => &mut self.text_subject_pushdown,
            "vector_topk" => &mut self.vector_topk,
            "topk_heap" => &mut self.topk_heap,
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
    pub snap: Arc<Snapshot>,
    local: RwLock<AppendVocab>,
    values: Vec<RwLock<FxHashMap<Id, Value>>>,
    next_bnode: AtomicU64,
    bnode_memo: parking_lot::Mutex<FxHashMap<(Vec<Id>, String), Id>>,
    /// blank nodes given to the query from outside (initial bindings) that are not
    /// stored ones: a fresh blank node per label
    outside_bnodes: parking_lot::Mutex<FxHashMap<String, Id>>,
    /// the blank nodes that labels inside the composite literals of the query text name
    /// (see [`Ctx::query_literal`])
    literal_bnodes: parking_lot::Mutex<FxHashMap<String, Id>>,
    pub deadline: Option<Instant>,
    pub cancel: Arc<AtomicBool>,
    pub dataset: DatasetSpec,
    /// the request's graph view when it does not read every graph: `dataset` is already
    /// limited to it (see [`DatasetSpec::restrict`]), and plans are redacted
    pub graphs: Option<Arc<crate::access::GraphAccess>>,
    pub now: oxsdatatypes::DateTime,
    pub base_iri: Option<oxiri::Iri<String>>,
    pub var_names: RwLock<Vec<String>>,
    /// Maximum number of rows any intermediate result may have (memory guard).
    pub max_rows: usize,
    /// Budget for the estimated bytes of intermediate results alive at once
    /// (`u64::MAX`: unlimited). See [`Ctx::charge`] and [`Ctx::check_output`].
    pub mem_limit: u64,
    /// estimated bytes of the tables currently held by running operators
    mem_live: AtomicU64,
    /// highest estimate seen (held tables plus an output under construction)
    mem_peak: AtomicU64,
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
    /// the caller's scope in the cache of remote SERVICE results (see
    /// [`QueryOptions::service_scope`](super::QueryOptions::service_scope))
    pub service_scope: Arc<str>,
    /// RDFS on read (see [`super::rdfs`])
    pub rdfs: Option<Arc<super::rdfs::RdfsSchema>>,
    pub opt: Optimizations,
    /// parsed geometries of this query
    pub geo: crate::geo::memo::GeoMemo,
    /// notes for the plan's reader, without duplicates (see [`Ctx::warn`])
    warnings: parking_lot::Mutex<Vec<PlanWarning>>,
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
    // Opt-in cursor accounting stays after the ordinary execution fields.
    cursor_memory: Option<Box<CursorMemory>>,
}

struct CursorMemory {
    failure: parking_lot::Mutex<Option<Budget>>,
    failed: AtomicBool,
    decode_peak: parking_lot::Mutex<u64>,
    decoded_reserved: AtomicU64,
    owner: parking_lot::Mutex<Weak<Ctx>>,
    regex: super::cursor_regex::RegexCache,
}

impl Ctx {
    pub fn new(snap: Arc<Snapshot>) -> Ctx {
        Ctx {
            extension_families: Vec::new(),
            extension_writers: Vec::new(),
            extension_ancestors: Vec::new(),
            extensions: None,
            extension_failure: Default::default(),
            calls_extensions: false,
            callback_bnodes: Default::default(),
            callback_terms: Default::default(),
            cursor_memory: None,
            snap,
            local: RwLock::new(AppendVocab::default()),
            values: (0..VALUE_SHARDS)
                .map(|_| RwLock::new(FxHashMap::default()))
                .collect(),
            next_bnode: AtomicU64::new(0),
            bnode_memo: Default::default(),
            outside_bnodes: Default::default(),
            literal_bnodes: Default::default(),
            deadline: None,
            cancel: Arc::new(AtomicBool::new(false)),
            dataset: DatasetSpec::default(),
            graphs: None,
            now: oxsdatatypes::DateTime::now(),
            base_iri: None,
            var_names: RwLock::new(Vec::new()),
            max_rows: 200_000_000,
            mem_limit: u64::MAX,
            mem_live: AtomicU64::new(0),
            mem_peak: AtomicU64::new(0),
            max_rows_produced: u64::MAX,
            rows_produced: Arc::new(AtomicU64::new(0)),
            allow_service: true,
            forbid_service: false,
            outbound: Default::default(),
            outbound_budget: crate::outbound::RequestBudget::new(&Default::default()),
            use_cache: true,
            service_scope: Arc::from(""),
            rdfs: None,
            opt: Optimizations::default(),
            geo: Default::default(),
            warnings: Default::default(),
            sampled: Default::default(),
            stars: Default::default(),
            probes: Default::default(),
        }
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
        if !ws.contains(&w) && self.retain_cursor_bytes(w.message.len() as u64 + 128) {
            ws.push(w);
        }
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
        self.check_cursor_memory()?;
        if self.calls_extensions
            && let Some(state) = &self.extension_failure
            && let Some(failure) = state.lock().as_ref()
        {
            return Err(failure.engine());
        }
        if self.cancel.load(Ordering::Relaxed) {
            return Err(Error::Cancelled);
        }
        if let Some(d) = self.deadline
            && Instant::now() > d
        {
            return Err(Error::Timeout);
        }
        Ok(())
    }

    pub(super) fn enable_cursor_accounting(&mut self) -> Result<()> {
        for shard in &self.values {
            *shard.write() = FxHashMap::default();
        }
        let bytes = self.local.read().bytes() as u64;
        std::mem::forget(self.charge(bytes)?);
        self.cursor_memory = Some(Box::new(CursorMemory {
            failure: Default::default(),
            failed: AtomicBool::new(false),
            decode_peak: Default::default(),
            decoded_reserved: AtomicU64::new(0),
            owner: Default::default(),
            regex: Default::default(),
        }));
        self.use_cache = false;
        self.opt.sampled_filters = false;
        #[cfg(feature = "geo")]
        {
            let vertices = self.geo.op_vertices();
            // Parsed geometry retention is separate from ID batches. Until its
            // cache has charge-owning entries, cursor evaluation does not keep it.
            self.geo = crate::geo::memo::GeoMemo::with_budget(0);
            self.geo.set_op_vertices(vertices);
        }
        Ok(())
    }

    pub(super) fn is_cursor(&self) -> bool {
        self.cursor_memory.is_some()
    }

    pub(super) fn attach_cursor_owner(self: &Arc<Self>) {
        if let Some(memory) = &self.cursor_memory {
            *memory.owner.lock() = Arc::downgrade(self);
        }
    }

    /// A retained cache belongs to the plan, which may also occur in context
    /// bookkeeping. Use a weak owner to avoid a plan/context reference cycle.
    pub(super) fn retained_charge(&self, bytes: u64) -> Result<Option<RetainedCharge>> {
        let Some(memory) = &self.cursor_memory else {
            return Ok(None);
        };
        let charge = self.charge(bytes)?;
        let owner = memory.owner.lock().clone();
        if owner.strong_count() == 0 {
            return Err(Error::invalid("cursor context owner has not been attached"));
        }
        charge.bytes.set(0);
        Ok(Some(RetainedCharge { owner, bytes }))
    }

    pub(super) fn cursor_regex(
        &self,
        pattern: &str,
        flags: &str,
    ) -> Result<Option<Arc<super::cursor_regex::RegexState>>> {
        self.cursor_memory
            .as_ref()
            .expect("cursor regex context")
            .regex
            .get(self, pattern, flags)
    }

    pub(super) fn memory_remaining(&self) -> u64 {
        self.mem_limit
            .saturating_sub(self.mem_live.load(Ordering::Relaxed))
    }

    pub(super) fn release_cursor_work(&self) {
        if let Some(memory) = &self.cursor_memory {
            memory.regex.clear();
            let bytes = std::mem::take(&mut *memory.decode_peak.lock());
            memory.decoded_reserved.store(0, Ordering::Release);
            self.mem_live.fetch_sub(bytes, Ordering::Relaxed);
        }
    }

    pub(super) fn check_cursor_memory(&self) -> Result<()> {
        if let Some(memory) = &self.cursor_memory
            && memory.failed.load(Ordering::Acquire)
            && let Some(budget) = *memory.failure.lock()
        {
            return Err(Error::BudgetExceeded(budget));
        }
        Ok(())
    }

    pub(super) fn fail_cursor(&self, error: &Error) {
        if let Some(memory) = &self.cursor_memory
            && let Error::BudgetExceeded(budget) = error
        {
            memory.failure.lock().get_or_insert(*budget);
            memory.failed.store(true, Ordering::Release);
        }
    }

    // Infallible expression interning must not turn a resource failure into an
    // ordinary unbound BIND. Record it, refuse allocation, and fail the next check.
    pub(crate) fn retain_cursor_bytes(&self, bytes: u64) -> bool {
        let Some(memory) = &self.cursor_memory else {
            return true;
        };
        if memory.failed.load(Ordering::Acquire) {
            return false;
        }
        match self.charge(bytes) {
            Ok(charge) => {
                std::mem::forget(charge);
                true
            }
            Err(Error::BudgetExceeded(budget)) => {
                memory.failure.lock().get_or_insert(budget);
                memory.failed.store(true, Ordering::Release);
                false
            }
            Err(_) => unreachable!("memory charge returns a budget error"),
        }
    }

    /// Reserve a conservative high-water estimate for transient term decoding.
    /// Cursor evaluation does not retain the eager per-ID value cache.
    pub(super) fn cursor_decode(&self, bytes: usize) -> bool {
        let Some(memory) = &self.cursor_memory else {
            return true;
        };
        // Planning and serial pulls need only one value's envelope. A Rayon
        // evaluation may hold a value on every worker plus the caller; admit
        // that larger envelope only when parallel decoding actually starts.
        let workers = if rayon::current_thread_index().is_some() {
            rayon::current_num_threads() as u64 + 1
        } else {
            1
        };
        let need = (bytes as u64)
            .saturating_mul(8)
            .saturating_add(1024)
            .saturating_mul(workers);
        if memory.decoded_reserved.load(Ordering::Acquire) >= need {
            return self.check_cursor_memory().is_ok();
        }
        let mut peak = memory.decode_peak.lock();
        if need > *peak {
            if !self.retain_cursor_bytes(need - *peak) {
                return false;
            }
            *peak = need;
            memory.decoded_reserved.store(need, Ordering::Release);
        }
        self.check_cursor_memory().is_ok()
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
    /// total passes [`Ctx::max_rows_produced`]. One atomic add per operator.
    #[inline]
    pub fn produced(&self, rows: usize) -> Result<()> {
        if self.is_cursor() {
            let prior = self
                .rows_produced
                .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |n| {
                    n.checked_add(rows as u64)
                })
                .map_err(|_| self.produced_exceeded(u64::MAX))?;
            let total = prior + rows as u64;
            if total > self.max_rows_produced {
                return Err(self.produced_exceeded(total));
            }
            return Ok(());
        }
        let total = self
            .rows_produced
            .fetch_add(rows as u64, Ordering::Relaxed)
            .saturating_add(rows as u64);
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
        self.check_rows(rows)?;
        if self.max_rows_produced != u64::MAX {
            // the output will count as produced: fail before it is built
            let total = self
                .rows_produced
                .load(Ordering::Relaxed)
                .saturating_add(rows as u64);
            if total > self.max_rows_produced {
                return Err(self.produced_exceeded(total));
            }
        }
        let need = self
            .mem_live
            .load(Ordering::Relaxed)
            .saturating_add(table_bytes(rows, width));
        self.note_peak(need);
        if need > self.mem_limit {
            return Err(self.memory_exceeded(need));
        }
        Ok(())
    }

    /// Hold `bytes` of estimated memory until the returned guard drops (see
    /// [`Charge::add`]).
    pub fn charge(&self, bytes: u64) -> Result<Charge<'_>> {
        let c = Charge {
            ctx: self,
            bytes: std::cell::Cell::new(0),
        };
        c.add(bytes)?;
        Ok(c)
    }

    /// The highest memory estimate of this context so far.
    pub fn mem_peak(&self) -> u64 {
        self.mem_peak.load(Ordering::Relaxed)
    }

    /// How many rows of `width` columns fit in what is left of the memory budget (for
    /// capping up-front reservations; the output is still checked as it grows).
    pub fn rows_within_budget(&self, width: usize) -> usize {
        if self.mem_limit == u64::MAX {
            return usize::MAX;
        }
        let left = self
            .mem_limit
            .saturating_sub(self.mem_live.load(Ordering::Relaxed));
        usize::try_from(left / table_bytes(1, width)).unwrap_or(usize::MAX)
    }

    #[inline]
    fn note_peak(&self, bytes: u64) {
        if bytes > self.mem_peak.load(Ordering::Relaxed) {
            self.mem_peak.fetch_max(bytes, Ordering::Relaxed);
        }
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
                None if self.retain_cursor_bytes(b.len() as u64 + 128) => {
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
            if !self.retain_cursor_bytes(b.len() as u64 + 128) {
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
        if !self.retain_cursor_bytes(label.len() as u64 + 128) {
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
        if !self.retain_cursor_bytes(key.len() as u64 + 96) {
            return Id::UNDEF;
        }
        Id::local(local.insert(key).0)
    }

    pub fn intern_value(&self, v: &Value) -> Id {
        match v {
            Value::Bool(b) => Id::from_bool(*b),
            Value::Integer(i) => match Id::from_i64(i64::from(*i)) {
                Some(id) => id,
                None => self.intern_term(&v.to_term()),
            },
            _ => self.intern_term(&v.to_term()),
        }
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
        if !self.retain_cursor_bytes(key.0.len() as u64 * 8 + s.len() as u64 + 128) {
            return Id::UNDEF;
        }
        let id = self.fresh_bnode();
        memo.insert(key, id);
        id
    }

    pub fn term(&self, id: Id) -> Option<Term> {
        if self.is_cursor() {
            return match id.tag() {
                Tag::Local => {
                    let local = self.local.read();
                    let key = local.get(id.payload())?;
                    self.cursor_decode(key.len()).then(|| id::key_to_term(key))
                }
                Tag::Vocab => self
                    .with_cursor_vocab_key(id.payload(), |key| {
                        self.cursor_decode(key.len()).then(|| id::key_to_term(key))
                    })
                    .inspect_err(|error| self.fail_cursor(error))
                    .ok()
                    .flatten()
                    .flatten(),
                Tag::Delta => self.snap.generation.dvocab.with(|vocab| {
                    vocab
                        .get(id.payload())
                        .and_then(|key| self.cursor_decode(key.len()).then(|| id::key_to_term(key)))
                }),
                _ => self.snap.term(id),
            };
        }
        match id.tag() {
            Tag::Local => self.local.read().get(id.payload()).map(id::key_to_term),
            Tag::BNode => Some(Term::BlankNode(bnode_for(id))),
            _ => self.snap.term(id),
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

    pub(super) fn with_local_key<T>(&self, id: u64, f: impl FnOnce(&[u8]) -> T) -> Option<T> {
        self.local.read().get(id).map(f)
    }

    /// Batch resolution is independent of a producer's sticky failure. Previously
    /// returned IDs remain valid; each decode still reserves its transient bytes.
    pub(super) fn batch_term(&self, id: Id) -> Result<Option<Term>> {
        let decode = |key: &[u8]| -> Result<Option<Term>> {
            let _charge = self.charge((key.len() as u64).saturating_mul(8).saturating_add(1024))?;
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
                .with(|v| v.get(id.payload()).map_or(Ok(None), decode)),
            Tag::Undef | Tag::Special => Ok(None),
            _ => {
                let _charge = self.charge(128)?;
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
                // Inline scalars need no retained cache or RDF round trip. Only
                // dictionary/local values bypass the uncharged eager cache.
                if self.is_cursor() {
                    let decode =
                        |key: &[u8]| self.cursor_decode(key.len()).then(|| Value::from_key(key));
                    return match id.tag() {
                        Tag::Vocab => match self.with_cursor_vocab_key(id.payload(), decode) {
                            Ok(value) => value.flatten(),
                            Err(error) => {
                                self.fail_cursor(&error);
                                None
                            }
                        },
                        Tag::Delta => self
                            .snap
                            .generation
                            .dvocab
                            .with(|v| v.get(id.payload()).and_then(decode)),
                        Tag::Local => self.local.read().get(id.payload()).and_then(decode),
                        _ => unreachable!("dictionary value tag"),
                    };
                }
                let shard = self.shard(id);
                if let Some(v) = shard.read().get(&id) {
                    return Some(v.clone());
                }
                let v = Value::from_term(&self.term(id)?);
                let mut w = shard.write();
                if w.len() > VALUE_SHARD_CAP {
                    w.clear();
                }
                w.insert(id, v.clone());
                Some(v)
            }
        }
    }

    #[inline]
    fn shard(&self, id: Id) -> &RwLock<FxHashMap<Id, Value>> {
        &self.values[(id.0.wrapping_mul(0x9E37_79B9_7F4A_7C15) >> 58) as usize % VALUE_SHARDS]
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

    pub fn local_len(&self) -> u64 {
        self.local.read().len()
    }
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
}

impl Charge<'_> {
    /// Hold `bytes` more; fails (holding nothing more) when that would exceed the
    /// context's memory budget.
    pub fn add(&self, bytes: u64) -> Result<()> {
        if bytes == 0 {
            return Ok(());
        }
        let ctx = self.ctx;
        // Validate local ownership before publishing a global increment. Rejected
        // requests never transiently wrap or consume another thread's allowance.
        let owned = self
            .bytes
            .get()
            .checked_add(bytes)
            .ok_or_else(|| ctx.memory_exceeded(u64::MAX))?;
        let prior = ctx
            .mem_live
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |live| {
                live.checked_add(bytes)
                    .filter(|next| *next <= ctx.mem_limit)
            })
            .map_err(|live| ctx.memory_exceeded(live.saturating_add(bytes)))?;
        let live = prior + bytes;
        self.bytes.set(owned);
        ctx.note_peak(live);
        Ok(())
    }
}

impl Drop for Charge<'_> {
    fn drop(&mut self) {
        self.ctx
            .mem_live
            .fetch_sub(self.bytes.get(), Ordering::Relaxed);
    }
}

/// An owned charge that follows a cursor buffer across pulls and threads.
pub(super) struct OwnedCharge {
    ctx: Arc<Ctx>,
    bytes: u64,
}

pub(super) struct RetainedCharge {
    owner: Weak<Ctx>,
    bytes: u64,
}

impl RetainedCharge {
    pub(super) fn owner(&self) -> Option<Arc<Ctx>> {
        self.owner.upgrade()
    }

    pub(super) fn resize(&mut self, bytes: u64) -> Result<()> {
        let ctx = self
            .owner
            .upgrade()
            .ok_or_else(|| Error::invalid("cursor context was released"))?;
        if bytes > self.bytes {
            let added = ctx.charge(bytes - self.bytes)?;
            added.bytes.set(0);
        } else {
            ctx.mem_live
                .fetch_sub(self.bytes - bytes, Ordering::Relaxed);
        }
        self.bytes = bytes;
        Ok(())
    }
}

impl Drop for RetainedCharge {
    fn drop(&mut self) {
        if let Some(ctx) = self.owner.upgrade() {
            ctx.mem_live.fetch_sub(self.bytes, Ordering::Relaxed);
        }
    }
}

impl OwnedCharge {
    pub(super) fn bytes(&self) -> u64 {
        self.bytes
    }

    pub(super) fn new(ctx: &Arc<Ctx>, bytes: u64) -> Result<Self> {
        let mut owned = Self {
            ctx: ctx.clone(),
            bytes: 0,
        };
        owned.resize(bytes)?;
        Ok(owned)
    }

    pub(super) fn resize(&mut self, bytes: u64) -> Result<()> {
        if bytes > self.bytes {
            let charge = self.ctx.charge(bytes - self.bytes)?;
            charge.bytes.set(0);
        } else {
            self.ctx
                .mem_live
                .fetch_sub(self.bytes - bytes, Ordering::Relaxed);
        }
        self.bytes = bytes;
        Ok(())
    }

    pub(super) fn retain(mut self) {
        self.bytes = 0;
    }
}

impl Drop for OwnedCharge {
    fn drop(&mut self) {
        self.ctx.mem_live.fetch_sub(self.bytes, Ordering::Relaxed);
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
    fn retained_cursor_charges_release_and_do_not_keep_context_alive() {
        let store = Store::in_memory(StoreOptions::default());
        let mut context = Ctx::new(store.snapshot());
        context.enable_cursor_accounting().unwrap();
        let context = Arc::new(context);
        context.attach_cursor_owner();
        let before = context.mem_live.load(Ordering::Relaxed);
        let mut retained = context.retained_charge(4096).unwrap().unwrap();
        assert_eq!(context.mem_live.load(Ordering::Relaxed), before + 4096);
        retained.resize(2048).unwrap();
        assert_eq!(context.mem_live.load(Ordering::Relaxed), before + 2048);
        drop(retained);
        assert_eq!(context.mem_live.load(Ordering::Relaxed), before);
        let retained = context.retained_charge(4096).unwrap();
        let weak = Arc::downgrade(&context);
        drop(context);
        assert!(weak.upgrade().is_none());
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
            assert_eq!(ctx.mem_live.load(Ordering::Relaxed), 80);
            assert_eq!(charge.bytes.get(), 80);
            charge.add(20).unwrap();
            drop(charge);
            assert_eq!(ctx.mem_live.load(Ordering::Relaxed), 0);
            // A different guard can overflow the global count while its own count
            // is representable. Failure must not corrupt either guard's release.
            let first = ctx.charge(limit).unwrap();
            assert!(ctx.charge(1).is_err());
            assert_eq!(ctx.mem_live.load(Ordering::Relaxed), limit);
            drop(first);
            assert_eq!(ctx.mem_live.load(Ordering::Relaxed), 0);
            drop(ctx.charge(1).unwrap());
            assert_eq!(ctx.mem_live.load(Ordering::Relaxed), 0);
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
            assert_eq!(ctx.mem_live.load(Ordering::Relaxed), limit - 10);
            drop(owner);
            assert_eq!(ctx.mem_live.load(Ordering::Relaxed), 0);
        }
    }
}
