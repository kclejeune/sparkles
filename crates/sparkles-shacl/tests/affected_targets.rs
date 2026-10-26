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

fn query_fixture(query: &str) -> (Store, Shapes, Vec<Term>) {
    let store = Store::in_memory(StoreOptions::default());
    store
        .load(&[Source::from_bytes(
            b"<urn:Alice> <urn:name> \"Alice\" . <urn:Bob> <urn:bad> true .".to_vec(),
            RdfFormat::Turtle,
            None,
        )])
        .unwrap();
    let text = format!(
        r#"@prefix sh: <http://www.w3.org/ns/shacl#> . <urn:S> a sh:NodeShape ; sh:targetNode <urn:Alice>, <urn:Bob> ; sh:sparql [ sh:select "{query}" ] ."#
    );
    let shapes = Shapes::parse(&text, RdfFormat::Turtle, None).unwrap();
    let nodes = ["urn:Alice", "urn:Bob"]
        .into_iter()
        .map(|iri| NamedNode::new(iri).unwrap().into())
        .collect();
    (store, shapes, nodes)
}

#[test]
fn local_query_dependencies_do_not_report_an_unrelated_existing_violation() {
    use oxrdf::{GraphName, Literal, Quad};
    use sparkles_core::sparql::{QueryOptions, update};
    let (store, shapes, nodes) = query_fixture("SELECT $this WHERE { $this <urn:bad> true }");
    let before = store.snapshot();
    update::update(
        &store,
        "INSERT DATA { <urn:Alice> <urn:name> \"Alicia\" }",
        &QueryOptions::default(),
    )
    .unwrap();
    let after = store.snapshot();
    let changes = [Quad::new(
        NamedNode::new("urn:Alice").unwrap(),
        NamedNode::new("urn:name").unwrap(),
        Literal::new_simple_literal("Alicia"),
        GraphName::DefaultGraph,
    )];
    let opts = ValidateOptions {
        parallel: false,
        ..Default::default()
    };
    let expected = sparkles_shacl::validate_nodes(&after, &shapes, &nodes[..1], &opts).unwrap();
    assert!(expected.conforms);
    let actual = validate_affected(
        &before,
        &after,
        &shapes,
        &changes,
        &nodes[..1],
        &opts,
        AffectedLimits::default(),
    )
    .unwrap();
    assert_eq!(
        rows(&actual),
        rows(&expected),
        "unavailable localization must not broaden a local query to Bob"
    );
}

#[test]
fn genuinely_global_query_dependencies_still_enumerate_actual_targets() {
    let (store, shapes, nodes) = query_fixture("SELECT $this WHERE { ?other <urn:bad> true }");
    let snapshot = store.snapshot();
    let opts = ValidateOptions {
        parallel: false,
        ..Default::default()
    };
    let expected = sparkles_shacl::validate_nodes(&snapshot, &shapes, &nodes, &opts).unwrap();
    let actual = validate_affected(
        &snapshot,
        &snapshot,
        &shapes,
        &[],
        &nodes[..1],
        &opts,
        AffectedLimits::default(),
    )
    .unwrap();
    assert_eq!(rows(&actual), rows(&expected));
    assert_eq!(actual.results.len(), 2);
}

#[test]
fn unsupported_query_dependency_production_is_incomplete_not_a_global_report() {
    let (store, shapes, nodes) =
        query_fixture("SELECT $this WHERE { $this <urn:friend> ?friend . ?friend <urn:bad> true }");
    let snapshot = store.snapshot();
    let error = validate_affected(
        &snapshot,
        &snapshot,
        &shapes,
        &[],
        &nodes[..1],
        &ValidateOptions::default(),
        AffectedLimits::default(),
    )
    .expect_err("indirect query dependency producers need early admission");
    assert!(
        matches!(
            error.downcast_ref::<sparkles_core::Error>(),
            Some(sparkles_core::Error::Unsupported(_))
        ),
        "{error:#}"
    );
}

const LIST_QUERY_SHAPES: &str = r#"
    @prefix sh: <http://www.w3.org/ns/shacl#> .
    <urn:S> a sh:NodeShape ; sh:targetNode <urn:head> ;
      sh:sparql [ sh:select 'SELECT $this WHERE { $this <http://jena.apache.org/ARQ/list#member> "bad" }' ] .
