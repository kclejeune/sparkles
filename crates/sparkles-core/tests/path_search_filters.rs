//! Path search with skipped predicates and a degree limit (`path:excludePredicate` and
//! `path:maxDegree`, spec U09 Phase 1 and F07 §4.9): the acceptance examples, the ends
//! that are exempt from the limit, the degree in a masked view, and random graphs checked
//! against paths enumerated here by brute force, in one graph and over several graphs.

use sparkles_core::access::{Caller, GraphAccess, Graphs, Limits, Protection, Rule, TripleRules};
use sparkles_core::io::{RdfFormat, Source};
use sparkles_core::sparql::exec::PlanInfo;
use sparkles_core::sparql::{QueryOptions, query};
use sparkles_core::store::{Store, StoreOptions};
use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

const PREFIXES: &str = "PREFIX ex: <http://ex/> PREFIX path: <urn:x-sparkles:path#> \
    PREFIX rdf: <http://www.w3.org/1999/02/22-rdf-syntax-ns#> ";

fn load(s: &Store, text: String, format: RdfFormat) {
    s.load(&[Source::from_bytes(text.into_bytes(), format, None)])
        .unwrap();
}

fn store_of(ttl: &str) -> Store {
    let s = Store::in_memory(StoreOptions::default());
    load(
        &s,
        format!("@prefix ex: <http://ex/> .\n{ttl}"),
        RdfFormat::Turtle,
    );
    s
}

fn short(t: &Option<oxrdf::Term>) -> String {
    match t {
        None => "-".into(),
        Some(oxrdf::Term::NamedNode(n)) => {
            let s = n.as_str();
            s.strip_prefix("http://ex/")
                .or_else(|| s.strip_prefix("http://www.w3.org/1999/02/22-rdf-syntax-ns#"))
                .unwrap_or(s)
                .to_string()
        }
        Some(oxrdf::Term::Literal(l)) => l.value().to_string(),
        Some(t) => t.to_string(),
    }
}

fn run(s: &Store, q: &str, opts: &QueryOptions) -> (Vec<BTreeMap<String, String>>, PlanInfo) {
    let r =
        query(s.snapshot(), &format!("{PREFIXES}{q}"), opts).unwrap_or_else(|e| panic!("{q}: {e}"));
    let rows = r
        .rows()
        .iter()
        .map(|row| {
            r.vars
                .iter()
                .zip(row)
                .map(|(v, t)| (v.clone(), short(t)))
                .collect()
        })
        .collect();
    (rows, r.plan)
}

fn rows(s: &Store, q: &str) -> Vec<BTreeMap<String, String>> {
    run(s, q, &QueryOptions::default()).0
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

fn find<'a>(p: &'a PlanInfo, op: &str) -> Option<&'a PlanInfo> {
    if p.operator == op {
        return Some(p);
    }
    p.children.iter().find_map(|c| find(c, op))
}

fn counter(p: &PlanInfo, name: &str) -> Option<u64> {
    find(p, "PathSearch")?
        .counters
        .as_ref()?
        .get(name)?
        .as_u64()
}

/// The paths of a search, each as its nodes in path order joined by spaces, from rows
/// per edge with `?path`, `?i`, `?s`, `?p` and `?o`, given the source of each path.
fn paths(rows: &[BTreeMap<String, String>], from: &str) -> BTreeSet<String> {
    let mut by: BTreeMap<String, BTreeMap<usize, (String, String)>> = BTreeMap::new();
    for r in rows {
        by.entry(r["path"].clone())
            .or_default()
            .insert(r["i"].parse().unwrap(), (r["s"].clone(), r["o"].clone()));
    }
    by.into_values()
        .map(|edges| {
            let mut nodes = vec![from.to_string()];
            for (s, o) in edges.values() {
                let last = nodes.last().unwrap().clone();
                nodes.push(if *s == last { o.clone() } else { s.clone() });
            }
            nodes.join(" ")
        })
        .collect()
}

fn search(params: &str) -> String {
    format!(
        "SELECT ?path ?i ?s ?p ?o WHERE {{ SERVICE path:search {{ [] {params} ; \
         path:pathIndex ?path ; path:edgeIndex ?i ; path:edgeSubject ?s ; \
         path:edgePredicate ?p ; path:edgeObject ?o }} }}"
    )
}

