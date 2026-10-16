//! Regression tests for the roadmap item 8 merge fixes (graph algorithms, spec F14).

use sparkles_core::io::{RdfFormat, Source};
use sparkles_core::sparql::algorun::{AlgoRequest, expand_iri, query_text_with_summary};
use sparkles_core::sparql::{QueryOptions, query};
use sparkles_core::store::{Store, StoreOptions};

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
