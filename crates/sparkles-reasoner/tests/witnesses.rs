//! Reasoner witnesses through the library (spec F18 §7.1): a run that asks for them keeps
//! them with the closure in the cache, an incremental run maintains them, and a run that
//! does not ask keeps none.

use oxrdf::{GraphName, NamedNode, Quad, Term, Triple};
use sparkles_core::incr::{Atom, FactKey, Witness};
use sparkles_core::store::{Store, StoreOptions};
use sparkles_reasoner::{
    Cache, Extras, GraphRef, Incremental, Inputs, Method, Profile, ReasonOptions, ReasonReport,
    materialize_incremental,
};

const EX: &str = "http://ex.org/";
const RDF: &str = "http://www.w3.org/1999/02/22-rdf-syntax-ns#";
const RDFS: &str = "http://www.w3.org/2000/01/rdf-schema#";

fn iri(l: &str) -> NamedNode {
    if l.starts_with("http") {
        NamedNode::new_unchecked(l)
    } else {
        NamedNode::new_unchecked(format!("{EX}{l}"))
    }
}

fn t(s: &str, p: &str, o: &str) -> Triple {
    Triple::new(iri(s), iri(p), Term::NamedNode(iri(o)))
}

fn sub() -> String {
    format!("{RDFS}subClassOf")
}

fn ty() -> String {
    format!("{RDF}type")
}

fn change(s: &Store, g: Option<&str>, add: &[Triple], del: &[Triple]) {
    let gn = match g {
        Some(g) => GraphName::NamedNode(iri(g)),
        None => GraphName::DefaultGraph,
    };
    let quad = |t: &Triple| {
        Quad::new(
            t.subject.clone(),
            t.predicate.clone(),
            t.object.clone(),
            gn.clone(),
        )
    };
    let mut txn = s.write();
    let mut labels = Default::default();
    for t in del {
        let q = txn.encode_quad(&quad(t), &mut labels).unwrap();
        txn.delete(q).unwrap();
    }
    for t in add {
        let q = txn.encode_quad(&quad(t), &mut labels).unwrap();
        txn.insert(q).unwrap();
    }
    txn.commit().unwrap();
}

fn run(s: &Store, since: Option<u64>, cache: &Cache, opts: &ReasonOptions) -> ReasonReport {
    materialize_incremental(
        s,
        &Profile::RdfsSimple,
        &Extras::default(),
        Incremental {
            since,
            cache: Some(cache),
        },
        opts,
    )
    .unwrap_or_else(|e| panic!("{e:#}"))
}

fn key(t: &Triple, g: Option<&str>) -> FactKey {
    let g = g.map(|g| Term::NamedNode(iri(g)));
    FactKey::of_terms(
        &t.subject.clone().into(),
        &t.predicate.clone().into(),
        &t.object,
        g.as_ref(),
    )
}

fn witness(ts: &[(&Triple, Option<&str>)]) -> Witness {
    Witness::new(ts.iter().map(|(t, g)| Atom::Fact(key(t, *g))).collect())
}

fn with(max: usize) -> ReasonOptions {
    ReasonOptions {
        witnesses: Some(max),
        ..Default::default()
    }
}

/// A fact with two derivations keeps both; removing one premise leaves the other, and
/// the dependents of a quad are the facts whose witnesses name it.
#[test]
fn witnesses_follow_incremental_runs() {
    let s = Store::in_memory(StoreOptions::default());
    let (a, b) = (t("i", &ty(), "A"), t("i", &ty(), "B"));
    let (ac, bc) = (t("A", &sub(), "C"), t("B", &sub(), "C"));
    change(
        &s,
        None,
        &[a.clone(), b.clone(), ac.clone(), bc.clone()],
        &[],
    );
    let cache = Cache::default();
    let r = run(&s, None, &cache, &with(8));
    let w = r.witnesses.expect("the run reports its witnesses");
    assert!(w.built && w.facts > 0 && w.truncated == 0, "{w:?}");
    let target = t("i", &ty(), "C");
    let got = cache.witnesses(&target).expect("witnesses are kept");
    assert_eq!(got.commit, r.receipt.as_ref().unwrap().commit.seq);
    assert!(!got.annotation.truncated);
    let mut want = vec![
        witness(&[(&a, None), (&ac, None)]),
        witness(&[(&b, None), (&bc, None)]),
    ];
    want.sort();
    assert_eq!(got.annotation.witnesses, want);
    // an asserted fact that no rule derives is its own witness
    let own = cache.witnesses(&ac).unwrap();
    assert_eq!(own.annotation.witnesses, vec![witness(&[(&ac, None)])]);
    let (_, deps) = cache.witness_dependents(&key(&a, None)).unwrap();
    assert!(deps.contains(&target), "{deps:?}");
    assert!(!deps.contains(&t("i", &ty(), "B")));

    let since = r.receipt.unwrap().commit.seq;
    change(&s, None, &[], std::slice::from_ref(&ac));
    let r = run(&s, Some(since), &cache, &with(8));
    assert_eq!(r.method, Method::Incremental, "{:?}", r.fallback);
    let w = r.witnesses.unwrap();
    assert!(!w.built, "{w:?}");
    let got = cache.witnesses(&target).unwrap();
    assert_eq!(
        got.annotation.witnesses,
        vec![witness(&[(&b, None), (&bc, None)])]
    );
    assert!(!got.annotation.truncated);
    let (_, deps) = cache.witness_dependents(&key(&a, None)).unwrap();
    assert!(!deps.contains(&target), "{deps:?}");

    // the last support goes, and the fact with it
    let since = r.receipt.unwrap().commit.seq;
    change(&s, None, &[], std::slice::from_ref(&bc));
    let r = run(&s, Some(since), &cache, &with(8));
    assert_eq!(r.method, Method::Incremental);
    assert!(cache.witnesses(&target).is_none());

    // and comes back with an insertion
    let since = r.receipt.unwrap().commit.seq;
    change(&s, None, std::slice::from_ref(&ac), &[]);
    run(&s, Some(since), &cache, &with(8));
    let got = cache.witnesses(&target).unwrap();
    assert_eq!(
        got.annotation.witnesses,
        vec![witness(&[(&a, None), (&ac, None)])]
    );
}

