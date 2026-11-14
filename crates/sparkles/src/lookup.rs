//! Term ids for pattern reads, with a small memo of the base vocabulary's answers.
//!
//! A pattern read such as `contains` or `quads` on a subject looks up the id of each bound
//! term before it touches an index. The base vocabulary of a generation is a sorted
//! dictionary that is searched by comparing keys, which took more time than the index
//! probe of a point lookup. Programs repeat the same predicates, classes and subjects, so
//! each thread remembers what the base vocabulary answered for the keys it asked for last.
//!
//! The memo is exact. It maps a generation's unique id and a vocabulary key to the result
//! of `Vocab::find` on that generation's base vocabulary, which never changes while the
//! generation lives, and it remembers an absent key as well as a present one. The delta
//! vocabulary, which grows and which a failed commit can roll back, is read on every
//! lookup as before. A slot holds the whole key, so two keys with one hash never share an
//! answer.
//!
//! The memo costs each thread that reads patterns up to [`SLOTS`] slots of 56 bytes and
//! the key bytes they hold, about 100 KiB with keys of 50 bytes. A key longer than
//! [`MAX_KEY`] is not remembered, which bounds a thread's memo at about 300 KiB.

use crate::id::{self, Id};
use crate::store::Snapshot;
use oxrdf::TermRef;
use rustc_hash::FxHasher;
use std::cell::RefCell;
use std::hash::Hasher;

/// Slots of a thread's memo, a power of two.
const SLOTS: usize = 1024;
/// The longest key a slot keeps.
const MAX_KEY: usize = 256;

#[derive(Default)]
struct Slot {
    used: bool,
    /// the generation whose base vocabulary answered
    uid: u64,
    hash: u64,
    /// the key's index in that base vocabulary, or `None` when it is not there
    base: Option<u64>,
    key: Vec<u8>,
}

thread_local! {
    static MEMO: RefCell<Vec<Slot>> = const { RefCell::new(Vec::new()) };
    /// scratch for the key of the term being looked up
    static KEY: RefCell<Vec<u8>> = const { RefCell::new(Vec::new()) };
}

/// The index of `key` in the base vocabulary of the snapshot's generation.
fn base_index(snap: &Snapshot, key: &[u8]) -> Option<u64> {
    let find = || snap.generation.vocab.find(key).ok();
    if key.len() > MAX_KEY {
        return find();
    }
    let uid = snap.generation.uid;
    let mut hasher = FxHasher::default();
    hasher.write_u64(uid);
    hasher.write(key);
    let hash = hasher.finish();
    MEMO.with(|memo| {
        // The search below runs no code that reads patterns, so the memo is never borrowed
        // twice. If that ever changed, the lookup would search without the memo.
        let Ok(mut memo) = memo.try_borrow_mut() else {
            return find();
        };
        if memo.is_empty() {
            memo.resize_with(SLOTS, Slot::default);
        }
        // the high bits of an Fx hash vary most
        let slot = &mut memo[(hash >> 32) as usize & (SLOTS - 1)];
        if slot.used && slot.hash == hash && slot.uid == uid && slot.key == key {
            return slot.base;
        }
        let base = find();
        slot.used = true;
        slot.uid = uid;
        slot.hash = hash;
        slot.base = base;
        slot.key.clear();
        slot.key.extend_from_slice(key);
        base
    })
}

/// [`Snapshot::lookup_key`], with the base vocabulary's answer remembered.
fn key_id(snap: &Snapshot, key: &[u8]) -> Option<Id> {
    if let Some(i) = base_index(snap, key) {
        return Some(Id::vocab(i));
    }
    snap.generation
        .dvocab
        .find(key)
        .filter(|&i| i < snap.dvocab_len)
        .map(Id::delta)
}

/// Build a key in the thread's scratch buffer and look it up.
fn with_key(snap: &Snapshot, write: impl FnOnce(&mut Vec<u8>)) -> Option<Id> {
    KEY.with(|scratch| match scratch.try_borrow_mut() {
        Ok(mut key) => {
            key.clear();
            write(&mut key);
            let found = key_id(snap, &key);
            if key.capacity() > 4 * MAX_KEY {
                *key = Vec::new();
            }
            found
        }
        Err(_) => {
            let mut key = Vec::new();
            write(&mut key);
            key_id(snap, &key)
        }
    })
}

/// [`Snapshot::lookup_iri`] for a pattern read.
pub(crate) fn iri_id(snap: &Snapshot, iri: &str) -> Option<Id> {
    with_key(snap, |key| {
        key.push(b'<');
        key.extend_from_slice(iri.as_bytes());
    })
}

