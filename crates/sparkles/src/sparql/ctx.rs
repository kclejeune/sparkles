//! Per-query execution context: snapshot, local vocabulary, decode cache,
//! cancellation and dataset description.

use super::table::VarId;
use super::value::Value;
use crate::error::{Error, Result};
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
    /// scans decode (and cache) only the key columns they read
    pub selective_columns: bool,
}

impl Optimizations {
    pub const NAMES: [&str; 7] = [
        "range_pushdown",
        "incremental_group",
        "count_join_runs",
        "metadata_counts",
        "batched_paths",
        "topk_prefilter",
        "selective_columns",
    ];

    /// Everything on.
    pub const ALL: Optimizations = Optimizations {
        range_pushdown: true,
        incremental_group: true,
        count_join_runs: true,
        metadata_counts: true,
        batched_paths: true,
        topk_prefilter: true,
        selective_columns: true,
    };

    /// Everything off: the generic operators only.
    pub const NONE: Optimizations = Optimizations {
        range_pushdown: false,
        incremental_group: false,
        count_join_runs: false,
        metadata_counts: false,
        batched_paths: false,
        topk_prefilter: false,
        selective_columns: false,
    };

    fn flag(&mut self, name: &str) -> Option<&mut bool> {
        Some(match name {
            "range_pushdown" => &mut self.range_pushdown,
            "incremental_group" => &mut self.incremental_group,
            "count_join_runs" => &mut self.count_join_runs,
            "metadata_counts" => &mut self.metadata_counts,
            "batched_paths" => &mut self.batched_paths,
            "topk_prefilter" => &mut self.topk_prefilter,
            "selective_columns" => &mut self.selective_columns,
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
    pub allow_service: bool,
    /// consult / fill the store's result cache
    pub use_cache: bool,
    pub opt: Optimizations,
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
            allow_service: true,
            use_cache: true,
            opt: Optimizations::default(),
        }
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

    pub fn check_rows(&self, n: usize) -> Result<()> {
        if n > self.max_rows {
            return Err(Error::MemoryLimit(format!(
                "intermediate result of {n} rows exceeds the limit of {}",
                self.max_rows
            )));
        }
        Ok(())
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
