//! Declared nested read models. No write or browser delivery is asserted here.
#![cfg(feature = "shacl")]

use sparkles::Dataset;
use sparkles::access::{GraphAccess, GraphRule, Graphs};
use sparkles::forms::{FormRequest, FormSettings, ShapesSource};
use sparkles::sparql::QueryOptions;
use std::sync::Arc;

const PREFIX: &str = "@prefix sh: <http://www.w3.org/ns/shacl#> . @prefix ex: <http://ex/> . ";

fn dataset(shapes: &str, data: &str) -> (Dataset, FormRequest) {
    let ds = Dataset::memory();
    ds.load_str_into(
        &format!("{PREFIX}{shapes}"),
        sparkles::io::RdfFormat::Turtle,
        "urn:shapes",
    )
    .unwrap();
    ds.update(&format!("PREFIX ex: <http://ex/> INSERT DATA {{ {data} }}"))
        .unwrap();
    let request = FormRequest::new(
        "http://ex/root",
        FormSettings {
            shapes: ShapesSource::Graphs,
            graphs: vec!["urn:shapes".into()],
            ..Default::default()
        },
    );
    (ds, request)
}

#[test]
fn nested_model_keeps_shared_identity_cycles_and_all_visible_declared_shapes() {
    let (ds, req) = dataset(
        "ex:R sh:targetNode ex:root ; sh:property [ sh:path ex:p ; sh:node ex:S ] , [ sh:path ex:q ; sh:node ex:T ] , [ sh:path ex:r ; sh:node ex:S ] .
         ex:S sh:property [ sh:path ex:name ] , [ sh:path ex:next ; sh:node ex:S ] .
         ex:T sh:property [ sh:path ex:age ] .",
        "ex:root ex:p _:shared ; ex:r _:other . GRAPH <urn:second> { ex:root ex:p _:shared ; ex:q _:shared . _:shared ex:age 8 } _:shared a ex:Person ; ex:name \"same\" ; ex:next _:shared . _:other ex:name \"same\" .",
    );
    let before = ds.stats(&Default::default()).unwrap().terms;
    let nested = ds.forms().nested_model(&req, &Default::default()).unwrap();
    assert_eq!(nested.nested_nodes.len(), 2);
    let shared = nested
        .nested_nodes
        .iter()
        .find(|n| n.anchors.len() == 4)
        .unwrap();
    assert!(
        shared
            .anchors
            .iter()
            .any(|a| a.graph.as_deref() == Some("urn:second"))
    );
    assert!(shared.anchors.iter().any(|a| a.parent == shared.id));
    let same_field = shared
        .anchors
        .iter()
        .filter(|a| a.parent == "root" && a.field == "<http://ex/p>")
        .collect::<Vec<_>>();
    assert_eq!(same_field.len(), 2);
    assert!(same_field.iter().any(|a| a.graph.is_none()));
    assert!(
        same_field
            .iter()
            .any(|a| a.graph.as_deref() == Some("urn:second"))
    );
    assert!(
        shared
            .model
            .fields
            .iter()
            .any(|f| f.path == "http://ex/name")
    );
    assert!(
        shared
            .model
            .fields
            .iter()
            .any(|f| f.path == "http://ex/age")
    );
    assert!(shared.model.types.contains(&"http://ex/Person".to_owned()));
    let age = shared
        .model
        .fields
        .iter()
        .find(|f| f.path == "http://ex/age")
        .unwrap();
    assert_eq!(age.values[0].graph.as_deref(), Some("urn:second"));
    let name = shared
        .model
        .fields
        .iter()
        .find(|f| f.path == "http://ex/name")
        .unwrap();
    assert_eq!(name.values[0].graph, None);
    assert_ne!(nested.nested_nodes[0].term, nested.nested_nodes[1].term);
    let ordinary = ds.forms().model(&req, &Default::default()).unwrap();
    assert_eq!(
        serde_json::to_value(ordinary).unwrap(),
        serde_json::to_value(nested.root).unwrap()
    );
    assert_eq!(ds.stats(&Default::default()).unwrap().terms, before);
}

