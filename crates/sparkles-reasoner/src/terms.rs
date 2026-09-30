//! Term handling for the engine: classification and decoding of ids, plus a local
//! dictionary for terms that do not exist in the store yet (head constants, literals
//! computed by builtins, blank nodes from `makeTemp` / `makeSkolem`).

use oxrdf::{BlankNode, Literal, NamedNode, Term};
use rustc_hash::FxHashMap;
use sparkles::id::{self, Id, Tag};
use sparkles::sparql::value::Value;
use sparkles::store::Snapshot;
use std::sync::{Arc, Mutex, RwLock};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Kind {
    Iri,
    BNode,
    Literal,
    Other,
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
    terms: Vec<LocalTerm>,
}

/// Term context shared by all evaluation threads.
pub(crate) struct Terms {
    pub snap: Arc<Snapshot>,
    /// base vocabulary id range `[lo, hi)` holding literals
    lit_range: (u64, u64),
    locals: RwLock<Locals>,
    regex_cache: Mutex<FxHashMap<String, Option<regex::Regex>>>,
    pub rdf_first: u64,
    pub rdf_rest: u64,
    pub rdf_nil: u64,
    pub now: u64,
}

impl Terms {
    pub fn new(snap: Arc<Snapshot>) -> Terms {
        let lit_range = snap.generation.vocab.prefix_range(b"\"");
        let mut t = Terms {
            snap,
            lit_range,
            locals: RwLock::new(Locals::default()),
            regex_cache: Mutex::new(FxHashMap::default()),
            rdf_first: 0,
            rdf_rest: 0,
            rdf_nil: 0,
            now: 0,
        };
        let rdf = |l: &str| Term::NamedNode(NamedNode::new_unchecked(format!("{}{l}", crate::parser::RDF_NS)));
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

    /// Id of a term: inline, existing in the store, or a (new) local id.
    pub fn id_for(&self, t: &Term) -> u64 {
        if let Some(id) = id::inline_id(t) {
            return id.0;
        }
        if let Term::BlankNode(b) = t {
            if let Some(id) = self.snap.lookup_term(t) {
                return id.0;
            }
            // rule-level blank node constant: one local bnode per label
            let mut key = b"\0bn".to_vec();
            key.extend_from_slice(b.as_str().as_bytes());
            return self.local_keyed(key, LocalTerm::BNode);
        }
        let key = id::term_key(t);
        if let Some(id) = self.snap.lookup_key(&key) {
            return id.0;
        }
        self.local_keyed(key.clone(), LocalTerm::Key(key))
    }

    fn local_keyed(&self, key: Vec<u8>, term: LocalTerm) -> u64 {
        if let Some(&id) = self.locals.read().unwrap().map.get(&key) {
            return id;
        }
        let mut l = self.locals.write().unwrap();
        if let Some(&id) = l.map.get(&key) {
            return id;
        }
        let id = Id::local(l.terms.len() as u64).0;
        l.terms.push(term);
        l.map.insert(key, id);
        id
    }

    /// A fresh blank node (`makeTemp`).
    pub fn fresh_bnode(&self) -> u64 {
        let mut l = self.locals.write().unwrap();
        let id = Id::local(l.terms.len() as u64).0;
        l.terms.push(LocalTerm::BNode);
        id
    }

    /// A blank node determined by `args` (`makeSkolem`).
    pub fn skolem(&self, args: &[u64]) -> u64 {
        let mut key = b"\0sk".to_vec();
        for a in args {
            key.extend_from_slice(&a.to_le_bytes());
        }
        self.local_keyed(key, LocalTerm::BNode)
    }

    pub fn local(&self, id: u64) -> Option<LocalTerm> {
        let i = Id(id);
        (i.tag() == Tag::Local).then(|| self.locals.read().unwrap().terms.get(i.payload() as usize).cloned())?
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
                if p >= self.lit_range.0 && p < self.lit_range.1 {
                    Kind::Literal
                } else {
                    Kind::Iri
                }
            }
            Tag::Delta => match self.snap.key(i).as_deref().and_then(|k| k.first().copied()) {
                Some(b'<') => Kind::Iri,
                Some(b'"') => Kind::Literal,
                _ => Kind::Other,
            },
            Tag::Local => match self.local(id) {
                Some(LocalTerm::BNode) => Kind::BNode,
                Some(LocalTerm::Key(k)) if k.first() == Some(&b'<') => Kind::Iri,
                Some(LocalTerm::Key(_)) => Kind::Literal,
                None => Kind::Other,
            },
            _ => Kind::Other,
        }
    }

    pub fn term(&self, id: u64) -> Option<Term> {
        let i = Id(id);
        if i.tag() == Tag::Local {
            return Some(match self.local(id)? {
                LocalTerm::Key(k) => id::key_to_term(&k),
                LocalTerm::BNode => Term::BlankNode(BlankNode::new_unchecked(format!("r{:x}", i.payload()))),
            });
        }
        self.snap.term(i)
    }

    pub fn value(&self, id: u64) -> Option<Value> {
        self.term(id).map(|t| Value::from_term(&t))
    }

    /// Lexical form used by `strConcat` / `uriConcat` / `regex` (Jena `lex()`).
    pub fn lexical(&self, id: u64) -> Option<String> {
        Some(match self.term(id)? {
            Term::NamedNode(n) => n.into_string(),
            Term::BlankNode(b) => b.into_string(),
            Term::Literal(l) => l.value().to_string(),
        })
    }

    pub fn regex(&self, pattern: &str) -> Option<regex::Regex> {
        let mut c = self.regex_cache.lock().unwrap();
        c.entry(pattern.to_string())
            .or_insert_with(|| anchored_regex(pattern))
            .clone()
    }
}

/// Java `Matcher.matches()` semantics: the whole string must match.
pub(crate) fn anchored_regex(pattern: &str) -> Option<regex::Regex> {
    regex::Regex::new(&format!("^(?:{pattern})$")).ok()
}