/// U03 §7's people, organizations and cities, every one typed, with `alice knows bob`.
const PEOPLE: &str = "
ex:alice a ex:Person ; ex:worksFor ex:acme ; ex:knows ex:bob .
ex:bob a ex:Person ; ex:worksFor ex:beta .
ex:acme a ex:Org ; ex:locatedIn ex:lyon .
ex:beta a ex:Org ; ex:locatedIn ex:lyon .
ex:lyon a ex:City .
";

#[test]
fn excluded_predicates_are_not_edges() {
    let s = store_of(PEOPLE);
    // A1: without rdf:type, alice and bob are one edge apart through ex:knows, and no
    // path of the k shortest passes through ex:Person
    let r = rows(
        &s,
        &search(
            "path:source ex:alice ; path:target ex:bob ; path:direction path:both ; \
             path:excludePredicate rdf:type ; path:algorithm path:kShortest ; path:k 5 ; \
             path:maxLength 6",
        ),
    );
    let got = paths(&r, "alice");
    assert!(got.contains("alice bob"), "{got:?}");
    assert!(got.iter().all(|p| !p.contains("Person")), "{got:?}");
    assert!(r.iter().all(|row| row["p"] != "type"), "{r:?}");
    // with rdf:type, the path through the class is one of the k shortest
    let r = rows(
        &s,
        &search(
            "path:source ex:alice ; path:target ex:bob ; path:direction path:both ; \
             path:algorithm path:kShortest ; path:k 5 ; path:maxLength 6",
        ),
    );
    assert!(paths(&r, "alice").contains("alice Person bob"));
    // the shortest path to lyon avoids the excluded predicates in every mode
    for alg in [
        "path:algorithm path:shortest",
        "path:algorithm path:allShortest",
        "path:algorithm path:all ; path:maxLength 4",
    ] {
        let r = rows(
            &s,
            &search(&format!(
                "path:source ex:alice ; path:target ex:lyon ; path:direction path:both ; \
                 path:excludePredicate rdf:type, ex:worksFor ; {alg}"
            )),
        );
        assert!(r.is_empty(), "{alg}: {r:?}");
    }
    // a predicate the store does not have skips nothing
    let r = rows(
        &s,
        &search("path:source ex:alice ; path:target ex:lyon ; path:excludePredicate ex:nothing"),
    );
    assert_eq!(paths(&r, "alice"), ["alice acme lyon".to_string()].into());
    // a path to every node, without and with rdf:type: the classes drop out
    let reach = |extra: &str| -> BTreeSet<String> {
        rows(
            &s,
            &format!(
                "SELECT ?t WHERE {{ SERVICE path:search {{ [] path:source ex:alice ; \
                 path:target ?t {extra} }} }}"
            ),
        )
        .into_iter()
        .map(|r| r["t"].clone())
        .collect()
    };
    assert!(reach("").contains("Person"));
    let without = reach("; path:excludePredicate rdf:type");
    assert_eq!(
        without,
        ["acme", "beta", "bob", "lyon"]
            .map(String::from)
            .into_iter()
            .collect()
    );
    // the plan names them
    let (_, plan) = run(
        &s,
        &search("path:source ex:alice ; path:target ex:bob ; path:excludePredicate rdf:type"),
        &QueryOptions::default(),
    );
    let d = &find(&plan, "PathSearch").unwrap().description;
    assert!(
        d.contains("every predicate except <http://www.w3.org/1999/02/22-rdf-syntax-ns#type>"),
        "{d}"
    );
}

