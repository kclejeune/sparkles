//! Regression tests for the review follow-ups of roadmap item 8 (graph algorithms, spec
//! F14).

use sparkles_core::io::{RdfFormat, Source};
use sparkles_core::sparql::algorun::{AlgoRequest, query_text};
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

fn request(v: serde_json::Value) -> AlgoRequest {
    AlgoRequest::from_json(v).unwrap()
}

/// The name of a named projection and the metric are the only free strings of a
/// request. Both are checked before they reach the query text.
#[test]
fn the_projection_name_and_the_metric_of_a_request_are_validated() {
    for name in ["a\"b", "", "-lead", "has space", "x\\u0022"] {
        let req = request(serde_json::json!({"algorithm": "wcc", "projection": name}));
        assert!(query_text(&req, &[], false).is_err(), "{name:?}");
    }
    let req = request(serde_json::json!({"algorithm": "wcc", "projection": "friends_2-x"}));
    let (q, _) = query_text(&req, &[], false).unwrap();
    assert!(q.contains("algo:projection \"friends_2-x\""), "{q}");

    for metric in ["cosine", "jaccard\"", "Jaccard", ""] {
        let req = request(serde_json::json!({
            "algorithm": "nodeSimilarity",
            "projection": {"predicates": ["http://ex/p"]},
            "params": {"metric": metric}
        }));
        assert!(query_text(&req, &[], false).is_err(), "{metric:?}");
    }
    let s = trig("ex:a ex:p ex:c . ex:b ex:p ex:c .");
    for metric in ["jaccard", "overlap"] {
        let req = request(serde_json::json!({
            "algorithm": "nodeSimilarity",
            "projection": {"predicates": ["http://ex/p"], "direction": "forward"},
            "params": {"metric": metric}
        }));
        let (q, _) = query_text(&req, &[], false).unwrap();
        assert!(q.contains(&format!("algo:metric algo:{metric}")), "{q}");
        let r = query(s.snapshot(), &q, &QueryOptions::default()).unwrap();
        assert_eq!(r.len(), 2, "{metric}");
    }
}

/// An integer parameter keeps its exact value: two seeds that one double cannot tell
/// apart give two different runs.
#[test]
fn integer_parameters_keep_their_exact_value() {
    let s = trig("ex:a ex:p ex:b . ex:b ex:p ex:c . ex:c ex:p ex:a .");
    let embedding = |seed: &str| {
        let q = format!(
            "{PREFIXES}SELECT ?n ?e WHERE {{ SERVICE algo:fastRP {{ [] algo:predicate ex:p ; algo:dimension 8 ; algo:seed {seed} ; algo:node ?n ; algo:embedding ?e }} }} ORDER BY ?n"
        );
        let r = query(s.snapshot(), &q, &QueryOptions::default()).unwrap();
        format!("{:?}", r.rows())
    };
    let (a, b) = (embedding("9007199254740992"), embedding("9007199254740993"));
    assert_ne!(a, b, "seeds 2^53 and 2^53 + 1 ran the same");
    assert_eq!(a, embedding("9007199254740992"));
    // the largest seed is exact too
    assert_ne!(
        embedding("9223372036854775807"),
        embedding("9223372036854775806")
    );
    // a whole double is still accepted while it is exact, and refused above 2^53
    assert_eq!(embedding("5.0e0"), embedding("5"));
    for bad in ["1.0e17", "-1", "2.5"] {
        let q = format!(
            "{PREFIXES}SELECT ?n WHERE {{ SERVICE algo:fastRP {{ [] algo:predicate ex:p ; algo:seed {bad} ; algo:node ?n }} }}"
        );
        assert!(
            query(s.snapshot(), &q, &QueryOptions::default()).is_err(),
            "{bad}"
        );
    }
}

/// An exact integer above `i64::MAX` is refused with a message that says what an integer
/// parameter may be, not with "takes a number".
#[test]
fn an_integer_above_the_exact_range_is_refused_by_name() {
    let s = trig("ex:a ex:p ex:b . ex:b ex:p ex:c . ex:c ex:p ex:a .");
    for big in [
        "9223372036854775808",
        "18446744073709551615",
        "99999999999999999999",
    ] {
        let q = format!(
            "{PREFIXES}SELECT ?n WHERE {{ SERVICE algo:fastRP {{ [] algo:predicate ex:p ; algo:seed {big} ; algo:node ?n }} }}"
        );
        let e = query(s.snapshot(), &q, &QueryOptions::default())
            .err()
            .unwrap_or_else(|| panic!("{big} was accepted"))
            .to_string();
        assert!(
            e.contains("algo:seed takes an integer from 0 to 9223372036854775807"),
            "{big}: {e}"
        );
    }
}

/// The iteration and level counts are 32-bit. A larger value is refused instead of
/// wrapping to a few iterations or none.
#[test]
fn a_count_above_u32_is_refused_and_not_wrapped() {
    let s = trig("ex:a ex:p ex:b . ex:b ex:p ex:c . ex:c ex:p ex:a .");
    let run = |algo: &str, param: &str, value: &str| {
        query(
            s.snapshot(),
            &format!(
                "{PREFIXES}SELECT ?n WHERE {{ SERVICE algo:{algo} {{ [] algo:predicate ex:p ; algo:{param} {value} ; algo:node ?n }} }}"
            ),
            &QueryOptions::default(),
        )
    };
    for (algo, param) in [
        ("pageRank", "maxIterations"),
        ("labelPropagation", "maxIterations"),
        ("louvain", "maxIterations"),
        ("louvain", "maxLevels"),
        ("leiden", "maxLevels"),
    ] {
        // 2^32 wrapped to 0
        for value in ["4294967296", "4294967297", "9223372036854775807"] {
            let e = run(algo, param, value)
                .err()
                .unwrap_or_else(|| panic!("{algo} {param} {value} was accepted"))
                .to_string();
            assert!(
                e.contains(&format!("algo:{param} must be at most 4294967295")),
                "{algo} {param} {value}: {e}"
            );
        }
    }
    // the largest 32-bit count is still accepted
    assert_eq!(
        run("pageRank", "maxIterations", "4294967295")
            .unwrap()
            .len(),
        3
    );
}

/// The test changes a process-wide limit, so it is the only betweenness test of this
/// file. In `graph_algorithms.rs` it raced the betweenness tests that run without
/// sampling.
#[test]
fn a13_betweenness_over_the_node_limit() {
    let s = trig(
        "ex:a ex:p ex:b . ex:b ex:p ex:c . ex:c ex:p ex:a . ex:c ex:p ex:d .
         ex:d ex:p ex:e . ex:e ex:p ex:d . ex:f ex:p ex:g . ex:h a ex:N .",
    );
    sparkles_core::algo::pool::set_exact_betweenness_max(3);
    let run = |extra: &str| {
        query(
            s.snapshot(),
            &format!(
                "{PREFIXES}SELECT * WHERE {{ SERVICE algo:betweenness {{ [] algo:predicate ex:p ;{extra} algo:node ?n ; algo:score ?r }} }}"
            ),
            &QueryOptions::default(),
        )
    };
    let e = run("").err().expect("an error over the limit").to_string();
    assert!(e.contains("algo:samples"), "{e}");
    // sampling is allowed
    assert!(!run(" algo:samples 2 ;").unwrap().rows().is_empty());
    sparkles_core::algo::pool::set_exact_betweenness_max(100_000);
}
