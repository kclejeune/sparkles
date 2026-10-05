#![cfg(all(feature = "shacl", feature = "shex"))]
use serde_json::json;
use sparkles::Dataset;

#[test]
fn memory_branch_guards_inherit_schema_and_keep_mutable_state_isolated() {
    for shex in [false, true] {
        let ds = Dataset::memory();
        ds.update("INSERT DATA { <urn:n> a <urn:T>; <urn:p> 1 }")
            .unwrap();
        let cfg = if shex {
            json!({"language":"shex", "mode":"reject", "schema":{"inline":"<urn:S> { a [<urn:T>] ; <urn:p> . }", "format":"shexc"}, "shapeMap":"{ FOCUS a <urn:T> }@<urn:S>"})
        } else {
            json!({"mode":"reject", "shapes":{"inline":"@prefix sh: <http://www.w3.org/ns/shacl#> . <urn:S> a sh:NodeShape; sh:targetClass <urn:T>; sh:property [ sh:path <urn:p>; sh:minCount 1 ] ."}})
        };
        if shex {
            ds.validation()
                .guard()
                .set_shex(
                    serde_json::from_value(cfg).unwrap(),
                    &sparkles_shex::NoImports,
                )
                .unwrap();
        } else {
            ds.validation()
                .guard()
                .set_shacl(serde_json::from_value(cfg).unwrap())
                .unwrap();
        }
        ds.create_branch("work", &Default::default()).unwrap();
        let work = ds.branch("work").unwrap();
        assert!(work.update("INSERT DATA { <urn:bad> a <urn:T> }").is_err());
        assert_eq!(work.len(), 2);
        ds.validation().guard().reset().unwrap();
        ds.update("INSERT DATA { <urn:bad> a <urn:T> }").unwrap();
        assert!(work.update("INSERT DATA { <urn:bad> a <urn:T> }").is_err());
        let mut editable = work.validation().guard().get().unwrap().json()["config"].clone();
        editable["mode"] = json!("warn");
        if shex {
            work.validation()
                .guard()
                .set_shex(
                    serde_json::from_value(editable).unwrap(),
                    &sparkles_shex::NoImports,
                )
                .unwrap();
        } else {
            work.validation()
                .guard()
                .set_shacl(serde_json::from_value(editable).unwrap())
                .unwrap();
        }
        work.update("INSERT DATA { <urn:bad> a <urn:T> }").unwrap();
        assert!(ds.write_guard().is_none());
        assert!(work.write_guard().is_some());
    }
}

#[test]
fn memory_branch_reasoning_schema_and_cache_are_independent() {
    let ds = Dataset::memory();
    ds.update("INSERT DATA { GRAPH <urn:schema> { <urn:A> <http://www.w3.org/2000/01/rdf-schema#subClassOf> <urn:B> } <urn:n> a <urn:A> }").unwrap();
    ds.reasoning()
        .rdfs()
        .set(sparkles::reasoning::rdfs::NewSchema::Graph(
            "urn:schema".into(),
        ))
        .unwrap();
    ds.create_branch("work", &Default::default()).unwrap();
    let work = ds.branch("work").unwrap();
    assert!(!std::sync::Arc::ptr_eq(
        &ds.reasoning().rdfs().get().unwrap(),
        &work.reasoning().rdfs().get().unwrap()
    ));
    assert!(work.ask("ASK { <urn:n> a <urn:B> }").unwrap());
    work.update("DELETE DATA { GRAPH <urn:schema> { <urn:A> <http://www.w3.org/2000/01/rdf-schema#subClassOf> <urn:B> } }").unwrap();
    assert!(!work.ask("ASK { <urn:n> a <urn:B> }").unwrap());
    assert!(ds.ask("ASK { <urn:n> a <urn:B> }").unwrap());
    work.reasoning().rdfs().reset().unwrap();
    assert!(ds.reasoning().rdfs().get().is_some());
}

