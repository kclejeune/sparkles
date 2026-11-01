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
    // regression (eleventh review): the count is that of the kept witnesses
    assert_eq!(got.annotation.count, 1);
}

/// Regression (eleventh review of item 416): the input scans of the witness pass count
/// in the report.
#[test]
fn witness_scans_are_reported() {
    let s = Store::in_memory(StoreOptions::default());
    change(&s, None, &[t("i", &ty(), "A"), t("A", &sub(), "C")], &[]);
    let plain = run(&s, None, &Cache::default(), &ReasonOptions::default()).base_scans;
    let with_w = run(&s, None, &Cache::default(), &with(8)).base_scans;
    assert!(with_w > plain, "{with_w} {plain}");
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

fn run_with(
    s: &Store,
    profile: &Profile,
    since: Option<u64>,
    cache: &Cache,
    opts: &ReasonOptions,
) -> anyhow::Result<ReasonReport> {
    materialize_incremental(
        s,
        profile,
        &Extras::default(),
        Incremental {
            since,
            cache: Some(cache),
        },
        opts,
    )
}

/// Regression (review of item 416): an asserted fact that a head action also derives has
/// supports that no instance shows, so its witnesses are truncated, as are those of every
/// asserted fact under rules with head actions.
#[test]
fn head_actions_truncate_asserted_facts() {
    let s = Store::in_memory(StoreOptions::default());
    let first = format!("{RDF}first");
    let rest = format!("{RDF}rest");
    let nil = format!("{RDF}nil");
    let facts = [
        t("s", "list", "l"),
        t("l", &first, "b"),
        t("l", &rest, &nil),
        t("s", "p", "b"),
    ];
    change(&s, None, &facts, &[]);
    let rules = Profile::Rules(format!(
        "[m: (?s <{EX}list> ?l) -> listMapAsObject(?s, <{EX}p>, ?l)]"
    ));
    let cache = Cache::default();
    let r = run_with(&s, &rules, None, &cache, &with(8)).unwrap_or_else(|e| panic!("{e:#}"));
    assert!(r.witnesses.is_some());
    let got = cache.witnesses(&t("s", "p", "b")).unwrap();
    assert!(got.annotation.truncated, "{got:?}");
    assert!(
        got.annotation
            .witnesses
            .contains(&witness(&[(&facts[3], None)]))
    );
}

/// Regression (review of item 416): a run that does not keep witnesses, after one that
/// did, was charged for the kept witnesses before it dropped them, and failed under a
/// limit that the run without them fits.
#[test]
fn dropped_witnesses_are_not_charged() {
    let classes = ["A", "B", "C", "D", "E"];
    let mut facts: Vec<Triple> = (0..500).map(|i| t(&format!("i{i}"), &ty(), "A")).collect();
    for w in classes.windows(2) {
        facts.push(t(w[0], &sub(), w[1]));
    }
    // the peak of an incremental run without witnesses over a closure kept without them
    let plain = {
        let s = Store::in_memory(StoreOptions::default());
        change(&s, None, &facts, &[]);
        let cache = Cache::default();
        let r = run(&s, None, &cache, &ReasonOptions::default());
        change(&s, None, &[t("j", &ty(), "A")], &[]);
        let r = run(
            &s,
            Some(r.receipt.unwrap().commit.seq),
            &cache,
            &ReasonOptions::default(),
        );
        assert_eq!(r.method, Method::Incremental);
        r.peak_bytes as usize
    };
    let s = Store::in_memory(StoreOptions::default());
    change(&s, None, &facts, &[]);
    let cache = Cache::default();
    let r = run(&s, None, &cache, &with(8));
    let bytes = r.witnesses.unwrap().bytes as usize;
    assert!(bytes > plain, "{bytes} {plain}");
    change(&s, None, &[t("j", &ty(), "A")], &[]);
    let limit = ReasonOptions {
        max_memory: Some(plain + bytes / 2),
        ..Default::default()
    };
    let r = run_with(
        &s,
        &Profile::RdfsSimple,
        Some(r.receipt.unwrap().commit.seq),
        &cache,
        &limit,
    )
    .unwrap_or_else(|e| panic!("{e:#}"));
    assert_eq!(r.method, Method::Incremental, "{:?}", r.fallback);
    assert!(r.witnesses.is_none());
}

/// A run with witnesses under a limit that the run without them fits fails for their
/// memory instead of passing it.
#[test]
fn witnesses_fail_within_the_memory_limit() {
    let mut facts: Vec<Triple> = (0..500).map(|i| t(&format!("i{i}"), &ty(), "A")).collect();
    for w in ["A", "B", "C", "D", "E"].windows(2) {
        facts.push(t(w[0], &sub(), w[1]));
    }
    let s = Store::in_memory(StoreOptions::default());
    change(&s, None, &facts, &[]);
    let plain = run(&s, None, &Cache::default(), &ReasonOptions::default()).peak_bytes as usize;
    let opts = ReasonOptions {
        max_memory: Some(plain + plain / 4),
        witnesses: Some(8),
        ..Default::default()
    };
    let e = run_with(&s, &Profile::RdfsSimple, None, &Cache::default(), &opts).unwrap_err();
    assert!(
        e.downcast_ref::<sparkles_reasoner::MemoryLimitExceeded>()
            .is_some(),
        "{e:#}"
    );
}

/// Regression (second review of item 416): an update that discards witnesses, here every
/// witness of a chain whose quads move to a second input graph, kept charging their
/// memory until it finished, and failed under a limit that a full build fits.
#[test]
fn discarded_witnesses_release_their_memory() {
    let (g1, g2) = ("g1", "g2");
    let n = 40;
    let mut chain: Vec<Triple> = (0..n)
        .map(|k| t(&format!("C{k}"), &sub(), &format!("C{}", k + 1)))
        .collect();
    chain.push(t("i", &ty(), "C0"));
    let opts = |max_memory| ReasonOptions {
        witnesses: Some(8),
        max_memory,
        inputs: Inputs {
            data_graphs: vec![
                GraphRef::Named(iri(g1).into_string()),
                GraphRef::Named(iri(g2).into_string()),
            ],
            ..Default::default()
        },
        ..Default::default()
    };
    let s = Store::in_memory(StoreOptions::default());
    change(&s, Some(g1), &chain, &[]);
    change(&s, Some(g2), &chain, &[]);
    let cache = Cache::default();
    let r = run(&s, None, &cache, &opts(None));
    let peak = r.peak_bytes as usize;
    let w = r.witnesses.unwrap();
    assert_eq!(w.truncated, 0, "{w:?}");
    // the quads of the smaller keys go, so that nearly every witness changes
    let gone: Vec<Triple> = chain
        .iter()
        .filter(|q| key(q, Some(g1)) < key(q, Some(g2)))
        .cloned()
        .collect();
    let keep: Vec<Triple> = chain
        .iter()
        .filter(|q| key(q, Some(g1)) > key(q, Some(g2)))
        .cloned()
        .collect();
    change(&s, Some(g1), &[], &gone);
    change(&s, Some(g2), &[], &keep);
    let r = run(
        &s,
        Some(r.receipt.unwrap().commit.seq),
        &cache,
        // the update fits in 1.1 times the full build's peak once the discarded
        // witnesses are given back; before, it needed about 1.24 times that peak
        &opts(Some(peak + peak / 10)),
    );
    assert_eq!(r.method, Method::Incremental, "{:?}", r.fallback);
    let got = cache.witnesses(&t("i", &ty(), &format!("C{n}"))).unwrap();
    assert!(!got.annotation.truncated);
    assert_eq!(got.annotation.witnesses.len(), 1);
}

/// Regression (twelfth review of item 416): a rule that tests for absent data can derive
/// a fact from fewer facts than the closure holds, also when its test fails now, so the
/// facts it may derive are truncated.
#[test]
fn rules_with_negation_truncate_their_heads() {
    let s = Store::in_memory(StoreOptions::default());
    let facts = [
        t("s", "marker", "yes"),
        t("s", "blocked", "yes"),
        t("s", "target", "yes"),
    ];
    change(&s, None, &facts, &[]);
    let rules = Profile::Rules(format!(
        "[n: (?s <{EX}marker> <{EX}yes>) noValue(?s <{EX}blocked> <{EX}yes>) -> (?s <{EX}target> <{EX}yes>)]"
    ));
    let cache = Cache::default();
    run_with(&s, &rules, None, &cache, &with(8)).unwrap_or_else(|e| panic!("{e:#}"));
    let got = cache.witnesses(&facts[2]).unwrap();
    assert!(got.annotation.truncated, "{got:?}");

    let s = Store::in_memory(StoreOptions::default());
    let first = format!("{RDF}first");
    let rest = format!("{RDF}rest");
    let nil = format!("{RDF}nil");
    change(
        &s,
        None,
        &[
            t("s", "list", "l"),
            t("l", &first, "a"),
            t("l", &rest, &nil),
        ],
        &[],
    );
    let rules = Profile::Rules(format!(
        "[l: (?s <{EX}list> ?l) listNotContains(?l, <{EX}b>) -> (?s <{EX}target> <{EX}yes>)]"
    ));
    let cache = Cache::default();
    run_with(&s, &rules, None, &cache, &with(8)).unwrap_or_else(|e| panic!("{e:#}"));
    let got = cache.witnesses(&t("s", "target", "yes")).unwrap();
    assert!(got.annotation.truncated, "{got:?}");
}

/// Regression (thirteenth review of item 416): under a program with a negation, a subset
/// of the facts can derive a competing list cell and break a list that a support reads,
/// so list-reading instances keep no witnesses and their facts are truncated.
#[test]
fn list_witnesses_under_negation_are_truncated() {
    let s = Store::in_memory(StoreOptions::default());
    let rest = format!("{RDF}rest");
    let nil = format!("{RDF}nil");
    change(
        &s,
        None,
        &[
            t("s", "marker", "yes"),
            t("s", "seed", "yes"),
            t("l", &rest, &nil),
        ],
        &[],
    );
    let rules = Profile::Rules(format!(
        "[a: (?l <{RDF}rest> <{RDF}nil>) noValue(<{EX}s> <{EX}marker> <{EX}yes>) -> (?l <{RDF}first> <{EX}bad>)]
         [b: (<{EX}s> <{EX}seed> <{EX}yes>) -> (<{EX}l> <{RDF}first> <{EX}good>)]
         [c: (?l <{RDF}first> <{EX}good>), listContains(?l, <{EX}good>) -> (<{EX}s> <{EX}target> <{EX}yes>)]"
    ));
    let cache = Cache::default();
    run_with(&s, &rules, None, &cache, &with(8)).unwrap_or_else(|e| panic!("{e:#}"));
    let got = cache.witnesses(&t("s", "target", "yes")).unwrap();
    assert!(got.annotation.truncated, "{got:?}");
    assert!(got.annotation.witnesses.is_empty(), "{got:?}");
}

/// Regression (thirteenth review of item 416): an asserted default geometry can also be
/// derived from the feature's geometry, so with default geometries on it is truncated.
#[test]
fn asserted_default_geometries_are_truncated() {
    const GEO: &str = "http://www.opengis.net/ont/geosparql#";
    let s = Store::in_memory(StoreOptions::default());
    let has = format!("{GEO}hasGeometry");
    let default = format!("{GEO}hasDefaultGeometry");
    change(&s, None, &[t("f", &has, "g"), t("f", &default, "g")], &[]);
    let cache = Cache::default();
    let extras = Extras {
        geo_default_geometry: true,
        ..Extras::default()
    };
    materialize_incremental(
        &s,
        &Profile::RdfsSimple,
        &extras,
        Incremental {
            since: None,
            cache: Some(&cache),
        },
        &with(8),
    )
    .unwrap_or_else(|e| panic!("{e:#}"));
    let got = cache.witnesses(&t("f", &default, "g")).unwrap();
    assert!(got.annotation.truncated, "{got:?}");
}

/// Regression (fourteenth review of item 416): a negation can let a smaller set of facts
/// derive a fact through an intermediate the closure lacks, so a program with one keeps
/// every fact truncated.
#[test]
fn negation_truncates_through_absent_intermediates() {
    let s = Store::in_memory(StoreOptions::default());
    let facts = [t("x", "a", "yes"), t("x", "b", "yes"), t("x", "d", "yes")];
    change(&s, None, &facts, &[]);
    let rules = Profile::Rules(format!(
        "[n: (?x <{EX}a> <{EX}yes>), noValue(?x <{EX}b> <{EX}yes>) -> (?x <{EX}p> <{EX}yes>)]
         [p: (?x <{EX}p> <{EX}yes>) -> (?x <{EX}t> <{EX}yes>)]
         [d: (?x <{EX}d> <{EX}yes>) -> (?x <{EX}t> <{EX}yes>)]"
    ));
    let cache = Cache::default();
    run_with(&s, &rules, None, &cache, &with(8)).unwrap_or_else(|e| panic!("{e:#}"));
    let got = cache.witnesses(&t("x", "t", "yes")).unwrap();
    assert!(got.annotation.truncated, "{got:?}");
}

/// Regression (fourteenth review of item 416): a list that is not plain in the closure
/// can be plain in a subset of it, where a list builtin that fails now succeeds, so the
/// facts of list-reading rules are truncated while any list is tangled.
#[test]
fn tangled_lists_truncate_list_readers() {
    let s = Store::in_memory(StoreOptions::default());
    let first = format!("{RDF}first");
    let rest = format!("{RDF}rest");
    let nil = format!("{RDF}nil");
    change(
        &s,
        None,
        &[
            t("l", &first, "bad"),
            t("l", &rest, &nil),
            t("s", "seed", "yes"),
            t("s", "d", "yes"),
        ],
        &[],
    );
    let rules = Profile::Rules(format!(
        "[g: (<{EX}s> <{EX}seed> <{EX}yes>) -> (<{EX}l> <{RDF}first> <{EX}good>)]
         [c: (?l <{RDF}first> <{EX}good>), listContains(?l, <{EX}good>) -> (<{EX}s> <{EX}t> <{EX}yes>)]
         [d: (?s <{EX}d> <{EX}yes>) -> (?s <{EX}t> <{EX}yes>)]"
    ));
    let cache = Cache::default();
    run_with(&s, &rules, None, &cache, &with(8)).unwrap_or_else(|e| panic!("{e:#}"));
    let got = cache.witnesses(&t("s", "t", "yes")).unwrap();
    assert!(got.annotation.truncated, "{got:?}");
}

/// Regression (fifteenth review of item 416): a tangled list makes the program not
/// monotone also past its readers, through facts the closure lacks, so every fact is
/// truncated.
#[test]
fn tangled_lists_truncate_through_absent_intermediates() {
    let s = Store::in_memory(StoreOptions::default());
    let first = format!("{RDF}first");
    let rest = format!("{RDF}rest");
    let nil = format!("{RDF}nil");
    change(
        &s,
        None,
        &[
            t("l", &first, "bad"),
            t("l", &rest, &nil),
            t("s", "seed", "yes"),
            t("s", "d", "yes"),
        ],
        &[],
    );
    let rules = Profile::Rules(format!(
        "[g: (<{EX}s> <{EX}seed> <{EX}yes>) -> (<{EX}l> <{RDF}first> <{EX}good>)]
         [c: (?l <{RDF}first> <{EX}good>), listContains(?l, <{EX}good>) -> (<{EX}s> <{EX}p> <{EX}yes>)]
         [p: (?s <{EX}p> <{EX}yes>) -> (?s <{EX}t> <{EX}yes>)]
         [d: (?s <{EX}d> <{EX}yes>) -> (?s <{EX}t> <{EX}yes>)]"
    ));
    let cache = Cache::default();
    run_with(&s, &rules, None, &cache, &with(8)).unwrap_or_else(|e| panic!("{e:#}"));
    assert!(cache.witnesses(&t("s", "p", "yes")).is_none());
    let got = cache.witnesses(&t("s", "t", "yes")).unwrap();
    assert!(got.annotation.truncated, "{got:?}");
}

/// Regression (fifteenth review of item 416): a feature with fewer geometries gains a
/// default geometry, so with default geometries on the program is not monotone: every
/// fact is truncated, and list readers keep no witnesses.
#[test]
fn default_geometries_make_programs_non_monotone() {
    const GEO: &str = "http://www.opengis.net/ont/geosparql#";
    let s = Store::in_memory(StoreOptions::default());
    let has = format!("{GEO}hasGeometry");
    let first = format!("{RDF}first");
    let rest = format!("{RDF}rest");
    let nil = format!("{RDF}nil");
    change(
        &s,
        None,
        &[
            t("f", &has, "g1"),
            t("f", &has, "g2"),
            t("l", &first, "good"),
            t("l", &rest, &nil),
        ],
        &[],
    );
    let rules = Profile::Rules(format!(
        "[c: (<{EX}f> <{GEO}hasGeometry> <{EX}g1>), (?l <{RDF}first> <{EX}good>), listContains(?l, <{EX}good>) -> (<{EX}f> <{EX}t> <{EX}yes>)]"
    ));
    let cache = Cache::default();
    let extras = Extras {
        geo_default_geometry: true,
        ..Extras::default()
    };
    materialize_incremental(
        &s,
        &rules,
        &extras,
        Incremental {
            since: None,
            cache: Some(&cache),
        },
        &with(8),
    )
    .unwrap_or_else(|e| panic!("{e:#}"));
    let got = cache.witnesses(&t("f", "t", "yes")).unwrap();
    assert!(got.annotation.truncated, "{got:?}");
    assert!(got.annotation.witnesses.is_empty(), "{got:?}");
    let asserted = cache.witnesses(&t("f", &has, "g1")).unwrap();
    assert!(asserted.annotation.truncated, "{asserted:?}");
}
