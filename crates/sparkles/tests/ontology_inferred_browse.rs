#![cfg(feature = "ontology")]

use serde_json::{Value, json};
use sparkles::Dataset;
use sparkles::handles::ontology::wire;
use sparkles::reasoning::ReasonRequest;
use sparkles::sparql::QueryOptions;
use sparkles_reasoner::Profile;

fn fixture() -> Dataset {
    let ds = Dataset::memory();
    ds.update(
        "PREFIX : <urn:> PREFIX owl: <http://www.w3.org/2002/07/owl#>
         PREFIX rdfs: <http://www.w3.org/2000/01/rdf-schema#>
         INSERT DATA {
           :A a owl:Class; rdfs:subClassOf :B . :B a owl:Class; rdfs:subClassOf :C . :C a owl:Class .
           :p a owl:ObjectProperty; rdfs:subPropertyOf :q, :p2 .
           :p2 a owl:ObjectProperty; rdfs:subPropertyOf :p .
           :q a owl:ObjectProperty; rdfs:subPropertyOf :r . :r a owl:ObjectProperty .
           :dp a owl:DatatypeProperty; rdfs:subPropertyOf :dq .
           :dq a owl:DatatypeProperty; rdfs:subPropertyOf :dr . :dr a owl:DatatypeProperty .
           :i a owl:NamedIndividual, :A .
         }",
    )
    .unwrap();
    ds
}

fn call(ds: &Dataset, op: &str, args: Value) -> sparkles::Result<Value> {
    wire::call(ds, op, &args, &QueryOptions::default())
}

fn materialize(ds: &Dataset) {
    ds.reasoning()
        .run(&ReasonRequest {
            profile: Profile::Rdfs,
            ..Default::default()
        })
        .unwrap();
}

fn selection(ds: &Dataset) -> Value {
    json!({"inferences":true, "at":format!("commit:{}",ds.head_commit().seq)})
}

#[test]
fn inferred_browse_kept_roots_and_children_read_only_pinned_pages() {
    let ds = fixture();
    call(&ds, "classify", json!({"threads":1})).unwrap();
    let head = ds.head_commit().seq;
    let roots = call(
        &ds,
        "keptClassification",
        json!({"part":"roots", "limit":1, "at":format!("commit:{head}")}),
    )
    .unwrap();
    assert_eq!(roots["status"], "available");
    assert_eq!(roots["page"]["items"][0]["iri"], "urn:C");
    assert_eq!(roots["page"]["items"][0]["children"], 1);
    let children = call(
        &ds,
        "keptClassification",
        json!({"part":"children", "about":"urn:C", "limit":1,
               "resultId":roots["result"]["resultId"], "at":format!("commit:{head}")}),
    )
    .unwrap();
    assert_eq!(children["page"]["items"][0]["iri"], "urn:B");
    assert_eq!(ds.head_commit().seq, head);
}

#[test]
fn inferred_browse_materialized_property_hierarchy_reduces_transitive_edges_and_cycles() {
    let ds = fixture();
    materialize(&ds);
    let mut args = selection(&ds);
    args["kind"] = json!("objectProperty");
    args["iri"] = json!("urn:r");
    let page = call(&ds, "hierarchy", args.clone()).unwrap();
    assert_eq!(page["children"].as_array().unwrap().len(), 1);
    assert_eq!(page["children"][0]["iri"], "urn:q");
    assert_eq!(page["inferenceSource"]["kind"], "materialization");
    assert_eq!(page["inferenceSource"]["profile"], "rdfs");
    args["sourceId"] = page["inferenceSource"]["sourceId"].clone();
    args["iri"] = json!("urn:q");
    let children = call(&ds, "hierarchy", args).unwrap();
    assert_eq!(children["children"].as_array().unwrap().len(), 1);
    assert_eq!(children["children"][0]["iri"], "urn:p");
    assert_eq!(children["children"][0]["equivalenceCount"], 2);
}