#[test]
fn historical_memory_branch_guards_read_the_forks_schema_graphs() {
    for shex in [false, true] {
        let ds = Dataset::memory();
        let graph = |strict: bool| {
            if shex {
                let schema = sparkles_shex::Schema::parse_shexc(
                    if strict {
                        "<urn:S> { <urn:p> . }"
                    } else {
                        "<urn:S> { <urn:p> .? }"
                    },
                    None,
                )
                .unwrap();
                sparkles_shex::shexr::to_graph(&schema)
                    .iter()
                    .map(|t| format!("{t} ."))
                    .collect::<Vec<_>>()
                    .join("\n")
            } else {
                format!(
                    "<urn:S> a <http://www.w3.org/ns/shacl#NodeShape>; <http://www.w3.org/ns/shacl#targetClass> <urn:T>; <http://www.w3.org/ns/shacl#property> <urn:PS> . <urn:PS> <http://www.w3.org/ns/shacl#path> <urn:p> {} .",
                    if strict {
                        "; <http://www.w3.org/ns/shacl#minCount> 1"
                    } else {
                        ""
                    }
                )
            }
        };
        ds.update(&format!(
            "INSERT DATA {{ GRAPH <urn:schema> {{ {} }} <urn:n> a <urn:T>; <urn:p> 1 }}",
            graph(true)
        ))
        .unwrap();
        let cfg = if shex {
            json!({"language":"shex", "mode":"reject", "schema":{"graphs":["urn:schema"]}, "shapeMap":"{ FOCUS a <urn:T> }@<urn:S>"})
        } else {
            json!({"mode":"reject", "shapes":{"graphs":["urn:schema"]}})
        };
        if shex {
            ds.validation()
                .guard()
                .set_shex(
                    serde_json::from_value(cfg).unwrap(),
                    &sparkles_shex::NoImports,
                )
                .unwrap();
        } else {
            ds.validation()
                .guard()
                .set_shacl(serde_json::from_value(cfg).unwrap())
                .unwrap();
        }
        ds.snapshots()
            .create("strict", &sparkles::history::At::Head, &Default::default())
            .unwrap();
        ds.update(&format!(
            "CLEAR GRAPH <urn:schema>; INSERT DATA {{ GRAPH <urn:schema> {{ {} }} }}",
            graph(false)
        ))
        .unwrap();
        ds.create_branch(
            "historical",
            &sparkles::branch::BranchOptions {
                at: sparkles::history::At::Snapshot("strict".into()),
                ..Default::default()
            },
        )
        .unwrap();
        let fork = ds.branch("historical").unwrap();
        assert!(
            fork.update("INSERT DATA { <urn:bad> a <urn:T> }").is_err(),
            "historical {shex:?} schema must reject missing p"
        );
        assert!(!fork.ask("ASK { <urn:bad> a <urn:T> }").unwrap());
        ds.update("INSERT DATA { <urn:bad> a <urn:T> }").unwrap();
    }
}

#[test]
fn failed_historical_memory_schema_load_keeps_the_raw_branch_fail_closed() {
    let ds = Dataset::memory();
    ds.snapshots()
        .create("empty", &sparkles::history::At::Head, &Default::default())
        .unwrap();
    let schema = sparkles_shex::Schema::parse_shexc("<urn:S> { <urn:p> . }", None).unwrap();
    let triples = sparkles_shex::shexr::to_graph(&schema)
        .iter()
        .map(|t| format!("{t} ."))
        .collect::<Vec<_>>()
        .join("\n");
    ds.update(&format!(
        "INSERT DATA {{ GRAPH <urn:schema> {{ {triples} }} <urn:n> a <urn:T>; <urn:p> 1 }}"
    ))
    .unwrap();
    ds.validation()
        .guard()
        .set_shex(
            serde_json::from_value(json!({
                "language":"shex", "mode":"reject", "schema":{"graphs":["urn:schema"]},
                "shapeMap":"{ FOCUS a <urn:T> }@<urn:S>"
            }))
            .unwrap(),
            &sparkles_shex::NoImports,
        )
        .unwrap();
    assert!(
        ds.create_branch(
            "invalid",
            &sparkles::branch::BranchOptions {
                at: sparkles::history::At::Snapshot("empty".into()),
                ..Default::default()
            }
        )
        .is_err()
    );
    assert!(ds.branch("invalid").is_err());
    let raw = ds.store().branch("invalid").unwrap();
    assert!(matches!(
        sparkles::sparql::update::update(
            &raw,
            "INSERT DATA { <urn:bad> a <urn:T> }",
            &Default::default()
        ),
        Err(sparkles::Error::GuardMissing(_))
    ));
    assert_eq!(raw.snapshot().len(), 0);
}