#[test]
fn nested_models_do_not_reuse_hidden_children_between_principals() {
    let (ds, req) = dataset(
        "ex:R sh:targetNode ex:root ; sh:property [ sh:path ex:p ; sh:node ex:S ] . ex:S sh:property [ sh:path ex:name ] .",
        "GRAPH <urn:public> { ex:root ex:p _:visible . _:visible ex:name \"public\" } GRAPH <urn:secret> { ex:root ex:p _:hidden . _:hidden ex:name \"secret\" }",
    );
    let restricted = QueryOptions {
        graphs: Some(Arc::new(GraphAccess::graphs(
            Graphs::Only(GraphRule::new(["urn:public", "urn:shapes"], &[])),
            Graphs::All,
        ))),
        ..Default::default()
    };
    for order in [[true, false], [false, true]] {
        for full in order {
            let opts = if full {
                QueryOptions::default()
            } else {
                restricted.clone()
            };
            let model = ds.forms().nested_model(&req, &opts).unwrap();
            assert_eq!(model.nested_nodes.len(), if full { 2 } else { 1 });
            let json = serde_json::to_string(&model).unwrap();
            if !full {
                assert!(!json.contains("secret"));
            }
        }
    }
}

#[test]
fn nested_model_refuses_identity_depth_and_shared_preparation_exhaustion() {
    let mut values = String::from("ex:root ex:p _:n0 . ");
    for n in 0..64 {
        values.push_str(&format!("_:n{n} ex:next _:n{} . ", n + 1));
    }
    let (ds, req) = dataset(
        "ex:R sh:targetNode ex:root ; sh:property [ sh:path ex:p ; sh:node ex:S ] . ex:S sh:property [ sh:path ex:next ; sh:node ex:S ] .",
        &values,
    );
    let error = ds
        .forms()
        .nested_model(&req, &Default::default())
        .unwrap_err();
    assert!(error.to_string().contains("depth allowance"));
    // A smaller request can refuse in the pre-existing target producer before
    // reaching this adapter. Keep that refusal as a separate diagnostic control.
    let minimal = QueryOptions {
        max_memory_bytes: Some(128),
        ..Default::default()
    };
    let early = ds.forms().nested_model(&req, &minimal).unwrap_err();
    eprintln!("minimal request refusal before adapter preparation: {early}");
    let small = QueryOptions {
        max_memory_bytes: Some(128_000),
        ..Default::default()
    };
    let error = ds.forms().nested_model(&req, &small).unwrap_err();
    assert!(
        error.to_string().contains("preparation allowance"),
        "{error}"
    );
    let cancelled = QueryOptions {
        cancel: Some(Arc::new(std::sync::atomic::AtomicBool::new(true))),
        ..Default::default()
    };
    assert!(matches!(
        ds.forms().nested_model(&req, &cancelled),
        Err(sparkles::Error::Cancelled)
    ));
}

#[test]
fn nested_model_rejects_an_unadmitted_complex_path_instead_of_omitting_children() {
    let (ds, req) = dataset(
        "ex:R sh:targetNode ex:root ; sh:property [ sh:path (ex:p ex:q) ; sh:node ex:S ] . ex:S sh:property [ sh:path ex:name ] .",
        "ex:root ex:p _:a . _:a ex:q _:b . _:b ex:name \"child\" .",
    );
    let error = ds
        .forms()
        .nested_model(&req, &Default::default())
        .unwrap_err();
    assert!(error.to_string().contains("complex nested path producer"));
}

#[test]
fn nested_preparation_admits_node_group_labels_before_rendering_them() {
    let large = "label".repeat(210_000);
    let (ds, req) = dataset(
        &format!(
            "ex:R sh:targetNode ex:root ; sh:node ex:Grouped . ex:Grouped sh:name \"{large}\" ; sh:property [ sh:path ex:p ] ."
        ),
        "ex:root ex:p \"value\" .",
    );
    let opts = QueryOptions {
        max_memory_bytes: Some(128_000),
        ..Default::default()
    };
    let error = ds.forms().nested_model(&req, &opts).unwrap_err();
    assert!(
        error.to_string().contains("preparation allowance"),
        "{error}"
    );
}

#[test]
fn nested_identity_cap_counts_distinct_visible_children_without_truncation() {
    let mut data = String::new();
    for n in 0..65 {
        data.push_str(&format!(
            "ex:root ex:p _:n{n} . _:n{n} ex:name \"child\" . "
        ));
    }
    let (ds, req) = dataset(
        "ex:R sh:targetNode ex:root ; sh:property [ sh:path ex:p ; sh:node ex:S ] . ex:S sh:property [ sh:path ex:name ] .",
        &data,
    );
    let error = ds
        .forms()
        .nested_model(&req, &Default::default())
        .unwrap_err();
    assert!(
        error.to_string().contains("identity or depth allowance"),
        "{error}"
    );
}

