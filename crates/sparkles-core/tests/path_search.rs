//! Path search (`SERVICE path:search`, spec F07): the acceptance examples, and searches
//! on random graphs checked against paths enumerated here by brute force and against
//! the property paths of the same reachability.

use sparkles_core::access::{Caller, GraphAccess, Graphs, Limits, Protection, Rule, TripleRules};
use sparkles_core::io::{RdfFormat, Source};
use sparkles_core::sparql::{QueryOptions, query};
use sparkles_core::store::{Store, StoreOptions};
use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

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
        .iter()
        .map(|row| {
            r.vars
                .iter()
                .zip(row)
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
    for case in 0..6 {
        let n = 5 + r.below(3);
        let m = n + r.below(2 * n);
        let mut triples: BTreeMap<E, Option<usize>> = BTreeMap::new();
        for _ in 0..m {
            let w = r.below(4);
            triples.insert((r.below(n), 0, r.below(n)), (w > 0).then_some(w));
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
