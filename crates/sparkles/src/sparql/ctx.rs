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
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::Instant;

/// Jena's IRI for the default graph (`Quad.defaultGraphIRI`).
pub const DEFAULT_GRAPH_IRI: &str = "urn:x-arq:DefaultGraph";
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
}

impl Optimizations {
    pub const NAMES: [&str; 20] = [
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
                tracing::warn!("SPARKLES_DISABLE_OPTIMIZATIONS: {e}");
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
    pub snap: Arc<Snapshot>,
    local: RwLock<AppendVocab>,
    values: Vec<RwLock<FxHashMap<Id, Value>>>,
    next_bnode: AtomicU64,
    bnode_memo: parking_lot::Mutex<FxHashMap<(Vec<Id>, String), Id>>,
    pub deadline: Option<Instant>,
    pub cancel: Arc<AtomicBool>,
    pub dataset: DatasetSpec,
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
    pub opt: Optimizations,
    /// parsed geometries of this query
    pub geo: crate::geo::memo::GeoMemo,
    /// notes for the plan's reader, without duplicates (see [`Ctx::warn`])
    warnings: parking_lot::Mutex<Vec<PlanWarning>>,
}

impl Ctx {
    pub fn new(snap: Arc<Snapshot>) -> Ctx {
        Ctx {
            snap,
            local: RwLock::new(AppendVocab::default()),
            values: (0..VALUE_SHARDS)
                .map(|_| RwLock::new(FxHashMap::default()))
                .collect(),
            next_bnode: AtomicU64::new(0),
            bnode_memo: Default::default(),
            deadline: None,
            cancel: Arc::new(AtomicBool::new(false)),
            dataset: DatasetSpec::default(),
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
            opt: Optimizations::default(),
            geo: Default::default(),
            warnings: Default::default(),
        }
    }

    /// Record a warning for the plan (once, however often it is raised).
    pub fn warn(&self, w: PlanWarning) {
        let mut ws = self.warnings.lock();
        if !ws.contains(&w) {
            ws.push(w);
        }
    }

    /// The warnings recorded so far.
    pub fn warnings(&self) -> Vec<PlanWarning> {
        self.warnings.lock().clone()
    }

    #[inline]
    pub fn check(&self) -> Result<()> {
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

    fn memory_exceeded(&self, requested: u64) -> Error {
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

    /// The id of a term: stored id if the term exists in the store, else a local id.
    pub fn intern_term(&self, t: &Term) -> Id {
        if let Term::BlankNode(b) = t
            && let Some(id) = parse_bnode_label(b.as_str())
        {
            return id;
        }
        if let Some(id) = id::inline_id(t) {
            return id;
        }
        let key = id::term_key(t);
        self.intern_key(&key)
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
        Id::local(self.local.write().insert(key).0)
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
        *self
            .bnode_memo
            .lock()
            .entry((row, s.to_string()))
            .or_insert_with(|| self.fresh_bnode())
    }

    pub fn term(&self, id: Id) -> Option<Term> {
        match id.tag() {
            Tag::Local => self.local.read().get(id.payload()).map(id::key_to_term),
            Tag::BNode => Some(Term::BlankNode(bnode_for(id))),
            _ => self.snap.term(id),
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
        let live = ctx
            .mem_live
            .fetch_add(bytes, Ordering::Relaxed)
            .saturating_add(bytes);
        if live > ctx.mem_limit {
            ctx.mem_live.fetch_sub(bytes, Ordering::Relaxed);
            return Err(ctx.memory_exceeded(live));
        }
        self.bytes.set(self.bytes.get() + bytes);
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