#[test]
fn historical_nested_nodes_read_the_selected_commit_and_are_read_only() {
    let (ds, mut req) = dataset(
        "ex:R sh:targetNode ex:root ; sh:property [ sh:path ex:p ; sh:node ex:S ] . ex:S sh:property [ sh:path ex:name ] .",
        "ex:root ex:p _:child . _:child ex:name \"before\" .",
    );
    ds.store()
        .create_snapshot("nested-before", &sparkles::history::At::Head, None)
        .unwrap();
    req.at = Some(sparkles::history::At::Snapshot("nested-before".into()));
    ds.update(
        "PREFIX ex: <http://ex/> INSERT { ?child ex:name \"after\" } WHERE { ex:root ex:p ?child }",
    )
    .unwrap();
    let model = ds.forms().nested_model(&req, &Default::default()).unwrap();
    assert!(!model.root.editable);
    assert!(!model.nested_nodes[0].model.editable);
    assert!(
        model.nested_nodes[0]
            .model
            .fields
            .iter()
            .all(|f| !f.editable)
    );
    let json = serde_json::to_string(&model).unwrap();
    assert!(json.contains("before"));
    assert!(!json.contains("after"));
}

#[test]
fn predicate_protections_hide_nested_anchors_in_both_principal_orders() {
    use sparkles::access::triples::{Caller, Limits, Protection, Rule, TripleRules};
    let (ds, req) = dataset(
        "ex:R sh:targetNode ex:root ; sh:property [ sh:path ex:p ; sh:node ex:S ] . ex:S sh:property [ sh:path ex:name ] .",
        "ex:root ex:p _:child . _:child ex:name \"private child\" .",
    );
    let restriction = QueryOptions {
        graphs: Some(Arc::new(GraphAccess::with_triples(
            Graphs::All,
            Graphs::All,
            TripleRules {
                rules: vec![Rule {
                    protection: Arc::new(Protection {
                        name: "private-child".into(),
                        predicates: Some(vec!["http://ex/p".into()]),
                        classes: None,
                        subclasses: false,
                        graphs: None,
                        pattern: None,
                        prefixes: Default::default(),
                        hide_inferences: false,
                    }),
                    read: Graphs::none(),
                    write: Graphs::none(),
                }],
                caller: Caller::default(),
                limits: Limits::default(),
            },
        ))),
        ..Default::default()
    };
    for order in [[true, false], [false, true]] {
        for full in order {
            let opts = if full {
                QueryOptions::default()
            } else {
                restriction.clone()
            };
            let model = ds.forms().nested_model(&req, &opts).unwrap();
            assert_eq!(model.nested_nodes.len(), usize::from(full));
            if !full {
                assert!(
                    !serde_json::to_string(&model)
                        .unwrap()
                        .contains("private child")
                );
            }
        }
    }
}

#[test]
fn visible_blank_named_graphs_refuse_instead_of_returning_an_incomplete_model() {
    use oxrdf::{BlankNode, GraphName, Literal, NamedNode, Quad};
    let (ds, req) = dataset(
        "ex:R sh:targetNode ex:root ; sh:property [ sh:path ex:p ; sh:node ex:S ] . ex:S sh:property [ sh:path ex:name ] .",
        "",
    );
    ds.transaction(|txn| {
        let graph = GraphName::BlankNode(BlankNode::new("graph").unwrap());
        let child = BlankNode::new("child").unwrap();
        txn.insert(
            Quad::new(
                NamedNode::new("http://ex/root").unwrap(),
                NamedNode::new("http://ex/p").unwrap(),
                child.clone(),
                graph.clone(),
            )
            .as_ref(),
        )?;
        txn.insert(
            Quad::new(
                child,
                NamedNode::new("http://ex/name").unwrap(),
                Literal::new_simple_literal("blank graph child"),
                graph,
            )
            .as_ref(),
        )?;
        Ok(())
    })
    .unwrap();
    let error = ds
        .forms()
        .nested_model(&req, &Default::default())
        .unwrap_err();
    assert!(
        error.to_string().contains("blank-named graph context"),
        "{error}"
    );
    let restricted = QueryOptions {
        graphs: Some(Arc::new(GraphAccess::graphs(
            Graphs::Only(GraphRule::new(["urn:shapes"], &[])),
            Graphs::none(),
        ))),
        ..Default::default()
    };
    let model = ds.forms().nested_model(&req, &restricted).unwrap();
    assert!(model.nested_nodes.is_empty());
    assert!(
        !serde_json::to_string(&model)
            .unwrap()
            .contains("blank graph child")
    );
}