#[test]
fn errors_of_the_new_parameters() {
    let s = store_of(PEOPLE);
    // A3
    let e = error(
        &s,
        &search(
            "path:source ex:alice ; path:target ex:bob ; path:predicate ex:knows ; \
             path:excludePredicate rdf:type",
        ),
    );
    assert!(
        e.contains("path:search: path:excludePredicate cannot be combined with path:predicate"),
        "{e}"
    );
    for (params, want) in [
        (
            "path:excludePredicate \"x\"",
            "path:excludePredicate takes an IRI",
        ),
        ("path:excludePredicate rdf:type, rdf:type", "is given twice"),
        ("path:maxDegree 0", "path:maxDegree must be positive"),
        (
            "path:maxDegree -1",
            "path:maxDegree takes a non-negative integer",
        ),
        (
            "path:maxDegree 1 ; path:maxDegree 2",
            "path:maxDegree is given twice",
        ),
        ("path:maxDegree ?d", "path:maxDegree must be a constant"),
    ] {
        let e = error(
            &s,
            &search(&format!(
                "path:source ex:alice ; path:target ex:bob ; {params}"
            )),
        );
        assert!(e.contains("path:search: "), "{params}: {e}");
        assert!(e.contains(want), "{params}: {e}");
    }
    let e = error(
        &s,
        "SELECT ?x WHERE { SERVICE path:search { [] path:source ex:alice ; path:target ?x ; \
         path:start ?a ; path:end ?b ; path:excludePredicate rdf:type . ?a ex:knows ?b } }",
    );
    assert!(
        e.contains("path:excludePredicate and a nested pattern"),
        "{e}"
    );
}

/// `n` people who live in `ex:lyon`, each also knowing the next one.
fn hub_city(n: usize) -> String {
    let mut ttl = String::new();
    for i in 0..n {
        ttl.push_str(&format!("ex:p{i} ex:livesIn ex:lyon .\n"));
    }
    ttl.push_str("ex:p0 ex:knows ex:q1 . ex:q1 ex:knows ex:q2 . ex:q2 ex:knows ex:p1 .\n");
    ttl
}

#[test]
fn hubs_are_not_passed_through() {
    let s = store_of(&hub_city(2000));
    let q = |extra: &str| {
        search(&format!(
            "path:source ex:p0 ; path:target ex:p1 ; path:direction path:both {extra}"
        ))
    };
    // through the city the two are two edges apart
    assert_eq!(
        paths(&rows(&s, &q("")), "p0"),
        ["p0 lyon p1".to_string()].into()
    );
    // A2: the city has 2,000 edges, so a limit of 1,000 skips it
    let (r, plan) = run(&s, &q("; path:maxDegree 1000"), &QueryOptions::default());
    assert_eq!(paths(&r, "p0"), ["p0 q1 q2 p1".to_string()].into());
    assert!(counter(&plan, "skippedHubs").unwrap() >= 1, "{plan:?}");
    let d = &find(&plan, "PathSearch").unwrap().description;
    assert!(d.contains("maxDegree=1000"), "{d}");
    // a limit above the city's degree changes nothing, and the counter is 0
    let (r, plan) = run(&s, &q("; path:maxDegree 2000"), &QueryOptions::default());
    assert_eq!(paths(&r, "p0"), ["p0 lyon p1".to_string()].into());
    assert_eq!(counter(&plan, "skippedHubs"), Some(0));
    // without the parameter there is no counter
    let (_, plan) = run(&s, &q(""), &QueryOptions::default());
    assert_eq!(counter(&plan, "skippedHubs"), None);
    // every mode avoids the hub
    for alg in [
        "path:algorithm path:allShortest",
        "path:algorithm path:kShortest ; path:k 3 ; path:maxLength 4",
        "path:algorithm path:all ; path:maxLength 4",
    ] {
        let r = rows(&s, &q(&format!("; path:maxDegree 1000 ; {alg}")));
        assert_eq!(paths(&r, "p0"), ["p0 q1 q2 p1".to_string()].into(), "{alg}");
    }
    // weighted searches avoid it too
    let r = rows(
        &s,
        &q("; path:maxDegree 1000 ; path:weight ex:w ; path:defaultWeight 1"),
    );
    assert_eq!(paths(&r, "p0"), ["p0 q1 q2 p1".to_string()].into());
}

