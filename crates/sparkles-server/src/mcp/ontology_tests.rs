//! `ontology_axioms` (spec F21 Phase 3): listings, edits with provenance, previews and
//! the write checks.

use super::*;

const ONTO: &str = "@prefix ex: <http://ex.org/> .
@prefix owl: <http://www.w3.org/2002/07/owl#> .
@prefix rdfs: <http://www.w3.org/2000/01/rdf-schema#> .
ex:onto a owl:Ontology .
ex:Professor a owl:Class . ex:Person a owl:Class . ex:Course a owl:Class .
ex:teaches a owl:ObjectProperty .
ex:Professor rdfs:subClassOf ex:Person .
";

fn writable(cfg: McpConfig) -> McpServer {
    let mut st = AppState::standalone(StoreOptions::default(), cfg.max_timeout);
    st.read_only = false;
    let st = Arc::new(st);
    let ds = st.attach("t", DbType::Mem, None).unwrap();
    load(&ds, ONTO);
    ds.store.set_prefix("ex", "http://ex.org/").unwrap();
    McpServer::new(st, cfg)
}

#[tokio::test(flavor = "multi_thread")]
async fn lists_axioms() {
    let mut c = Client::start(server_with(&[("t", &[ONTO])], McpConfig::default()));
    let out = c
        .structured("ontology_axioms", json!({"type": "SubClassOf"}))
        .await;
    assert_eq!(out["total"], 1, "{out}");
    let ofn = out["axioms"][0]["ofn"].as_str().unwrap();
    assert!(ofn.starts_with("SubClassOf("), "{ofn}");
    assert!(ofn.contains("Professor"), "{ofn}");
    let out = c
        .structured("ontology_axioms", json!({"about": "http://ex.org/teaches"}))
        .await;
    assert_eq!(out["total"], 1, "{out}");
    // a read-only server lists but does not edit
    let (_, e) = c
        .error(
            "ontology_axioms",
            json!({"add": ["SubClassOf(<http://ex.org/Course> owl:Thing)"]}),
        )
        .await;
    assert_eq!(e["code"], "forbidden", "{e}");
}

