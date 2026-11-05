//! `ontology_entities`, `ontology_classify`, `ontology_dl_query`, `ontology_parse` and
//! the inferred listings of `ontology_axioms` (roadmap items 565, 578, 586 and 661).

use super::*;

const ONTO: &str = "@prefix ex: <http://ex.org/> .
@prefix owl: <http://www.w3.org/2002/07/owl#> .
@prefix rdfs: <http://www.w3.org/2000/01/rdf-schema#> .
ex:onto a owl:Ontology .
ex:Professor a owl:Class ; rdfs:label \"Professor\"@en .
ex:Person a owl:Class . ex:Agent a owl:Class . ex:Course a owl:Class .
ex:teaches a owl:ObjectProperty .
ex:Professor rdfs:subClassOf ex:Person .
ex:Person rdfs:subClassOf ex:Agent .
";

fn server(cfg: McpConfig, read_only: bool) -> McpServer {
    let mut st = AppState::standalone(StoreOptions::default(), cfg.max_timeout);
    st.read_only = read_only;
    let st = Arc::new(st);
    let ds = st.attach("t", DbType::Mem, None).unwrap();
    load(&ds, ONTO);
    ds.store.set_prefix("ex", "http://ex.org/").unwrap();
    McpServer::new(st, cfg)
}

fn updates() -> McpConfig {
    McpConfig {
        allow_update: true,
        ..McpConfig::default()
    }
}