#[test]
fn the_ends_are_exempt_from_the_limit() {
    let s = store_of(&hub_city(50));
    // the hub as the source and as the target is expanded
    let pair = |from: &str, to: &str, extra: &str| {
        rows(
            &s,
            &search(&format!(
                "path:source ex:{from} ; path:target ex:{to} ; path:direction path:both ; \
                 path:maxDegree 10 {extra}"
            )),
        )
    };
    assert_eq!(
        paths(&pair("lyon", "p7", ""), "lyon"),
        ["lyon p7".to_string()].into()
    );
    assert_eq!(
        paths(&pair("p7", "lyon", ""), "p7"),
        ["p7 lyon".to_string()].into()
    );
    // a cycle through the hub as source and target
    let r = pair("lyon", "lyon", "; path:algorithm path:shortest");
    assert_eq!(r.len(), 2, "{r:?}");
    // but a path between two people does not pass through it
    assert!(pair("p7", "p8", "").is_empty());
    // with an unbound target, the hub ends paths but is not passed through: from p7 the
    // only node reached is the hub
    let reach = |dir: &str| -> BTreeSet<String> {
        rows(
            &s,
            &format!(
                "SELECT ?t WHERE {{ SERVICE path:search {{ [] path:source ex:p7 ; \
                 path:target ?t ; path:direction {dir} ; path:maxDegree 10 }} }}"
            ),
        )
        .into_iter()
        .map(|r| r["t"].clone())
        .collect()
    };
    assert_eq!(reach("path:both"), ["lyon".to_string()].into());
    // forward the city has no edges of its own, so it is no hub, and it is still the
    // only node p7 reaches
    let forward: BTreeSet<String> = rows(
        &s,
        "SELECT ?t WHERE { SERVICE path:search { [] path:source ex:p7 ; path:target ?t ; \
         path:direction path:forward ; path:maxDegree 10 } }",
    )
    .into_iter()
    .map(|r| r["t"].clone())
    .collect();
    assert_eq!(forward, ["lyon".to_string()].into());
    // with an unbound source, paths start at the hub but do not pass through it
    let from: BTreeSet<String> = rows(
        &s,
        "SELECT ?f WHERE { SERVICE path:search { [] path:source ?f ; path:target ex:p7 ; \
         path:direction path:both ; path:maxDegree 10 } }",
    )
    .into_iter()
    .map(|r| r["f"].clone())
    .collect();
    assert_eq!(from, ["lyon".to_string()].into());
}

#[test]
fn degree_is_counted_after_predicates_and_direction() {
    // x has 3 ex:a edges out, 3 ex:b edges out and 3 edges in
    let mut ttl = String::from("ex:s ex:a ex:x . ex:x ex:a ex:t .\n");
    for i in 0..2 {
        ttl.push_str(&format!(
            "ex:x ex:a ex:a{i} . ex:x ex:b ex:b{i} . ex:i{i} ex:a ex:x .\n"
        ));
    }
    ttl.push_str("ex:x ex:b ex:b2 .\n");
    let s = store_of(&ttl);
    let found = |extra: &str| {
        !rows(
            &s,
            &search(&format!("path:source ex:s ; path:target ex:t {extra}")),
        )
        .is_empty()
    };
    // forward over every predicate x has 6 edges
    assert!(found("; path:maxDegree 6"));
    assert!(!found("; path:maxDegree 5"));
    // with ex:b skipped it has 3
    assert!(found("; path:maxDegree 3 ; path:excludePredicate ex:b"));
    assert!(!found("; path:maxDegree 2 ; path:excludePredicate ex:b"));
    // over ex:a alone, 3
    assert!(found("; path:maxDegree 3 ; path:predicate ex:a"));
    assert!(!found("; path:maxDegree 2 ; path:predicate ex:a"));
    // both ways over ex:a, 6 (3 out and 3 in)
    assert!(found(
        "; path:maxDegree 6 ; path:predicate ex:a ; path:direction path:both"
    ));
    assert!(!found(
        "; path:maxDegree 5 ; path:predicate ex:a ; path:direction path:both"
    ));
    // a self-loop followed both ways is one edge
    let s2 = store_of(&format!("{ttl} ex:x ex:a ex:x ."));
    let found2 = |extra: &str| {
        !rows(
            &s2,
            &search(&format!(
                "path:source ex:s ; path:target ex:t ; path:direction path:both {extra}"
            )),
        )
        .is_empty()
    };
    assert!(found2("; path:maxDegree 7 ; path:predicate ex:a"));
    assert!(!found2("; path:maxDegree 6 ; path:predicate ex:a"));
    assert!(found2("; path:maxDegree 10 ; path:excludePredicate ex:b"));
    assert!(!found2("; path:maxDegree 6 ; path:excludePredicate ex:b"));
    assert!(found2("; path:maxDegree 10"));
    assert!(!found2("; path:maxDegree 9"));
}

