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

pub struct Ctx {
    pub snap: Arc<Snapshot>,
    local: RwLock<AppendVocab>,
    values: RwLock<FxHashMap<Id, Value>>,
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
}

impl Ctx {
    pub fn new(snap: Arc<Snapshot>) -> Ctx {
        Ctx {
            snap,
            local: RwLock::new(AppendVocab::default()),
            values: RwLock::new(FxHashMap::default()),
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
            Tag::Local => self
                .local
                .read()
                .get(id.payload())
                .map(id::key_to_term),
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
            Tag::BNode => Some(Value::BNode(bnode_for(id).as_str().into())),
            _ => {
                if let Some(v) = self.values.read().get(&id) {
                    return Some(v.clone());
                }
                let v = Value::from_term(&self.term(id)?);
                let mut w = self.values.write();
                if w.len() > 1_000_000 {
                    w.clear();
                }
                w.insert(id, v.clone());
                Some(v)
            }
        }
    }

    /// Is the id an IRI / blank node / literal (without full decoding where possible)?
    pub fn kind(&self, id: Id) -> TermKind {
        match id.tag() {
            Tag::Undef | Tag::Special => TermKind::None,
            Tag::Bool | Tag::Int | Tag::Double => TermKind::Literal,
            Tag::BNode => TermKind::BNode,
            Tag::Vocab | Tag::Delta => match self.snap.key(id) {
                Some(k) if id::is_key_iri(&k) => TermKind::Iri,
                Some(_) => TermKind::Literal,
                None => TermKind::None,
            },
            Tag::Local => match self.local.read().get(id.payload()) {
                Some(k) if id::is_key_iri(k) => TermKind::Iri,
                Some(_) => TermKind::Literal,
                None => TermKind::None,
            },
        }
    }

    pub fn local_len(&self) -> u64 {
        self.local.read().len()
    }
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum TermKind {
    None,
    Iri,
    BNode,
    Literal,
}