#[test]
fn inferred_browse_individual_types_and_frame_keep_actual_materialization_provenance() {
    let ds = fixture();
    materialize(&ds);
    let head = ds.head_commit().seq;
    let mut args = selection(&ds);
    args["iri"] = json!("urn:i");
    let types = call(&ds, "individuals", args.clone()).unwrap();
    let names: Vec<_> = types["individuals"][0]["types"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v["iri"].as_str().unwrap())
        .collect();
    assert!(names.contains(&"urn:A") && names.contains(&"urn:B") && names.contains(&"urn:C"));
    args["sourceId"] = types["inferenceSource"]["sourceId"].clone();
    let term = call(&ds, "term", args).unwrap();
    assert_eq!(term["inferenceSource"], types["inferenceSource"]);
    assert!(term["axioms"].as_array().unwrap().iter().any(|a| {
        a["graph"] == sparkles::reasoning::INFERRED_GRAPH
            && a["ofn"].as_str().unwrap().contains("urn:C")
    }));
    assert_eq!(ds.head_commit().seq, head);
}

#[test]
fn inferred_browse_missing_stale_and_replaced_materializations_never_look_empty() {
    let ds = fixture();
    let args = selection(&ds);
    assert!(call(&ds, "hierarchy", args).is_err());
    materialize(&ds);
    let mut args = selection(&ds);
    args["kind"] = json!("dataProperty");
    let first = call(&ds, "hierarchy", args.clone()).unwrap();
    let source = first["inferenceSource"]["sourceId"].clone();
    ds.reasoning().clear().unwrap();
    args["sourceId"] = source;
    assert!(call(&ds, "hierarchy", args).is_err());
    materialize(&ds);
    ds.update("INSERT DATA { <urn:new> a <urn:A> }").unwrap();
    assert!(call(&ds, "individuals", selection(&ds)).is_err());
}

