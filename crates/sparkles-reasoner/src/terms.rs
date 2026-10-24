//! Term handling for the engine: classification and decoding of ids, plus a local
//! dictionary for terms that do not exist in the store yet (head constants, literals
//! computed by builtins, blank nodes from `makeTemp` / `makeSkolem`).
//!
//! The local dictionary is charged to the run's budget ([`Terms::set_budget`]): each
//! term's key twice (the map's and the table's copy), its map entry, the map's growth
//! and each segment of the table when it is first used, before they are allocated; and
//! the regular expressions it compiled, at an estimate each.

use crate::budget::{Budget, MemoryExceeded, table_bytes, table_grown};
use oxrdf::{BlankNode, Literal, NamedNode, Term};
use rustc_hash::FxHashMap;
use sparkles_core::id::{self, Id, Tag};
use sparkles_core::sparql::value::Value;
use sparkles_core::store::Snapshot;
use std::mem::size_of;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, OnceLock, RwLock};

/// Regular expressions compiled for `regex` with a pattern that is not a constant, kept
/// at most: a full cache starts over.
const REGEX_CACHE: usize = 256;
/// The estimate of the heap of one compiled regular expression, besides its pattern.
const REGEX_BYTES: usize = 16 << 10;
/// The heap of a key's map entry besides the key's bytes, and of a table slot.
const ENTRY_BYTES: usize = size_of::<(Vec<u8>, u64)>();
const SLOT_BYTES: usize = size_of::<OnceLock<LocalTerm>>();

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Kind {
    Iri,
    BNode,
    Literal,
    /// RDF 1.2 triple term: valid as an object only
    Triple,
    Other,
}

/// Kind of a vocabulary key (`<iri`, `"literal`, `(triple`).
fn key_kind(k: &[u8]) -> Kind {
    match k.first() {
        Some(b'<') => Kind::Iri,
        Some(b'"') => Kind::Literal,
        Some(b'(') => Kind::Triple,
        _ => Kind::Other,
    }
}

#[derive(Clone, Debug)]
pub(crate) enum LocalTerm {
    /// vocabulary key of an IRI or literal
    Key(Vec<u8>),
    /// a blank node that must be created when writing
    BNode,
}

#[derive(Default)]
struct Locals {
    map: FxHashMap<Vec<u8>, u64>,
    /// local ids handed out
    len: u64,
}

/// The local terms by local id, append-only, read without a lock: segment `k` holds
/// `SEGMENT << k` slots.
struct LocalTable {
    segments: Vec<OnceLock<Box<[OnceLock<LocalTerm>]>>>,
}

const SEGMENT: u64 = 1024;

impl LocalTable {
    fn new() -> LocalTable {
        LocalTable {
            segments: (0..40).map(|_| OnceLock::new()).collect(),
        }
    }

    fn at(i: u64) -> (usize, usize) {
        let k = 63 - (i / SEGMENT + 1).leading_zeros() as u64;
        (k as usize, (i - SEGMENT * ((1 << k) - 1)) as usize)
    }

    fn get(&self, i: u64) -> Option<&LocalTerm> {
        let (k, off) = Self::at(i);
        self.segments.get(k)?.get()?.get(off)?.get()
    }

    /// The bytes slot `i` makes the table allocate: its segment's, when it is the
    /// segment's first.
    fn growth(&self, i: u64) -> usize {
        let (k, _) = Self::at(i);
        match self.segments.get(k) {
            Some(s) if s.get().is_none() => (SEGMENT << k) as usize * SLOT_BYTES,
            _ => 0,
        }
    }

    /// Set slot `i` (each slot once, by the holder of the write lock).
    fn set(&self, i: u64, t: LocalTerm) {
        let (k, off) = Self::at(i);
        let seg =
            self.segments[k].get_or_init(|| (0..(SEGMENT << k)).map(|_| OnceLock::new()).collect());
        let _ = seg[off].set(t);
    }
}

