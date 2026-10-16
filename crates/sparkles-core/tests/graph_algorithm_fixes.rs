//! Regression tests for the roadmap item 8 merge fixes (graph algorithms, spec F14).

use sparkles_core::io::{RdfFormat, Source};
use sparkles_core::sparql::algorun::{AlgoRequest, expand_iri, query_text_with_summary};
use sparkles_core::sparql::{QueryOptions, query};
use sparkles_core::store::{Store, StoreOptions};

use std::collections::{BTreeMap, BTreeSet};

const PREFIXES: &str = "PREFIX ex: <http://ex/> PREFIX algo: <urn:x-sparkles:algo#> ";

fn trig(data: &str) -> Store {
    let s = Store::in_memory(StoreOptions::default());
    s.load(&[Source::from_bytes(
        format!("@prefix ex: <http://ex/> .\n{data}").into_bytes(),
        RdfFormat::TriG,
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

fn rows(s: &Store, q: &str) -> Vec<BTreeMap<String, String>> {
    let r = query(
        s.snapshot(),
        &format!("{PREFIXES}{q}"),
        &QueryOptions::default(),
    )
    .unwrap_or_else(|e| panic!("{q}: {e}"));
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

// The planner unrolls `GRAPH ?g { SERVICE algo:… }` into one branch for each named graph, so
// the two tests below hold the scoping of a nested pattern and of a variable source.
/// Guard: a nested edge pattern under GRAPH ?g must build one projection
/// per graph from that graph's edges only.
#[test]
fn nested_pattern_under_graph_var_is_scoped_per_graph() {
    let s = trig("ex:g1 { ex:a ex:p ex:b } ex:g2 { ex:c ex:p ex:d }");
    let r = rows(
        &s,
        "SELECT ?g ?n WHERE { GRAPH ?g { SERVICE algo:wcc { [] algo:start ?s ; algo:end ?e ; \
         algo:node ?n ; algo:component ?c . ?s ex:p ?e } } }",
    );
    let got: BTreeSet<(String, String)> = r
        .iter()
        .map(|row| (row["g"].clone(), row["n"].clone()))
        .collect();
    let want: BTreeSet<(String, String)> = [("g1", "a"), ("g1", "b"), ("g2", "c"), ("g2", "d")]
        .iter()
        .map(|(g, n)| (g.to_string(), n.to_string()))
        .collect();
    assert_eq!(got, want);
}

/// Guard: a variable algo:source under GRAPH ?g must seed each graph's
/// run only with the rows bound to that graph.
#[test]
fn variable_source_under_graph_var_uses_the_rows_of_that_graph() {
    // Both graphs hold the same chain a -> b -> c. g1 seeds a, g2 seeds c.
    let s = trig(
        "ex:g1 { ex:a ex:p ex:b . ex:b ex:p ex:c . ex:a a ex:Seed } \
         ex:g2 { ex:a ex:p ex:b . ex:b ex:p ex:c . ex:c a ex:Seed }",
    );
    let r = rows(
        &s,
        "SELECT ?g ?n ?d WHERE { GRAPH ?g { ?s a ex:Seed . SERVICE algo:bfs { [] algo:predicate ex:p ; \
         algo:source ?s ; algo:node ?n ; algo:depth ?d } } }",
    );
    let got: BTreeSet<(String, String, String)> = r
        .iter()
        .map(|row| (row["g"].clone(), row["n"].clone(), row["d"].clone()))
        .collect();
    // g1: a=0, b=1, c=2. g2: only c=0 (c has no out-edges).
    let want: BTreeSet<(String, String, String)> = [
        ("g1", "a", "0"),
        ("g1", "b", "1"),
        ("g1", "c", "2"),
        ("g2", "c", "0"),
    ]
    .iter()
    .map(|(g, n, d)| (g.to_string(), n.to_string(), d.to_string()))
    .collect();
    assert_eq!(got, want);
}

/// A prefixed-name source is spliced into the query text, so it must be validated as an
/// IRI after expansion. The MCP `graph_algorithm` tool and every other front end build
/// their text through this function.
#[test]
fn prefixed_source_cannot_inject_query_syntax() {
    let req = AlgoRequest::from_json(serde_json::json!({
        "algorithm": "pageRank",
        "projection": {"predicates": ["http://ex/p"]},
        "params": {
            "source": "ex:a> ; algo:node ?n0 } ?node ?score ?summary } #",
            "limit": 100
        }
    }))
    .unwrap();
    let prefixes = vec![("ex".to_string(), "http://ex/".to_string())];
    assert!(query_text_with_summary(&req, &prefixes).is_err());
}

#[test]
fn prefixed_names_keep_valid_forms() {
    let p = vec![("ex".to_string(), "http://ex/".to_string())];
    assert_eq!(expand_iri("ex:a", &p).unwrap(), "http://ex/a");
    assert_eq!(expand_iri("ex:", &p).unwrap(), "http://ex/");
    assert_eq!(expand_iri("ex:a%3Eb", &p).unwrap(), "http://ex/a%3Eb");
    assert_eq!(expand_iri("<http://ex/a>", &p).unwrap(), "http://ex/a");
    assert!(expand_iri("<http://ex/a b>", &p).is_err());
    assert!(expand_iri("ex:a b", &p).is_err());
    assert!(expand_iri("ex:a>", &p).is_err());
    assert!(expand_iri("<ex:a>b>", &p).is_err());
    // a namespace that is not itself a valid IRI prefix is refused once expanded
    let bad = vec![("bad".to_string(), "not an iri ".to_string())];
    assert!(expand_iri("bad:x", &bad).is_err());
}

/// An algorithm call's rows are counted once against `max_rows_produced`.
#[test]
fn algorithm_rows_are_counted_once() {
    let s = trig("ex:a ex:p ex:b . ex:b ex:p ex:c . ex:d ex:p ex:e .");
    let q = format!(
        "{PREFIXES}SELECT ?n ?c WHERE {{ SERVICE algo:wcc {{ [] algo:predicate ex:p ; algo:node ?n ; algo:component ?c }} }}"
    );
    let o = QueryOptions {
        no_cache: true,
        ..Default::default()
    };
    let r = query(s.snapshot(), &q, &o).unwrap();
    assert_eq!(r.len(), 5);
    let v = format!(
        "{PREFIXES}SELECT ?n ?c WHERE {{ VALUES (?n ?c) {{ (1 1) (2 2) (3 3) (4 4) (5 5) }} }}"
    );
    let baseline = query(s.snapshot(), &v, &o).unwrap();
    assert_eq!(
        r.rows_produced, baseline.rows_produced,
        "wcc counted {} rows, a 5-row VALUES counts {}",
        r.rows_produced, baseline.rows_produced
    );
}

/// A budget of exactly the rows produced passes, cold and from the cache.
#[test]
fn algorithm_rows_fit_a_budget_of_their_own_count() {
    let s = trig("ex:a ex:p ex:b . ex:b ex:p ex:c . ex:d ex:p ex:e .");
    let q = format!(
        "{PREFIXES}SELECT ?n ?c WHERE {{ SERVICE algo:wcc {{ [] algo:predicate ex:p ; algo:node ?n ; algo:component ?c }} }}"
    );
    let cold = query(
        s.snapshot(),
        &q,
        &QueryOptions {
            no_cache: true,
            ..Default::default()
        },
    )
    .unwrap();
    let o = QueryOptions {
        max_rows_produced: Some(cold.rows_produced),
        ..Default::default()
    };
    // the first run is cold and fills the cache, the second is a cache hit
    for _ in 0..2 {
        let r = query(s.snapshot(), &q, &o).unwrap();
        assert_eq!(r.len(), 5);
        assert!(r.rows_produced <= cold.rows_produced);
    }
}

/// A huge FastRP dimension fails with an error, not a panic or an abort.
#[test]
fn fastrp_huge_dimension_is_refused() {
    // four nodes: 4 * 2^62 wraps usize to 0
    let s = trig("ex:a ex:p ex:b . ex:c ex:p ex:d .");
    for algo in ["fastRP", "node2vec"] {
        for dim in ["4611686018427387904", "100000", "16385", "1e20"] {
            let q = format!(
                "{PREFIXES}SELECT ?n ?e WHERE {{ SERVICE algo:{algo} {{ [] algo:predicate ex:p ; algo:dimension {dim} ; algo:node ?n ; algo:embedding ?e }} }}"
            );
            let snap = s.snapshot();
            let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                query(snap, &q, &QueryOptions::default()).map(|r| r.len())
            }));
            match r {
                Err(_) => panic!("{algo} dimension {dim}: the query panicked"),
                Ok(Ok(n)) => panic!("{algo} dimension {dim}: accepted with {n} rows"),
                Ok(Err(e)) => assert!(
                    e.to_string().contains("dimension"),
                    "{algo} dimension {dim}: {e}"
                ),
            }
        }
    }
}

/// The largest dimension of a vector index is still accepted, and an empty graph still
/// returns no rows.
#[test]
fn fastrp_dimension_bounds() {
    let s = trig("ex:a ex:p ex:b .");
    let q = format!(
        "{PREFIXES}SELECT ?n ?e WHERE {{ SERVICE algo:fastRP {{ [] algo:predicate ex:p ; algo:dimension 16384 ; algo:node ?n ; algo:embedding ?e }} }}"
    );
    assert_eq!(
        query(s.snapshot(), &q, &QueryOptions::default())
            .unwrap()
            .len(),
        2
    );
    let empty = trig("ex:a ex:q ex:b .");
    assert_eq!(
        query(empty.snapshot(), &q, &QueryOptions::default())
            .unwrap()
            .len(),
        0
    );
}

/// `nodeSelfInfluence` is a FastRP parameter, and an empty `iterationWeights` needs it.
#[test]
fn fastrp_node_self_influence_is_a_parameter() {
    let s = trig("ex:a ex:p ex:b . ex:b ex:p ex:c .");
    let run = |params: &str| {
        let q = format!(
            "{PREFIXES}SELECT ?n ?e WHERE {{ SERVICE algo:fastRP {{ [] algo:predicate ex:p ; algo:dimension 8 ; {params} algo:node ?n ; algo:embedding ?e }} }}"
        );
        query(s.snapshot(), &q, &QueryOptions::default())
    };
    assert_eq!(
        run("algo:iterationWeights (1.0) ; algo:nodeSelfInfluence 0.5 ;")
            .unwrap()
            .len(),
        3
    );
    assert_eq!(
        run("algo:iterationWeights () ; algo:nodeSelfInfluence 1 ;")
            .unwrap()
            .len(),
        3
    );
    let Err(e) = run("algo:iterationWeights () ;") else {
        panic!("an empty list with no self influence was accepted");
    };
    assert!(e.to_string().contains("iterationWeights"), "{e}");
    assert_eq!(run("").unwrap().len(), 3);
}

/// `algo:samples` at or above the node count is an exact run, so it obeys the exact limit.
/// The test changes a process-wide limit, so it is the only betweenness test of this file.
#[test]
fn betweenness_samples_at_or_above_n_obey_the_exact_limit() {
    sparkles_core::algo::pool::set_exact_betweenness_max(2);
    let s = trig("ex:a ex:p ex:b . ex:b ex:p ex:c . ex:c ex:p ex:d .");
    let run = |samples: u32| {
        let q = format!(
            "{PREFIXES}SELECT ?n ?r WHERE {{ SERVICE algo:betweenness {{ [] algo:predicate ex:p ; algo:samples {samples} ; algo:node ?n ; algo:score ?r }} }}"
        );
        query(s.snapshot(), &q, &QueryOptions::default())
    };
    let (above, equal, below) = (run(1000), run(4), run(3));
    sparkles_core::algo::pool::set_exact_betweenness_max(100_000);
    assert!(
        above.is_err(),
        "an exact run on 4 nodes passed a limit of 2"
    );
    assert!(equal.is_err(), "samples equal to n is an exact run");
    assert_eq!(below.unwrap().len(), 4, "samples below n stays sampled");
    // within the limit, samples at or above n stays exact and allowed
    assert_eq!(run(1000).unwrap().len(), 4);
}