fn materialize(s: &McpServer) {
    s.state
        .get("t")
        .unwrap()
        .dataset
        .reasoning()
        .run(&sparkles::reasoning::ReasonRequest {
            profile: sparkles_reasoner::Profile::Rdfs,
            ..Default::default()
        })
        .unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn entities_are_listed_and_reported() {
    let mut c = Client::start(server(McpConfig::default(), true));
    let out = c
        .structured("ontology_entities", json!({"kind": "Class"}))
        .await;
    assert_eq!(out["dataset"], "t", "{out}");
    assert_eq!(out["total"], 4, "{out}");
    let page = c
        .structured("ontology_entities", json!({"limit": 2, "offset": 1}))
        .await;
    assert_eq!(page["entities"].as_array().unwrap().len(), 2, "{page}");
    assert_eq!(page["next"], 3, "{page}");
    let out = c
        .structured(
            "ontology_entities",
            json!({"iri": "http://ex.org/Professor"}),
        )
        .await;
    assert_eq!(out["kinds"], json!(["Class"]), "{out}");
    assert_eq!(out["labels"][0]["@value"], "Professor", "{out}");
    let supers: Vec<&str> = out["class"]["superClasses"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c["ofn"].as_str().unwrap())
        .collect();
    assert_eq!(supers.len(), 1, "{out}");
    assert!(supers[0].contains("Person"), "{supers:?}");
    // a name the ontology does not know, and a listing member on a report
    let (_, e) = c
        .error("ontology_entities", json!({"iri": "http://ex.org/Nobody"}))
        .await;
    assert_eq!(e["code"], "not-found", "{e}");
    let (_, e) = c
        .error(
            "ontology_entities",
            json!({"iri": "http://ex.org/Professor", "offset": 1}),
        )
        .await;
    assert_eq!(e["code"], "bad-argument", "{e}");
    let (_, e) = c.error("ontology_entities", json!({"kind": "Thing"})).await;
    assert_eq!(e["code"], "bad-argument", "{e}");
}

/// spec F21 §7 over MCP: the inferred view and its freshness.
#[tokio::test(flavor = "multi_thread")]
async fn inferences_are_read_with_their_freshness() {
    let s = server(updates(), false);
    let mut c = Client::start(s.clone());
    let inferred =
        json!({"type": "SubClassOf", "about": "http://ex.org/Professor", "inferences": true});
    // nothing materialized: the asserted axioms, and the answer says so
    let out = c.structured("ontology_axioms", inferred.clone()).await;
    assert_eq!(out["inferences"], json!({"included": false}), "{out}");
    assert_eq!(out["total"], 1, "{out}");
    let plain = c
        .structured("ontology_axioms", json!({"type": "SubClassOf"}))
        .await;
    assert!(plain.get("inferences").is_none(), "{plain}");
    materialize(&s);
    let out = c.structured("ontology_axioms", inferred.clone()).await;
    assert_eq!(out["inferences"]["included"], true, "{out}");
    assert_eq!(out["inferences"]["stale"], false, "{out}");
    assert_eq!(out["inferences"]["profile"], "rdfs", "{out}");
    let ofn: Vec<&str> = out["axioms"]
        .as_array()
        .unwrap()
        .iter()
        .map(|a| a["ofn"].as_str().unwrap())
        .collect();
    assert!(
        ofn.iter().any(|a| a.contains("Agent")),
        "the closure is read: {ofn:?}"
    );
    // a later write makes the view stale
    c.structured(
        "ontology_axioms",
        json!({"add": ["Declaration(Class(ex:Room))"], "provenance": false}),
    )
    .await;
    let out = c.structured("ontology_axioms", inferred).await;
    assert_eq!(out["inferences"]["stale"], true, "{out}");
    assert_eq!(out["inferences"]["commitsSince"], 1, "{out}");
    // the entity report reads them too
    let out = c
        .structured(
            "ontology_entities",
            json!({"iri": "http://ex.org/Professor", "inferences": true}),
        )
        .await;
    assert_eq!(out["inferences"]["included"], true, "{out}");
    let supers = out["class"]["superClasses"].to_string();
    assert!(
        supers.contains("Agent"),
        "the inferred superclass: {supers}"
    );
    // a classification reads the asserted axioms only
    let (_, e) = c
        .error("ontology_classify", json!({"inferences": true}))
        .await;
    assert_eq!(e["code"], "bad-argument", "{e}");
    // an edit changes the asserted axioms only
    let (_, e) = c
        .error(
            "ontology_axioms",
            json!({"add": ["Declaration(Class(ex:Hall))"], "inferences": true}),
        )
        .await;
    assert_eq!(e["code"], "bad-argument", "{e}");
}

#[tokio::test(flavor = "multi_thread")]
async fn classification_reads_and_writes_the_taxonomy() {
    // a read-only server classifies and refuses the write
    let mut c = Client::start(server(McpConfig::default(), true));
    let out = c
        .structured(
            "ontology_classify",
            json!({"about": "http://ex.org/Professor"}),
        )
        .await;
    assert_eq!(out["consistent"], true, "{out}");
    assert_eq!(out["unsatisfiableCount"], 0, "{out}");
    let supers = out["class"]["superClasses"].to_string();
    assert!(supers.contains("http://ex.org/Agent"), "{out}");
    let (_, e) = c
        .error(
            "ontology_classify",
            json!({"target": "http://ex.org/taxonomy"}),
        )
        .await;
    assert_eq!(e["code"], "forbidden", "{e}");
    let (_, e) = c.error("ontology_classify", json!({"dryRun": true})).await;
    assert_eq!(e["code"], "bad-argument", "{e}");
    // with updates: a preview writes nothing, and the write is one commit
    let s = server(updates(), false);
    let mut c = Client::start(s.clone());
    let before = head(&s, "t");
    let target = json!({"target": "http://ex.org/taxonomy", "message": "taxonomy"});
    let mut preview = target.clone();
    preview["dryRun"] = json!(true);
    let out = c.structured("ontology_classify", preview).await;
    assert_eq!(out["dryRun"], true, "{out}");
    assert_eq!(out["wouldCommit"], true, "{out}");
    assert_eq!(head(&s, "t"), before);
    let mut guarded = target.clone();
    guarded["ifHead"] = json!(before + 5);
    let (_, e) = c.error("ontology_classify", guarded).await;
    assert_eq!(e["code"], "precondition-failed", "{e}");
    let out = c.structured("ontology_classify", target.clone()).await;
    assert!(out["written"].is_object(), "{out}");
    assert_eq!(head(&s, "t"), before + 1);
    let mut partial = target;
    partial["partial"] = json!(true);
    let (_, e) = c.error("ontology_classify", partial).await;
    assert_eq!(e["code"], "bad-argument", "{e}");
}

#[tokio::test(flavor = "multi_thread")]
async fn dl_queries_and_manchester_text() {
    let mut c = Client::start(server(McpConfig::default(), true));
    let out = c
        .structured(
            "ontology_dl_query",
            json!({"expression": "<http://ex.org/Professor>", "answers": "superclasses"}),
        )
        .await;
    assert_eq!(out["dataset"], "t", "{out}");
    let supers = out["superclasses"].to_string();
    assert!(supers.contains("http://ex.org/Person"), "{out}");
    assert!(supers.contains("http://ex.org/Agent"), "{out}");
    assert!(out.get("subclasses").is_none(), "{out}");
    let out = c
        .structured(
            "ontology_dl_query",
            json!({"expression": "ObjectSomeValuesFrom(<http://ex.org/teaches> <http://ex.org/Course>)", "syntax": "functional"}),
        )
        .await;
    assert_eq!(out["satisfiable"], true, "{out}");
    let (_, e) = c.error("ontology_dl_query", json!({})).await;
    assert_eq!(e["code"], "bad-argument", "{e}");
    let (_, e) = c
        .error(
            "ontology_dl_query",
            json!({"expression": "<http://ex.org/Professor>", "inferences": true}),
        )
        .await;
    assert_eq!(e["code"], "bad-argument", "{e}");
    let out = c
        .structured(
            "ontology_parse",
            json!({"text": "<http://ex.org/Person> and (<http://ex.org/teaches> some <http://ex.org/Course>)"}),
        )
        .await;
    assert_eq!(out["as"], "classExpression", "{out}");
    let ofn = out["ofn"].as_str().unwrap();
    assert!(ofn.starts_with("ObjectIntersectionOf("), "{ofn}");
    let out = c
        .structured(
            "ontology_parse",
            json!({"text": "<http://ex.org/Professor> SubClassOf <http://ex.org/Person>", "as": "axiom"}),
        )
        .await;
    assert!(
        out["ofn"].as_str().unwrap().starts_with("SubClassOf("),
        "{out}"
    );
    let (_, e) = c.error("ontology_parse", json!({"text": "and and"})).await;
    assert_eq!(e["code"], "invalid-manchester", "{e}");
}

#[test]
fn the_taxonomy_write_is_offered_with_updates_only() {
    let defs = |cfg| super::super::ontology_reads::tool_defs(&cfg);
    let read = defs(McpConfig::default());
    let classify = read.iter().find(|d| d.name == "ontology_classify").unwrap();
    assert!(classify.read_only);
    assert!(classify.input["properties"].get("target").is_none());
    assert!(
        read.iter().all(|d| d.read_only),
        "only reads without updates"
    );
    let write = defs(updates());
    let classify = write
        .iter()
        .find(|d| d.name == "ontology_classify")
        .unwrap();
    assert!(!classify.read_only);
    for k in ["target", "message", "dryRun", "changes", "ifHead"] {
        assert!(classify.input["properties"].get(k).is_some(), "{k}");
    }
    use super::super::branches::call_writes;
    assert!(call_writes(
        "ontology_classify",
        Some(&json!({"target": "http://ex.org/t"}))
    ));
    assert!(!call_writes("ontology_classify", Some(&json!({}))));
    assert!(!call_writes("ontology_entities", None));
}

#[tokio::test(flavor = "multi_thread")]
async fn answers_keep_to_the_byte_budget() {
    let mut data = String::from("@prefix owl: <http://www.w3.org/2002/07/owl#> .\n");
    for i in 0..200 {
        data.push_str(&format!(
            "<http://ex.org/a-rather-long-class-name-{i}> a owl:Class .\n"
        ));
    }
    let cfg = McpConfig {
        max_bytes: 2048,
        ..McpConfig::default()
    };
    let mut c = Client::start(server_with(&[("t", &[data.as_str()])], cfg));
    let (_, e) = c.error("ontology_entities", json!({"limit": 200})).await;
    assert_eq!(e["code"], "budget-result-bytes", "{e}");
    let out = c.structured("ontology_entities", json!({"limit": 5})).await;
    assert_eq!(out["total"], 200, "{out}");
}
