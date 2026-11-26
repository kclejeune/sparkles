//! Path search (`SERVICE path:search`, spec F07): the acceptance examples, and searches
//! on random graphs checked against paths enumerated here by brute force and against
//! the property paths of the same reachability.

use sparkles_core::access::{Caller, GraphAccess, Graphs, Limits, Protection, Rule, TripleRules};
use sparkles_core::io::{RdfFormat, Source};
use sparkles_core::sparql::{QueryOptions, query};
use sparkles_core::store::{Store, StoreOptions};
use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;
use std::time::{Duration, Instant};

const PREFIXES: &str = "PREFIX ex: <http://ex/> PREFIX path: <urn:x-sparkles:path#> ";

fn store_of(ttl: &str) -> Store {
    let s = Store::in_memory(StoreOptions::default());
    s.load(&[Source::from_bytes(
        format!("@prefix ex: <http://ex/> .\n{ttl}").into_bytes(),
        RdfFormat::Turtle,
        None,
    )])
    .unwrap();
    s
}

/// The local name of an `ex:` IRI, or the term as text.
fn short(t: &Option<oxrdf::Term>) -> String {
    match t {
        None => "-".into(),
        Some(oxrdf::Term::NamedNode(n)) => n
            .as_str()
            .strip_prefix("http://ex/")
            .unwrap_or(n.as_str())
            .to_string(),
        Some(oxrdf::Term::Literal(l)) => l.value().to_string(),
        Some(t) => t.to_string(),
    }
}

fn rows_with(s: &Store, q: &str, opts: &QueryOptions) -> Vec<BTreeMap<String, String>> {
    let r =
        query(s.snapshot(), &format!("{PREFIXES}{q}"), opts).unwrap_or_else(|e| panic!("{q}: {e}"));
    r.rows()
        .unwrap()
        .iter()
        .map(|row| {
            r.variables()
                .iter()
                .zip(row.values())
                .map(|(v, t)| (v.clone(), short(t)))
                .collect()
        })
        .collect()
}

fn rows(s: &Store, q: &str) -> Vec<BTreeMap<String, String>> {
    rows_with(s, q, &QueryOptions::default())
}

fn error(s: &Store, q: &str) -> String {
    match query(
        s.snapshot(),
        &format!("{PREFIXES}{q}"),
        &QueryOptions::default(),
    ) {
        Ok(_) => panic!("{q}: expected an error"),
        Err(e) => e.to_string(),
    }
}

/// Paths of a per-edge result as node sequences, by path index: rows with `?path`,
/// `?i`, `?s` and `?o` (forward edges).
fn node_paths(rows: &[BTreeMap<String, String>]) -> BTreeMap<String, Vec<String>> {
    let mut by: BTreeMap<String, BTreeMap<usize, (String, String)>> = BTreeMap::new();
    for r in rows {
        let i: usize = r["i"].parse().unwrap();
        by.entry(r["path"].clone())
            .or_default()
            .insert(i, (r["s"].clone(), r["o"].clone()));
    }
    by.into_iter()
        .map(|(k, edges)| {
            let mut nodes = vec![edges[&0].0.clone()];
            for (i, (s, o)) in &edges {
                assert_eq!(*s, *nodes.last().unwrap(), "edge {i} continues the path");
                nodes.push(o.clone());
            }
            (k, nodes)
        })
        .collect()
}

const GRAPH: &str = "
ex:a ex:p ex:b . ex:b ex:p ex:c . ex:c ex:p ex:d . ex:a ex:p ex:e . ex:e ex:p ex:d .
ex:d ex:p ex:a . ex:b ex:p ex:d . ex:x ex:q ex:y .
";

fn search(params: &str) -> String {
    format!(
        "SELECT ?path ?i ?s ?o WHERE {{ SERVICE path:search {{ [] {params} ; \
         path:pathIndex ?path ; path:edgeIndex ?i ; path:edgeSubject ?s ; path:edgeObject ?o }} }}"
    )
}

fn sequences(s: &Store, params: &str) -> BTreeSet<String> {
    node_paths(&rows(s, &search(params)))
        .into_values()
        .map(|n| n.join(" "))
        .collect()
}

#[test]
fn acceptance_examples() {
    let s = store_of(GRAPH);
    let pair = "path:source ex:a ; path:target ex:d ; path:predicate ex:p";
    // A1: one shortest path of length 2
    let one = sequences(&s, pair);
    assert_eq!(one.len(), 1);
    assert!(one.contains("a b d") || one.contains("a e d"), "{one:?}");
    // A2
    assert_eq!(
        sequences(&s, &format!("{pair} ; path:algorithm path:allShortest")),
        ["a b d", "a e d"].map(String::from).into()
    );
    // A3
    assert_eq!(
        sequences(
            &s,
            &format!("{pair} ; path:algorithm path:kShortest ; path:k 3")
        ),
        ["a b c d", "a b d", "a e d"].map(String::from).into()
    );
    // A4
    assert_eq!(
        sequences(
            &s,
            &format!("{pair} ; path:algorithm path:all ; path:maxLength 3")
        ),
        ["a b c d", "a b d", "a e d"].map(String::from).into()
    );
    // A5: the shortest cycle, and the empty path with minLength 0
    let cyc = sequences(
        &s,
        "path:source ex:a ; path:target ex:a ; path:predicate ex:p",
    );
    assert_eq!(cyc.len(), 1);
    assert!(
        cyc.contains("a b d a") || cyc.contains("a e d a"),
        "{cyc:?}"
    );
    let zero = rows(
        &s,
        "SELECT ?len ?i WHERE { SERVICE path:search { [] path:source ex:a ; path:target ex:a ; \
         path:predicate ex:p ; path:minLength 0 ; path:length ?len ; path:edgeIndex ?i } }",
    );
    assert_eq!(zero.len(), 1);
    assert_eq!((zero[0]["len"].as_str(), zero[0]["i"].as_str()), ("0", "-"));
    // A6
    assert!(
        sequences(
            &s,
            "path:source ex:x ; path:target ex:a ; path:predicate ex:p"
        )
        .is_empty()
    );
    assert!(
        sequences(
            &s,
            "path:source ex:x ; path:target ex:a ; path:direction path:both"
        )
        .is_empty()
    );
    // A7: each source reaches what ex:p+ reaches
    let found: BTreeSet<(String, String)> = rows(
        &s,
        "SELECT ?a ?b WHERE { VALUES ?a { ex:a ex:b } SERVICE path:search { \
         [] path:source ?a ; path:target ?b ; path:predicate ex:p } }",
    )
    .into_iter()
    .map(|r| (r["a"].clone(), r["b"].clone()))
    .collect();
    let reach: BTreeSet<(String, String)> = rows(
        &s,
        "SELECT ?a ?b WHERE { VALUES ?a { ex:a ex:b } ?a ex:p+ ?b }",
    )
    .into_iter()
    .map(|r| (r["a"].clone(), r["b"].clone()))
    .collect();
    assert_eq!(found, reach);
    assert!(
        found.contains(&("a".into(), "a".into())),
        "the cycle back to a"
    );
    // A10
    let e = error(
        &s,
        &search("path:source ex:a ; path:target ex:x ; path:maxVisited 2"),
    );
    assert!(e.contains("path:maxVisited"), "{e}");
}

#[test]
fn rows_per_path_and_per_edge() {
    let s = store_of(GRAPH);
    let per_path = rows(
        &s,
        "SELECT * WHERE { SERVICE path:search { [] path:source ex:a ; path:target ex:d ; \
         path:predicate ex:p ; path:algorithm path:allShortest ; path:length ?len ; path:cost ?c ; \
         path:pathIndex ?path } }",
    );
    assert_eq!(per_path.len(), 2);
    assert!(per_path.iter().all(|r| r["len"] == "2" && r["c"] == "2"));
    let ids: BTreeSet<&str> = per_path.iter().map(|r| r["path"].as_str()).collect();
    assert_eq!(ids, ["0", "1"].into());
    // the predicate of each edge, and the constants are not output columns
    let per_edge = rows(
        &s,
        "SELECT * WHERE { SERVICE path:search { [] path:source ex:a ; path:target ex:d ; \
         path:predicate ex:p ; path:edgePredicate ?p } }",
    );
    assert_eq!(per_edge.len(), 2);
    assert!(per_edge.iter().all(|r| r["p"] == "p" && r.len() == 1));
}

#[test]
fn directions() {
    let s = store_of(GRAPH);
    // backward: edges followed from object to subject, reported as stored
    let r = rows(
        &s,
        &search(
            "path:source ex:d ; path:target ex:a ; path:predicate ex:p ; path:direction path:backward ; path:algorithm path:allShortest",
        ),
    );
    let mut paths: BTreeMap<String, Vec<(String, String)>> = BTreeMap::new();
    for row in &r {
        paths
            .entry(row["path"].clone())
            .or_default()
            .push((row["s"].clone(), row["o"].clone()));
    }
    let got: BTreeSet<Vec<(String, String)>> = paths.into_values().collect();
    let want: BTreeSet<Vec<(String, String)>> = [
        vec![("b".into(), "d".into()), ("a".into(), "b".into())],
        vec![("e".into(), "d".into()), ("a".into(), "e".into())],
    ]
    .into();
    assert_eq!(got, want);
    // both ways: x reaches y and back
    let both = rows(
        &s,
        "SELECT ?len WHERE { SERVICE path:search { [] path:source ex:y ; path:target ex:x ; \
         path:direction path:both ; path:length ?len } }",
    );
    assert_eq!(both.len(), 1);
    assert_eq!(both[0]["len"], "1");
}

#[test]
fn sources_and_targets_from_the_group() {
    let s = store_of(GRAPH);
    // pairs from VALUES: each row gets its own pair's paths
    let r = rows(
        &s,
        "SELECT ?a ?b ?len WHERE { VALUES (?a ?b) { (ex:a ex:c) (ex:b ex:a) (ex:x ex:a) } \
         SERVICE path:search { [] path:source ?a ; path:target ?b ; path:predicate ex:p ; \
         path:length ?len } }",
    );
    let got: BTreeSet<(String, String, String)> = r
        .into_iter()
        .map(|r| (r["a"].clone(), r["b"].clone(), r["len"].clone()))
        .collect();
    assert_eq!(
        got,
        [
            ("a".into(), "c".into(), "2".into()),
            ("b".into(), "a".into(), "2".into())
        ]
        .into()
    );
    // a bound target and no source: the shortest path from every node that reaches it
    let r = rows(
        &s,
        "SELECT ?a ?len WHERE { BIND(ex:c AS ?b) SERVICE path:search { [] path:source ?a ; \
         path:target ?b ; path:predicate ex:p ; path:length ?len } }",
    );
    let got: BTreeMap<String, String> = r
        .into_iter()
        .map(|r| (r["a"].clone(), r["len"].clone()))
        .collect();
    assert_eq!(
        got,
        [("a", "2"), ("b", "1"), ("c", "4"), ("d", "3"), ("e", "4")]
            .map(|(a, b)| (a.to_string(), b.to_string()))
            .into()
    );
    // nothing binds either end
    let e = error(
        &s,
        "SELECT * WHERE { SERVICE path:search { [] path:source ?a ; path:target ?b } }",
    );
    assert!(e.contains("neither") || e.contains("binds neither"), "{e}");
    // a pattern of the group binds the target; a FILTER on the outputs
    let r = rows(
        &s,
        "SELECT ?b ?len WHERE { ?b ex:p ex:d . SERVICE path:search { [] path:source ex:a ; \
         path:target ?b ; path:predicate ex:p ; path:length ?len } FILTER(?len > 1) }",
    );
    let got: BTreeSet<(String, String)> = r
        .into_iter()
        .map(|r| (r["b"].clone(), r["len"].clone()))
        .collect();
    assert_eq!(got, [("c".to_string(), "2".to_string())].into());
}

#[test]
fn limits_and_errors() {
    let s = store_of(GRAPH);
    let r = rows(
        &s,
        "SELECT ?b WHERE { SERVICE path:search { [] path:source ex:a ; path:target ?b ; \
         path:predicate ex:p ; path:limit 2 } }",
    );
    assert_eq!(r.len(), 2);
    let r = rows(
        &s,
        "SELECT ?b WHERE { SERVICE path:search { [] path:source ex:a ; path:target ?b ; \
         path:predicate ex:p ; path:maxLength 1 } }",
    );
    let got: BTreeSet<&str> = r.iter().map(|r| r["b"].as_str()).collect();
    assert_eq!(got, ["b", "e"].into());
    for (q, want) in [
        ("path:source ex:a", "missing path:target"),
        (
            "path:source ex:a ; path:target ex:d ; path:bogus 1",
            "unknown parameter",
        ),
        (
            "path:source ex:a ; path:target ex:d ; path:algorithm path:kShortest",
            "path:k",
        ),
        (
            "path:source ex:a ; path:target ex:d ; path:algorithm path:all",
            "path:maxLength",
        ),
        (
            "path:source ex:a ; path:target ex:d ; path:minLength 2",
            "path:minLength",
        ),
        (
            "path:source ex:a ; path:target ex:d ; path:maxLength ?m",
            "constant",
        ),
        (
            "path:source ex:a ; path:source ex:b ; path:target ex:d",
            "twice",
        ),
        (
            "path:source ex:a ; path:target ex:d ; path:direction path:up",
            "direction",
        ),
    ] {
        let e = error(
            &s,
            &format!("SELECT * WHERE {{ SERVICE path:search {{ [] {q} }} }}"),
        );
        assert!(e.contains(want), "{q}: {e}");
    }
    let e = error(
        &s,
        "SELECT * WHERE { SERVICE path:search { [] path:source ex:a ; path:target ?t ; \
         path:algorithm path:kShortest ; path:k 2 } }",
    );
    assert!(e.contains("bound target"), "{e}");
}

