//! The id space of a circuit: the ids of the store's current generation, plus local ids
//! for terms the store does not hold, such as values computed by `BIND` or an aggregate
//! and the skolem blank nodes of a CONSTRUCT template.
//!
//! A term is always represented by the store's id when the snapshot of the latest pass
//! holds it, so equal terms have equal ids. When a later commit adds a term that the
//! circuit holds as a local id, [`Terms::absorbed`] reports the pair and the circuit
//! rewrites its state.

use crate::id::{self, Id, Tag};
use crate::store::Snapshot;
use oxrdf::Term;
use rustc_hash::FxHashMap;
use std::sync::Arc;

#[derive(Clone, Debug, Default)]
pub struct Terms {
    keys: Vec<Arc<[u8]>>,
    index: FxHashMap<Arc<[u8]>, u64>,
}

/// An immutable term table frozen with one view version so streaming cursors can decode
/// `Tag::Local` ids without touching the circuit's lock.
#[derive(Clone, Debug, Default)]
pub struct ViewTerms {
    keys: Arc<[Arc<[u8]>]>,
}

impl ViewTerms {
    pub fn len(&self) -> usize {
        self.keys.len()
    }

    pub fn is_empty(&self) -> bool {
        self.keys.is_empty()
    }

    pub fn key_bytes(&self, index: usize) -> Option<&[u8]> {
        self.keys.get(index).map(|k| k.as_ref())
    }

    /// The term of an id of this version's space.
    pub fn term(&self, snap: &Snapshot, i: Id) -> Option<Term> {
        match i.tag() {
            Tag::Local => self
                .keys
                .get(i.payload() as usize)
                .map(|k| id::key_to_term(k)),
            Tag::Undef => None,
            _ => snap.term(i),
        }
    }

    /// The bytes an unshared term table keeps allocated on the heap.
    pub fn retained_bytes(&self) -> u64 {
        if self.keys.is_empty() {
            return 0;
        }
        let slice = self.keys.len() * std::mem::size_of::<Arc<[u8]>>() + 16;
        let keys: usize = self.keys.iter().map(|k| k.len() + 16).sum();
        (slice + keys) as u64
    }
}

impl Terms {
    pub fn len(&self) -> usize {
        self.index.len()
    }

    pub fn is_empty(&self) -> bool {
        self.index.is_empty()
    }

    pub fn key_count(&self) -> usize {
        self.keys.len()
    }

    pub fn bytes(&self) -> u64 {
        self.keys.iter().map(|k| k.len() as u64 + 64).sum()
    }

    /// Freeze the current local term table into an immutable snapshot.
    pub fn snapshot(&self) -> ViewTerms {
        if self.keys.is_empty() {
            return ViewTerms::default();
        }
        ViewTerms {
            keys: self.keys.iter().cloned().collect(),
        }
    }

    /// The id of the term with vocabulary key `key`.
    pub fn intern_key(&mut self, snap: &Snapshot, key: &[u8]) -> Id {
        let t = id::key_to_term(key);
        self.intern(snap, &t)
    }

    /// The id of `t`: the store's when `snap` holds it, else a local one.
    pub fn intern(&mut self, snap: &Snapshot, t: &Term) -> Id {
        if let Term::BlankNode(b) = t
            && let Some(id) = crate::store::parse_bnode_label(b.as_str())
        {
            return id;
        }
        if let Some(i) = id::inline_id(t) {
            return i;
        }
        let key = id::term_key(t);
        if !matches!(t, Term::BlankNode(_))
            && let Some(i) = snap.lookup_key(&key)
        {
            return i;
        }
        self.local(key)
    }

    fn local(&mut self, key: Vec<u8>) -> Id {
        if let Some(&n) = self.index.get(key.as_slice()) {
            return Id::local(n);
        }
        let n = self.keys.len() as u64;
        let key: Arc<[u8]> = key.into();
        self.keys.push(key.clone());
        self.index.insert(key, n);
        Id::local(n)
    }

    /// The term of an id of this space.
    pub fn term(&self, snap: &Snapshot, i: Id) -> Option<Term> {
        match i.tag() {
            Tag::Local => self
                .keys
                .get(i.payload() as usize)
                .map(|k| id::key_to_term(k)),
            Tag::Undef => None,
            _ => snap.term(i),
        }
    }

    /// The vocabulary key of an id, which means the same term in every generation.
    pub fn key(&self, snap: &Snapshot, i: Id) -> Option<Vec<u8>> {
        match i.tag() {
            Tag::Local => self.keys.get(i.payload() as usize).map(|k| k.to_vec()),
            Tag::Vocab | Tag::Delta => snap.key(i).map(|k| k.into_owned()),
            Tag::Undef => None,
            _ => snap.term(i).map(|t| id::term_key(&t)),
        }
    }

    /// The local ids whose terms `snap`'s delta vocabulary added since it held
    /// `from_len` terms, with the store's ids they become. They leave this space.
    pub fn absorbed(&mut self, snap: &Snapshot, from_len: u64) -> Vec<(Id, Id)> {
        let mut out = Vec::new();
        if self.index.is_empty() {
            return out;
        }
        for i in from_len..snap.dvocab_len {
            if let Some(k) = snap.generation.dvocab.get(i)
                && let Some(n) = self.index.remove(k.as_slice())
            {
                out.push((Id::local(n), Id::delta(i)));
            }
        }
        out
    }
}