/// Term context shared by all evaluation threads.
pub(crate) struct Terms {
    pub snap: Arc<Snapshot>,
    /// base vocabulary id range `[lo, hi)` holding literals
    lit_range: (u64, u64),
    locals: RwLock<Locals>,
    table: LocalTable,
    regex_cache: Mutex<FxHashMap<String, Option<regex::Regex>>>,
    /// the budget the local terms are charged to, and the bytes charged
    budget: RwLock<Arc<Budget>>,
    charged: AtomicUsize,
    pub rdf_first: u64,
    pub rdf_rest: u64,
    pub rdf_nil: u64,
    pub now: u64,
}

impl Drop for Terms {
    fn drop(&mut self) {
        self.budget.read().unwrap().release(*self.charged.get_mut());
    }
}

impl Terms {
    pub fn new(snap: Arc<Snapshot>) -> Terms {
        let lit_range = snap.generation.vocab.prefix_range(b"\"");
        let mut t = Terms {
            snap,
            lit_range,
            locals: RwLock::new(Locals::default()),
            table: LocalTable::new(),
            regex_cache: Mutex::new(FxHashMap::default()),
            budget: RwLock::new(Budget::unlimited()),
            charged: AtomicUsize::new(0),
            rdf_first: 0,
            rdf_rest: 0,
            rdf_nil: 0,
            now: 0,
        };
        let rdf = |l: &str| {
            Term::NamedNode(NamedNode::new_unchecked(format!(
                "{}{l}",
                crate::parser::RDF_NS
            )))
        };
        t.rdf_first = t.id_for(&rdf("first"));
        t.rdf_rest = t.id_for(&rdf("rest"));
        t.rdf_nil = t.id_for(&rdf("nil"));
        let now = oxsdatatypes::DateTime::now();
        t.now = t.id_for(&Term::Literal(Literal::new_typed_literal(
            now.to_string(),
            oxrdf::vocab::xsd::DATE_TIME,
        )));
        t
    }

    /// Charge the local terms to `budget` from now on, or fail when they do not fit.
    pub fn set_budget(&self, budget: &Arc<Budget>) -> Result<(), MemoryExceeded> {
        let mut b = self.budget.write().unwrap();
        let n = self.charged.load(Ordering::Relaxed);
        budget.try_charge(n)?;
        b.release(n);
        *b = budget.clone();
        Ok(())
    }

    /// The budget the local terms are charged to.
    pub fn budget(&self) -> Arc<Budget> {
        self.budget.read().unwrap().clone()
    }

    /// The bytes the local terms are charged.
    pub fn heap_bytes(&self) -> usize {
        self.charged.load(Ordering::Relaxed)
    }

    /// Charge `n` bytes, failing past the limit with `strict`.
    fn charge(&self, n: usize, strict: bool) -> Result<(), MemoryExceeded> {
        let b = self.budget.read().unwrap();
        if strict {
            b.try_charge(n)?;
        } else {
            b.charge(n);
        }
        self.charged.fetch_add(n, Ordering::Relaxed);
        Ok(())
    }

    fn release(&self, n: usize) {
        let n = n.min(self.charged.load(Ordering::Relaxed));
        self.budget.read().unwrap().release(n);
        self.charged.fetch_sub(n, Ordering::Relaxed);
    }

    /// Id of a term: inline, existing in the store, or a (new) local id, charged to the
    /// budget even past its limit (for the run to find at its next check).
    pub fn id_for(&self, t: &Term) -> u64 {
        self.id_charged(t, false)
            .expect("charges that may pass the limit do not fail")
    }

    /// [`id_for`](Self::id_for), failing without a new term when it does not fit the
    /// budget.
    pub fn try_id_for(&self, t: &Term) -> Result<u64, MemoryExceeded> {
        self.id_charged(t, true)
    }

    /// The id of the IRI or literal with vocabulary key `key`: in the store, else a
    /// (new) local id, failing without a new term when it does not fit the budget.
    pub fn try_id_for_key(&self, key: &[u8]) -> Result<u64, MemoryExceeded> {
        if let Some(id) = self.snap.lookup_key(key) {
            return Ok(id.0);
        }
        self.local_keyed(key.to_vec(), LocalTerm::Key(Vec::new()), true)
    }