"#;

fn list_store() -> Store {
    let store = Store::in_memory(StoreOptions::default());
    store
        .load(&[Source::from_bytes(
            br#"@prefix rdf: <http://www.w3.org/1999/02/22-rdf-syntax-ns#> .
        <urn:head> rdf:first "one" ; rdf:rest <urn:tail> .
        <urn:tail> rdf:first "two" ; rdf:rest rdf:nil ."#
                .to_vec(),
            RdfFormat::Turtle,
            None,
        )])
        .unwrap();
    store
}

const BAD_LIST_EDIT: &str = r#"DELETE DATA { <urn:tail> <http://www.w3.org/1999/02/22-rdf-syntax-ns#first> "two" } ;
    INSERT DATA { <urn:tail> <http://www.w3.org/1999/02/22-rdf-syntax-ns#first> "bad" }"#;

#[test]
fn graph_reading_property_functions_do_not_miss_a_downstream_list_edit() {
    use oxrdf::{GraphName, Literal, Quad};
    use sparkles_core::sparql::{QueryOptions, update};
    let store = list_store();
    let shapes = Shapes::parse(LIST_QUERY_SHAPES, RdfFormat::Turtle, None).unwrap();
    let before = store.snapshot();
    update::update(&store, BAD_LIST_EDIT, &QueryOptions::default()).unwrap();
    let after = store.snapshot();
    let full = sparkles_shacl::validate(&after, &shapes, &ValidateOptions::default()).unwrap();
    assert!(!full.conforms);
    assert_eq!(full.results.len(), 1);
    assert_eq!(
        full.results[0].focus_node,
        Term::NamedNode(NamedNode::new("urn:head").unwrap())
    );
    let changes = [Quad::new(
        NamedNode::new("urn:tail").unwrap(),
        NamedNode::new("http://www.w3.org/1999/02/22-rdf-syntax-ns#first").unwrap(),
        Literal::new_simple_literal("bad"),
        GraphName::DefaultGraph,
    )];
    let touched = [Term::NamedNode(NamedNode::new("urn:tail").unwrap())];
    let error = validate_affected(
        &before,
        &after,
        &shapes,
        &changes,
        &touched,
        &ValidateOptions::default(),
        AffectedLimits::default(),
    )
    .expect_err("synthetic predicates need their actual graph-read producer");
    assert!(
        matches!(
            error.downcast_ref::<sparkles_core::Error>(),
            Some(sparkles_core::Error::Unsupported(_))
        ),
        "{error:#}"
    );
}

#[test]
fn a_strict_write_guard_rejects_a_property_function_downstream_violation() {
    use sparkles_core::{
        Error,
        guard::{GuardMode, Severity},
        sparql::{QueryOptions, update},
    };
    use sparkles_shacl::guard::{
        self, BaselinePolicy, DataGraphSel, SetOutcome, ShapesSource, ValidationConfig,
    };
    let store = list_store();
    let config = ValidationConfig {
        format: 2,
        language: None,
        mode: GuardMode::Reject,
        shapes: ShapesSource {
            inline: Some(LIST_QUERY_SHAPES.into()),
            ..Default::default()
        },
        data_graph: DataGraphSel::Named("default".into()),
        include_inferences: false,
        threshold: Severity::Violation,
        baseline: BaselinePolicy::Strict,
        timeout_seconds: 60.0,
        report_limit: 100,
        updated: None,
    };
    assert!(matches!(
        guard::set_config(&store, Some(config)).unwrap(),
        SetOutcome::Installed(..)
    ));
    // Prime any unknown-head baseline before testing dependency-based admission.
    update::update(
        &store,
        "INSERT DATA { <urn:other> <urn:unrelated> 1 }",
        &QueryOptions::default(),
    )
    .unwrap();
    let result = update::update(&store, BAD_LIST_EDIT, &QueryOptions::default());
    assert!(
        matches!(result, Err(Error::Rejected(_))),
        "guard accepted a downstream synthetic-predicate violation: {result:?}"
    );
    let shapes = Shapes::parse(LIST_QUERY_SHAPES, RdfFormat::Turtle, None).unwrap();
    assert!(
        sparkles_shacl::validate(&store.snapshot(), &shapes, &ValidateOptions::default())
            .unwrap()
            .conforms
    );
}
