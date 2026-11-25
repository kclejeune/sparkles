//! The id space of a circuit: the ids of the store's current generation, plus local ids
//! for terms the store does not hold, such as values computed by `BIND` or an aggregate
//! and the skolem blank nodes of a CONSTRUCT template.
//!
//! A term is always represented by the store's id when the snapshot of the latest pass
//! holds it, so equal terms have equal ids. When a later commit adds a term that the
//! circuit holds as a local id, [`Terms::absorbed`] reports the pair and the circuit
//! rewrites its state.

use crate::id::{self, Id, Tag};
use crate::sparql::viewsvc::PublishedBytes;
use crate::store::Snapshot;
use oxrdf::Term;
use parking_lot::Mutex;
use rustc_hash::FxHashMap;
use std::sync::atomic::AtomicU64;
use std::sync::{Arc, Weak};

/// Both the mutable dictionary and its immutable versions share this backing.
#[derive(Debug)]
struct Key {
    bytes: Arc<[u8]>,
    published: Mutex<Weak<PublishedBytes>>,
}

impl Key {
    fn retained_bytes(&self) -> u64 {
        (self.bytes.len() + std::mem::size_of::<Self>() + 32) as u64
    }

    fn freeze(self: &Arc<Self>, pool: Option<&Arc<AtomicU64>>) -> FrozenKey {
        let published = pool.map(|pool| {
            let mut last = self.published.lock();
            if let Some(held) = last.upgrade() {
                return held;
            }
            let held = Arc::new(PublishedBytes::new(
                pool,
                self.retained_bytes() + (std::mem::size_of::<PublishedBytes>() + 16) as u64,
            ));
            *last = Arc::downgrade(&held);
            held
        });
        FrozenKey {
            key: self.clone(),
            published,
        }
    }
}

#[derive(Debug)]
struct FrozenKey {
    key: Arc<Key>,
    // A live-only circuit may retain the same bytes after the last view detaches.
    // Only a published table owns a strong reference to the view-pool charge.
    published: Option<Arc<PublishedBytes>>,
}

#[derive(Clone, Debug, Default)]
pub struct Terms {
    // Drop the lookup table's aliases before releasing the keys' backing.
    index: FxHashMap<Arc<[u8]>, u64>,
    keys: Vec<Arc<Key>>,
    bytes: u64,
}

/// An immutable term table frozen with one view version so streaming cursors can decode
/// `Tag::Local` ids without touching the circuit's lock.
#[derive(Debug, Default)]
pub struct ViewTerms {
    keys: Arc<[FrozenKey]>,
    key_bytes: u64,
    published_key_bytes: u64,
    pub(crate) published: Option<PublishedBytes>,
}

impl ViewTerms {
    pub fn len(&self) -> usize {
        self.keys.len()
    }

    pub fn is_empty(&self) -> bool {
        self.keys.is_empty()
    }

    pub fn key_bytes(&self, index: usize) -> Option<&[u8]> {
        self.keys.get(index).map(|k| k.key.bytes.as_ref())
    }

    /// The term of an id of this version's space.
    pub fn term(&self, snap: &Snapshot, i: Id) -> Option<Term> {
        match i.tag() {
            Tag::Local => self.key_bytes(i.payload() as usize).map(id::key_to_term),
            Tag::Undef => None,
            _ => snap.term(i),
        }
    }

    /// The bytes owned by the table, excluding the shared key backings.
    pub(crate) fn table_bytes(&self) -> u64 {
        (std::mem::size_of::<Self>() + self.keys.len() * std::mem::size_of::<FrozenKey>() + 16)
            as u64
    }

    pub(crate) fn key_storage_bytes(&self) -> u64 {
        self.key_bytes
    }

    pub(crate) fn published_bytes(&self) -> u64 {
        self.published.as_ref().map_or(0, PublishedBytes::bytes) + self.published_key_bytes
    }

    /// The bytes an unshared term table keeps allocated on the heap.
    pub fn retained_bytes(&self) -> u64 {
        self.table_bytes() + self.key_bytes
    }
}