    fn id_charged(&self, t: &Term, strict: bool) -> Result<u64, MemoryExceeded> {
        if let Some(id) = id::inline_id(t) {
            return Ok(id.0);
        }
        if let Term::BlankNode(b) = t {
            if let Some(id) = self.snap.lookup_term(t) {
                return Ok(id.0);
            }
            // rule-level blank node constant: one local bnode per label
            let mut key = b"\0bn".to_vec();
            key.extend_from_slice(b.as_str().as_bytes());
            return self.local_keyed(key, LocalTerm::BNode, strict);
        }
        let key = id::term_key(t);
        if let Some(id) = self.snap.lookup_key(&key) {
            return Ok(id.0);
        }
        // the table gets its copy of the key once the term is charged
        self.local_keyed(key, LocalTerm::Key(Vec::new()), strict)
    }

    /// The local id of `key`, made with `term` (a key term gets a copy of `key`) when
    /// it is new: its bytes, its entry, the map's growth and a new segment of the table
    /// are charged first.
    fn local_keyed(
        &self,
        key: Vec<u8>,
        term: LocalTerm,
        strict: bool,
    ) -> Result<u64, MemoryExceeded> {
        if let Some(&id) = self.locals.read().unwrap().map.get(&key) {
            return Ok(id);
        }
        let mut l = self.locals.write().unwrap();
        if let Some(&id) = l.map.get(&key) {
            return Ok(id);
        }
        let n = l.len;
        let copy = matches!(term, LocalTerm::Key(_));
        let (cap, len) = (l.map.capacity(), l.map.len());
        let grow = if len == cap {
            table_bytes(table_grown(cap, len, 1), ENTRY_BYTES)
        } else {
            0
        };
        // the map's key and the table's copy; the entry and the slot are part of the
        // map's table and of the segment
        let bytes = key.len() * (1 + copy as usize) + self.table.growth(n) + grow;
        self.charge(bytes, strict)?;
        let term = match term {
            LocalTerm::Key(_) => LocalTerm::Key(key.clone()),
            t => t,
        };
        self.table.set(n, term);
        l.len += 1;
        let id = Id::local(n).0;
        l.map.insert(key, id);
        if grow > 0 {
            // the old table is freed: the map's heap is now its new table
            self.release(table_bytes(cap, ENTRY_BYTES));
        }
        Ok(id)
    }

    /// A fresh blank node (`makeTemp`), failing when it does not fit the budget.
    pub fn try_fresh_bnode(&self) -> Result<u64, MemoryExceeded> {
        let mut l = self.locals.write().unwrap();
        let n = l.len;
        self.charge(self.table.growth(n), true)?;
        self.table.set(n, LocalTerm::BNode);
        l.len += 1;
        Ok(Id::local(n).0)
    }

    /// A blank node determined by `args` (`makeSkolem`), failing when it does not fit
    /// the budget.
    pub fn try_skolem(&self, args: &[u64]) -> Result<u64, MemoryExceeded> {
        let mut key = b"\0sk".to_vec();
        for a in args {
            key.extend_from_slice(&a.to_le_bytes());
        }
        self.local_keyed(key, LocalTerm::BNode, true)
    }

    pub fn local(&self, id: u64) -> Option<LocalTerm> {
        let i = Id(id);
        (i.tag() == Tag::Local)
            .then(|| self.table.get(i.payload()).cloned())
            .flatten()
    }

    /// The kind of a local id's term, without cloning it.
    fn local_kind(&self, id: u64) -> Kind {
        match self.table.get(Id(id).payload()) {
            Some(LocalTerm::BNode) => Kind::BNode,
            Some(LocalTerm::Key(k)) => key_kind(k),
            None => Kind::Other,
        }
    }

    pub fn kind(&self, id: u64) -> Kind {
        let i = Id(id);
        if i.is_inline() {
            // inline numbers, booleans, dates, … (whatever the library inlines)
            return Kind::Literal;
        }
        match i.tag() {
            Tag::BNode => Kind::BNode,
            Tag::Vocab => {
                let p = i.payload();
                let v = &self.snap.generation.vocab;
                if p >= self.lit_range.0 && p < self.lit_range.1 {
                    Kind::Literal
                } else if v.is_triple(p) {
                    Kind::Triple
                } else if v.is_iri(p) {
                    Kind::Iri
                } else {
                    Kind::Other
                }
            }
            Tag::Delta => self.snap.key(i).map_or(Kind::Other, |k| key_kind(&k)),
            Tag::Local => self.local_kind(id),
            _ => Kind::Other,
        }
    }

