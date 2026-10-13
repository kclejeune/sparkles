//! Graph algorithms (`SERVICE algo:…`, spec F14): the acceptance examples, parameter
//! errors, access control, write-back through SPARQL Update, and the budgets.

use sparkles_core::access::{Caller, GraphAccess, Graphs, Limits, Protection, Rule, TripleRules};
use sparkles_core::io::{RdfFormat, Source};
use sparkles_core::sparql::update::update;
use sparkles_core::sparql::{QueryOptions, query};
use sparkles_core::store::{Store, StoreOptions};
use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

const PREFIXES: &str = "PREFIX ex: <http://ex/> PREFIX algo: <urn:x-sparkles:algo#> \
                        PREFIX rdf: <http://www.w3.org/1999/02/22-rdf-syntax-ns#> ";

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

/// The graph of the acceptance examples: `ex:p` edges a→b, b→c, c→a, c→d, d→e, e→d, f→g,
/// and a node h with a type and no edges.
const A: &str = "
ex:a ex:p ex:b . ex:b ex:p ex:c . ex:c ex:p ex:a . ex:c ex:p ex:d .
ex:d ex:p ex:e . ex:e ex:p ex:d . ex:f ex:p ex:g . ex:h a ex:N .
";

/// A map from node to the value of one output.
fn by_node(rows: &[BTreeMap<String, String>], col: &str) -> BTreeMap<String, String> {
    rows.iter()
        .map(|r| (r["n"].clone(), r[col].clone()))
        .collect()
}

fn groups(m: &BTreeMap<String, String>) -> BTreeSet<Vec<String>> {
    let mut g: BTreeMap<&String, Vec<String>> = BTreeMap::new();
    for (n, c) in m {
        g.entry(c).or_default().push(n.clone());
    }
    g.into_values().collect()
}

fn set(items: &[&[&str]]) -> BTreeSet<Vec<String>> {
    items
        .iter()
        .map(|g| g.iter().map(|s| s.to_string()).collect())
        .collect()
}

#[test]
fn a1_wcc() {
    let s = store_of(A);
    let r = rows(
        &s,
        "SELECT ?n ?c WHERE { SERVICE algo:wcc { [] algo:predicate ex:p ; algo:node ?n ; algo:component ?c } }",
    );
    assert_eq!(
        groups(&by_node(&r, "c")),
        set(&[&["a", "b", "c", "d", "e"], &["f", "g"]])
    );
    // with the class, h is its own component
    let r = rows(
        &s,
        "SELECT ?n ?c WHERE { SERVICE algo:wcc { [] algo:predicate ex:p ; algo:nodeClass ex:N ; \
         algo:node ?n ; algo:component ?c } }",
    );
    assert_eq!(
        by_node(&r, "c").len(),
        1,
        "only the members of the class are nodes: {r:?}"
    );
}

#[test]
fn a2_scc() {
    let s = store_of(A);
    let r = rows(
        &s,
        "SELECT ?n ?c WHERE { SERVICE algo:scc { [] algo:predicate ex:p ; algo:node ?n ; algo:component ?c } }",
    );
    assert_eq!(
        groups(&by_node(&r, "c")),
        set(&[&["a", "b", "c"], &["d", "e"], &["f"], &["g"]])
    );
}

#[test]
fn a3_pagerank() {
    let s = store_of(A);
    let r = rows(
        &s,
        "SELECT ?n ?r WHERE { SERVICE algo:pageRank { [] algo:predicate ex:p ; algo:maxIterations 100 ; \
         algo:node ?n ; algo:score ?r } }",
    );
    let score: BTreeMap<String, f64> = r
        .iter()
        .map(|x| (x["n"].clone(), x["r"].parse().unwrap()))
        .collect();
    let total: f64 = score.values().sum();
    assert!((total - 1.0).abs() < 1e-6, "{total}");
    for hi in ["d", "e"] {
        for lo in ["a", "b", "c"] {
            assert!(score[hi] > score[lo], "{hi} above {lo}: {score:?}");
        }
    }
    // ORDER BY and LIMIT apply as to any rows
    let top = rows(
        &s,
        "SELECT ?n WHERE { SERVICE algo:pageRank { [] algo:predicate ex:p ; algo:node ?n ; algo:score ?r } } \
         ORDER BY DESC(?r) LIMIT 2",
    );
    let names: BTreeSet<&str> = top.iter().map(|r| r["n"].as_str()).collect();
    assert_eq!(names, ["d", "e"].into());
}

