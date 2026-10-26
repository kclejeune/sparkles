//! Exact SHACL target semantics during bounded affected-target admission.

use oxrdf::{NamedNode, Term};
use sparkles_core::io::{RdfFormat, Source};
use sparkles_core::store::{Store, StoreOptions};
use sparkles_shacl::incremental::{AffectedLimits, validate_affected};
use sparkles_shacl::{Shapes, ValidateOptions, ValidationReport};

fn rows(report: &ValidationReport) -> Vec<(String, String, String)> {
    let mut rows: Vec<_> = report
        .results
        .iter()
        .map(|r| {
            (
                r.focus_node.to_string(),
                r.source_shape.to_string(),
                r.source_constraint_component.to_string(),
            )
        })
        .collect();
    rows.sort();
    rows
}

#[test]
fn a_shared_focus_expression_keeps_each_owning_shapes_identity() {
    let store = Store::in_memory(StoreOptions::default());
    store
        .load(&[Source::from_bytes(
            b"<urn:A> <urn:required> 1 . <urn:B> <urn:other> 1 .".to_vec(),
            RdfFormat::Turtle,
            None,
        )])
        .unwrap();
    let shapes = Shapes::parse(
        r#"
        @prefix sh: <http://www.w3.org/ns/shacl#> .
        @prefix shnex: <http://www.w3.org/ns/shacl-node-expr#> .
        _:self shnex:var "focusNode" .
        <urn:A> a sh:NodeShape ; sh:targetNode _:self ;
            sh:property [ sh:path <urn:required> ; sh:minCount 1 ] .
        <urn:B> a sh:NodeShape ; sh:targetNode _:self ;
            sh:property [ sh:path <urn:required> ; sh:minCount 1 ] .
    "#,
        RdfFormat::Turtle,
        None,
    )
    .unwrap();
    let nodes: Vec<Term> = ["urn:A", "urn:B"]
        .into_iter()
        .map(|iri| NamedNode::new(iri).unwrap().into())
        .collect();
    let opts = ValidateOptions {
        parallel: false,
        ..Default::default()
    };
    let snapshot = store.snapshot();
    let expected = sparkles_shacl::validate_nodes(&snapshot, &shapes, &nodes, &opts).unwrap();
    assert_eq!(expected.results.len(), 1);
    assert_eq!(expected.results[0].focus_node, nodes[1]);
    let actual = validate_affected(
        &snapshot,
        &snapshot,
        &shapes,
        &[],
        &nodes,
        &opts,
        AffectedLimits::default(),
    )
    .unwrap();
    assert_eq!(
        rows(&actual),
        rows(&expected),
        "a cached target expression must keep its owning shape focus"
    );
}

#[test]
fn node_local_target_filters_keep_ordinary_target_membership() {
    let store = Store::in_memory(StoreOptions::default());
    store
        .load(&[Source::from_bytes(
            b"<urn:A> <urn:required> 1 . <urn:B> <urn:other> 1 .".to_vec(),
            RdfFormat::Turtle,
            None,
        )])
        .unwrap();
    let shapes = Shapes::parse(
        r#"
        @prefix sh: <http://www.w3.org/ns/shacl#> .
        <urn:S> a sh:NodeShape ; sh:targetWhere [ sh:nodeKind sh:IRI ] ;
            sh:property [ sh:path <urn:required> ; sh:minCount 1 ] .
    "#,
        RdfFormat::Turtle,
        None,
    )
    .unwrap();
    let nodes: Vec<Term> = ["urn:A", "urn:B"]
        .into_iter()
        .map(|iri| NamedNode::new(iri).unwrap().into())
        .collect();
    let opts = ValidateOptions {
        parallel: false,
        ..Default::default()
    };
    let snapshot = store.snapshot();
    let expected = sparkles_shacl::validate_nodes(&snapshot, &shapes, &nodes, &opts).unwrap();
    let actual = validate_affected(
        &snapshot,
        &snapshot,
        &shapes,
        &[],
        &nodes,
        &opts,
        AffectedLimits::default(),
    )
    .unwrap();
    assert_eq!(rows(&actual), rows(&expected));
    assert_eq!(actual.results.len(), 1);
    assert_eq!(actual.results[0].focus_node, nodes[1]);
}