#[test]
fn degree_in_a_masked_view_and_per_graph() {
    // x is a hub only through triples a protection hides
    let mut ttl = String::from("ex:s ex:p ex:x . ex:x ex:p ex:t .\n");
    for i in 0..5 {
        ttl.push_str(&format!("ex:x ex:secret ex:h{i} .\n"));
    }
    let s = store_of(&ttl);
    let q = search("path:source ex:s ; path:target ex:t ; path:maxDegree 3");
    assert!(rows(&s, &q).is_empty());
    let p = Protection {
        name: "secret".into(),
        predicates: Some(vec!["http://ex/secret".into()]),
        classes: None,
        subclasses: false,
        graphs: None,
        pattern: None,
        prefixes: Default::default(),
        hide_inferences: false,
    };
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
    let (r, _) = run(&s, &q, &opts);
    assert_eq!(paths(&r, "s"), ["s x t".to_string()].into());

    // per named graph: x is a hub in g1 only
    let s = Store::in_memory(StoreOptions::default());
    let mut nq = String::new();
    for g in ["g1", "g2"] {
        nq.push_str(&format!(
            "<http://ex/s> <http://ex/p> <http://ex/x> <http://ex/{g}> .\n\
             <http://ex/x> <http://ex/p> <http://ex/t> <http://ex/{g}> .\n"
        ));
    }
    for i in 0..5 {
        nq.push_str(&format!(
            "<http://ex/x> <http://ex/p> <http://ex/h{i}> <http://ex/g1> .\n"
        ));
    }
    load(&s, nq, RdfFormat::NQuads);
    let got: BTreeSet<String> = rows(
        &s,
        "SELECT ?g WHERE { GRAPH ?g { SERVICE path:search { [] path:source ex:s ; \
         path:target ex:t ; path:maxDegree 3 } } }",
    )
    .into_iter()
    .map(|r| r["g"].clone())
    .collect();
    assert_eq!(got, ["g2".to_string()].into());
    // in the union graph x has 6 edges: the triple in both graphs counts once
    let union = |d: u64| {
        rows(
            &s,
            &format!(
                "SELECT * WHERE {{ GRAPH <urn:x-arq:UnionGraph> {{ SERVICE path:search {{ [] \
                 path:source ex:s ; path:target ex:t ; path:maxDegree {d} }} }} }}"
            ),
        )
        .len()
    };
    assert_eq!(union(6), 1);
    assert_eq!(union(5), 0);
}

// --------------------------------------------------------------- random graphs ------

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

/// A stored triple (subject, predicate, object).
type T = (usize, usize, usize);

/// The first id of the literal objects of the random graphs.
const LIT: usize = 100;

/// Every simple path (the last node may be the first) from `s` of at most `max` edges
/// over `adj` (node → [(next node, triple)]), as (end, triples).
fn brute(adj: &BTreeMap<usize, Vec<(usize, T)>>, s: usize, max: usize) -> Vec<(usize, Vec<T>)> {
    fn go(
        adj: &BTreeMap<usize, Vec<(usize, T)>>,
        s: usize,
        x: usize,
        max: usize,
        on: &mut Vec<usize>,
        path: &mut Vec<T>,
        out: &mut Vec<(usize, Vec<T>)>,
    ) {
        if path.len() == max {
            return;
        }
        for &(y, t) in adj.get(&x).map(Vec::as_slice).unwrap_or(&[]) {
            if y != s && on.contains(&y) {
                continue;
            }
            path.push(t);
            out.push((y, path.clone()));
            if y != s {
                on.push(y);
                go(adj, s, y, max, on, path, out);
                on.pop();
            }
            path.pop();
        }
    }
    let mut out = Vec::new();
    go(adj, s, s, max, &mut vec![s], &mut Vec::new(), &mut out);
    out
}

/// The nodes of a path from `s`, in order.
fn walk(s: usize, path: &[T]) -> Vec<usize> {
    let mut nodes = vec![s];
    for &(a, _, b) in path {
        let x = *nodes.last().unwrap();
        nodes.push(if a == x { b } else { a });
    }
    nodes
}