#[test]
fn inferred_browse_never_discloses_hidden_producer_inputs_through_graph_or_triple_grants() {
    use sparkles::access::triples::{Caller, Limits, Protection, Rule, TripleRules};
    use sparkles::access::{GraphAccess, GraphRule, Graphs};
    use sparkles_reasoner::inputs::{GraphRef, Inputs};
    use std::sync::Arc;
    let ds = fixture();
    ds.update("INSERT DATA { GRAPH <urn:hidden> { <urn:Secret> a <http://www.w3.org/2002/07/owl#Class> } }").unwrap();
    ds.reasoning()
        .run(&ReasonRequest {
            profile: Profile::Rdfs,
            inputs: Inputs {
                data_graphs: vec![GraphRef::Default, GraphRef::Named("urn:hidden".into())],
                ..Default::default()
            },
            ..Default::default()
        })
        .unwrap();
    let partial = call(&ds, "hierarchy", selection(&ds)).unwrap_err();
    assert!(matches!(partial, sparkles::Error::NotPermitted(_)));
    assert!(!partial.to_string().contains("urn:hidden"));
    let graphs = Graphs::Only(GraphRule::new(
        ["default", sparkles::reasoning::INFERRED_GRAPH],
        &[],
    ));
    let triple_rules = TripleRules {
        rules: vec![Rule {
            protection: Arc::new(Protection {
                name: "secret".into(),
                predicates: Some(vec!["urn:secret".into()]),
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
    };
    for access in [
        GraphAccess::graphs(graphs.clone(), graphs),
        GraphAccess::with_triples(Graphs::All, Graphs::All, triple_rules),
    ] {
        let options = QueryOptions {
            graphs: Some(Arc::new(access)),
            ..Default::default()
        };
        for op in ["hierarchy", "individuals", "term"] {
            let mut args = selection(&ds);
            if op == "term" {
                args["iri"] = json!("urn:i");
            }
            let error = wire::call(&ds, op, &args, &options).unwrap_err();
            assert!(matches!(error, sparkles::Error::NotPermitted(_)));
            let message = error.to_string();
            assert!(
                !message.contains("urn:hidden")
                    && !message.contains("Secret")
                    && !message.contains("sha256:")
            );
        }
    }
}

#[test]
fn inferred_browse_wide_property_pages_keep_cursor_membership_and_refuse_unpinned_continuations() {
    let ds = fixture();
    let mut update = String::from("INSERT DATA {");
    for i in 0..1301 {
        update.push_str(&format!("<urn:wide{i:04}> a <http://www.w3.org/2002/07/owl#ObjectProperty>; <http://www.w3.org/2000/01/rdf-schema#subPropertyOf> <urn:r> ."));
    }
    update.push('}');
    ds.update(&update).unwrap();
    materialize(&ds);
    let mut args = selection(&ds);
    args["kind"] = json!("objectProperty");
    args["iri"] = json!("urn:r");
    args["limit"] = json!(1000);
    let first = call(&ds, "hierarchy", args.clone()).unwrap();
    assert_eq!(first["total"], 1302);
    assert_eq!(first["children"].as_array().unwrap().len(), 1000);
    args["after"] = first["nextAfter"].clone();
    assert!(call(&ds, "hierarchy", args.clone()).is_err());
    args["sourceId"] = first["inferenceSource"]["sourceId"].clone();
    let last = call(&ds, "hierarchy", args.clone()).unwrap();
    assert_eq!(last["children"].as_array().unwrap().len(), 302);
    assert!(last["nextAfter"].is_null());
    let names: Vec<_> = first["children"]
        .as_array()
        .unwrap()
        .iter()
        .chain(last["children"].as_array().unwrap())
        .map(|row| row["iri"].as_str().unwrap())
        .collect();
    assert!(names.windows(2).all(|pair| pair[0] < pair[1]));
    args["sourceId"] = json!(format!("sha256:{}", "0".repeat(64)));
    assert!(matches!(
        call(&ds, "hierarchy", args),
        Err(sparkles::Error::Conflict(_))
    ));
}

fn history_fixture(options: sparkles::store::StoreOptions) -> (tempfile::TempDir, Dataset) {
    let directory = tempfile::tempdir().unwrap();
    let ds = Dataset::open_with(directory.path(), options).unwrap();
    ds.update("INSERT DATA {
        <urn:A> a <http://www.w3.org/2002/07/owl#Class>; <http://www.w3.org/2000/01/rdf-schema#subClassOf> <urn:B> .
        <urn:B> a <http://www.w3.org/2002/07/owl#Class> .
        <urn:i> a <http://www.w3.org/2002/07/owl#NamedIndividual>, <urn:A> .
    }").unwrap();
    materialize(&ds);
    (directory, ds)
}

#[test]
fn inferred_browse_history_range_allows_only_verified_unrelated_commits() {
    let (_directory, ds) = history_fixture(Default::default());
    let producer = ds.head_commit().seq;
    let exact = call(&ds, "hierarchy", selection(&ds)).unwrap();
    ds.update("INSERT DATA { GRAPH <urn:other> { <urn:other> <urn:p> 1 } }")
        .unwrap();
    let newer = call(&ds, "hierarchy", selection(&ds)).unwrap();
    assert_eq!(newer["inferenceSource"]["producerCommit"], producer);
    assert_eq!(newer["commit"], ds.head_commit().seq);
    assert_ne!(
        newer["inferenceSource"]["sourceId"],
        exact["inferenceSource"]["sourceId"]
    );
    let historical = call(
        &ds,
        "hierarchy",
        json!({"inferences":true, "at":format!("commit:{producer}"),
        "sourceId":exact["inferenceSource"]["sourceId"]}),
    )
    .unwrap();
    assert_eq!(historical["inferenceSource"], exact["inferenceSource"]);
    ds.update("INSERT DATA { <urn:A> <urn:changed> true }")
        .unwrap();
    ds.update("DELETE DATA { <urn:A> <urn:changed> true }")
        .unwrap();
    // Recorded changes are conservatively stale even when the net facts were restored.
    assert!(matches!(
        call(&ds, "hierarchy", selection(&ds)),
        Err(sparkles::Error::Conflict(_))
    ));
}

#[test]
fn inferred_browse_never_uses_newer_producer_for_history_or_accepts_manual_inference_changes() {
    let (_directory, ds) = history_fixture(Default::default());
    let producer = ds.head_commit().seq;
    ds.update("INSERT DATA { GRAPH <urn:x-sparkles:inferred> { <urn:invented> <urn:p> 1 } }")
        .unwrap();
    assert!(matches!(
        call(&ds, "hierarchy", selection(&ds)),
        Err(sparkles::Error::Conflict(_))
    ));
    assert!(
        call(
            &ds,
            "hierarchy",
            json!({"inferences":true,"at":format!("commit:{producer}")})
        )
        .is_ok()
    );
    materialize(&ds);
    assert!(matches!(
        call(
            &ds,
            "hierarchy",
            json!({"inferences":true,"at":format!("commit:{producer}")})
        ),
        Err(sparkles::Error::Conflict(_))
    ));
}

#[test]
fn inferred_browse_requires_complete_enabled_history_and_never_treats_a_gap_as_empty() {
    let (_directory, ds) = history_fixture(sparkles::store::StoreOptions {
        change_log: false,
        ..Default::default()
    });
    // An exact producer snapshot needs no later-history proof.
    assert!(call(&ds, "hierarchy", selection(&ds)).is_ok());
    ds.update("INSERT DATA { GRAPH <urn:other> { <urn:x> <urn:p> 1 } }")
        .unwrap();
    assert!(matches!(
        call(&ds, "hierarchy", selection(&ds)),
        Err(sparkles::Error::Conflict(_))
    ));
    ds.store()
        .set_change_log_settings(sparkles::store::ChangeLogSettings {
            enabled: Some(true),
            ..Default::default()
        })
        .unwrap();
    ds.update("INSERT DATA { GRAPH <urn:other> { <urn:y> <urn:p> 2 } }")
        .unwrap();
    assert!(matches!(
        call(&ds, "hierarchy", selection(&ds)),
        Err(sparkles::Error::Conflict(_))
    ));
}

#[test]
fn inferred_browse_rejects_cancelled_or_expired_primary_and_composed_read_controls() {
    use sparkles::task::Cancellation;
    use std::sync::{Arc, atomic::AtomicBool};
    let (_directory, ds) = history_fixture(Default::default());
    ds.update("INSERT DATA { GRAPH <urn:other> { <urn:x> <urn:p> 1 } }")
        .unwrap();
    let primary = Arc::new(AtomicBool::new(false));
    let secondary = Arc::new(AtomicBool::new(true));
    let options = QueryOptions {
        cancel: Some(primary.clone()),
        cancellation: Some(Cancellation::from_flags([secondary])),
        ..Default::default()
    };
    assert!(matches!(
        wire::call(&ds, "hierarchy", &selection(&ds), &options),
        Err(sparkles::Error::Cancelled)
    ));
    assert!(!primary.load(std::sync::atomic::Ordering::Relaxed));
    let options = QueryOptions {
        deadline: Some(std::time::Instant::now()),
        ..Default::default()
    };
    assert!(matches!(
        wire::call(&ds, "hierarchy", &selection(&ds), &options),
        Err(sparkles::Error::Timeout)
    ));
    let options = QueryOptions {
        cancel: Some(Arc::new(AtomicBool::new(true))),
        ..Default::default()
    };
    assert!(matches!(
        wire::call(&ds, "hierarchy", &selection(&ds), &options),
        Err(sparkles::Error::Cancelled)
    ));
}

#[test]
fn inferred_browse_fresh_empty_materialization_is_distinct_from_a_missing_source() {
    let ds = Dataset::memory();
    let mut args = selection(&ds);
    args["kind"] = json!("objectProperty");
    assert!(matches!(
        call(&ds, "hierarchy", args),
        Err(sparkles::Error::NotFound(_))
    ));
    materialize(&ds);
    let mut args = selection(&ds);
    args["kind"] = json!("objectProperty");
    let page = call(&ds, "hierarchy", args).unwrap();
    assert_eq!(page["total"], 0);
    assert_eq!(page["children"], json!([]));
    assert_eq!(page["inferenceSource"]["kind"], "materialization");
    assert!(page["inferenceSource"]["complete"].is_null());
}

#[test]
fn inferred_browse_owl_rl_profile_preserves_its_own_source_and_indirect_types() {
    let ds = fixture();
    ds.reasoning()
        .run(&ReasonRequest {
            profile: Profile::OwlRl,
            ..Default::default()
        })
        .unwrap();
    let mut args = selection(&ds);
    args["kind"] = json!("dataProperty");
    args["iri"] = json!("urn:dr");
    let page = call(&ds, "hierarchy", args).unwrap();
    assert_eq!(page["inferenceSource"]["profile"], "owl-rl");
    assert_eq!(
        page["children"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v["iri"].as_str().unwrap())
            .collect::<Vec<_>>(),
        vec!["urn:dq"]
    );
    let mut args = selection(&ds);
    args["iri"] = json!("urn:i");
    args["sourceId"] = page["inferenceSource"]["sourceId"].clone();
    let types = call(&ds, "individuals", args).unwrap();
    assert_eq!(types["inferenceSource"], page["inferenceSource"]);
    assert!(
        types["individuals"][0]["types"]
            .as_array()
            .unwrap()
            .iter()
            .any(|v| v["iri"] == "urn:C")
    );
}

#[test]
fn inferred_browse_kept_roots_report_empty_bottom_inconsistent_and_incomplete_outcomes() {
    let cases = [
        ("", "consistent", true, None),
        (
            "<urn:U> a <http://www.w3.org/2002/07/owl#Class>; <http://www.w3.org/2000/01/rdf-schema#subClassOf> <http://www.w3.org/2002/07/owl#Nothing> .",
            "consistent",
            true,
            Some(2),
        ),
        (
            "<urn:A> a <http://www.w3.org/2002/07/owl#Class> . <http://www.w3.org/2002/07/owl#Thing> <http://www.w3.org/2000/01/rdf-schema#subClassOf> <http://www.w3.org/2002/07/owl#Nothing> .",
            "inconsistent",
            true,
            Some(3),
        ),
        (
            "<urn:A> a <http://www.w3.org/2002/07/owl#Class> . <urn:Opaque> a <http://www.w3.org/2000/01/rdf-schema#Datatype> . <urn:p> a <http://www.w3.org/2002/07/owl#DatatypeProperty>; <http://www.w3.org/2000/01/rdf-schema#range> <urn:Opaque> .",
            "consistent",
            false,
            None,
        ),
    ];
    for (data, consistency, complete, bottom_count) in cases {
        let ds = Dataset::memory();
        if !data.is_empty() {
            ds.update(&format!("INSERT DATA {{ {data} }}")).unwrap();
        }
        call(&ds, "classify", json!({"threads":1})).unwrap();
        let page = call(
            &ds,
            "keptClassification",
            json!({"part":"roots", "at":format!("commit:{}",ds.head_commit().seq)}),
        )
        .unwrap();
        assert_eq!(page["status"], "available");
        assert_eq!(page["outcome"]["complete"], complete);
        assert_eq!(page["outcome"]["consistency"], consistency);
        if data.is_empty() {
            assert_eq!(page["page"]["total"], 0);
        }
        if let Some(count) = bottom_count {
            let rows = page["page"]["items"].as_array().unwrap();
            assert_eq!(rows.len(), 1);
            assert_eq!(rows[0]["iri"], "http://www.w3.org/2002/07/owl#Nothing");
            assert_eq!(rows[0]["equivalenceCount"], count);
            assert_eq!(rows[0]["children"], 0);
        }
        if !complete {
            assert!(page["outcome"]["excludedCount"].as_u64().unwrap() > 0);
        }
    }
}