#[test]
fn one_search_per_named_graph() {
    // GRAPH ?g around the call searches each named graph on its own
    let s = Store::in_memory(StoreOptions::default());
    s.load(&[Source::from_bytes(
        "<http://ex/a> <http://ex/p> <http://ex/b> <http://ex/g1> .
         <http://ex/b> <http://ex/p> <http://ex/c> <http://ex/g1> .
         <http://ex/a> <http://ex/p> <http://ex/c> <http://ex/g2> .
         <http://ex/b> <http://ex/p> <http://ex/c> <http://ex/g3> .\n"
            .as_bytes()
            .to_vec(),
        RdfFormat::NQuads,
        None,
    )])
    .unwrap();
    let r = rows(
        &s,
        "SELECT ?g ?len WHERE { GRAPH ?g { SERVICE path:search { [] path:source ex:a ; \
         path:target ex:c ; path:length ?len } } }",
    );
    let got: BTreeSet<(String, String)> = r
        .into_iter()
        .map(|r| (r["g"].clone(), r["len"].clone()))
        .collect();
    assert_eq!(
        got,
        [("g1", "2"), ("g2", "1")]
            .map(|(a, b)| (a.to_string(), b.to_string()))
            .into()
    );
    // the default graph is empty
    assert!(
        rows(
            &s,
            "SELECT * WHERE { SERVICE path:search { [] path:source ex:a ; path:target ex:c } }"
        )
        .is_empty()
    );
    // the union graph merges them
    let r = rows(
        &s,
        "SELECT ?len WHERE { GRAPH <urn:x-arq:UnionGraph> { SERVICE path:search { [] path:source ex:a ; \
         path:target ex:c ; path:algorithm path:all ; path:maxLength 3 ; path:length ?len } } }",
    );
    let mut lens: Vec<String> = r.into_iter().map(|r| r["len"].clone()).collect();
    lens.sort();
    assert_eq!(lens, ["1", "2"]);
}

/// Weights of very different sizes: a search from both ends adds a path's weights in two
/// orders, and in plain floating point the four unit weights of `s a b c t` vanish into
/// 1e16 when they are added to it one at a time. The costs are kept with twice the
/// precision, so the cheaper path `s q t` is still found (ROADMAP item 150).
#[test]
fn weights_of_mixed_magnitudes_from_both_ends() {
    let s = store_of(
        "ex:s ex:p ex:a {| ex:w 1 |} . ex:a ex:p ex:b {| ex:w 1 |} .
         ex:b ex:p ex:c {| ex:w 1 |} . ex:c ex:p ex:t {| ex:w 1e16 |} .
         ex:s ex:p ex:q {| ex:w 1e16 |} . ex:q ex:p ex:t {| ex:w 2 |} .
         ex:s ex:p ex:x {| ex:w 1e16 |} . ex:s ex:p ex:y {| ex:w 1e16 |} .",
    );
    for dir in ["path:forward", "path:both"] {
        let r = rows(
            &s,
            &format!(
                "SELECT ?path ?i ?s ?o ?c WHERE {{ SERVICE path:search {{ [] path:source ex:s ; \
                 path:target ex:t ; path:predicate ex:p ; path:direction {dir} ; path:weight ex:w ; \
                 path:cost ?c ; path:pathIndex ?path ; path:edgeIndex ?i ; path:edgeSubject ?s ; \
                 path:edgeObject ?o }} }}"
            ),
        );
        let paths = node_paths(&r);
        assert_eq!(paths.len(), 1, "{dir}");
        assert_eq!(paths.values().next().unwrap().join(" "), "s q t", "{dir}");
        assert_eq!(r[0]["c"].parse::<f64>().unwrap(), 1e16 + 2.0, "{dir}");
    }
}

/// Yen's candidates of mixed magnitudes: `s b t` costs M+9, while `s c a t` and `s c b a t`
/// cost M+8. The spurs from `c` and `b` round differently when a candidate's cost is the
/// root's plus the spur's in plain floating point, so candidates are ranked by their whole
/// cost in double-double (ROADMAP item 150).
#[test]
fn yen_candidates_of_mixed_magnitudes_rank_by_their_exact_cost() {
    let s = store_of(
        "ex:s ex:p ex:a {| ex:w 5 |} . ex:a ex:p ex:t {| ex:w 2 |} .
         ex:s ex:p ex:b {| ex:w 10000000000000004 |} . ex:b ex:p ex:a {| ex:w 1 |} .
         ex:b ex:p ex:t {| ex:w 5 |} .
         ex:s ex:p ex:c {| ex:w 10000000000000004 |} . ex:c ex:p ex:b {| ex:w 1 |} .
         ex:c ex:p ex:a {| ex:w 2 |} .",
    );
    let r = rows(
        &s,
        "SELECT ?path ?i ?s ?o WHERE { SERVICE path:search { [] path:source ex:s ; \
         path:target ex:t ; path:predicate ex:p ; path:algorithm path:kShortest ; path:k 3 ; \
         path:weight ex:w ; path:pathIndex ?path ; path:edgeIndex ?i ; path:edgeSubject ?s ; \
         path:edgeObject ?o } }",
    );
    let paths: Vec<String> = node_paths(&r).values().map(|p| p.join(" ")).collect();
    assert_eq!(paths.len(), 3, "{paths:?}");
    assert_eq!(paths[0], "s a t", "{paths:?}");
    assert_eq!(paths[1], "s b a t", "{paths:?}");
    assert!(
        paths[2] == "s c a t" || paths[2] == "s c b a t",
        "{paths:?}"
    );
}

/// Costs at the top of the floating-point range: a sum whose rounded part reaches past the
/// largest double while its rounding error brings it back under is still finite, so the
/// route through `y` beats the one through `x`, whose cost is about twice the largest double.
#[test]
fn weights_near_the_largest_double_from_both_ends() {
    let m = f64::MAX;
    let u = 2f64.powi(969);
    let w = |x: f64| format!("{x:e}");
    let s = store_of(&format!(
        "ex:s ex:p ex:a {{| ex:w {} |}} . ex:a ex:p ex:b {{| ex:w {} |}} .
         ex:b ex:p ex:x {{| ex:w {} |}} . ex:x ex:p ex:t {{| ex:w {} |}} .
         ex:b ex:p ex:y {{| ex:w {} |}} . ex:y ex:p ex:t {{| ex:w {} |}} .",
        w(m - 4.0 * u),
        w(2.0 * u),
        w(m - 4.0 * u),
        w(u),
        w(u),
        w(2.0 * u),
    ));
    let r = rows(
        &s,
        "SELECT ?path ?i ?s ?o WHERE { SERVICE path:search { [] path:source ex:s ; \
         path:target ex:t ; path:predicate ex:p ; path:weight ex:w ; path:pathIndex ?path ; \
         path:edgeIndex ?i ; path:edgeSubject ?s ; path:edgeObject ?o } }",
    );
    assert_eq!(
        node_paths(&r).values().next().unwrap().join(" "),
        "s a b y t"
    );
}

/// Two large halves met in the middle: the route through the `a` nodes costs about
/// `MAX + u/2` and the one through `b` about `MAX + u/4`. Both round to the largest
/// double, and only the rounding errors tell them apart.
#[test]
fn large_halves_near_the_largest_double_from_both_ends() {
    let m = f64::MAX;
    let u = 2f64.powi(969);
    let a = 2f64.powi(1023);
    let w = |x: f64| format!("{x:e}");
    let s = store_of(&format!(
        "ex:s ex:p ex:a1 {{| ex:w {} |}} . ex:a1 ex:p ex:a2 {{| ex:w {} |}} .
         ex:a2 ex:p ex:a3 {{| ex:w {} |}} . ex:a3 ex:p ex:t {{| ex:w {} |}} .
         ex:a1 ex:p ex:z {{| ex:w {} |}} .
         ex:s ex:p ex:b1 {{| ex:w {} |}} . ex:b1 ex:p ex:t {{| ex:w {} |}} .",
        w(a - 2.0 * u),
        w(u),
        w(1.5 * u),
        w(a - 4.0 * u),
        w(m),
        w(m - 4.0 * u),
        w(4.25 * u),
    ));
    let r = rows(
        &s,
        "SELECT ?path ?i ?s ?o WHERE { SERVICE path:search { [] path:source ex:s ; \
         path:target ex:t ; path:predicate ex:p ; path:weight ex:w ; path:pathIndex ?path ; \
         path:edgeIndex ?i ; path:edgeSubject ?s ; path:edgeObject ?o } }",
    );
    assert_eq!(node_paths(&r).values().next().unwrap().join(" "), "s b1 t");
}

#[test]
fn weights_from_reifiers() {
    // A8: the weights sit on the reifiers of the edges
    let s = store_of(
        "ex:a ex:p ex:b {| ex:w 1 |} . ex:b ex:p ex:d {| ex:w 5 |} .
         ex:a ex:p ex:e {| ex:w 1 |} . ex:e ex:p ex:d {| ex:w 1.5 |} .
         ex:a ex:p ex:f . ex:f ex:p ex:d .",
    );
    let q = |extra: &str| {
        format!(
            "SELECT ?path ?i ?s ?o ?c WHERE {{ SERVICE path:search {{ [] path:source ex:a ; \
             path:target ex:d ; path:predicate ex:p ; path:weight ex:w {extra} ; path:cost ?c ; \
             path:pathIndex ?path ; path:edgeIndex ?i ; path:edgeSubject ?s ; path:edgeObject ?o }} }}"
        )
    };
    let r = rows(&s, &q(""));
    let paths = node_paths(&r);
    assert_eq!(paths.len(), 1);
    // f's edges have the default weight 1: a f d costs 2, a e d 2.5
    assert_eq!(paths.values().next().unwrap().join(" "), "a f d");
    assert_eq!(r[0]["c"].parse::<f64>().unwrap(), 2.0);
    let dt = rows(
        &s,
        "SELECT (DATATYPE(?c) AS ?dt) WHERE { SERVICE path:search { [] path:source ex:a ; \
         path:target ex:d ; path:predicate ex:p ; path:weight ex:w ; path:cost ?c } }",
    );
    assert_eq!(dt[0]["dt"], "http://www.w3.org/2001/XMLSchema#double");
    let r = rows(&s, &q("; path:defaultWeight 10"));
    assert_eq!(node_paths(&r).values().next().unwrap().join(" "), "a e d");
    // k shortest by cost
    let r = rows(&s, &q("; path:algorithm path:kShortest ; path:k 3"));
    let mut by_cost: Vec<(f64, String)> = node_paths(&r)
        .into_iter()
        .map(|(k, n)| {
            let c: f64 = r.iter().find(|x| x["path"] == k).unwrap()["c"]
                .parse()
                .unwrap();
            (c, n.join(" "))
        })
        .collect();
    by_cost.sort_by(|a, b| a.0.total_cmp(&b.0));
    assert_eq!(
        by_cost.iter().map(|x| x.1.as_str()).collect::<Vec<_>>(),
        ["a f d", "a e d", "a b d"]
    );
    // a weight that is not a number
    let s = store_of("ex:a ex:p ex:b {| ex:w \"heavy\" |} .");
    let e = error(
        &s,
        "SELECT * WHERE { SERVICE path:search { [] path:source ex:a ; path:target ex:b ; path:weight ex:w } }",
    );
    assert!(e.contains("weight"), "{e}");
}

#[test]
fn all_shortest_over_zero_weight_edges() {
    // A-Z1: s -> m and s -> t cost 1, and m -> t costs 0. Both `s t` and `s m t` cost 1.
    // The dictionary numbers terms in sorted order, so the two names decide whether the
    // search settles m or t first.
    for (m, t) in [("a", "t"), ("z", "t"), ("t", "a"), ("t", "z")] {
        let (m, t) = (format!("n{m}"), format!("n{t}"));
        let edges = format!(
            "ex:s ex:p ex:{m} {{| ex:w 1 |}} . ex:s ex:p ex:{t} {{| ex:w 1 |}} . \
             ex:{m} ex:p ex:{t} {{| ex:w 0 |}} ."
        );
        let params = format!(
            "path:source ex:s ; path:target ex:{t} ; path:weight ex:w ; \
             path:algorithm path:allShortest"
        );
        let want: BTreeSet<String> = [format!("s {t}"), format!("s {m} {t}")].into();
        let s = store_of(&edges);
        assert_eq!(sequences(&s, &params), want, "{m} {t}");
        let costs = rows(
            &s,
            &format!("SELECT ?c WHERE {{ SERVICE path:search {{ [] {params} ; path:cost ?c }} }}"),
        );
        assert_eq!(costs.len(), 2, "{m} {t}");
        for r in costs {
            assert_eq!(r["c"].parse::<f64>().unwrap(), 1.0, "{m} {t}");
        }
        // A-Z2: a zero-weight edge back from t to m makes a cycle, which is not followed
        let s = store_of(&format!("{edges} ex:{t} ex:p ex:{m} {{| ex:w 0 |}} ."));
        assert_eq!(sequences(&s, &params), want, "{m} {t} with a cycle");
        // an unbound target gets every cheapest simple path to each node
        let r = rows(
            &s,
            "SELECT ?t ?path WHERE { SERVICE path:search { [] path:source ex:s ; \
             path:weight ex:w ; path:algorithm path:allShortest ; path:target ?t ; \
             path:pathIndex ?path } }",
        );
        let to_t: BTreeSet<&String> = r
            .iter()
            .filter(|x| x["t"] == t)
            .map(|x| &x["path"])
            .collect();
        assert_eq!(to_t.len(), 2, "{m} {t} with an unbound target");
    }
}

/// `s -> a -> t` of weight 1, and a zero-weight clique of `n` nodes joined to `t`.
fn clique_behind_target(n: usize) -> String {
    let mut ttl = String::from("ex:s ex:p ex:a {| ex:w 1 |} . ex:a ex:p ex:t {| ex:w 1 |} . ");
    for i in 0..n {
        ttl += &format!("ex:t ex:p ex:c{i} {{| ex:w 0 |}} . ex:c{i} ex:p ex:t {{| ex:w 0 |}} . ");
        for j in (0..n).filter(|&j| j != i) {
            ttl += &format!("ex:c{i} ex:p ex:c{j} {{| ex:w 0 |}} . ");
        }
    }
    ttl
}

#[test]
fn all_shortest_prunes_a_zero_weight_cycle_behind_the_target() {
    let s = store_of(&clique_behind_target(10));
    let params = "path:source ex:s ; path:target ex:t ; path:weight ex:w ; \
                  path:algorithm path:allShortest";
    let opts = QueryOptions {
        timeout: Some(Duration::from_secs(1)),
        ..Default::default()
    };
    let q = format!(
        "{PREFIXES}SELECT ?path WHERE {{ SERVICE path:search {{ [] {params} ; path:limit 2 ; \
         path:pathIndex ?path }} }}"
    );
    let r = query(s.snapshot(), &q, &opts).expect("the search finishes inside its deadline");
    assert_eq!(r.len(), 1);
    assert_eq!(sequences(&s, params), BTreeSet::from(["s a t".to_string()]));
    // with an unbound target the clique's paths are real output, and pruning keeps them
    let s = store_of(&clique_behind_target(4));
    let r = rows(
        &s,
        "SELECT ?t ?path WHERE { SERVICE path:search { [] path:source ex:s ; \
         path:weight ex:w ; path:algorithm path:allShortest ; path:target ?t ; \
         path:pathIndex ?path } }",
    );
    let mut per: BTreeMap<String, usize> = BTreeMap::new();
    for x in &r {
        *per.entry(x["t"].clone()).or_default() += 1;
    }
    assert_eq!(per["t"], 1);
    assert_eq!(per["a"], 1);
    for i in 0..4 {
        assert_eq!(per[&format!("c{i}")], 16, "c{i}");
    }
}