/// Random graphs over three predicates, searched with a skipped predicate and a degree
/// limit in every mode and direction, against brute force: a path may not use a skipped
/// predicate, and no node inside it may have more edges than the limit. With `graphs`
/// the triples are spread over named graphs, some in two, and searched in the union
/// graph, where the degree is read from the edges rather than from the index counts.
fn random_graphs(seed: u64, cases: usize, graphs: bool) {
    let mut r = Rng(seed);
    for case in 0..cases {
        let n = 6 + r.below(4);
        let m = 2 * n + r.below(2 * n);
        let mut triples: BTreeSet<T> = BTreeSet::new();
        for _ in 0..m {
            // a fifth of the objects are the literals 100 to 102, which end paths
            let o = if r.below(5) == 0 {
                LIT + r.below(3)
            } else {
                r.below(n)
            };
            triples.insert((r.below(n), r.below(3), o));
        }
        let s = Store::in_memory(StoreOptions::default());
        let text: String = triples
            .iter()
            .map(|&(a, p, b)| {
                let o = if b >= LIT {
                    format!("\"{b}\"^^<http://www.w3.org/2001/XMLSchema#integer>")
                } else {
                    format!("<http://ex/n{b}>")
                };
                let t = format!("<http://ex/n{a}> <http://ex/p{p}> {o}");
                if graphs {
                    let g = r.below(3);
                    let mut q = format!("{t} <http://ex/g{g}> .\n");
                    if r.below(3) == 0 {
                        q.push_str(&format!("{t} <http://ex/g{}> .\n", (g + 1) % 3));
                    }
                    q
                } else {
                    format!("{t} .\n")
                }
            })
            .collect();
        load(&s, text, RdfFormat::NQuads);
        let wrap = |inner: String| {
            if graphs {
                format!("GRAPH <urn:x-arq:UnionGraph> {{ {inner} }}")
            } else {
                inner
            }
        };
        for (dir, both, back) in [
            ("path:forward", false, false),
            ("path:backward", false, true),
            ("path:both", true, false),
        ] {
            let skip = r.below(4); // 3: skip nothing
            let limit = 1 + r.below(4);
            let mut adj: BTreeMap<usize, Vec<(usize, T)>> = BTreeMap::new();
            for &(a, p, b) in &triples {
                if p == skip {
                    continue;
                }
                let t = (a, p, b);
                // no edge leaves a literal
                if (back || both) && b < LIT {
                    adj.entry(b).or_default().push((a, t));
                }
                if !back && !(both && a == b) {
                    adj.entry(a).or_default().push((b, t));
                }
            }
            let degree = |x: usize| adj.get(&x).map_or(0, Vec::len);
            let ok = |src: usize, p: &[T]| {
                let nodes = walk(src, p);
                nodes[1..nodes.len() - 1]
                    .iter()
                    .all(|&x| degree(x) <= limit)
            };
            let max = 4;
            let src = r.below(n);
            let all: Vec<(usize, Vec<T>)> = brute(&adj, src, max)
                .into_iter()
                .filter(|(_, p)| ok(src, p))
                .collect();
            let params = format!(
                "path:direction {dir} ; path:maxDegree {limit}{}",
                if skip < 3 {
                    format!(" ; path:excludePredicate ex:p{skip}")
                } else {
                    String::new()
                }
            );
            let ctx = format!("graphs={graphs} case {case} {dir} skip p{skip} limit {limit}");
            let paths_of = |target: &str, alg: &str| -> BTreeSet<Vec<T>> {
                let q = format!(
                    "SELECT ?path ?i ?s ?p ?o WHERE {{ {} }}",
                    wrap(format!(
                        "SERVICE path:search {{ [] path:source ex:n{src} ; path:target {target} ; \
                         {params} ; {alg} ; path:pathIndex ?path ; path:edgeIndex ?i ; \
                         path:edgeSubject ?s ; path:edgePredicate ?p ; path:edgeObject ?o }}"
                    ))
                );
                let mut by: BTreeMap<String, BTreeMap<usize, T>> = BTreeMap::new();
                for row in rows(&s, &q) {
                    let num = |k: &str| {
                        row[k]
                            .trim_start_matches(|c: char| !c.is_ascii_digit())
                            .parse::<usize>()
                            .unwrap()
                    };
                    by.entry(row["path"].clone())
                        .or_default()
                        .insert(row["i"].parse().unwrap(), (num("s"), num("p"), num("o")));
                }
                by.into_values()
                    .map(|v| v.into_values().collect())
                    .collect()
            };
            let got = paths_of("?t", "path:algorithm path:all ; path:maxLength 4");
            let want: BTreeSet<Vec<T>> = all.iter().map(|(_, p)| p.clone()).collect();
            assert_eq!(got, want, "{ctx}: all paths from n{src}");
            // the shortest paths to every node, without a length limit
            let unbounded: Vec<(usize, Vec<T>)> = brute(&adj, src, n + 1)
                .into_iter()
                .filter(|(_, p)| ok(src, p))
                .collect();
            let shortest_to = |t: usize| {
                unbounded
                    .iter()
                    .filter(|(e, _)| *e == t)
                    .map(|(_, p)| p.len())
                    .min()
            };
            let reach: BTreeMap<usize, usize> = paths_of("?t", "path:algorithm path:shortest")
                .into_iter()
                .map(|p| (*walk(src, &p).last().unwrap(), p.len()))
                .collect();
            let want_reach: BTreeMap<usize, usize> = (0..n)
                .chain(LIT..LIT + 3)
                .filter_map(|t| shortest_to(t).map(|l| (t, l)))
                .collect();
            assert_eq!(reach, want_reach, "{ctx}: shortest to every node");
            for t in 0..n {
                let to_t: Vec<&Vec<T>> = all
                    .iter()
                    .filter(|(e, _)| *e == t)
                    .map(|(_, p)| p)
                    .collect();
                let shortest = to_t.iter().map(|p| p.len()).min();
                let target = format!("ex:n{t}");
                let ctx = format!("{ctx} n{src}→n{t}");
                let sp = paths_of(
                    &target,
                    "path:algorithm path:allShortest ; path:maxLength 4",
                );
                let want: BTreeSet<Vec<T>> = to_t
                    .iter()
                    .filter(|p| Some(p.len()) == shortest)
                    .map(|p| (*p).clone())
                    .collect();
                assert_eq!(sp, want, "{ctx}: all shortest");
                let one = paths_of(&target, "path:algorithm path:shortest");
                assert_eq!(
                    one.iter().next().map(Vec::len),
                    shortest_to(t),
                    "{ctx}: one shortest"
                );
                let k = 3;
                let ks = paths_of(
                    &target,
                    &format!("path:algorithm path:kShortest ; path:k {k} ; path:maxLength 4"),
                );
                let mut lens: Vec<usize> = ks.iter().map(Vec::len).collect();
                lens.sort();
                let mut want_lens: Vec<usize> = to_t.iter().map(|p| p.len()).collect();
                want_lens.sort();
                want_lens.truncate(k);
                assert_eq!(lens, want_lens, "{ctx}: k shortest lengths");
                for p in &ks {
                    assert!(to_t.contains(&p), "{ctx}: {p:?} is a path");
                }
            }
            // paths into a target from every source
            let t = r.below(n);
            let into: BTreeMap<usize, usize> = rows(
                &s,
                &format!(
                    "SELECT ?f ?len WHERE {{ {} }}",
                    wrap(format!(
                        "SERVICE path:search {{ [] path:source ?f ; path:target ex:n{t} ; \
                         {params} ; path:length ?len }}"
                    ))
                ),
            )
            .into_iter()
            .map(|row| {
                (
                    row["f"].trim_start_matches('n').parse().unwrap(),
                    row["len"].parse().unwrap(),
                )
            })
            .collect();
            let want_into: BTreeMap<usize, usize> = (0..n)
                .filter_map(|f| {
                    brute(&adj, f, n + 1)
                        .into_iter()
                        .filter(|(e, p)| *e == t && ok(f, p))
                        .map(|(_, p)| p.len())
                        .min()
                        .map(|l| (f, l))
                })
                .collect();
            assert_eq!(into, want_into, "{ctx}: shortest into n{t}");
        }
    }
}

