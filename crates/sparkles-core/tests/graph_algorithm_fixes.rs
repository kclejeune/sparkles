//! Regression tests for the roadmap item 8 merge fixes (graph algorithms, spec F14).

use sparkles_core::sparql::algorun::{AlgoRequest, expand_iri, query_text_with_summary};

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