#[test]
fn a4_personalized_pagerank() {
    let s = store_of(A);
    let r = rows(
        &s,
        "SELECT ?n ?r WHERE { SERVICE algo:pageRank { [] algo:predicate ex:p ; algo:maxIterations 100 ; \
         algo:source ex:f ; algo:node ?n ; algo:score ?r } }",
    );
    let score: BTreeMap<String, f64> = r
        .iter()
        .map(|x| (x["n"].clone(), x["r"].parse().unwrap()))
        .collect();
    assert!(score["g"] > 0.0);
    for n in ["a", "b", "c", "d", "e"] {
        assert_eq!(score[n], 0.0, "{n}");
    }
}

#[test]
fn a4_source_from_the_group() {
    let s = store_of(A);
    let r = rows(
        &s,
        "SELECT ?n ?d WHERE { VALUES ?s { ex:a } SERVICE algo:bfs { [] algo:predicate ex:p ; \
         algo:source ?s ; algo:node ?n ; algo:depth ?d } }",
    );
    let d = by_node(&r, "d");
    // A7
    assert_eq!(d["a"], "0");
    assert_eq!(d["b"], "1");
    assert_eq!(d["c"], "2");
    assert_eq!(d["d"], "3");
    assert_eq!(d["e"], "4");
    assert!(!d.contains_key("f"), "unreached nodes are left out");
}

#[test]
fn a5_a6_triangles_and_cores() {
    let s = store_of(A);
    let r = rows(
        &s,
        "SELECT ?n ?t WHERE { SERVICE algo:triangleCount { [] algo:predicate ex:p ; algo:node ?n ; algo:triangles ?t } }",
    );
    let t = by_node(&r, "t");
    for n in ["a", "b", "c"] {
        assert_eq!(t[n], "1", "{n}");
    }
    for n in ["d", "e", "f", "g"] {
        assert_eq!(t[n], "0", "{n}");
    }
    let r = rows(
        &s,
        "SELECT ?n ?k WHERE { SERVICE algo:kCore { [] algo:predicate ex:p ; algo:node ?n ; algo:core ?k } }",
    );
    let k = by_node(&r, "k");
    for n in ["a", "b", "c"] {
        assert_eq!(k[n], "2", "{n}");
    }
    for n in ["d", "e", "f", "g"] {
        assert_eq!(k[n], "1", "{n}");
    }
}

#[test]
fn a8_hidden_triples_are_not_edges() {
    // a protection hides the triples whose subject is typed ex:Hidden: c's edges to a and
    // d are gone, so {a b c} and {d e} are two components
    let s = store_of(&format!("{A} ex:c a ex:Hidden ."));
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
    let q = "SELECT ?n ?c WHERE { SERVICE algo:wcc { [] algo:predicate ex:p ; algo:node ?n ; algo:component ?c } }";
    let c = by_node(&rows_with(&s, q, &opts), "c");
    assert_eq!(
        groups(&c),
        set(&[&["a", "b", "c"], &["d", "e"], &["f", "g"]])
    );
    // without the protection c joins d
    let c = by_node(&rows(&s, q), "c");
    assert_eq!(groups(&c), set(&[&["a", "b", "c", "d", "e"], &["f", "g"]]));
}

#[test]
fn a10_node_similarity() {
    let s =
        store_of("ex:u1 ex:likes ex:x, ex:y . ex:u2 ex:likes ex:x, ex:y . ex:u3 ex:likes ex:x .");
    let r = rows(
        &s,
        "SELECT ?n ?o ?s WHERE { SERVICE algo:nodeSimilarity { [] algo:predicate ex:likes ; \
         algo:node ?n ; algo:otherNode ?o ; algo:similarity ?s } }",
    );
    let got: BTreeSet<(String, String, String)> = r
        .iter()
        .map(|x| (x["n"].clone(), x["o"].clone(), x["s"].clone()))
        .collect();
    assert!(
        got.contains(&("u1".into(), "u2".into(), "1".into()))
            || got
                .iter()
                .any(|g| g.0 == "u1" && g.1 == "u2" && g.2.parse::<f64>().unwrap() == 1.0),
        "{got:?}"
    );
    assert!(
        got.iter().any(|g| g.0 == "u1"
            && g.1 == "u3"
            && (g.2.parse::<f64>().unwrap() - 0.5).abs() < 1e-12)
    );
}

#[test]
fn a13_betweenness_over_the_node_limit() {
    let s = store_of(A);
    sparkles_core::algo::pool::set_exact_betweenness_max(3);
    let e = error(
        &s,
        "SELECT * WHERE { SERVICE algo:betweenness { [] algo:predicate ex:p ; algo:node ?n ; algo:score ?r } }",
    );
    assert!(e.contains("algo:samples"), "{e}");
    // sampling is allowed
    let r = rows(
        &s,
        "SELECT * WHERE { SERVICE algo:betweenness { [] algo:predicate ex:p ; algo:samples 2 ; algo:node ?n ; algo:score ?r } }",
    );
    assert!(!r.is_empty());
    sparkles_core::algo::pool::set_exact_betweenness_max(100_000);
}