/// The heap estimate for decoding a vocabulary key into an owned term.
pub(super) fn decoded_key_heap_bytes(key: &[u8]) -> u64 {
    match key.first() {
        Some(b'(') => {
            let mut pos = 1;
            let mut bytes =
                (std::mem::size_of::<oxrdf::Triple>() + 3 * std::mem::size_of::<Term>()) as u64;
            for _ in 0..3 {
                let len = crate::vocab::read_varint(key, &mut pos) as usize;
                bytes = bytes.saturating_add(decoded_key_heap_bytes(&key[pos..pos + len]));
                pos += len;
            }
            bytes
        }
        Some(b'_') => 32,
        // The row's slots are already admitted. Encoded scalar keys cover
        // the value, language and datatype strings that decoding owns.
        _ => key.len() as u64,
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
        self.bytes
    }

    /// Projected bytes of freezing this term table into a [`ViewTerms`], excluding the
    /// shared local key backings already counted in [`Self::bytes`]. Matches
    /// `ViewTerms::retained_bytes() - ViewTerms::key_storage_bytes()` (or
    /// `ViewTerms::published_bytes() - ViewTerms::key_storage_bytes()` when `has_pool`).
    pub(crate) fn projected_frozen_bytes(&self, has_pool: bool) -> crate::error::Result<u64> {
        let keys = u64::try_from(self.keys.len())
            .map_err(|_| super::budget::budget_error(sparkles_budget::ParentRefused::Overflow))?;
        let table = (std::mem::size_of::<ViewTerms>() as u64)
            .checked_add(
                keys.checked_mul(std::mem::size_of::<FrozenKey>() as u64)
                    .ok_or_else(|| {
                        super::budget::budget_error(sparkles_budget::ParentRefused::Overflow)
                    })?,
            )
            .and_then(|n| n.checked_add(16))
            .ok_or_else(|| super::budget::budget_error(sparkles_budget::ParentRefused::Overflow))?;
        if !has_pool {
            return Ok(table);
        }
        let per_key_pub = (std::mem::size_of::<PublishedBytes>() + 16) as u64;
        table
            .checked_add(16)
            .and_then(|n| n.checked_add(keys.checked_mul(per_key_pub)?))
            .ok_or_else(|| super::budget::budget_error(sparkles_budget::ParentRefused::Overflow))
    }

    /// Freeze the current local term table into an immutable snapshot.
    pub fn snapshot(&self) -> ViewTerms {
        self.snapshot_in(None)
    }

    pub(crate) fn snapshot_in(&self, pool: Option<&Arc<AtomicU64>>) -> ViewTerms {
        if self.keys.is_empty() {
            return ViewTerms::default();
        }
        let mut key_bytes = 0;
        let mut published_key_bytes = 0;
        let keys = self
            .keys
            .iter()
            .map(|key| {
                let frozen = key.freeze(pool);
                key_bytes += key.retained_bytes();
                published_key_bytes += frozen.published.as_deref().map_or(0, PublishedBytes::bytes);
                frozen
            })
            .collect();
        ViewTerms {
            keys,
            key_bytes,
            published_key_bytes,
            published: None,
        }
    }

    /// The id of the term with vocabulary key `key`.
    pub fn intern_key(&mut self, snap: &Snapshot, key: &[u8]) -> Id {
        let t = id::key_to_term(key);
        self.intern(snap, &t)
    }

    /// Like [`intern_key`](Self::intern_key), pre-admitting any newly inserted local key
    /// via `reserve` before allocating and inserting it.
    pub(crate) fn intern_key_budgeted(
        &mut self,
        snap: &Snapshot,
        key: &[u8],
        mut reserve: impl FnMut(u64) -> crate::error::Result<()>,
    ) -> crate::error::Result<Id> {
        let t = id::key_to_term(key);
        if let Term::BlankNode(b) = &t
            && let Some(id) = crate::store::parse_bnode_label(b.as_str())
        {
            return Ok(id);
        }
        if let Some(i) = id::inline_id(&t) {
            return Ok(i);
        }
        let encoded = id::term_key(&t);
        if !matches!(t, Term::BlankNode(_))
            && let Some(i) = snap.lookup_key(&encoded)
        {
            return Ok(i);
        }
        if let Some(&n) = self.index.get(encoded.as_slice()) {
            return Ok(Id::local(n));
        }
        let added = encoded.len() as u64 * 2 + 48;
        reserve(added)?;
        Ok(self.local(encoded))
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
        let added = key.len() as u64 * 2 + 48;
        let bytes: Arc<[u8]> = key.into();
        self.keys.push(Arc::new(Key {
            bytes: bytes.clone(),
            published: Mutex::new(Weak::new()),
        }));
        self.index.insert(bytes, n);
        self.bytes = self.bytes.saturating_add(added);
        Id::local(n)
    }

    /// The term of an id of this space.
    pub fn term(&self, snap: &Snapshot, i: Id) -> Option<Term> {
        match i.tag() {
            Tag::Local => self
                .keys
                .get(i.payload() as usize)
                .map(|k| id::key_to_term(&k.bytes)),
            Tag::Undef => None,
            _ => snap.term(i),
        }
    }

    /// Decode an owned term only after admitting storage for its decoded representation.
    pub(crate) fn term_budgeted(
        &self,
        snap: &Snapshot,
        i: Id,
        lease: &mut super::budget::ScratchLease,
    ) -> crate::error::Result<Option<Term>> {
        if matches!(i.tag(), Tag::Undef | Tag::Special) {
            return Ok(None);
        }
        let mut scratch = lease.sibling();
        let mut decode = |key: &[u8]| {
            lease.try_reserve(decoded_key_heap_bytes(key))?;
            Ok::<_, crate::error::Error>(id::key_to_term(key))
        };
        match i.tag() {
            Tag::Local => self
                .keys
                .get(i.payload() as usize)
                .map(|key| decode(&key.bytes))
                .transpose(),
            Tag::Delta => snap
                .generation
                .dvocab
                .with(|v| v.get(i.payload()).map(decode).transpose()),
            Tag::Vocab => snap
                .generation
                .vocab
                .get_with_checked(i.payload(), |bytes| scratch.try_set(bytes as u64), decode)?
                .transpose(),
            _ => {
                lease.try_reserve(144)?;
                Ok(snap.term(i))
            }
        }
    }

    /// The vocabulary key of an id, which means the same term in every generation.
    pub fn key(&self, snap: &Snapshot, i: Id) -> Option<Vec<u8>> {
        match i.tag() {
            Tag::Local => self
                .keys
                .get(i.payload() as usize)
                .map(|k| k.bytes.to_vec()),
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