#[test]
fn all_shortest_path_enumeration_honours_the_timeout() {
    let s = store_of(&clique_behind_target(9));
    let opts = QueryOptions {
        timeout: Some(Duration::from_millis(200)),
        ..Default::default()
    };
    let started = Instant::now();
    let r = query(
        s.snapshot(),
        &format!(
            "{PREFIXES}SELECT (COUNT(*) AS ?n) WHERE {{ SERVICE path:search {{ [] \
             path:source ex:s ; path:weight ex:w ; path:algorithm path:allShortest ; \
             path:target ?t ; path:pathIndex ?path }} }}"
        ),
        &opts,
    );
    let err = r.err().expect("the enumeration is cut off").to_string();
    assert!(err.contains("timed out"), "{err}");
    assert!(
        started.elapsed() < Duration::from_secs(2),
        "{:?}",
        started.elapsed()
    );
}

/// A zero-weight chain `ex:n0 ... ex:n{len}` with an edge in each direction.
fn two_way_zero_weight_chain(len: usize) -> String {
    let mut ttl = String::new();
    for i in 0..len {
        ttl += &format!("ex:n{i} ex:p ex:n{} {{| ex:w 0 |}} . ", i + 1);
        ttl += &format!("ex:n{} ex:p ex:n{i} {{| ex:w 0 |}} . ", i + 1);
    }
    ttl
}

#[test]
fn zero_weight_chains_reconstruct_inside_the_deadline() {
    const LEN: usize = 2000;
    let s = store_of(&two_way_zero_weight_chain(LEN));
    let opts = QueryOptions {
        timeout: Some(Duration::from_secs(5)),
        ..Default::default()
    };
    let want: Vec<String> = (0..=LEN).map(|i| format!("n{i}")).collect();
    for algorithm in ["path:shortest", "path:allShortest"] {
        let r = rows_with(
            &s,
            &search(&format!(
                "path:source ex:n0 ; path:target ex:n{LEN} ; path:weight ex:w ; \
                 path:algorithm {algorithm}"
            )),
            &opts,
        );
        let paths = node_paths(&r);
        assert_eq!(paths.len(), 1, "{algorithm}");
        assert_eq!(paths.into_values().next().unwrap(), want, "{algorithm}");
    }
}

#[test]
fn all_shortest_charges_its_settling_order_to_the_memory_budget() {
    // a zero-weight star: with an unbound target both shortest modes settle every leaf,
    // and path:limit 1 stops the output after the first path
    let mut ttl = String::new();
    for i in 0..10_000 {
        ttl += &format!("ex:c ex:p ex:l{i} {{| ex:w 0 |}} . ");
    }
    let s = store_of(&ttl);
    let run = |algorithm: &str, memory: Option<u64>| {
        let opts = QueryOptions {
            max_memory_bytes: memory,
            ..Default::default()
        };
        query(
            s.snapshot(),
            &format!(
                "{PREFIXES}SELECT ?t WHERE {{ SERVICE path:search {{ [] path:source ex:c ; \
                 path:target ?t ; path:weight ex:w ; path:algorithm {algorithm} ; \
                 path:limit 1 }} }}"
            ),
            &opts,
        )
    };
    let shortest = run("path:shortest", None).unwrap();
    let peak = shortest.mem_peak_bytes;
    assert!(peak > 0);
    // the settling-order map that allShortest builds is held in memory, so the budget
    // that was enough for the shortest mode is not enough for it
    run("path:shortest", Some(peak)).expect("the shortest mode fits its own peak");
    match run("path:allShortest", Some(peak)) {
        Err(sparkles_core::Error::BudgetExceeded(b)) => {
            assert_eq!(b.kind, sparkles_core::BudgetKind::Memory);
        }
        r => panic!("expected a memory error: {:?}", r.map(|r| r.len())),
    }
    let all = run("path:allShortest", None).unwrap();
    assert!(
        all.mem_peak_bytes >= peak + 10_000 * 16,
        "{}",
        all.mem_peak_bytes
    );
}

#[test]
fn hidden_triples_are_not_edges() {
    // A9: a protection hides the triples of e, so the path through e is gone
    let s = store_of(&format!("{GRAPH} ex:e a ex:Hidden ."));
    let mut p = Protection {
        name: "hidden".into(),
        predicates: None,
        classes: Some(vec!["http://ex/Hidden".into()]),
        subclasses: true,
        graphs: None,
        pattern: None,
        prefixes: Default::default(),
        hide_inferences: false,
    };
    p.subclasses = false;
    let opts = QueryOptions {
        graphs: Some(Arc::new(GraphAccess::with_triples(
            Graphs::All,
            Graphs::All,
            TripleRules {
                rules: vec![Rule {
                    protection: Arc::new(p),
                    read: Graphs::none(),
                    write: Graphs::none(),
                }],
                caller: Caller::default(),
                limits: Limits::default(),
            },
        ))),
        ..Default::default()
    };
    let r = rows_with(
        &s,
        &search(
            "path:source ex:a ; path:target ex:d ; path:predicate ex:p ; path:algorithm path:allShortest",
        ),
        &opts,
    );
    let got: BTreeSet<String> = node_paths(&r).into_values().map(|n| n.join(" ")).collect();
    assert_eq!(got, ["a b d".to_string()].into());
}

// --------------------------------------------------------------- random graphs ------

/// A small deterministic generator (xorshift).
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
    fn below(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }
}

/// An edge in path order: (from, predicate, to, stored subject, stored object).
type E = (usize, usize, usize);

/// Every simple path (the last node may be the first) from `s` of at most `max` edges,
/// over the adjacency `adj` (from → [(predicate, to, triple)]).
fn brute(
    adj: &BTreeMap<usize, Vec<(usize, usize, E)>>,
    s: usize,
    max: usize,
) -> Vec<(usize, Vec<E>)> {
    let mut out = Vec::new();
    fn go(
        adj: &BTreeMap<usize, Vec<(usize, usize, E)>>,
        s: usize,
        x: usize,
        max: usize,
        on: &mut Vec<usize>,
        path: &mut Vec<E>,
        out: &mut Vec<(usize, Vec<E>)>,
    ) {
        if path.len() == max {
            return;
        }
        for &(_, y, t) in adj.get(&x).map(Vec::as_slice).unwrap_or(&[]) {
            if y == s {
                path.push(t);
                out.push((y, path.clone()));
                path.pop();
                continue;
            }
            if on.contains(&y) {
                continue;
            }
            path.push(t);
            out.push((y, path.clone()));
            on.push(y);
            go(adj, s, y, max, on, path, out);
            on.pop();
            path.pop();
        }
    }
    go(adj, s, s, max, &mut vec![s], &mut Vec::new(), &mut out);
    out
}

#[test]
fn random_graphs_against_brute_force() {
    let mut r = Rng(0x5eed_1234_abcd);
    for case in 0..6 {
        let n = 5 + r.below(4);
        let m = n + r.below(2 * n);
        let mut triples: BTreeSet<E> = BTreeSet::new();
        for _ in 0..m {
            triples.insert((r.below(n), r.below(2), r.below(n)));
        }
        let ttl: String = triples
            .iter()
            .map(|(a, p, b)| format!("ex:n{a} ex:p{p} ex:n{b} .\n"))
            .collect();
        let s = store_of(&ttl);
        for (dir, both, back) in [
            ("path:forward", false, false),
            ("path:backward", false, true),
            ("path:both", true, false),
        ] {
            // the adjacency in path order, edges as stored triples
            let mut adj: BTreeMap<usize, Vec<(usize, usize, E)>> = BTreeMap::new();
            for &(a, p, b) in &triples {
                let t = (a, p, b);
                if back || both {
                    adj.entry(b).or_default().push((p, a, t));
                }
                if !back && !(both && a == b) {
                    adj.entry(a).or_default().push((p, b, t));
                }
            }
            let max = 4;
            let src = r.below(n);
            let all = brute(&adj, src, max);
            let paths_of = |q: &str| -> BTreeMap<String, Vec<E>> {
                let mut by: BTreeMap<String, BTreeMap<usize, E>> = BTreeMap::new();
                for row in rows(&s, q) {
                    let num = |k: &str| {
                        row[k]
                            .trim_start_matches(|c: char| !c.is_ascii_digit())
                            .parse::<usize>()
                            .unwrap()
                    };
                    let (sv, pv, ov) = (num("s"), num("p"), num("o"));
                    by.entry(row["path"].clone())
                        .or_default()
                        .insert(row["i"].parse().unwrap(), (sv, pv, ov));
                }
                by.into_iter()
                    .map(|(k, v)| (k, v.into_values().collect()))
                    .collect()
            };
            let q = |target: &str, alg: &str| {
                format!(
                    "SELECT ?t ?path ?i ?s ?p ?o WHERE {{ SERVICE path:search {{ [] path:source ex:n{src} ; \
                     path:target {target} ; path:direction {dir} ; {alg} ; path:pathIndex ?path ; \
                     path:edgeIndex ?i ; path:edgeSubject ?s ; path:edgePredicate ?p ; path:edgeObject ?o }} }}"
                )
            };
            // all paths up to 4 edges, to every node
            let got: BTreeSet<Vec<E>> =
                paths_of(&q("?t", "path:algorithm path:all ; path:maxLength 4"))
                    .into_values()
                    .collect();
            let want: BTreeSet<Vec<E>> = all.iter().map(|(_, p)| p.clone()).collect();
            assert_eq!(got, want, "case {case} {dir}: all paths from n{src}");
            for t in 0..n {
                let to_t: Vec<&Vec<E>> = all
                    .iter()
                    .filter(|(e, _)| *e == t)
                    .map(|(_, p)| p)
                    .collect();
                let shortest = to_t.iter().map(|p| p.len()).min();
                let target = format!("ex:n{t}");
                let ctx = format!("case {case} {dir} n{src}→n{t}");
                // the shortest paths, if any is within 4 edges (longer ones may exist)
                let sp: BTreeSet<Vec<E>> = paths_of(&q(
                    &target,
                    "path:algorithm path:allShortest ; path:maxLength 4",
                ))
                .into_values()
                .collect();
                let want: BTreeSet<Vec<E>> = to_t
                    .iter()
                    .filter(|p| Some(p.len()) == shortest)
                    .map(|p| (*p).clone())
                    .collect();
                assert_eq!(sp, want, "{ctx}: all shortest");
                let one = paths_of(&q(
                    &target,
                    "path:algorithm path:shortest ; path:maxLength 4",
                ));
                assert_eq!(
                    one.len(),
                    usize::from(shortest.is_some()),
                    "{ctx}: one shortest"
                );
                if let Some(p) = one.values().next() {
                    assert!(want.contains(p), "{ctx}: {p:?} is a shortest path");
                }
                // k shortest: the lengths of the k best
                let k = 3;
                let ks = paths_of(&q(
                    &target,
                    &format!("path:algorithm path:kShortest ; path:k {k} ; path:maxLength 4"),
                ));
                let mut lens: Vec<usize> = ks.values().map(Vec::len).collect();
                lens.sort();
                let mut want_lens: Vec<usize> = to_t.iter().map(|p| p.len()).collect();
                want_lens.sort();
                want_lens.truncate(k);
                assert_eq!(lens, want_lens, "{ctx}: k shortest lengths");
                for p in ks.values() {
                    assert!(to_t.contains(&p), "{ctx}: {p:?} is a path");
                }
            }
            // unbounded shortest from the source matches `+` reachability
            if dir == "path:forward" {
                let reach: BTreeSet<String> = rows(
                    &s,
                    &format!("SELECT ?t WHERE {{ ex:n{src} (ex:p0|ex:p1)+ ?t }}"),
                )
                .into_iter()
                .map(|r| r["t"].clone())
                .collect();
                let found: BTreeSet<String> = rows(
                    &s,
                    &format!("SELECT ?t WHERE {{ SERVICE path:search {{ [] path:source ex:n{src} ; path:target ?t }} }}"),
                )
                .into_iter()
                .map(|r| r["t"].clone())
                .collect();
                assert_eq!(found, reach, "case {case}: reachability");
            }
        }
    }
}

#[test]
fn random_weighted_graphs_against_brute_force() {
    let mut r = Rng(0x0dd_ba11_7e57);
    for case in 0..40 {
        let n = 5 + r.below(3);
        let m = n + r.below(2 * n);
        let mut triples: BTreeMap<E, Option<usize>> = BTreeMap::new();
        for _ in 0..m {
            // 0 stands for an edge without a weight, 4 for a weight of zero (A-Z3)
            let w = r.below(5);
            let w = match w {
                0 => None,
                4 => Some(0),
                w => Some(w),
            };
            triples.insert((r.below(n), 0, r.below(n)), w);
        }
        let ttl: String = triples
            .iter()
            .map(|((a, _, b), w)| match w {
                Some(w) => format!("ex:n{a} ex:p0 ex:n{b} {{| ex:w {w} |}} .\n"),
                None => format!("ex:n{a} ex:p0 ex:n{b} .\n"),
            })
            .collect();
        let s = store_of(&ttl);
        let weight = |t: &E| triples[t].unwrap_or(1);
        let mut adj: BTreeMap<usize, Vec<(usize, usize, E)>> = BTreeMap::new();
        for &(a, p, b) in triples.keys() {
            adj.entry(a).or_default().push((p, b, (a, p, b)));
        }
        let src = r.below(n);
        let all = brute(&adj, src, n);
        for t in 0..n {
            let ctx = format!("case {case} n{src}→n{t}");
            let costs = |max: usize| -> Vec<usize> {
                let mut c: Vec<usize> = all
                    .iter()
                    .filter(|(e, p)| *e == t && p.len() <= max)
                    .map(|(_, p)| p.iter().map(weight).sum())
                    .collect();
                c.sort();
                c
            };
            let got = |extra: &str| -> Vec<usize> {
                let mut c: Vec<usize> = rows(
                    &s,
                    &format!(
                        "SELECT ?c WHERE {{ SERVICE path:search {{ [] path:source ex:n{src} ; path:target ex:n{t} ; \
                         path:weight ex:w ; path:cost ?c {extra} }} }}"
                    ),
                )
                .into_iter()
                .map(|r| r["c"].parse::<f64>().unwrap() as usize)
                .collect();
                c.sort();
                c
            };
            let best = costs(n).first().copied();
            assert_eq!(got("").first().copied(), best, "{ctx}: cheapest");
            let best4 = costs(4).first().copied();
            assert_eq!(
                got("; path:maxLength 4").first().copied(),
                best4,
                "{ctx}: cheapest within 4"
            );
            let mut k3 = costs(4);
            k3.truncate(3);
            assert_eq!(
                got("; path:algorithm path:kShortest ; path:k 3 ; path:maxLength 4"),
                k3,
                "{ctx}: 3 cheapest"
            );
            let ties_all = costs(n).iter().filter(|c| Some(**c) == best).count();
            assert_eq!(
                got("; path:algorithm path:allShortest").len(),
                ties_all,
                "{ctx}: all cheapest"
            );
            let ties = costs(4).iter().filter(|c| Some(**c) == best4).count();
            assert_eq!(
                got("; path:algorithm path:allShortest ; path:maxLength 4").len(),
                ties,
                "{ctx}: all cheapest within 4"
            );
        }
    }
}