/// [`Snapshot::lookup_term`] for a pattern read, without owning the term.
pub(crate) fn term_id(snap: &Snapshot, term: TermRef<'_>) -> Option<Id> {
    match term {
        TermRef::NamedNode(n) => iri_id(snap, n.as_str()),
        TermRef::BlankNode(b) => crate::store::parse_bnode_label(b.as_str()),
        TermRef::Literal(l) => {
            if let Some(id) = id::inline_literal(l.value(), l.datatype().as_str()) {
                return Some(id);
            }
            with_key(snap, |key| id::write_literal_ref_key(l, key))
        }
        TermRef::Triple(_) => snap.lookup_term(&term.into_owned()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Dataset;
    use oxrdf::{GraphName, Literal, NamedNode, Quad, Term};

    fn iri(s: &str) -> NamedNode {
        NamedNode::new_unchecked(s)
    }

    /// The terms a lookup can be asked for: IRIs, literals of each kind, inline numbers,
    /// a blank node label and a triple term, present and absent.
    fn probes() -> Vec<Term> {
        let mut terms: Vec<Term> = Vec::new();
        for i in 0..40 {
            terms.push(iri(&format!("http://example.org/s{i}")).into());
            terms.push(Literal::new_simple_literal(format!("name {i}")).into());
            terms.push(
                Literal::new_language_tagged_literal_unchecked(format!("nom {i}"), "fr").into(),
            );
            terms.push(
                Literal::new_typed_literal(format!("v{i}"), iri("http://example.org/dt")).into(),
            );
            terms.push(Literal::from(i as i64).into());
        }
        terms.push(iri("http://example.org/absent").into());
        terms.push(Literal::new_simple_literal("absent").into());
        terms.push(Literal::new_simple_literal("x".repeat(MAX_KEY + 10)).into());
        terms.push(oxrdf::BlankNode::new_unchecked("b1f").into());
        terms.push(Term::Triple(Box::new(oxrdf::Triple::new(
            iri("http://example.org/s1"),
            iri("http://example.org/p"),
            Literal::new_simple_literal("name 1"),
        ))));
        terms
    }

    fn check(snap: &Snapshot) {
        for t in probes() {
            // twice, so that the second answer comes from the memo
            for _ in 0..2 {
                assert_eq!(term_id(snap, t.as_ref()), snap.lookup_term(&t), "{t}");
            }
            if let Term::NamedNode(n) = &t {
                assert_eq!(iri_id(snap, n.as_str()), snap.lookup_iri(n.as_str()), "{n}");
            }
        }
    }

    /// The memo answers as the snapshot does: for terms of the base vocabulary, for terms a
    /// later commit adds to the delta vocabulary, and after a compaction replaces the
    /// generation, when the same keys have other ids.
    #[test]
    fn memo_answers_as_the_snapshot_does() {
        let ds = Dataset::memory();
        let p = iri("http://example.org/p");
        let quads = |from: usize, to: usize| -> Vec<Quad> {
            let terms = probes();
            (from..to)
                .map(|i| {
                    Quad::new(
                        iri(&format!("http://example.org/s{}", i % 40)),
                        p.clone(),
                        terms[i].clone(),
                        GraphName::DefaultGraph,
                    )
                })
                .collect()
        };
        // nothing stored: every key is absent, and the memo remembers that
        check(&ds.store().snapshot());
        // a first half in the delta vocabulary: the remembered absences must not hide it
        ds.extend(quads(0, 100).iter().map(Quad::as_ref)).unwrap();
        check(&ds.store().snapshot());
        // a new generation with the terms in its base vocabulary
        ds.compact().unwrap();
        check(&ds.store().snapshot());
        // more terms in the delta vocabulary of that generation
        let old = ds.store().snapshot();
        ds.extend(quads(100, 200).iter().map(Quad::as_ref)).unwrap();
        check(&ds.store().snapshot());
        // the older snapshot still reads its own, shorter delta vocabulary
        check(&old);
        ds.compact().unwrap();
        check(&ds.store().snapshot());
        check(&old);
    }

    /// Two datasets hold the same keys under different ids, and a thread that reads both in
    /// turn gets each one's own.
    #[test]
    fn memo_keeps_generations_apart() {
        let a = Dataset::memory();
        let b = Dataset::memory();
        let p = iri("http://example.org/p");
        let add = |ds: &Dataset, names: &[&str]| {
            for n in names {
                ds.insert(Quad::new(iri(n), p.clone(), iri(n), GraphName::DefaultGraph).as_ref())
                    .unwrap();
            }
            ds.compact().unwrap();
        };
        add(
            &a,
            &[
                "http://example.org/a",
                "http://example.org/b",
                "http://example.org/c",
            ],
        );
        add(&b, &["http://example.org/c", "http://example.org/zz"]);
        for _ in 0..3 {
            for ds in [&a, &b] {
                let snap = ds.store().snapshot();
                for n in ["a", "b", "c", "zz", "none"] {
                    let n = format!("http://example.org/{n}");
                    assert_eq!(iri_id(&snap, &n), snap.lookup_iri(&n), "{n}");
                }
            }
        }
        let quad = |n: &str| Quad::new(iri(n), p.clone(), iri(n), GraphName::DefaultGraph);
        assert!(a.contains(quad("http://example.org/a").as_ref()).unwrap());
        assert!(!b.contains(quad("http://example.org/a").as_ref()).unwrap());
        assert!(b.contains(quad("http://example.org/zz").as_ref()).unwrap());
        assert!(!a.contains(quad("http://example.org/zz").as_ref()).unwrap());
    }
}