#[test]
fn a14_max_memory_fails_before_building() {
    let s = store_of(A);
    let e = error(
        &s,
        "SELECT * WHERE { SERVICE algo:wcc { [] algo:predicate ex:p ; algo:maxMemory 16 ; algo:node ?n ; algo:component ?c } }",
    );
    assert!(e.contains("memory"), "{e}");
}

#[test]
fn parameter_errors() {
    let s = store_of(A);
    let e = error(
        &s,
        "SELECT * WHERE { SERVICE algo:wcc { [] algo:dampingFactor 0.5 ; algo:node ?n } }",
    );
    assert!(e.contains("not a parameter of algo:wcc"), "{e}");
    let e = error(
        &s,
        "SELECT * WHERE { SERVICE algo:pageRank { [] algo:bogus 1 ; algo:node ?n } }",
    );
    assert!(e.contains("unknown parameter"), "{e}");
    let e = error(
        &s,
        "SELECT * WHERE { SERVICE algo:bfs { [] algo:node ?n ; algo:depth ?d } }",
    );
    assert!(e.contains("algo:source"), "{e}");
    let e = error(
        &s,
        "SELECT * WHERE { SERVICE algo:pageRank { [] algo:node ?n ; algo:score ?n } }",
    );
    assert!(e.contains("each output"), "{e}");
}

#[test]
fn directions_and_weights() {
    let s = store_of(
        "ex:a ex:p ex:b {| ex:w 5 |} . ex:b ex:p ex:c {| ex:w 1 |} . ex:a ex:p ex:c {| ex:w 10 |} .",
    );
    let r = rows(
        &s,
        "SELECT ?n ?d WHERE { SERVICE algo:sssp { [] algo:predicate ex:p ; algo:weight ex:w ; \
         algo:source ex:a ; algo:node ?n ; algo:distance ?d } }",
    );
    let d = by_node(&r, "d");
    assert_eq!(d["c"], "6");
    // backward edges: from c, a is reached through b with the same weights
    let r = rows(
        &s,
        "SELECT ?n ?d WHERE { SERVICE algo:sssp { [] algo:predicate ex:p ; algo:direction algo:backward ; \
         algo:weight ex:w ; algo:source ex:c ; algo:node ?n ; algo:distance ?d } }",
    );
    assert_eq!(by_node(&r, "d")["a"], "6");
}

#[test]
fn write_back_with_sparql_update() {
    let s = store_of(A);
    update(
        &s,
        &format!(
            "{PREFIXES} INSERT {{ GRAPH <urn:graph:rank> {{ ?n ex:rank ?r }} }} \
             WHERE {{ SERVICE algo:pageRank {{ [] algo:predicate ex:p ; algo:maxIterations 50 ; algo:node ?n ; algo:score ?r }} }}"
        ),
        &QueryOptions::default(),
    )
    .unwrap();
    let r = rows(
        &s,
        "SELECT ?n ?r WHERE { GRAPH <urn:graph:rank> { ?n ex:rank ?r } }",
    );
    assert_eq!(r.len(), 7);
    // a second run replaces the values
    update(
        &s,
        &format!(
            "{PREFIXES} DELETE {{ GRAPH <urn:graph:rank> {{ ?n ex:rank ?old }} }} \
             INSERT {{ GRAPH <urn:graph:rank> {{ ?n ex:rank ?r }} }} \
             WHERE {{ SERVICE algo:pageRank {{ [] algo:predicate ex:p ; algo:maxIterations 100 ; algo:node ?n ; algo:score ?r }} \
             OPTIONAL {{ GRAPH <urn:graph:rank> {{ ?n ex:rank ?old }} }} }}"
        ),
        &QueryOptions::default(),
    )
    .unwrap();
    let r = rows(
        &s,
        "SELECT ?n ?r WHERE { GRAPH <urn:graph:rank> { ?n ex:rank ?r } }",
    );
    assert_eq!(r.len(), 7, "no stale value is left: {r:?}");
}

#[test]
fn summary_and_stats_mode() {
    let s = store_of(A);
    let r = rows(
        &s,
        "SELECT ?s WHERE { SERVICE algo:louvain { [] algo:predicate ex:p ; algo:summary ?s } }",
    );
    assert_eq!(r.len(), 1);
    let v: serde_json::Value = serde_json::from_str(&r[0]["s"]).unwrap();
    assert_eq!(v["nodes"], 7);
    assert!(v["modularity"].is_number());
}