#[tokio::test(flavor = "multi_thread")]
async fn edits_with_preview_and_provenance() {
    let cfg = McpConfig {
        allow_update: true,
        ..McpConfig::default()
    };
    let server = writable(cfg);
    let mut c = Client::start(server.clone());
    let before = head(&server, "t");
    let axiom = "SubClassOf(ex:Professor ObjectSomeValuesFrom(ex:teaches ex:Course))";
    // a preview writes nothing
    let out = c
        .structured(
            "ontology_axioms",
            json!({"add": [axiom], "dryRun": true, "changes": 5, "message": "teaching"}),
        )
        .await;
    assert_eq!(out["dryRun"], true, "{out}");
    assert_eq!(out["wouldCommit"], true, "{out}");
    assert!(out["activity"].as_str().unwrap().starts_with("urn:uuid:"));
    assert_eq!(head(&server, "t"), before);
    // ifHead guards the edit
    let (_, e) = c
        .error(
            "ontology_axioms",
            json!({"add": [axiom], "ifHead": before + 5}),
        )
        .await;
    assert_eq!(e["code"], "precondition-failed", "{e}");
    let out = c
        .structured(
            "ontology_axioms",
            json!({
                "add": [axiom],
                "remove": ["SubClassOf(ex:Professor ex:Person)"],
                "message": "teaching",
                "source": {"iri": "http://ex.org/notes/1", "title": "Notes"},
                "agent": {"name": "helper", "model": "m1"},
                "ifHead": before,
            }),
        )
        .await;
    assert_eq!(out["committed"], true, "{out}");
    assert_eq!(out["removed"], 1, "{out}");
    let act = out["activity"].as_str().unwrap().to_string();
    assert_eq!(head(&server, "t"), before + 1);
    // the added axiom carries its provenance as annotations
    let listed = c
        .structured("ontology_axioms", json!({"type": "SubClassOf"}))
        .await;
    assert_eq!(listed["total"], 1, "{listed}");
    let ofn = listed["axioms"][0]["ofn"].as_str().unwrap().to_string();
    assert!(
        ofn.contains("prov:wasGeneratedBy") || ofn.contains("wasGeneratedBy"),
        "{ofn}"
    );
    assert!(ofn.contains(&act), "{ofn}");
    assert!(
        ofn.contains("http://ex.org/notes/1") || ofn.contains("ex:notes/1"),
        "{ofn}"
    );
    // the activity is described by annotation assertions
    let about = c
        .structured(
            "ontology_axioms",
            json!({"about": act, "type": "AnnotationAssertion"}),
        )
        .await;
    let lines: Vec<&str> = about["axioms"]
        .as_array()
        .unwrap()
        .iter()
        .map(|a| a["ofn"].as_str().unwrap())
        .collect();
    assert!(
        lines
            .iter()
            .any(|l| l.contains("wasAssociatedWith") && l.contains("urn:x-sparkles:principal:")),
        "{lines:?}"
    );
    assert!(
        lines
            .iter()
            .any(|l| l.contains("rdfs:label") && l.contains("teaching")),
        "{lines:?}"
    );
    // the ontology stays in OWL 2 DL: the PROV-O properties are declared
    let st = server.state.get("t").unwrap();
    let o = st
        .dataset
        .ontology()
        .open(&Default::default(), &Default::default())
        .unwrap();
    let dl =
        &sparkles::handles::ontology::check_opened(&o, &Default::default(), &Default::default())
            .unwrap()[0];
    assert!(
        dl.conforms,
        "{:?}",
        dl.violations.iter().map(|v| &v.message).collect::<Vec<_>>()
    );
    // the annotated axiom is removed as it is listed
    let out = c
        .structured(
            "ontology_axioms",
            json!({"remove": [ofn], "provenance": false}),
        )
        .await;
    assert_eq!(out["removed"], 1, "{out}");
    assert!(out.get("activity").is_none());
    // an invalid axiom names its place
    let (t, e) = c
        .error("ontology_axioms", json!({"add": ["SubClassOf(ex:A)"]}))
        .await;
    assert_eq!(e["code"], "invalid-axiom", "{e}");
    assert!(t.contains("add[0]"), "{t}");
}

#[tokio::test(flavor = "multi_thread")]
async fn provenance_refuses_prov_o_typing() {
    let cfg = McpConfig {
        allow_update: true,
        ..McpConfig::default()
    };
    let server = writable(cfg);
    load(
        &server.state.get("t").unwrap(),
        "<http://www.w3.org/ns/prov#wasGeneratedBy> a <http://www.w3.org/2002/07/owl#ObjectProperty> .",
    );
    let mut c = Client::start(server.clone());
    let (_, e) = c
        .error(
            "ontology_axioms",
            json!({"add": ["SubClassOf(ex:Course owl:Thing)"]}),
        )
        .await;
    assert_eq!(e["code"], "provenance-conflict", "{e}");
    let out = c
        .structured(
            "ontology_axioms",
            json!({"add": ["SubClassOf(ex:Course owl:Thing)"], "provenance": false}),
        )
        .await;
    assert_eq!(out["added"], 1, "{out}");
}

#[test]
fn edits_are_offered_with_updates_only() {
    let list = super::super::ontology::tool_def(&McpConfig::default());
    assert!(list.read_only);
    assert!(list.input["properties"].get("add").is_none());
    assert!(list.input["properties"].get("about").is_some());
    let edit = super::super::ontology::tool_def(&McpConfig {
        allow_update: true,
        ..McpConfig::default()
    });
    assert!(!edit.read_only && edit.destructive);
    for k in [
        "add",
        "remove",
        "dryRun",
        "ifHead",
        "provenance",
        "agent",
        "source",
    ] {
        assert!(edit.input["properties"].get(k).is_some(), "{k}");
    }
}
