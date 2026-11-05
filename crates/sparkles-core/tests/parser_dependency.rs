//! The local parser's safety and registration APIs survive dependency refreshes.

use oxrdf::NamedNode;
use spargebra::SparqlParser;
use std::collections::HashSet;
use std::sync::Arc;

#[test]
fn query_and_update_nesting_are_rejected_before_recursive_parsing() {
    let nested = format!("{}?s <urn:p> ?o{}", "{".repeat(300), "}".repeat(300));
    for result in [
        SparqlParser::new()
            .parse_query(&format!("SELECT * WHERE {nested}"))
            .map(|_| ()),
        SparqlParser::new()
            .parse_update(&format!("DELETE WHERE {nested}"))
            .map(|_| ()),
    ] {
        let error = result.unwrap_err().to_string();
        assert!(error.contains("nested deeper than"), "{error}");
    }
}

#[test]
fn long_expression_chains_are_rejected_before_building_recursive_algebra() {
    let expression = std::iter::repeat_n("true", 2000)
        .collect::<Vec<_>>()
        .join(" || ");
    let error = SparqlParser::new()
        .parse_query(&format!("SELECT * {{ FILTER({expression}) }}"))
        .unwrap_err()
        .to_string();
    assert!(error.contains("nested deeper than"), "{error}");
}

#[test]
fn shared_aggregate_registration_preserves_existing_names_when_extended() {
    let names = Arc::new(HashSet::from([NamedNode::new_unchecked("urn:shared")]));
    let parser = SparqlParser::new().with_custom_aggregate_functions(&names);
    let extended = parser
        .clone()
        .with_custom_aggregate_function(NamedNode::new_unchecked("urn:extra"));
    for name in ["urn:shared", "urn:extra"] {
        assert!(
            extended
                .clone()
                .parse_query(&format!(
                    "SELECT (<{name}>(DISTINCT ?x) AS ?v) {{ VALUES ?x {{1}} }}"
                ))
                .is_ok()
        );
    }
    assert!(
        parser
            .parse_query("SELECT (<urn:shared>(DISTINCT ?x) AS ?v) { VALUES ?x {1} }")
            .is_ok()
    );
    assert_eq!(names.len(), 1);
}