/// A bound of 1 keeps the smaller witness and says that others may exist.
#[test]
fn a_small_bound_truncates() {
    let s = Store::in_memory(StoreOptions::default());
    let facts = [
        t("i", &ty(), "A"),
        t("i", &ty(), "B"),
        t("A", &sub(), "C"),
        t("B", &sub(), "C"),
    ];
    change(&s, None, &facts, &[]);
    let cache = Cache::default();
    let r = run(&s, None, &cache, &with(1));
    assert!(r.witnesses.unwrap().truncated > 0);
    let got = cache.witnesses(&t("i", &ty(), "C")).unwrap();
    assert!(got.annotation.truncated);
    assert_eq!(got.annotation.witnesses.len(), 1);
}

/// Without the option no witnesses are kept, and turning it on or changing the bound
/// builds them again.
#[test]
fn witnesses_are_off_by_default() {
    let s = Store::in_memory(StoreOptions::default());
    change(&s, None, &[t("i", &ty(), "A"), t("A", &sub(), "C")], &[]);
    let cache = Cache::default();
    let r = run(&s, None, &cache, &ReasonOptions::default());
    assert!(r.witnesses.is_none());
    assert!(cache.witnesses(&t("i", &ty(), "C")).is_none());
    assert!(cache.witness_report().is_none());
    let since = r.receipt.unwrap().commit.seq;
    change(&s, None, &[t("j", &ty(), "A")], &[]);
    let r = run(&s, Some(since), &cache, &with(4));
    assert_eq!(r.method, Method::Incremental);
    assert!(r.witnesses.unwrap().built);
    assert!(cache.witnesses(&t("j", &ty(), "C")).is_some());
    let since = r.receipt.unwrap().commit.seq;
    change(&s, None, &[t("k", &ty(), "A")], &[]);
    let r = run(&s, Some(since), &cache, &with(2));
    assert!(r.witnesses.unwrap().built, "a new bound builds again");
    let since = r.receipt.unwrap().commit.seq;
    change(&s, None, &[t("l", &ty(), "A")], &[]);
    let r = run(&s, Some(since), &cache, &ReasonOptions::default());
    assert!(r.witnesses.is_none());
    assert!(cache.witnesses(&t("l", &ty(), "C")).is_none());
}

#[test]
fn bounds_are_checked() {
    let s = Store::in_memory(StoreOptions::default());
    let cache = Cache::default();
    for max in [0, sparkles_reasoner::MAX_WITNESSES_LIMIT + 1] {
        let e = materialize_incremental(
            &s,
            &Profile::RdfsSimple,
            &Extras::default(),
            Incremental {
                since: None,
                cache: Some(&cache),
            },
            &with(max),
        )
        .unwrap_err();
        assert!(e.to_string().contains("maxWitnesses"), "{e:#}");
    }
}

/// With two input graphs a witness names the quad of the smaller key among those that
/// hold the triple, and moves to the other graph's quad when that one goes.
#[test]
fn witnesses_name_quads_of_the_input_graphs() {
    let s = Store::in_memory(StoreOptions::default());
    let (g1, g2) = ("g1", "g2");
    let a = t("i", &ty(), "A");
    let ac = t("A", &sub(), "C");
    change(&s, Some(g1), &[a.clone(), ac.clone()], &[]);
    change(&s, Some(g2), std::slice::from_ref(&a), &[]);
    let opts = ReasonOptions {
        witnesses: Some(8),
        inputs: Inputs {
            data_graphs: vec![
                GraphRef::Named(iri(g1).into_string()),
                GraphRef::Named(iri(g2).into_string()),
            ],
            ..Default::default()
        },
        ..Default::default()
    };
    let cache = Cache::default();
    let r = run(&s, None, &cache, &opts);
    let target = t("i", &ty(), "C");
    let first = key(&a, Some(g1)).min(key(&a, Some(g2)));
    let other = key(&a, Some(g1)).max(key(&a, Some(g2)));
    let got = cache.witnesses(&target).unwrap();
    let mut want = vec![Atom::Fact(first), Atom::Fact(key(&ac, Some(g1)))];
    want.sort();
    assert_eq!(got.annotation.witnesses, vec![Witness::new(want)]);
    // the quad of the smaller key goes, and the triple stays in the other graph
    let gone = if first == key(&a, Some(g1)) { g1 } else { g2 };
    let since = r.receipt.unwrap().commit.seq;
    change(&s, Some(gone), &[], std::slice::from_ref(&a));
    let r = run(&s, Some(since), &cache, &opts);
    assert_eq!(r.method, Method::Incremental, "{:?}", r.fallback);
    let got = cache.witnesses(&target).unwrap();
    let mut want = vec![Atom::Fact(other), Atom::Fact(key(&ac, Some(g1)))];
    want.sort();
    assert_eq!(got.annotation.witnesses, vec![Witness::new(want)]);
    assert!(!got.annotation.truncated);
}