/// The cheapest path and the k cheapest paths with weights, which search from both ends
/// (ROADMAP item 150), on random graphs in every direction, with skipped predicates and
/// hubs, against every simple path enumerated by brute force. Each path found must be one
/// of them, and the costs must be the cheapest in order.
#[test]
fn random_weighted_graphs_from_both_ends_against_brute_force() {
    let mut r = Rng(0x0b1d_1ec7_ed00);
    for case in 0..60 {
        let n = 6 + r.below(5);
        let m = n + r.below(2 * n);
        let mut triples: BTreeMap<E, Option<usize>> = BTreeMap::new();
        for _ in 0..m {
            let w = match r.below(6) {
                0 => None,
                1 => Some(0),
                w => Some(w - 1),
            };
            triples.insert((r.below(n), r.below(3), r.below(n)), w);
        }
        let ttl: String = triples
            .iter()
            .map(|((a, p, b), w)| match w {
                Some(w) => format!("ex:n{a} ex:p{p} ex:n{b} {{| ex:w {w} |}} .\n"),
                None => format!("ex:n{a} ex:p{p} ex:n{b} .\n"),
            })
            .collect();
        let s = store_of(&ttl);
        let weight = |t: &E| triples[t].unwrap_or(1);
        let skip_p2 = r.below(2) == 0;
        let max_degree = [None, Some(2), Some(3)][r.below(3)];
        let (dir, back, both) = [
            ("path:forward", false, false),
            ("path:backward", true, false),
            ("path:both", false, true),
        ][r.below(3)];
        let mut adj: BTreeMap<usize, Vec<(usize, usize, E)>> = BTreeMap::new();
        for &(a, p, b) in triples.keys() {
            if skip_p2 && p == 2 {
                continue;
            }
            let t = (a, p, b);
            if back || both {
                adj.entry(b).or_default().push((p, a, t));
            }
            if !back && !(both && a == b) {
                adj.entry(a).or_default().push((p, b, t));
            }
        }
        let degree = |x: usize| adj.get(&x).map_or(0, Vec::len);
        let src = r.below(n);
        // the nodes of a path in path order, from the stored triples
        let nodes_of = |path: &[E]| -> Vec<usize> {
            let mut out = vec![src];
            for &(a, _, b) in path {
                let x = *out.last().unwrap();
                let fwd = !back && a == x;
                out.push(if fwd || (both && a == x) { b } else { a });
            }
            out
        };
        let all: Vec<(usize, Vec<E>)> = brute(&adj, src, n)
            .into_iter()
            .filter(|(_, p)| {
                let ns = nodes_of(p);
                // a hub may be an end of a path but no node in between
                ns[1..ns.len() - 1]
                    .iter()
                    .all(|&x| max_degree.is_none_or(|l| degree(x) <= l))
            })
            .collect();
        let mut extra = format!("path:direction {dir}");
        if skip_p2 {
            extra.push_str(" ; path:excludePredicate ex:p2");
        }
        if let Some(l) = max_degree {
            extra.push_str(&format!(" ; path:maxDegree {l}"));
        }
        for t in 0..n {
            let ctx = format!("case {case} {extra} n{src}→n{t}");
            let want: BTreeSet<&Vec<E>> = all
                .iter()
                .filter(|(e, _)| *e == t)
                .map(|(_, p)| p)
                .collect();
            let mut costs: Vec<usize> = want.iter().map(|p| p.iter().map(weight).sum()).collect();
            costs.sort();
            let found = |alg: &str| -> Vec<(Vec<E>, usize)> {
                let mut by: BTreeMap<usize, (BTreeMap<usize, E>, f64)> = BTreeMap::new();
                for row in rows(
                    &s,
                    &format!(
                        "SELECT * WHERE {{ SERVICE path:search {{ [] path:source ex:n{src} ; \
                         path:target ex:n{t} ; path:weight ex:w ; {extra} ; {alg} path:cost ?c ; \
                         path:pathIndex ?path ; path:edgeIndex ?i ; path:edgeSubject ?s ; \
                         path:edgePredicate ?p ; path:edgeObject ?o }} }}"
                    ),
                ) {
                    let num = |k: &str| {
                        row[k]
                            .trim_start_matches(|c: char| !c.is_ascii_digit())
                            .parse::<usize>()
                            .unwrap()
                    };
                    let e = by
                        .entry(row["path"].parse().unwrap())
                        .or_insert_with(|| (BTreeMap::new(), row["c"].parse().unwrap()));
                    e.0.insert(row["i"].parse().unwrap(), (num("s"), num("p"), num("o")));
                }
                by.into_values()
                    .map(|(es, c)| {
                        let p: Vec<E> = es.into_values().collect();
                        let sum: usize = p.iter().map(weight).sum();
                        assert_eq!(c, sum as f64, "{ctx}: the cost of {p:?}");
                        (p, sum)
                    })
                    .collect()
            };
            let one = found("");
            assert_eq!(
                one.len(),
                usize::from(!costs.is_empty()),
                "{ctx}: one cheapest"
            );
            if let Some((p, c)) = one.first() {
                assert!(want.contains(p), "{ctx}: {p:?} is a path");
                assert_eq!(Some(c), costs.first(), "{ctx}: cheapest");
            }
            let k = 4;
            let ks = found(&format!("path:algorithm path:kShortest ; path:k {k} ;"));
            let mut got: Vec<usize> = ks.iter().map(|(_, c)| *c).collect();
            got.sort();
            let mut best = costs.clone();
            best.truncate(k);
            assert_eq!(got, best, "{ctx}: {k} cheapest");
            let distinct: BTreeSet<&Vec<E>> = ks.iter().map(|(p, _)| p).collect();
            assert_eq!(distinct.len(), ks.len(), "{ctx}: distinct paths");
            for (p, _) in &ks {
                assert!(want.contains(p), "{ctx}: {p:?} is a path");
            }
        }
    }
}

/// The nodes of a path given as stored triples, walked from `src`: `ex:p0` triples
/// forward and `ex:p1` triples backward.
fn walk(src: usize, path: &[E]) -> Vec<usize> {
    let mut nodes = vec![src];
    for &(s, p, o) in path {
        nodes.push(if p == 0 { o } else { s });
    }
    nodes
}

#[test]
fn directions_per_predicate_edges_as_triple_terms_and_nested_patterns() {
    let s = store_of(
        "ex:a ex:knows ex:b . ex:c ex:parent ex:b . ex:c ex:knows ex:d . ex:a ex:parent ex:d .",
    );
    // knows forward, parent backward: a → b (knows), b → c (c parent b), c → d (knows)
    let q = |preds: &str| {
        format!(
            "SELECT ?i ?s ?p ?o ?e WHERE {{ SERVICE path:search {{ [] path:source ex:a ; path:target ex:d ; \
             {preds} ; path:algorithm path:all ; path:maxLength 3 ; path:edgeIndex ?i ; \
             path:edgeSubject ?s ; path:edgePredicate ?p ; path:edgeObject ?o ; path:edge ?e }} }} ORDER BY ?i"
        )
    };
    let r = rows(&s, &q("path:predicate ex:knows, (ex:parent path:backward)"));
    let got: Vec<String> = r
        .iter()
        .map(|x| format!("{} {} {}", x["s"], x["p"], x["o"]))
        .collect();
    assert_eq!(got, ["a knows b", "c parent b", "c knows d"]);
    assert_eq!(
        r[1]["e"],
        "<<( <http://ex/c> <http://ex/parent> <http://ex/b> )>>"
    );
    // the list's direction wins over path:direction, which applies to the others
    let r = rows(
        &s,
        &q("path:predicate (ex:knows path:forward), ex:parent ; path:direction path:backward"),
    );
    assert_eq!(r.len(), 3);
    // the edge as a triple term joins with the data
    let r = rows(
        &s,
        "SELECT ?e WHERE { SERVICE path:search { [] path:source ex:a ; path:target ex:b ; \
         path:predicate ex:knows ; path:edge ?e } FILTER(isTRIPLE(?e) && SUBJECT(?e) = ex:a) }",
    );
    assert_eq!(r.len(), 1);
    // edges from a nested pattern: the same path, with its edges' ends
    let nested = |pattern: &str| {
        format!(
            "SELECT ?i ?s ?o WHERE {{ SERVICE path:search {{ [] path:source ex:a ; path:target ex:d ; \
             path:algorithm path:all ; path:maxLength 3 ; path:start ?x ; path:end ?y ; \
             path:edgeIndex ?i ; path:edgeSubject ?s ; path:edgeObject ?o . {pattern} }} }} ORDER BY ?i"
        )
    };
    for pattern in [
        "{ ?x ex:knows ?y } UNION { ?y ex:parent ?x }",
        "{ SELECT ?x ?y WHERE { { ?x ex:knows ?y } UNION { ?y ex:parent ?x } } }",
    ] {
        let r = rows(&s, &nested(pattern));
        let got: Vec<String> = r.iter().map(|x| format!("{} {}", x["s"], x["o"])).collect();
        assert_eq!(got, ["a b", "b c", "c d"], "{pattern}");
    }
    // a filter in the pattern removes edges
    let r = rows(
        &s,
        &nested("{ ?x ex:knows ?y } UNION { ?y ex:parent ?x } FILTER(?x != ex:b)"),
    );
    assert!(r.is_empty());
    // errors
    for (q, needle) in [
        (
            nested("").replace(" . }", " }"),
            "path:start and path:end name the variables of a nested pattern",
        ),
        (
            "SELECT * { SERVICE path:search { [] path:source ex:a ; path:target ?t . ?x ex:knows ?y } }"
                .to_string(),
            "needs path:start and path:end",
        ),
        (
            nested("{ ?x ex:knows ?y }")
                .replace("path:start ?x", "path:start ?x ; path:predicate ex:knows"),
            "cannot be combined",
        ),
        (
            nested("{ ?x ex:knows ?y }")
                .replace("path:edgeIndex ?i", "path:edge ?e ; path:edgeIndex ?i"),
            "path:edge needs edges from predicates",
        ),
        (
            nested("{ ?z ex:knows ?y }"),
            "path:start ?x is not a variable of the nested pattern",
        ),
        (q("path:predicate (ex:knows ex:parent)"), "unknown direction"),
        (
            q("path:predicate (ex:knows)"),
            "a list of an IRI and a direction",
        ),
        (
            q("path:predicate ex:knows, (ex:knows path:backward)"),
            "given twice",
        ),
    ] {
        let e = error(&s, &q);
        assert!(e.contains(needle), "{q}: {e}");
    }
}

/// The co-author example of USAGE.md: edges that are joins, in the block's own triples,
/// with a FILTER of the block.
#[test]
fn edges_from_a_join_in_the_block() {
    let s = store_of(
        "ex:p1 ex:authorOf ex:d1 . ex:p3 ex:authorOf ex:d1 . ex:p3 ex:authorOf ex:d2 . \
         ex:p2 ex:authorOf ex:d2 . ex:p4 ex:authorOf ex:d3 .",
    );
    let q = |target: &str| {
        format!(
            "SELECT ?len WHERE {{ SERVICE path:search {{ [] path:source ex:p1 ; path:target {target} ; \
             path:start ?x ; path:end ?y ; path:length ?len . \
             ?x ex:authorOf ?doc . ?y ex:authorOf ?doc . FILTER(?x != ?y) }} }}"
        )
    };
    assert_eq!(rows(&s, &q("ex:p2"))[0]["len"], "2");
    assert!(rows(&s, &q("ex:p4")).is_empty());
    // without the FILTER every author is an edge to itself, and a search from a node to
    // itself finds that cycle
    let r = rows(
        &s,
        "SELECT ?len WHERE { SERVICE path:search { [] path:source ex:p1 ; path:target ex:p1 ; \
         path:start ?x ; path:end ?y ; path:length ?len . \
         ?x ex:authorOf ?doc . ?y ex:authorOf ?doc } }",
    );
    assert_eq!(r[0]["len"], "1");
}

/// Directions per predicate and edges from a nested pattern on random graphs, against
/// paths enumerated by brute force: `ex:p0` forward and `ex:p1` backward.
#[test]
fn random_graphs_with_mixed_directions() {
    let mut r = Rng(0x00d1_2ec7_1015);
    for case in 0..8 {
        let n = 5 + r.below(4);
        let m = n + r.below(2 * n);
        let mut triples: BTreeSet<E> = BTreeSet::new();
        for _ in 0..m {
            triples.insert((r.below(n), r.below(2), r.below(n)));
        }
        let ttl: String = triples
            .iter()
            .map(|(a, p, b)| format!("ex:n{a} ex:p{p} ex:n{b} .\n"))
            .collect();
        let s = store_of(&ttl);
        let mut adj: BTreeMap<usize, Vec<(usize, usize, E)>> = BTreeMap::new();
        for &(a, p, b) in &triples {
            let (from, to) = if p == 0 { (a, b) } else { (b, a) };
            adj.entry(from).or_default().push((p, to, (a, p, b)));
        }
        let src = r.below(n);
        let want: BTreeSet<Vec<E>> = brute(&adj, src, 4).into_iter().map(|(_, p)| p).collect();
        let want_nodes: BTreeSet<Vec<usize>> = want.iter().map(|p| walk(src, p)).collect();
        let num = |x: &str| {
            x.trim_start_matches(|c: char| !c.is_ascii_digit())
                .parse::<usize>()
                .unwrap()
        };
        let by_path = |rs: Vec<BTreeMap<String, String>>, with_p: bool| {
            let mut by: BTreeMap<String, BTreeMap<usize, E>> = BTreeMap::new();
            for row in rs {
                let p = if with_p { num(&row["p"]) } else { 0 };
                by.entry(row["path"].clone()).or_default().insert(
                    row["i"].parse().unwrap(),
                    (num(&row["s"]), p, num(&row["o"])),
                );
            }
            by.into_values()
                .map(|v| v.into_values().collect::<Vec<E>>())
                .collect::<Vec<_>>()
        };
        let common = format!(
            "path:source ex:n{src} ; path:target ?t ; path:algorithm path:all ; path:maxLength 4 ; \
             path:pathIndex ?path ; path:edgeIndex ?i ; path:edgeSubject ?s ; path:edgeObject ?o"
        );
        let got: BTreeSet<Vec<E>> = by_path(
            rows(
                &s,
                &format!(
                    "SELECT * WHERE {{ SERVICE path:search {{ [] {common} ; path:edgePredicate ?p ; \
                     path:predicate ex:p0, (ex:p1 path:backward) }} }}"
                ),
            ),
            true,
        )
        .into_iter()
        .collect();
        assert_eq!(got, want, "case {case}: directions per predicate");
        // the nested pattern's edges are the (start, end) pairs: compare node sequences
        let got: BTreeSet<Vec<usize>> = by_path(
            rows(
                &s,
                &format!(
                    "SELECT * WHERE {{ SERVICE path:search {{ [] {common} ; path:start ?x ; path:end ?y . \
                     {{ ?x ex:p0 ?y }} UNION {{ ?y ex:p1 ?x }} }} }}"
                ),
            ),
            false,
        )
        .into_iter()
        .map(|p| {
            let mut nodes = vec![p[0].0];
            nodes.extend(p.iter().map(|e| e.2));
            nodes
        })
        .collect();
        assert_eq!(got, want_nodes, "case {case}: nested pattern");
    }
}