#[test]
fn louvain_levels_and_leiden() {
    let s = store_of(
        "ex:a1 ex:p ex:a2, ex:a3 . ex:a2 ex:p ex:a3 . ex:b1 ex:p ex:b2, ex:b3 . ex:b2 ex:p ex:b3 . ex:a3 ex:p ex:b1 .",
    );
    for algo in ["louvain", "leiden"] {
        let q = format!(
            "SELECT ?n ?c ?l WHERE {{ SERVICE algo:{algo} {{ [] algo:predicate ex:p {seed}; \
             algo:node ?n ; algo:community ?c ; algo:level ?l }} }}",
            seed = if algo == "leiden" {
                "; algo:seed 1 "
            } else {
                ""
            }
        );
        let r = rows(&s, &q);
        assert!(r.iter().any(|x| x["l"] == "0"), "{algo}");
        let last: BTreeMap<String, String> = {
            let top = r
                .iter()
                .map(|x| x["l"].parse::<usize>().unwrap())
                .max()
                .unwrap();
            r.iter()
                .filter(|x| x["l"].parse::<usize>().unwrap() == top)
                .map(|x| (x["n"].clone(), x["c"].clone()))
                .collect()
        };
        assert_eq!(last["a1"], last["a2"], "{algo}");
        assert_eq!(last["b1"], last["b2"], "{algo}");
        assert_ne!(last["a1"], last["b1"], "{algo}");
        // the same on a second run
        assert_eq!(rows(&s, &q), r, "{algo}");
    }
}

#[test]
fn embeddings_have_the_asked_dimension() {
    let s = store_of(A);
    let r = rows(
        &s,
        "SELECT ?n ?e WHERE { SERVICE algo:fastRP { [] algo:predicate ex:p ; algo:dimension 8 ; algo:seed 3 ; \
         algo:node ?n ; algo:embedding ?e } }",
    );
    assert_eq!(r.len(), 7);
    for row in &r {
        assert_eq!(row["e"].split(',').count(), 8, "{}", row["e"]);
    }
}

#[test]
fn nested_pattern_edges() {
    let s = store_of("ex:a ex:knows ex:b . ex:b ex:knows ex:c . ex:x ex:other ex:y .");
    let r = rows(
        &s,
        "SELECT ?n ?c WHERE { SERVICE algo:wcc { [] algo:start ?s ; algo:end ?o ; algo:node ?n ; algo:component ?c . \
         ?s ex:knows ?o } }",
    );
    assert_eq!(groups(&by_node(&r, "c")), set(&[&["a", "b", "c"]]));
}

#[test]
fn limit_takes_the_top_rows() {
    let s = store_of(A);
    let r = rows(
        &s,
        "SELECT ?n WHERE { SERVICE algo:pageRank { [] algo:predicate ex:p ; algo:maxIterations 100 ; \
         algo:limit 2 ; algo:node ?n ; algo:score ?r } }",
    );
    let names: BTreeSet<&str> = r.iter().map(|x| x["n"].as_str()).collect();
    assert_eq!(names, ["d", "e"].into());
}

#[test]
fn graph_variable_runs_once_per_named_graph() {
    let s = Store::in_memory(StoreOptions::default());
    s.load(&[Source::from_bytes(
        b"@prefix ex: <http://ex/> .\nex:g1 { ex:a ex:p ex:b . }\nex:g2 { ex:c ex:p ex:d . ex:d ex:p ex:e . }".to_vec(),
        RdfFormat::TriG,
        None,
    )])
    .unwrap();
    let r = rows(
        &s,
        "SELECT ?g ?n ?c WHERE { GRAPH ?g { SERVICE algo:wcc { [] algo:predicate ex:p ; algo:node ?n ; algo:component ?c } } }",
    );
    assert_eq!(r.len(), 5, "{r:?}");
    let g1: BTreeSet<&str> = r
        .iter()
        .filter(|x| x["g"] == "g1")
        .map(|x| x["n"].as_str())
        .collect();
    let g2: BTreeSet<&str> = r
        .iter()
        .filter(|x| x["g"] == "g2")
        .map(|x| x["n"].as_str())
        .collect();
    assert_eq!(g1, ["a", "b"].into());
    assert_eq!(g2, ["c", "d", "e"].into());
    let r = rows(
        &s,
        "SELECT ?n WHERE { GRAPH ex:g2 { SERVICE algo:wcc { [] algo:predicate ex:p ; algo:node ?n } } }",
    );
    assert_eq!(r.len(), 3);
}
