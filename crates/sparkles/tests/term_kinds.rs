//! Term kinds and IRI prefix ranges read from ids, for base-vocabulary and delta ids.

use oxrdf::{Literal, NamedNode, Term};
use sparkles::id::{Id, Tag};
use sparkles::io::{RdfFormat, Source};
use sparkles::store::{Store, StoreOptions, TermKind};

const TTL: &str = r#"
@prefix ex: <http://ex.org/> .
ex:a ex:p "x", "y"@en, 1, <<( ex:a ex:p ex:b )>>, _:b1 .
ex:b ex:p <http://ex.org/sub/1>, <http://ex.org/sub/2>, <http://other.org/z> .
"#;

fn store() -> Store {
    let s = Store::in_memory(StoreOptions::default());
    s.load(&[Source::from_bytes(
        TTL.as_bytes().to_vec(),
        RdfFormat::Turtle,
        None,
    )])
    .unwrap();
    s
}

fn iri(s: &str) -> Term {
    NamedNode::new_unchecked(s).into()
}

#[test]
fn kinds_of_stored_terms() {
    let store = store();
    // terms added by a later write have delta ids
    let mut txn = store.write();
    let (s, p) = (
        txn.intern(&iri("http://ex.org/sub/3")).unwrap(),
        txn.intern(&iri("http://ex.org/p")).unwrap(),
    );
    let lit = txn
        .intern(&Literal::new_simple_literal("new").into())
        .unwrap();
    txn.insert([s, p, lit, Id::DEFAULT_GRAPH]).unwrap();
    txn.commit().unwrap();
    let snap = store.snapshot();
    let kind = |t: &Term| snap.term_kind(snap.lookup_term(t).unwrap());
    assert_eq!(kind(&iri("http://ex.org/a")), TermKind::Iri);
    assert_eq!(
        kind(&Literal::new_simple_literal("x").into()),
        TermKind::Literal
    );
    assert_eq!(
        kind(&Literal::new_language_tagged_literal_unchecked("y", "en").into()),
        TermKind::Literal
    );
    assert_eq!(kind(&Literal::from(1).into()), TermKind::Literal);
    assert_eq!(kind(&iri("http://ex.org/sub/3")), TermKind::Iri);
    assert_eq!(
        snap.lookup_iri("http://ex.org/sub/3").unwrap().tag(),
        Tag::Delta
    );
    assert_eq!(
        kind(&Literal::new_simple_literal("new").into()),
        TermKind::Literal
    );
    // the triple term and the blank node, as objects of ex:a ex:p
    let a = snap.lookup_iri("http://ex.org/a").unwrap();
    let p = snap.lookup_iri("http://ex.org/p").unwrap();
    let mut kinds = Vec::new();
    snap.scan(sparkles::index::Perm::Spo, &[a.0, p.0], |c| {
        if let sparkles::store::Chunk::Row(k) = c {
            kinds.push(snap.term_kind(Id(k[2])));
        } else if let sparkles::store::Chunk::Block(b, s, e) = c {
            kinds.extend((s..e).map(|i| snap.term_kind(Id(b.key(i)[2]))));
        }
        Ok(true)
    })
    .unwrap();
    assert!(kinds.contains(&TermKind::Triple), "{kinds:?}");
    assert!(kinds.contains(&TermKind::BNode), "{kinds:?}");
    assert_eq!(snap.term_kind(Id::UNDEF), TermKind::Other);
    assert_eq!(snap.term_kind(Id::local(3)), TermKind::Other);
    assert_eq!(snap.term_kind(Id::DEFAULT_GRAPH), TermKind::Other);
}

#[test]
fn iri_prefix_ranges() {
    let store = store();
    let snap = store.snapshot();
    let (lo, hi) = snap.iri_prefix_range("http://ex.org/sub/");
    let inside = |s: &str| {
        let id = snap.lookup_iri(s).unwrap();
        assert_eq!(id.tag(), Tag::Vocab);
        lo <= id && id < hi
    };
    assert!(inside("http://ex.org/sub/1"));
    assert!(inside("http://ex.org/sub/2"));
    assert!(!inside("http://ex.org/a"));
    assert!(!inside("http://other.org/z"));
    let (lo, hi) = snap.iri_prefix_range("http://nothing.example/");
    assert_eq!(lo, hi);
}