/// Item 275: a graph with hundreds of thousands of cheap long paths (length 7, cost 7)
/// and a few more expensive short paths within `path:maxLength`. Pareto-pruned search
/// over `(node, length)` pairs finds the optimal short paths and Yen's top-k within a
/// tight `path:maxVisited` budget without enumerating the cheap long paths.
#[test]
fn weighted_shortest_under_max_length_skips_many_cheap_long_paths() {
    // 6 intermediate layers of width 8 => 8^6 = 262,144 paths of length 7, cost 7.
    let mut ttl = String::new();
    let width = 8usize;
    let layers = 6usize;
    for j in 0..width {
        ttl += &format!("ex:s ex:p ex:l0_{j} {{| ex:w 1 |}} .\n");
        ttl += &format!("ex:l{}_{j} ex:p ex:t {{| ex:w 1 |}} .\n", layers - 1);
    }
    for layer in 0..(layers - 1) {
        for i in 0..width {
            for j in 0..width {
                ttl += &format!(
                    "ex:l{layer}_{i} ex:p ex:l{}_{j} {{| ex:w 1 |}} .\n",
                    layer + 1
                );
            }
        }
    }
    // Expensive short paths:
    // - len 2, cost 100: s -> m -> t
    // - len 3, cost 90 (two tied): s -> p1 -> q1 -> t and s -> p2 -> q2 -> t
    // - len 4, cost 80: s -> r1 -> r2 -> r3 -> t
    ttl += "ex:s ex:p ex:m {| ex:w 50 |} . ex:m ex:p ex:t {| ex:w 50 |} .\n";
    ttl += "ex:s ex:p ex:p1 {| ex:w 30 |} . ex:p1 ex:p ex:q1 {| ex:w 30 |} . ex:q1 ex:p ex:t {| ex:w 30 |} .\n";
    ttl += "ex:s ex:p ex:p2 {| ex:w 20 |} . ex:p2 ex:p ex:q2 {| ex:w 35 |} . ex:q2 ex:p ex:t {| ex:w 35 |} .\n";
    ttl += "ex:s ex:p ex:r1 {| ex:w 20 |} . ex:r1 ex:p ex:r2 {| ex:w 20 |} . \
            ex:r2 ex:p ex:r3 {| ex:w 20 |} . ex:r3 ex:p ex:t {| ex:w 20 |} .\n";
    let s = store_of(&ttl);

    let run = |extra: &str| -> Vec<(f64, String)> {
        let r = rows(
            &s,
            &format!(
                "SELECT ?path ?i ?s ?o ?c WHERE {{ SERVICE path:search {{ [] path:source ex:s ; \
                 path:target ex:t ; path:predicate ex:p ; path:weight ex:w ; path:maxVisited 500 ; \
                 {extra} ; path:cost ?c ; path:pathIndex ?path ; path:edgeIndex ?i ; \
                 path:edgeSubject ?s ; path:edgeObject ?o }} }}"
            ),
        );
        let mut by: BTreeMap<usize, (f64, String)> = BTreeMap::new();
        for (k, nodes) in node_paths(&r) {
            let cost: f64 = r.iter().find(|x| x["path"] == k).unwrap()["c"]
                .parse()
                .unwrap();
            by.insert(k.parse().unwrap(), (cost, nodes.join(" ")));
        }
        by.into_values().collect()
    };

    // No path of length <= 1 exists; finishes immediately within maxVisited 500.
    assert!(run("path:algorithm path:shortest ; path:maxLength 1").is_empty());
    assert!(run("path:algorithm path:allShortest ; path:maxLength 1").is_empty());
    assert!(run("path:algorithm path:kShortest ; path:k 3 ; path:maxLength 1").is_empty());

    // Shortest under maxLength 2, 3, 4.
    assert_eq!(
        run("path:algorithm path:shortest ; path:maxLength 2"),
        vec![(100.0, "s m t".into())]
    );
    let len3 = run("path:algorithm path:shortest ; path:maxLength 3");
    assert_eq!(len3.len(), 1);
    assert_eq!(len3[0].0, 90.0);
    assert!(
        len3[0].1 == "s p1 q1 t" || len3[0].1 == "s p2 q2 t",
        "{len3:?}"
    );
    assert_eq!(
        run("path:algorithm path:shortest ; path:maxLength 4"),
        vec![(80.0, "s r1 r2 r3 t".into())]
    );

    // allShortest under maxLength 3 finds both tied cost-90 paths.
    let mut all3 = run("path:algorithm path:allShortest ; path:maxLength 3");
    all3.sort_by(|a, b| a.1.cmp(&b.1));
    assert_eq!(
        all3,
        vec![(90.0, "s p1 q1 t".into()), (90.0, "s p2 q2 t".into()),]
    );

    // kShortest under maxLength 4 finds all 4 short paths in cost order (80, 90, 90, 100).
    let k4 = run("path:algorithm path:kShortest ; path:k 4 ; path:maxLength 4");
    assert_eq!(
        k4.iter().map(|(c, _)| *c).collect::<Vec<_>>(),
        vec![80.0, 90.0, 90.0, 100.0]
    );
    assert_eq!(k4[0].1, "s r1 r2 r3 t");
    assert_eq!(k4[3].1, "s m t");

    // Unbounded still finds a cheap length-7 path of cost 7.
    let unbounded = run("path:algorithm path:shortest");
    assert_eq!(unbounded.len(), 1);
    assert_eq!(unbounded[0].0, 7.0);
}

/// Edge cases for length-bounded weighted search (item 275): `maxLength 0` and `1`,
/// equal cost with different lengths, zero-weight edges and cycles, `minLength > 1` with
/// `kShortest`, unbound target/source in `path:shortest`, bad weight errors, and visit
/// limit enforcement.
#[test]
fn weighted_bounded_edge_cases() {
    // 1. maxLength 0 and 1, parallel edges, self-loops, equal cost / different lengths.
    let s = store_of(
        "ex:s ex:p ex:t {| ex:w 5 |} .
         ex:s ex:q ex:t {| ex:w 5 |} .
         ex:s ex:p ex:a {| ex:w 2 |} . ex:a ex:p ex:t {| ex:w 3 |} .
         ex:s ex:p ex:b {| ex:w 1 |} . ex:b ex:p ex:c {| ex:w 2 |} . ex:c ex:p ex:t {| ex:w 2 |} .
         ex:s ex:p ex:s {| ex:w 4 |} .
         ex:s ex:q ex:s {| ex:w 1.5 |} .",
    );
    // maxLength 0 with minLength 0 vs minLength 1
    let r = rows(
        &s,
        "SELECT ?len ?c WHERE { SERVICE path:search { [] path:source ex:s ; path:target ex:s ; \
         path:weight ex:w ; path:minLength 0 ; path:maxLength 0 ; path:length ?len ; path:cost ?c } }",
    );
    assert_eq!(r.len(), 1);
    assert_eq!((r[0]["len"].as_str(), r[0]["c"].as_str()), ("0", "0"));
    assert!(
        rows(
            &s,
            "SELECT ?len WHERE { SERVICE path:search { [] path:source ex:s ; path:target ex:s ; \
             path:weight ex:w ; path:maxLength 0 ; path:length ?len } }"
        )
        .is_empty()
    );
    assert!(
        rows(
            &s,
            "SELECT ?len WHERE { SERVICE path:search { [] path:source ex:s ; path:target ex:t ; \
             path:weight ex:w ; path:minLength 0 ; path:maxLength 0 ; path:length ?len } }"
        )
        .is_empty()
    );
    // maxLength 1 self-loop picks cheaper parallel self-loop (1.5 via ex:q)
    let r = rows(
        &s,
        "SELECT ?p ?c WHERE { SERVICE path:search { [] path:source ex:s ; path:target ex:s ; \
         path:weight ex:w ; path:maxLength 1 ; path:edgePredicate ?p ; path:cost ?c } }",
    );
    assert_eq!(r.len(), 1);
    assert_eq!(r[0]["p"], "q");
    assert_eq!(r[0]["c"].parse::<f64>().unwrap(), 1.5);

    // Equal cost (5) at lengths 1, 2, and 3 (over ex:p only):
    assert_eq!(
        sequences(
            &s,
            "path:source ex:s ; path:target ex:t ; path:predicate ex:p ; path:weight ex:w ; \
             path:algorithm path:allShortest ; path:maxLength 1"
        ),
        ["s t"].map(String::from).into()
    );
    assert_eq!(
        sequences(
            &s,
            "path:source ex:s ; path:target ex:t ; path:predicate ex:p ; path:weight ex:w ; \
             path:algorithm path:allShortest ; path:maxLength 2"
        ),
        ["s t", "s a t"].map(String::from).into()
    );
    assert_eq!(
        sequences(
            &s,
            "path:source ex:s ; path:target ex:t ; path:predicate ex:p ; path:weight ex:w ; \
             path:algorithm path:allShortest ; path:maxLength 3"
        ),
        ["s t", "s a t", "s b c t"].map(String::from).into()
    );

    // 2. Zero-weight edges and zero-weight cycle under maxLength.
    let z = store_of(
        "ex:s ex:p ex:a {| ex:w 0 |} . ex:a ex:p ex:b {| ex:w 0 |} .
         ex:b ex:p ex:a {| ex:w 0 |} . ex:b ex:p ex:c {| ex:w 0 |} .
         ex:c ex:p ex:t {| ex:w 0 |} .
         ex:s ex:p ex:x {| ex:w 5 |} . ex:x ex:p ex:t {| ex:w 5 |} .",
    );
    // maxLength 3 cannot reach the 4-hop zero-weight path -> returns cost-10 2-hop path.
    assert_eq!(
        sequences(
            &z,
            "path:source ex:s ; path:target ex:t ; path:predicate ex:p ; path:weight ex:w ; \
             path:maxLength 3"
        ),
        ["s x t"].map(String::from).into()
    );
    // maxLength 4 reaches the zero-weight path without looping on a <-> b.
    assert_eq!(
        sequences(
            &z,
            "path:source ex:s ; path:target ex:t ; path:predicate ex:p ; path:weight ex:w ; \
             path:maxLength 4"
        ),
        ["s a b c t"].map(String::from).into()
    );
    assert_eq!(
        sequences(
            &z,
            "path:source ex:s ; path:target ex:t ; path:predicate ex:p ; path:weight ex:w ; \
             path:algorithm path:allShortest ; path:maxLength 4"
        ),
        ["s a b c t"].map(String::from).into()
    );

    // 3. kShortest with minLength > 1 and maxLength: excludes cheap len-1 and len-5 paths.
    let km = store_of(
        "ex:s ex:p ex:t {| ex:w 1 |} .
         ex:s ex:p ex:a {| ex:w 5 |} . ex:a ex:p ex:t {| ex:w 5 |} .
         ex:s ex:p ex:b {| ex:w 3 |} . ex:b ex:p ex:c {| ex:w 3 |} . ex:c ex:p ex:t {| ex:w 3 |} .
         ex:s ex:p ex:d1 {| ex:w 0.1 |} . ex:d1 ex:p ex:d2 {| ex:w 0.1 |} .
         ex:d2 ex:p ex:d3 {| ex:w 0.1 |} . ex:d3 ex:p ex:d4 {| ex:w 0.1 |} .
         ex:d4 ex:p ex:t {| ex:w 0.1 |} .",
    );
    let r = rows(
        &km,
        "SELECT ?path ?c ?len WHERE { SERVICE path:search { [] path:source ex:s ; \
         path:target ex:t ; path:predicate ex:p ; path:weight ex:w ; \
         path:algorithm path:kShortest ; path:k 3 ; path:minLength 2 ; path:maxLength 3 ; \
         path:pathIndex ?path ; path:cost ?c ; path:length ?len } } ORDER BY ?path",
    );
    assert_eq!(r.len(), 2);
    assert_eq!(
        (r[0]["c"].parse::<f64>().unwrap(), r[0]["len"].as_str()),
        (9.0, "3")
    );
    assert_eq!(
        (r[1]["c"].parse::<f64>().unwrap(), r[1]["len"].as_str()),
        (10.0, "2")
    );

    // 4. Unbound target and unbound source in weighted path:shortest with maxLength.
    let u = store_of(
        "ex:s ex:p ex:a {| ex:w 1 |} . ex:a ex:p ex:b {| ex:w 1 |} . ex:b ex:p ex:t {| ex:w 1 |} .
         ex:s ex:p ex:t {| ex:w 10 |} . ex:t ex:p ex:s {| ex:w 2 |} .",
    );
    let fwd: BTreeMap<String, f64> = rows(
        &u,
        "SELECT ?t ?c WHERE { SERVICE path:search { [] path:source ex:s ; path:target ?t ; \
         path:predicate ex:p ; path:weight ex:w ; path:maxLength 2 ; path:cost ?c } }",
    )
    .into_iter()
    .map(|r| (r["t"].clone(), r["c"].parse().unwrap()))
    .collect();
    assert_eq!(
        fwd,
        BTreeMap::from([
            ("a".into(), 1.0),
            ("b".into(), 2.0),
            ("t".into(), 10.0),
            ("s".into(), 12.0),
        ])
    );
    let bwd: BTreeMap<String, f64> = rows(
        &u,
        "SELECT ?s ?c WHERE { SERVICE path:search { [] path:source ?s ; path:target ex:t ; \
         path:predicate ex:p ; path:weight ex:w ; path:maxLength 2 ; path:cost ?c } }",
    )
    .into_iter()
    .map(|r| (r["s"].clone(), r["c"].parse().unwrap()))
    .collect();
    assert_eq!(
        bwd,
        BTreeMap::from([
            ("b".into(), 1.0),
            ("a".into(), 2.0),
            ("s".into(), 10.0),
            ("t".into(), 12.0),
        ])
    );

    // 4b. Reverse Pareto fixture: s->x (0), x->a->t (0+0), x->t (5) with fixed target t and
    // maxLength 2 must keep the dearer shorter label at x so s reaches t at cost 5.
    let rev_pareto = store_of(
        "ex:s ex:p ex:x {| ex:w 0 |} . ex:x ex:p ex:a {| ex:w 0 |} . \
         ex:a ex:p ex:t {| ex:w 0 |} . ex:x ex:p ex:t {| ex:w 5 |} .",
    );
    for lim in ["", " ; path:limit 10"] {
        let got: BTreeMap<String, (f64, String)> = rows(
            &rev_pareto,
            &format!(
                "SELECT ?s ?c ?len WHERE {{ SERVICE path:search {{ [] path:source ?s ; \
                 path:target ex:t ; path:predicate ex:p ; path:weight ex:w ; path:maxLength 2{lim} ; \
                 path:cost ?c ; path:length ?len }} }}"
            ),
        )
        .into_iter()
        .map(|r| (r["s"].clone(), (r["c"].parse().unwrap(), r["len"].clone())))
        .collect();
        assert_eq!(
            got,
            BTreeMap::from([
                ("a".into(), (0.0, "1".into())),
                ("x".into(), (0.0, "2".into())),
                ("s".into(), (5.0, "2".into())),
            ])
        );
    }

    // 4c. Eager minLength 0 + path:limit 1 + maxVisited 1 returns the empty path without
    // opening the bounded search, for both unbound target and unbound source, and multi-target
    // VALUES short-circuits once path:limit is satisfied.
    let single_edge = store_of("ex:s ex:p ex:t . ex:t ex:p ex:u .");
    let fwd_empty = rows(
        &single_edge,
        "SELECT ?t ?len WHERE { SERVICE path:search { [] path:source ex:s ; path:target ?t ; \
         path:predicate ex:p ; path:weight ex:w ; path:minLength 0 ; path:maxLength 1 ; \
         path:limit 1 ; path:maxVisited 1 ; path:length ?len } }",
    );
    assert_eq!(fwd_empty.len(), 1);
    assert_eq!(
        (fwd_empty[0]["t"].as_str(), fwd_empty[0]["len"].as_str()),
        ("s", "0")
    );

    let bwd_empty = rows(
        &single_edge,
        "SELECT ?s ?len WHERE { SERVICE path:search { [] path:source ?s ; path:target ex:t ; \
         path:predicate ex:p ; path:weight ex:w ; path:minLength 0 ; path:maxLength 1 ; \
         path:limit 1 ; path:maxVisited 1 ; path:length ?len } }",
    );
    assert_eq!(bwd_empty.len(), 1);
    assert_eq!(
        (bwd_empty[0]["s"].as_str(), bwd_empty[0]["len"].as_str()),
        ("t", "0")
    );

    let multi_target_short_circuit = rows(
        &single_edge,
        "SELECT ?t ?len WHERE { VALUES ?t { ex:s ex:u } SERVICE path:search { \
         [] path:source ex:s ; path:target ?t ; path:predicate ex:p ; path:weight ex:w ; \
         path:minLength 0 ; path:maxLength 2 ; path:limit 1 ; path:maxVisited 1 ; path:length ?len } }",
    );
    assert_eq!(multi_target_short_circuit.len(), 1);
    assert_eq!(
        (
            multi_target_short_circuit[0]["t"].as_str(),
            multi_target_short_circuit[0]["len"].as_str()
        ),
        ("s", "0")
    );

    // 4d. Bounded eager kShortest stops once path:limit is filled by an empty path or a
    // prior target instead of opening Yen or advancing to the next target.
    let kshort_cycle = store_of("ex:s ex:p ex:a . ex:a ex:p ex:s . ex:s ex:p ex:u .");
    let kshort_empty_limit = rows(
        &kshort_cycle,
        "SELECT ?len WHERE { SERVICE path:search { [] path:source ex:s ; path:target ex:s ; \
         path:predicate ex:p ; path:weight ex:w ; path:algorithm path:kShortest ; path:k 2 ; \
         path:minLength 0 ; path:maxLength 2 ; path:limit 1 ; path:maxVisited 1 ; \
         path:length ?len } }",
    );
    assert_eq!(kshort_empty_limit.len(), 1);
    assert_eq!(kshort_empty_limit[0]["len"].as_str(), "0");

    let kshort_multi_target_limit = rows(
        &kshort_cycle,
        "SELECT ?t ?len WHERE { VALUES ?t { ex:s ex:u } SERVICE path:search { \
         [] path:source ex:s ; path:target ?t ; path:predicate ex:p ; path:weight ex:w ; \
         path:algorithm path:kShortest ; path:k 2 ; path:minLength 0 ; path:maxLength 2 ; \
         path:limit 1 ; path:maxVisited 1 ; path:length ?len } }",
    );
    assert_eq!(kshort_multi_target_limit.len(), 1);
    assert_eq!(
        (
            kshort_multi_target_limit[0]["t"].as_str(),
            kshort_multi_target_limit[0]["len"].as_str()
        ),
        ("s", "0")
    );

    let kshort_next_target_limit = rows(
        &kshort_cycle,
        "SELECT ?t ?len WHERE { VALUES ?t { ex:a ex:s } SERVICE path:search { \
         [] path:source ex:s ; path:target ?t ; path:predicate ex:p ; path:weight ex:w ; \
         path:algorithm path:kShortest ; path:k 1 ; path:minLength 1 ; path:maxLength 2 ; \
         path:limit 1 ; path:maxVisited 1 ; path:length ?len } }",
    );
    assert_eq!(kshort_next_target_limit.len(), 1);
    assert_eq!(
        (
            kshort_next_target_limit[0]["t"].as_str(),
            kshort_next_target_limit[0]["len"].as_str()
        ),
        ("a", "1")
    );

    // 5. allShortest with weight + maxLength still requires bound target / source.
    let e = error(
        &u,
        "SELECT ?t WHERE { SERVICE path:search { [] path:source ex:s ; path:target ?t ; \
         path:weight ex:w ; path:algorithm path:allShortest ; path:maxLength 2 } }",
    );
    assert!(e.contains("bound target"), "{e}");
    let e = error(
        &u,
        "SELECT ?s WHERE { SERVICE path:search { [] path:source ?s ; path:target ex:t ; \
         path:weight ex:w ; path:algorithm path:allShortest ; path:maxLength 2 } }",
    );
    assert!(e.contains("bound source"), "{e}");

    // 6. Negative weight fails under maxLength, and maxVisited fails when exceeded.
    let neg = store_of("ex:s ex:p ex:a {| ex:w -1 |} . ex:a ex:p ex:t {| ex:w 2 |} .");
    let e = error(
        &neg,
        "SELECT ?c WHERE { SERVICE path:search { [] path:source ex:s ; path:target ex:t ; \
         path:weight ex:w ; path:maxLength 2 ; path:cost ?c } }",
    );
    assert!(e.contains("weight"), "{e}");

    let e = error(
        &u,
        "SELECT ?c WHERE { SERVICE path:search { [] path:source ex:s ; path:target ex:t ; \
         path:predicate ex:p ; path:weight ex:w ; path:maxLength 4 ; path:maxVisited 1 ; path:cost ?c } }",
    );
    assert!(e.contains("path:maxVisited"), "{e}");
}