#[test]
fn random_graphs_against_brute_force() {
    random_graphs(0x9a7b_1234_5eed, 12, false);
}

#[test]
fn random_graphs_over_named_graphs_against_brute_force() {
    random_graphs(0x7e57_0a11_b0b5, 12, true);
}

#[test]
fn literals_end_paths_and_are_not_passed_through() {
    let s = store_of("ex:a ex:age 30 . ex:b ex:age 30 . ex:a ex:p ex:b .");
    let found = |params: &str| paths(&rows(&s, &search(params)), "a");
    // both ways the two share a literal, but a path does not pass through it
    assert_eq!(
        found(
            "path:source ex:a ; path:target ex:b ; path:direction path:both ; path:algorithm path:all ; path:maxLength 3"
        ),
        ["a b".to_string()].into()
    );
    // a literal ends a path, forward and both ways
    for dir in ["path:forward", "path:both"] {
        assert_eq!(
            found(&format!(
                "path:source ex:a ; path:target 30 ; path:direction {dir} ; path:algorithm path:allShortest"
            )),
            ["a 30".to_string()].into(),
            "{dir}"
        );
    }
    // and backward no edge reaches it, since none leaves it in path order
    assert!(found("path:source ex:a ; path:target 30 ; path:direction path:backward").is_empty());
    // the sources that reach the literal both ways are the two that state it, not each
    // other through it
    let from: BTreeSet<String> = rows(
        &s,
        "SELECT ?f WHERE { SERVICE path:search { [] path:source ?f ; path:target 30 ; \
         path:direction path:both ; path:maxLength 1 } }",
    )
    .into_iter()
    .map(|r| r["f"].clone())
    .collect();
    assert_eq!(from, ["a".to_string(), "b".to_string()].into());
}