    pub fn term(&self, id: u64) -> Option<Term> {
        let i = Id(id);
        if i.tag() == Tag::Local {
            return Some(match self.local(id)? {
                LocalTerm::Key(k) => id::key_to_term(&k),
                LocalTerm::BNode => {
                    Term::BlankNode(BlankNode::new_unchecked(format!("r{:x}", i.payload())))
                }
            });
        }
        self.snap.term(i)
    }

    /// The vocabulary key of a term (`None` for a local blank node).
    pub fn key_of(&self, id: u64) -> Option<Vec<u8>> {
        let i = Id(id);
        match i.tag() {
            Tag::Local => match self.local(id)? {
                LocalTerm::Key(k) => Some(k),
                LocalTerm::BNode => None,
            },
            Tag::Vocab | Tag::Delta => self.snap.key(i).map(|k| k.into_owned()),
            _ => self.snap.term(i).map(|t| id::term_key(&t)),
        }
    }

    /// Is this (possibly generalized) triple valid RDF?
    pub fn valid(&self, t: &[u64; 3]) -> bool {
        matches!(self.kind(t[0]), Kind::Iri | Kind::BNode)
            && self.kind(t[1]) == Kind::Iri
            && self.kind(t[2]) != Kind::Other
    }

    /// Read terms from a later snapshot of the same generation. Local terms that the
    /// store now has are returned with their store ids, for the caller to replace.
    pub fn rebase(&mut self, snap: Arc<Snapshot>) -> FxHashMap<u64, u64> {
        self.snap = snap;
        let mut moved = FxHashMap::default();
        let mut l = self.locals.write().unwrap();
        let Locals { map, .. } = &mut *l;
        let mut freed = 0;
        map.retain(|key, id| {
            if key.starts_with(b"\0") {
                return true;
            }
            match self.snap.lookup_key(key) {
                Some(s) => {
                    moved.insert(*id, s.0);
                    // the map's copy of the key; the table keeps its own
                    freed += key.len();
                    false
                }
                None => true,
            }
        });
        drop(l);
        self.release(freed);
        moved
    }

    pub fn value(&self, id: u64) -> Option<Value> {
        self.term(id).map(|t| Value::from_term(&t))
    }

    /// Lexical form used by `strConcat` / `uriConcat` / `regex` (Jena `lex()`).
    pub fn lexical(&self, id: u64) -> Option<String> {
        Some(match self.term(id)? {
            Term::NamedNode(n) => n.into_string(),
            Term::BlankNode(b) => b.into_string(),
            Term::Literal(l) => l.destruct().0,
            Term::Triple(t) => t.to_string(),
        })
    }

    /// The compiled pattern, from a cache of at most [`REGEX_CACHE`] patterns, each
    /// charged at an estimate before it is compiled; `Ok(None)` for an invalid one.
    pub fn try_regex(&self, pattern: &str) -> Result<Option<regex::Regex>, MemoryExceeded> {
        let mut c = self.regex_cache.lock().unwrap();
        if let Some(r) = c.get(pattern) {
            return Ok(r.clone());
        }
        if c.len() >= REGEX_CACHE {
            let n: usize = c.keys().map(|p| p.len() + REGEX_BYTES).sum();
            c.clear();
            self.release(n);
        }
        self.charge(pattern.len() + REGEX_BYTES, true)?;
        let r = anchored_regex(pattern);
        c.insert(pattern.to_string(), r.clone());
        Ok(r)
    }
}

/// Java `Matcher.matches()` semantics: the whole string must match.
pub(crate) fn anchored_regex(pattern: &str) -> Option<regex::Regex> {
    regex::Regex::new(&format!("^(?:{pattern})$")).ok()
}