/// Differential test for item 275: random weighted graphs with zero weights, decimal
/// weights, parallel edges, self-loops, all three directions, excluded predicates, hubs,
/// and `maxLength` in 1..=5, compared against brute-force simple-path enumeration for
/// `path:shortest` (bound pairs, unbound target, unbound source), `path:allShortest`, and
/// `path:kShortest`.
#[test]
fn random_weighted_bounded_against_brute_force() {
    let mut r = Rng(0x0275_b0ed_cafe);
    let weights: [Option<f64>; 8] = [
        None,
        Some(0.0),
        Some(0.1),
        Some(0.2),
        Some(0.3),
        Some(1.0),
        Some(2.0),
        Some(5.0),
    ];
    let exact = |w: f64| -> i128 { (w * (1u64 << 60) as f64) as i128 };
    let rounded = |x: i128| -> f64 { x as f64 / (1u64 << 60) as f64 };
    for case in 0..45 {
        let n = 5 + r.below(4);
        let m = n + r.below(3 * n);
        let mut triples: BTreeMap<E, Option<f64>> = BTreeMap::new();
        for _ in 0..m {
            let w = weights[r.below(weights.len())];
            triples.insert((r.below(n), r.below(3), r.below(n)), w);
        }
        let ttl: String = triples
            .iter()
            .map(|((a, p, b), w)| match w {
                Some(w) => format!("ex:n{a} ex:p{p} ex:n{b} {{| ex:w {w:?}e0 |}} .\n"),
                None => format!("ex:n{a} ex:p{p} ex:n{b} .\n"),
            })
            .collect();
        let s = store_of(&ttl);
        let weight = |t: &E| exact(triples[t].unwrap_or(1.0));
        let skip_p2 = r.below(3) == 0;
        let max_degree = [None, None, Some(2), Some(3)][r.below(4)];
        let (dir, back, both) = [
            ("path:forward", false, false),
            ("path:backward", true, false),
            ("path:both", false, true),
        ][r.below(3)];
        let max_len = 1 + r.below(5);
        let mut adj: BTreeMap<usize, Vec<(usize, usize, E)>> = BTreeMap::new();
        for &(a, p, b) in triples.keys() {
            if skip_p2 && p == 2 {
                continue;
            }
            let t = (a, p, b);
            if back || both {
                adj.entry(b).or_default().push((p, a, t));
            }
            if !back && !(both && a == b) {
                adj.entry(a).or_default().push((p, b, t));
            }
        }
        let degree = |x: usize| adj.get(&x).map_or(0, Vec::len);
        let src = r.below(n);
        let nodes_of = |start: usize, path: &[E]| -> Vec<usize> {
            let mut out = vec![start];
            for &(a, _, b) in path {
                let x = *out.last().unwrap();
                let fwd = !back && a == x;
                out.push(if fwd || (both && a == x) { b } else { a });
            }
            out
        };
        let mut extra = format!("path:direction {dir} ; path:maxLength {max_len}");
        if skip_p2 {
            extra.push_str(" ; path:excludePredicate ex:p2");
        }
        if let Some(l) = max_degree {
            extra.push_str(&format!(" ; path:maxDegree {l}"));
        }
        let all_from_src: Vec<(usize, Vec<E>, i128)> = brute(&adj, src, max_len)
            .into_iter()
            .filter(|(_, p)| {
                let ns = nodes_of(src, p);
                ns[1..ns.len() - 1]
                    .iter()
                    .all(|&x| max_degree.is_none_or(|l| degree(x) <= l))
            })
            .map(|(end, p)| {
                let c = p.iter().map(weight).sum();
                (end, p, c)
            })
            .collect();

        // Check unbound target in weighted path:shortest + maxLength.
        let unbound_fwd: BTreeMap<usize, f64> = rows(
            &s,
            &format!(
                "SELECT ?t ?c WHERE {{ SERVICE path:search {{ [] path:source ex:n{src} ; \
                 path:target ?t ; path:weight ex:w ; {extra} ; path:cost ?c }} }}"
            ),
        )
        .into_iter()
        .map(|row| {
            let t = row["t"]
                .trim_start_matches(|c: char| !c.is_ascii_digit())
                .parse::<usize>()
                .unwrap();
            (t, row["c"].parse::<f64>().unwrap())
        })
        .collect();

        // Check unbound source in weighted path:shortest + maxLength against brute force.
        let tgt = r.below(n);
        let unbound_bwd: BTreeMap<usize, f64> = rows(
            &s,
            &format!(
                "SELECT ?s ?c WHERE {{ SERVICE path:search {{ [] path:source ?s ; \
                 path:target ex:n{tgt} ; path:weight ex:w ; {extra} ; path:cost ?c }} }}"
            ),
        )
        .into_iter()
        .map(|row| {
            let s_id = row["s"]
                .trim_start_matches(|c: char| !c.is_ascii_digit())
                .parse::<usize>()
                .unwrap();
            (s_id, row["c"].parse::<f64>().unwrap())
        })
        .collect();
        for s_i in 0..n {
            let bwd_ctx = format!("case {case} {extra} n{s_i}→n{tgt} (unbound source)");
            let least_bwd = brute(&adj, s_i, max_len)
                .into_iter()
                .filter(|(end, p)| {
                    *end == tgt && {
                        let ns = nodes_of(s_i, p);
                        ns[1..ns.len() - 1]
                            .iter()
                            .all(|&x| max_degree.is_none_or(|l| degree(x) <= l))
                    }
                })
                .map(|(_, p)| p.iter().map(weight).sum::<i128>())
                .min();
            assert_eq!(
                unbound_bwd.get(&s_i).copied(),
                least_bwd.map(rounded),
                "{bwd_ctx}"
            );
        }

        let extract_paths = |q: &str| -> Vec<(Vec<E>, f64)> {
            let mut by: BTreeMap<usize, (BTreeMap<usize, E>, f64)> = BTreeMap::new();
            for row in rows(&s, q) {
                let num = |k: &str| {
                    row[k]
                        .trim_start_matches(|c: char| !c.is_ascii_digit())
                        .parse::<usize>()
                        .unwrap()
                };
                let e = by
                    .entry(row["path"].parse().unwrap())
                    .or_insert_with(|| (BTreeMap::new(), row["c"].parse().unwrap()));
                e.0.insert(row["i"].parse().unwrap(), (num("s"), num("p"), num("o")));
            }
            by.into_values()
                .map(|(es, c)| (es.into_values().collect(), c))
                .collect()
        };

        for t in 0..n {
            let ctx = format!("case {case} {extra} n{src}→n{t}");
            let mut to_t: Vec<(&Vec<E>, i128)> = all_from_src
                .iter()
                .filter(|(e, _, _)| *e == t)
                .map(|(_, p, c)| (p, *c))
                .collect();
            to_t.sort_by_key(|&(_, c)| c);
            let least = to_t.first().map(|&(_, c)| c);
            let best_set: BTreeSet<&Vec<E>> = to_t
                .iter()
                .filter(|&&(_, c)| Some(c) == least)
                .map(|&(p, _)| p)
                .collect();

            // Unbound target matches exact least cost.
            assert_eq!(
                unbound_fwd.get(&t).copied(),
                least.map(rounded),
                "{ctx}: unbound target cost"
            );

            // Bound shortest
            let one = extract_paths(&format!(
                "SELECT * WHERE {{ SERVICE path:search {{ [] path:source ex:n{src} ; \
                 path:target ex:n{t} ; path:weight ex:w ; {extra} ; path:cost ?c ; \
                 path:pathIndex ?path ; path:edgeIndex ?i ; path:edgeSubject ?s ; \
                 path:edgePredicate ?p ; path:edgeObject ?o }} }}"
            ));
            assert_eq!(one.len(), usize::from(least.is_some()), "{ctx}: shortest");
            if let Some((p, c)) = one.first() {
                assert!(
                    best_set.contains(p),
                    "{ctx}: shortest {p:?} not in {best_set:?}"
                );
                assert_eq!(*c, rounded(least.unwrap()), "{ctx}: shortest cost");
            }

            // Bound allShortest
            let all_s = extract_paths(&format!(
                "SELECT * WHERE {{ SERVICE path:search {{ [] path:source ex:n{src} ; \
                 path:target ex:n{t} ; path:weight ex:w ; path:algorithm path:allShortest ; \
                 {extra} ; path:cost ?c ; path:pathIndex ?path ; path:edgeIndex ?i ; \
                 path:edgeSubject ?s ; path:edgePredicate ?p ; path:edgeObject ?o }} }}"
            ));
            let all_set: BTreeSet<&Vec<E>> = all_s.iter().map(|(p, _)| p).collect();
            assert_eq!(
                all_set.len(),
                all_s.len(),
                "{ctx}: duplicate in allShortest"
            );
            assert_eq!(all_set, best_set, "{ctx}: allShortest paths");

            // Bound kShortest (k = 4)
            let k = 4;
            let ks = extract_paths(&format!(
                "SELECT * WHERE {{ SERVICE path:search {{ [] path:source ex:n{src} ; \
                 path:target ex:n{t} ; path:weight ex:w ; path:algorithm path:kShortest ; \
                 path:k {k} ; {extra} ; path:cost ?c ; path:pathIndex ?path ; path:edgeIndex ?i ; \
                 path:edgeSubject ?s ; path:edgePredicate ?p ; path:edgeObject ?o }} }}"
            ));
            let want_costs: Vec<f64> = to_t.iter().take(k).map(|&(_, c)| rounded(c)).collect();
            let got_costs: Vec<f64> = ks.iter().map(|(_, c)| *c).collect();
            assert_eq!(got_costs, want_costs, "{ctx}: kShortest costs");
            let all_valid: BTreeSet<&Vec<E>> = to_t.iter().map(|&(p, _)| p).collect();
            let distinct: BTreeSet<&Vec<E>> = ks.iter().map(|(p, _)| p).collect();
            assert_eq!(distinct.len(), ks.len(), "{ctx}: kShortest distinct");
            for (p, _) in &ks {
                assert!(all_valid.contains(p), "{ctx}: kShortest invalid path {p:?}");
            }
        }
    }
}