/// Weights from reifiers (U09 Phase 3) together with skipped predicates and a hub limit:
/// the cheapest path that avoids hubs and skipped triples, in every weighted mode.
#[test]
fn weights_with_skipped_predicates_and_hubs() {
    // a cheap way through the hub h, a typed way through the class C, and dearer plain
    // ways through b and c
    let mut ttl = String::from(
        "ex:a ex:p ex:h {| ex:w 0.1 |} . ex:h ex:p ex:t {| ex:w 0.1 |} .
         ex:a a ex:C . ex:t a ex:C .
         ex:a ex:p ex:b {| ex:w 0.5 |} . ex:b ex:p ex:t {| ex:w 0.5 |} .
         ex:a ex:p ex:c {| ex:w 1 |} . ex:c ex:p ex:t {| ex:w 1 |} .\n",
    );
    for i in 0..10 {
        ttl.push_str(&format!("ex:h ex:p ex:x{i} .\n"));
    }
    let s = store_of(&ttl);
    let base = "path:source ex:a ; path:target ex:t ; path:direction path:both ; path:weight ex:w";
    let cost = |params: &str| -> Vec<(String, String)> {
        let q = format!(
            "SELECT ?path ?c ?len WHERE {{ SERVICE path:search {{ [] {base} ; {params} ; \
             path:pathIndex ?path ; path:cost ?c ; path:length ?len }} }} ORDER BY ?path"
        );
        rows(&s, &q)
            .into_iter()
            .map(|r| (r["c"].clone(), r["len"].clone()))
            .collect()
    };
    let found = |params: &str| -> BTreeSet<String> {
        paths(&rows(&s, &search(&format!("{base} ; {params}"))), "a")
    };
    // without filters the hub is the cheapest way
    assert_eq!(
        found("path:algorithm path:shortest"),
        ["a h t".to_string()].into()
    );
    // without the hub the way through b is the cheapest
    assert_eq!(
        found("path:algorithm path:allShortest ; path:maxDegree 5"),
        ["a b t".to_string()].into()
    );
    // the typed triples weigh the default 1, so the class ties with the way through c
    assert_eq!(
        found("path:algorithm path:kShortest ; path:k 3 ; path:maxDegree 5"),
        [
            "a b t".to_string(),
            "a C t".to_string(),
            "a c t".to_string()
        ]
        .into()
    );
    assert_eq!(
        cost("path:algorithm path:kShortest ; path:k 3 ; path:maxDegree 5"),
        [
            ("1".to_string(), "2".to_string()),
            ("2".to_string(), "2".to_string()),
            ("2".to_string(), "2".to_string())
        ]
    );
    // k shortest in cost order, without the hub and the class
    assert_eq!(
        cost(
            "path:algorithm path:kShortest ; path:k 3 ; path:maxDegree 5 ; \
             path:excludePredicate rdf:type"
        ),
        [
            ("1".to_string(), "2".to_string()),
            ("2".to_string(), "2".to_string())
        ]
    );
    // a maximum length keeps the weighted search within it
    assert!(cost("path:algorithm path:shortest ; path:maxLength 1").is_empty());
}