/// Regression for Sol r1 P2: with wide finite weights where bidirectional split sums round
/// differently from source-order sums, bounded pair shortest and Yen must fall back to
/// source-order accumulation and return the strictly cheaper cycle P before cycle Q.
#[test]
fn weighted_bounded_wide_weight_cycle_ranking() {
    let m = 2f64.powi(-53);
    let d = 2f64.powi(-105);
    let e = d / 2.0;
    let delta = d / 4.0;
    let dbl = |w: f64| format!("\"{w:?}\"^^<http://www.w3.org/2001/XMLSchema#double>");
    let ttl = format!(
        "ex:s ex:p ex:p1 {{| ex:w {} |}} . ex:p1 ex:p ex:p2 {{| ex:w {} |}} . \
         ex:p2 ex:p ex:p3 {{| ex:w {} |}} . ex:p3 ex:p ex:s {{| ex:w {} |}} . \
         ex:s ex:p ex:q1 {{| ex:w {} |}} . ex:q1 ex:p ex:q2 {{| ex:w {} |}} . \
         ex:q2 ex:p ex:q3 {{| ex:w {} |}} . ex:q3 ex:p ex:s {{| ex:w {} |}} . \
         ex:s ex:p ex:d1 {{| ex:w {} |}} . ex:s ex:p ex:d2 {{| ex:w {} |}} . \
         ex:s ex:p ex:d3 {{| ex:w {} |}} .",
        dbl(1.0),
        dbl(m),
        dbl(e),
        dbl(e),
        dbl(0.5),
        dbl(0.5 + m),
        dbl(d),
        dbl(delta),
        dbl(2.0),
        dbl(2.0),
        dbl(2.0),
    );
    let s = store_of(&ttl);
    let cycle_p = vec![
        "s".to_string(),
        "p1".to_string(),
        "p2".to_string(),
        "p3".to_string(),
        "s".to_string(),
    ];
    let cycle_q = vec![
        "s".to_string(),
        "q1".to_string(),
        "q2".to_string(),
        "q3".to_string(),
        "s".to_string(),
    ];

    for lim in ["", " ; path:limit 10"] {
        // Pair shortest selects P.
        let r = rows(
            &s,
            &format!(
                "SELECT ?path ?i ?s ?o WHERE {{ SERVICE path:search {{ [] path:source ex:s ; \
                 path:target ex:s ; path:predicate ex:p ; path:weight ex:w ; path:minLength 1 ; \
                 path:maxLength 5{lim} ; path:pathIndex ?path ; path:edgeIndex ?i ; \
                 path:edgeSubject ?s ; path:edgeObject ?o }} }}"
            ),
        );
        let paths = node_paths(&r);
        assert_eq!(paths.len(), 1, "shortest ({lim}): {paths:?}");
        assert_eq!(paths["0"], cycle_p, "shortest ({lim}) must pick cycle P");

        // allShortest selects only P (does not accept both Q and P).
        let r = rows(
            &s,
            &format!(
                "SELECT ?path ?i ?s ?o WHERE {{ SERVICE path:search {{ [] path:source ex:s ; \
                 path:target ex:s ; path:predicate ex:p ; path:weight ex:w ; \
                 path:algorithm path:allShortest ; path:minLength 1 ; path:maxLength 5{lim} ; \
                 path:pathIndex ?path ; path:edgeIndex ?i ; path:edgeSubject ?s ; \
                 path:edgeObject ?o }} }}"
            ),
        );
        let paths = node_paths(&r);
        assert_eq!(paths.len(), 1, "allShortest ({lim}): {paths:?}");
        assert_eq!(paths["0"], cycle_p, "allShortest ({lim}) must keep only P");

        // kShortest k=1 returns P.
        let r = rows(
            &s,
            &format!(
                "SELECT ?path ?i ?s ?o WHERE {{ SERVICE path:search {{ [] path:source ex:s ; \
                 path:target ex:s ; path:predicate ex:p ; path:weight ex:w ; \
                 path:algorithm path:kShortest ; path:k 1 ; path:minLength 1 ; \
                 path:maxLength 5{lim} ; path:pathIndex ?path ; path:edgeIndex ?i ; \
                 path:edgeSubject ?s ; path:edgeObject ?o }} }}"
            ),
        );
        let paths = node_paths(&r);
        assert_eq!(paths.len(), 1, "kShortest k=1 ({lim}): {paths:?}");
        assert_eq!(paths["0"], cycle_p, "kShortest k=1 ({lim}) must pick P");

        // kShortest k=2 returns P first, then Q.
        let r = rows(
            &s,
            &format!(
                "SELECT ?path ?i ?s ?o WHERE {{ SERVICE path:search {{ [] path:source ex:s ; \
                 path:target ex:s ; path:predicate ex:p ; path:weight ex:w ; \
                 path:algorithm path:kShortest ; path:k 2 ; path:minLength 1 ; \
                 path:maxLength 5{lim} ; path:pathIndex ?path ; path:edgeIndex ?i ; \
                 path:edgeSubject ?s ; path:edgeObject ?o }} }}"
            ),
        );
        let paths = node_paths(&r);
        assert_eq!(paths.len(), 2, "kShortest k=2 ({lim}): {paths:?}");
        assert_eq!(paths["0"], cycle_p, "kShortest k=2 ({lim}) path 0");
        assert_eq!(paths["1"], cycle_q, "kShortest k=2 ({lim}) path 1");
    }
}

/// Regression for Sol r1 P1: eager bounded searches pre-admit frontier, Pareto vector,
/// parent, heap, and adjacency scan allocations under a search-scoped `Charge`, and
/// release that charge between repeated bound pairs.
#[test]
fn weighted_bounded_eager_frontier_memory_and_release() {
    let mut ttl = String::from("ex:z ex:p ex:t .\n");
    for i in 0..10_000 {
        ttl += &format!("ex:s ex:p ex:l{i} .\n");
    }
    let s = store_of(&ttl);
    let q_pair = format!(
        "{PREFIXES}SELECT ?len WHERE {{ SERVICE path:search {{ [] path:source ex:s ; \
         path:target ex:t ; path:predicate ex:p ; path:weight ex:w ; path:maxLength 2 ; \
         path:maxVisited 2 ; path:limit 1 ; path:length ?len }} }}"
    );
    let uncached = QueryOptions {
        no_cache: true,
        ..Default::default()
    };
    let res = query(s.snapshot(), &q_pair, &uncached).unwrap();
    // Old flat debit was ~240 KiB (2*48 + 10_000*24); real live allocations exceed 1 MiB.
    assert!(
        res.mem_peak_bytes > 1_000_000,
        "expected eager pair frontier peak > 1 MB, got {}",
        res.mem_peak_bytes
    );
    let tight = QueryOptions {
        max_memory_bytes: Some(512 * 1024),
        no_cache: true,
        ..Default::default()
    };
    match query(s.snapshot(), &q_pair, &tight) {
        Err(sparkles_core::Error::BudgetExceeded(b)) => {
            assert_eq!(b.kind, sparkles_core::BudgetKind::Memory);
        }
        Err(e) => panic!("expected memory budget refusal on eager pair frontier, got {e:?}"),
        Ok(_) => panic!("expected memory budget refusal on eager pair frontier, got Ok"),
    }

    // One-ended eager bounded search also pre-admits its frontier.
    let q_unbound = format!(
        "{PREFIXES}SELECT ?t WHERE {{ SERVICE path:search {{ [] path:source ex:s ; \
         path:target ?t ; path:predicate ex:p ; path:weight ex:w ; path:maxLength 1 ; \
         path:maxVisited 20000 ; path:limit 1 }} }}"
    );
    let unbound_res = query(s.snapshot(), &q_unbound, &uncached).unwrap();
    assert!(unbound_res.mem_peak_bytes > 1_000_000);
    match query(s.snapshot(), &q_unbound, &tight) {
        Err(sparkles_core::Error::BudgetExceeded(b)) => {
            assert_eq!(b.kind, sparkles_core::BudgetKind::Memory);
        }
        Err(e) => panic!("expected memory budget refusal on eager unbound frontier, got {e:?}"),
        Ok(_) => panic!("expected memory budget refusal on eager unbound frontier, got Ok"),
    }

    // Repeated bound pairs release their search-scoped working memory between searches.
    let mut multi_ttl = String::new();
    let mut vals = String::new();
    for g in 0..40 {
        multi_ttl += &format!("ex:z{g} ex:p ex:t{g} .\n");
        for i in 0..250 {
            multi_ttl += &format!("ex:s{g} ex:p ex:m{g}_{i} .\n");
        }
        vals += &format!("(ex:s{g} ex:t{g}) ");
    }
    let ms = store_of(&multi_ttl);
    let q_one = format!(
        "{PREFIXES}SELECT ?len WHERE {{ SERVICE path:search {{ [] path:source ex:s0 ; \
         path:target ex:t0 ; path:predicate ex:p ; path:weight ex:w ; path:maxLength 2 ; \
         path:limit 1 ; path:length ?len }} }}"
    );
    let one_peak = query(ms.snapshot(), &q_one, &uncached)
        .unwrap()
        .mem_peak_bytes;
    let q_many = format!(
        "{PREFIXES}SELECT ?s ?t ?len WHERE {{ VALUES (?s ?t) {{ {vals} }} SERVICE path:search {{ \
         [] path:source ?s ; path:target ?t ; path:predicate ex:p ; path:weight ex:w ; \
         path:maxLength 2 ; path:limit 1 ; path:length ?len }} }}"
    );
    let many_opts = QueryOptions {
        max_memory_bytes: Some(one_peak * 4),
        no_cache: true,
        ..Default::default()
    };
    let many_res = query(ms.snapshot(), &q_many, &many_opts)
        .expect("40 sequential pairs must release search-scoped frontier charges between pairs");
    assert!(
        many_res.mem_peak_bytes < one_peak * 4,
        "40 pairs peak {} should stay well below cumulative 40 * {}",
        many_res.mem_peak_bytes,
        one_peak
    );
}

/// Regression for Sol r1 P2 #3: eager bounded relaxations poll cancellation periodically
/// while relaxing a high-degree node with default weights.
#[test]
fn weighted_bounded_eager_cancels_during_edge_relaxation() {
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

    let mut ttl = String::from("ex:z ex:p ex:t .\n");
    for i in 0..10_000 {
        ttl += &format!("ex:s ex:p ex:l{i} .\n");
    }
    let s = store_of(&ttl);

    for q_body in [
        "SELECT ?len WHERE { SERVICE path:search { [] path:source ex:s ; path:target ex:t ; \
         path:predicate ex:p ; path:weight ex:w ; path:maxLength 2 ; path:maxVisited 2 ; \
         path:limit 1 ; path:length ?len } }",
        "SELECT ?t WHERE { SERVICE path:search { [] path:source ex:s ; path:target ?t ; \
         path:predicate ex:p ; path:weight ex:w ; path:maxLength 2 ; path:maxVisited 20000 ; \
         path:limit 1 } }",
    ] {
        let cancel = Arc::new(AtomicBool::new(false));
        let polls = Arc::new(AtomicUsize::new(0));
        let cancel_hook = cancel.clone();
        let polls_hook = polls.clone();
        let _guard = sparkles_core::sparql::pathsearch::test_hooks::on_traversal_poll(move || {
            let n = polls_hook.fetch_add(1, Ordering::Relaxed) + 1;
            if n == 2 {
                cancel_hook.store(true, Ordering::Release);
            }
        });
        let opts = QueryOptions {
            cancel: Some(cancel),
            no_cache: true,
            ..Default::default()
        };
        let res = query(s.snapshot(), &format!("{PREFIXES}{q_body}"), &opts).err();
        drop(_guard);
        assert!(
            matches!(res, Some(sparkles_core::Error::Cancelled)),
            "expected cancellation during edge relaxation, got {res:?}"
        );
        assert_eq!(
            polls.load(Ordering::Relaxed),
            2,
            "expected abort immediately on relaxation poll #2"
        );
    }
}

/// Regression for Neon r1 P3-2: one-ended bounded search settles `VIRT` at most once even
/// when shorter, more expensive cycles exist beside a longer, cheaper cycle.
#[test]
fn weighted_bounded_virt_visited_once() {
    let s = store_of(
        "ex:s ex:p ex:a1 {| ex:w 0.1 |} . ex:a1 ex:p ex:a2 {| ex:w 0.1 |} . \
         ex:a2 ex:p ex:a3 {| ex:w 0.1 |} . ex:a3 ex:p ex:s {| ex:w 0.1 |} . \
         ex:s ex:p ex:b1 {| ex:w 1.0 |} . ex:b1 ex:p ex:b2 {| ex:w 1.0 |} . \
         ex:b2 ex:p ex:s {| ex:w 1.0 |} . \
         ex:s ex:p ex:c1 {| ex:w 5.0 |} . ex:c1 ex:p ex:s {| ex:w 5.0 |} . \
         ex:s ex:p ex:s {| ex:w 20.0 |} .",
    );
    // 7 real nodes (s, a1, a2, a3, b1, b2, c1) + 1 VIRT visit = 8 visits total.
    // Without VIRT pruning, VIRT would be popped 4 times (11 visits) and fail maxVisited 8.
    for lim in ["", " ; path:limit 100"] {
        let got: BTreeMap<String, f64> = rows(
            &s,
            &format!(
                "SELECT ?t ?c WHERE {{ SERVICE path:search {{ [] path:source ex:s ; \
                 path:target ?t ; path:predicate ex:p ; path:weight ex:w ; path:maxLength 4 ; \
                 path:maxVisited 8{lim} ; path:cost ?c }} }}"
            ),
        )
        .into_iter()
        .map(|r| (r["t"].clone(), r["c"].parse().unwrap()))
        .collect();
        assert!(
            (got["s"] - 0.4).abs() < 1e-12,
            "cycle cost ({lim}): {got:?}"
        );
    }
}

/// Regression for Sol r2 P2 #1: Yen spurs with a shared prefix must include the prefix's
/// accumulated cost and weight range in wide-weight detection and source-order fallback so
/// rounded sums select the cheaper full path rather than a suffix that only looks cheaper
/// from zero.
#[test]
fn weighted_bounded_yen_shared_prefix_wide_weight_ranking() {
    let m = 2f64.powi(-53);
    let d = 2f64.powi(-105);
    let e = d / 2.0;
    let dbl = |w: f64| format!("\"{w:?}\"^^<http://www.w3.org/2001/XMLSchema#double>");
    let ttl = format!(
        "ex:s ex:p1 ex:p {{| ex:w {} |}} . ex:p ex:p1 ex:v {{| ex:w {} |}} . \
         ex:v ex:p1 ex:t {{| ex:w {} |}} . ex:v ex:p2 ex:t {{| ex:w {} |}} . \
         ex:v ex:p1 ex:x1 {{| ex:w {} |}} . ex:x1 ex:p1 ex:x2 {{| ex:w {} |}} . \
         ex:x2 ex:p1 ex:t {{| ex:w {} |}} .",
        dbl(1.0),
        dbl(m),
        dbl(0.0),
        dbl(d),
        dbl(e),
        dbl(e),
        dbl(e),
    );
    let s = store_of(&ttl);
    let path_zero = vec!["s", "p", "v", "t"]
        .into_iter()
        .map(String::from)
        .collect::<Vec<_>>();
    let path_three = vec!["s", "p", "v", "x1", "x2", "t"]
        .into_iter()
        .map(String::from)
        .collect::<Vec<_>>();

    for lim in ["", " ; path:limit 10"] {
        // allShortest must return both tied cost-1.0 routes (the 0-weight edge and the
        // 3-edge e,e,e suffix) and omit the dearer d-weight direct edge.
        let r = rows(
            &s,
            &format!(
                "SELECT ?path ?i ?s ?o ?c WHERE {{ SERVICE path:search {{ [] path:source ex:s ; \
                 path:target ex:t ; path:predicate ex:p1 , ex:p2 ; path:weight ex:w ; \
                 path:algorithm path:allShortest ; path:minLength 1 ; path:maxLength 5{lim} ; \
                 path:pathIndex ?path ; path:edgeIndex ?i ; path:edgeSubject ?s ; \
                 path:edgeObject ?o ; path:cost ?c }} }}"
            ),
        );
        let paths = node_paths(&r);
        assert_eq!(paths.len(), 2, "allShortest ({lim}): {paths:?}");
        let got_set: BTreeSet<Vec<String>> = paths.values().cloned().collect();
        assert_eq!(
            got_set,
            BTreeSet::from([path_zero.clone(), path_three.clone()]),
            "allShortest ({lim}) must keep both source-order ties"
        );
        for row in &r {
            let cost: f64 = row["c"].parse().unwrap();
            assert_eq!(cost, 1.0, "allShortest ({lim}) expected reported cost 1.0");
        }

        // kShortest k=2 must return the 0-edge route first and the tied 3-edge route second
        // before the dearer direct d route.
        let r = rows(
            &s,
            &format!(
                "SELECT ?path ?i ?s ?o ?c WHERE {{ SERVICE path:search {{ [] path:source ex:s ; \
                 path:target ex:t ; path:predicate ex:p1 , ex:p2 ; path:weight ex:w ; \
                 path:algorithm path:kShortest ; path:k 2 ; path:minLength 1 ; \
                 path:maxLength 5{lim} ; path:pathIndex ?path ; path:edgeIndex ?i ; \
                 path:edgeSubject ?s ; path:edgeObject ?o ; path:cost ?c }} }}"
            ),
        );
        let paths = node_paths(&r);
        assert_eq!(paths.len(), 2, "kShortest k=2 ({lim}): {paths:?}");
        assert_eq!(paths["0"], path_zero, "kShortest k=2 ({lim}) path 0");
        assert_eq!(paths["1"], path_three, "kShortest k=2 ({lim}) path 1");

        // kShortest k=3 places the dearer direct d route third with cost 1 + 2^-52.
        let r = rows(
            &s,
            &format!(
                "SELECT ?path ?i ?s ?o ?c WHERE {{ SERVICE path:search {{ [] path:source ex:s ; \
                 path:target ex:t ; path:predicate ex:p1 , ex:p2 ; path:weight ex:w ; \
                 path:algorithm path:kShortest ; path:k 3 ; path:minLength 1 ; \
                 path:maxLength 5{lim} ; path:pathIndex ?path ; path:edgeIndex ?i ; \
                 path:edgeSubject ?s ; path:edgeObject ?o ; path:cost ?c }} }}"
            ),
        );
        let paths = node_paths(&r);
        assert_eq!(paths.len(), 3, "kShortest k=3 ({lim}): {paths:?}");
        assert_eq!(paths["0"], path_zero, "kShortest k=3 ({lim}) path 0");
        assert_eq!(paths["1"], path_three, "kShortest k=3 ({lim}) path 1");
        assert_eq!(paths["2"], path_zero, "kShortest k=3 ({lim}) path 2 nodes");
        let cost2: f64 = r.iter().find(|row| row["path"] == "2").unwrap()["c"]
            .parse()
            .unwrap();
        assert_eq!(
            cost2,
            1.0 + 2f64.powi(-52),
            "kShortest k=3 ({lim}) third path must report 1 + 2^-52"
        );
    }
}

/// Regression for Sol r2 P2 #2: bounded weighted `path:shortest` with a variable source must
/// agree with bound-pair and forward unbound-target searches under wide weights, picking the
/// path with the lower source-order cost rather than a path that only looks cheaper in
/// target-to-source order.
#[test]
fn weighted_bounded_variable_source_wide_weight_ranking() {
    let m = 2f64.powi(-53);
    let d = 2f64.powi(-105);
    let e = d / 2.0;
    let dbl = |w: f64| format!("\"{w:?}\"^^<http://www.w3.org/2001/XMLSchema#double>");
    let ttl = format!(
        "ex:s ex:p1 ex:p {{| ex:w {} |}} . ex:p ex:p1 ex:v {{| ex:w {} |}} . \
         ex:v ex:p2 ex:t {{| ex:w {} |}} . \
         ex:v ex:p1 ex:x1 {{| ex:w {} |}} . ex:x1 ex:p1 ex:x2 {{| ex:w {} |}} . \
         ex:x2 ex:p1 ex:t {{| ex:w {} |}} .",
        dbl(1.0),
        dbl(m),
        dbl(d),
        dbl(e),
        dbl(e),
        dbl(e),
    );
    let s = store_of(&ttl);
    let expected_from_s: Vec<String> = ["s", "p", "v", "x1", "x2", "t"]
        .into_iter()
        .map(String::from)
        .collect();
    let expected_from_v: Vec<String> = ["v", "t"].into_iter().map(String::from).collect();

    for lim in ["", " ; path:limit 20"] {
        // Bound pair (s, t) picks the 3-edge suffix with cost 1.0.
        let r_pair = rows(
            &s,
            &format!(
                "SELECT ?path ?i ?s ?o ?c WHERE {{ SERVICE path:search {{ [] path:source ex:s ; \
                 path:target ex:t ; path:predicate ex:p1 , ex:p2 ; path:weight ex:w ; \
                 path:minLength 1 ; path:maxLength 5{lim} ; path:pathIndex ?path ; \
                 path:edgeIndex ?i ; path:edgeSubject ?s ; path:edgeObject ?o ; path:cost ?c }} }}"
            ),
        );
        let pair_paths = node_paths(&r_pair);
        assert_eq!(pair_paths["0"], expected_from_s, "bound pair ({lim})");
        assert_eq!(r_pair[0]["c"].parse::<f64>().unwrap(), 1.0);

        // Variable target (s, ?t) picks the same 3-edge suffix for ?t = ex:t.
        let r_fwd = rows(
            &s,
            &format!(
                "SELECT ?t ?path ?i ?s ?o ?c WHERE {{ SERVICE path:search {{ [] path:source ex:s ; \
                 path:target ?t ; path:predicate ex:p1 , ex:p2 ; path:weight ex:w ; \
                 path:minLength 1 ; path:maxLength 5{lim} ; path:pathIndex ?path ; \
                 path:edgeIndex ?i ; path:edgeSubject ?s ; path:edgeObject ?o ; path:cost ?c }} }}"
            ),
        );
        let fwd_for_t: Vec<_> = r_fwd.into_iter().filter(|r| r["t"] == "t").collect();
        let fwd_paths = node_paths(&fwd_for_t);
        assert_eq!(fwd_paths.len(), 1);
        assert_eq!(
            fwd_paths.values().next().unwrap(),
            &expected_from_s,
            "variable target ({lim})"
        );
        assert_eq!(fwd_for_t[0]["c"].parse::<f64>().unwrap(), 1.0);

        // Variable source (?src, t) must also pick the 3-edge suffix for ?src = ex:s with
        // cost 1.0, while still picking the direct d edge for ?src = ex:v (d < 3e).
        let r_rev = rows(
            &s,
            &format!(
                "SELECT ?src ?path ?i ?s ?o ?c WHERE {{ SERVICE path:search {{ [] path:source ?src ; \
                 path:target ex:t ; path:predicate ex:p1 , ex:p2 ; path:weight ex:w ; \
                 path:minLength 1 ; path:maxLength 5{lim} ; path:pathIndex ?path ; \
                 path:edgeIndex ?i ; path:edgeSubject ?s ; path:edgeObject ?o ; path:cost ?c }} }}"
            ),
        );
        let rev_for_s: Vec<_> = r_rev.iter().filter(|r| r["src"] == "s").cloned().collect();
        let rev_s_paths = node_paths(&rev_for_s);
        assert_eq!(rev_s_paths.len(), 1, "variable source s ({lim})");
        assert_eq!(
            rev_s_paths.values().next().unwrap(),
            &expected_from_s,
            "variable source ({lim}) must choose source-order 3-edge suffix for ex:s"
        );
        assert_eq!(
            rev_for_s[0]["c"].parse::<f64>().unwrap(),
            1.0,
            "variable source ({lim}) reported cost for ex:s"
        );

        let rev_for_v: Vec<_> = r_rev.iter().filter(|r| r["src"] == "v").cloned().collect();
        let rev_v_paths = node_paths(&rev_for_v);
        assert_eq!(rev_v_paths.len(), 1, "variable source v ({lim})");
        assert_eq!(
            rev_v_paths.values().next().unwrap(),
            &expected_from_v,
            "variable source ({lim}) must choose direct edge for ex:v"
        );
        assert_eq!(
            rev_for_v[0]["c"].parse::<f64>().unwrap(),
            d,
            "variable source ({lim}) reported cost for ex:v"
        );
    }
}

/// Regression for Sol r2 P2 #4: eager bounded weight lookup reduces reifier and weight rows
/// in place through checked streaming scans rather than materializing `Vec<Key>` scratch,
/// keeping memory flat and respecting cancellation and graph filtering.
#[test]
fn weighted_bounded_eager_high_reifier_weight_lookup_streams() {
    let mut ttl = String::from("ex:s ex:p ex:t .\n");
    for i in 0..5_000 {
        let w = if i == 4_200 { "1.5" } else { "2.0" };
        ttl += &format!("<< ex:s ex:p ex:t ~ ex:r{i} >> ex:w {w}e0 .\n");
    }
    let s = store_of(&ttl);
    let q = format!(
        "{PREFIXES}SELECT ?c WHERE {{ SERVICE path:search {{ [] path:source ex:s ; \
         path:target ex:t ; path:predicate ex:p ; path:weight ex:w ; path:maxLength 2 ; \
         path:limit 10 ; path:cost ?c }} }}"
    );
    let opts = QueryOptions {
        max_memory_bytes: Some(64 * 1024),
        no_cache: true,
        ..Default::default()
    };
    let res = query(s.snapshot(), &q, &opts).expect("streaming lookup must fit in 64 KiB");
    let got = rows(
        &s,
        "SELECT ?c WHERE { SERVICE path:search { [] path:source ex:s ; path:target ex:t ; \
         path:predicate ex:p ; path:weight ex:w ; path:maxLength 2 ; path:limit 10 ; path:cost ?c } }",
    );
    assert_eq!(got.len(), 1);
    assert!((got[0]["c"].parse::<f64>().unwrap() - 1.5).abs() < 1e-12);
    assert!(
        res.mem_peak_bytes < 64 * 1024,
        "expected small bounded peak on 5,000-reifier edge, got {}",
        res.mem_peak_bytes
    );
}
