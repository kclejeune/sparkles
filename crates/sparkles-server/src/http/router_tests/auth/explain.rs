//! `POST /$/reason/{ds}/explain` under restricted views (U10 §3.3, A9): a caller who
//! cannot read the ontology graph gets the answer of a triple that is not entailed.

use super::*;
use serde_json::json;

const DATA: &str = r#"@prefix ex: <http://ex.org/> .
@prefix rdfs: <http://www.w3.org/2000/01/rdf-schema#> .
<http://ex.org/hr> { ex:alice ex:manages ex:team7 . }
<http://ex.org/ontology> { ex:Manager rdfs:subClassOf ex:Employee . ex:manages rdfs:domain ex:Manager . }
"#;

fn users() -> String {
    let h = |pw: &str| hash_password_with(pw, 8, 1, 1).unwrap();
    format!(
        r#"
[[users]]
name = "efull"
password = "{efull}"
datasets = {{ ex = "read" }}

[[users]]
name = "ehr"
password = "{ehr}"
[[users.grants]]
dataset = "ex"
level = "read"
graphs = ["http://ex.org/hr", "urn:x-sparkles:inferred"]

[[users]]
name = "eboth"
password = "{eboth}"
[[users.grants]]
dataset = "ex"
level = "read"
graphs = ["http://ex.org/hr", "http://ex.org/ontology", "urn:x-sparkles:inferred"]
"#,
        efull = h("efull-pw"),
        ehr = h("ehr-pw"),
        eboth = h("eboth-pw"),
    )
}

fn server() -> (tempfile::TempDir, Arc<AppState>, Router) {
    let dir = tempfile::tempdir().unwrap();
    let f = Fixture {
        extra: users(),
        ..Default::default()
    };
    let config = dir.path().join("auth.toml");
    std::fs::write(&config, config_text(&f)).unwrap();
    let mut st =
        AppState::new(dir.path(), StoreOptions::default(), Duration::from_secs(30)).unwrap();
    st.auth = Some(Arc::new(Auth::open(&config, dir.path()).unwrap().0));
    let st = Arc::new(st);
    let ds = st.attach("ex", DbType::Mem, None).unwrap();
    ds.store
        .load(&[Source::from_bytes(
            DATA.as_bytes().to_vec(),
            oxrdfio::RdfFormat::TriG,
            None,
        )])
        .unwrap();
    let req = sparkles::reasoning::ReasonRequest {
        inputs: sparkles_reasoner::Inputs {
            data_graphs: vec![sparkles_reasoner::GraphRef::Named(
                "http://ex.org/hr".into(),
            )],
            ontology_graphs: vec![sparkles_reasoner::GraphRef::Named(
                "http://ex.org/ontology".into(),
            )],
            ..Default::default()
        },
        ..Default::default()
    };
    ds.dataset.reasoning().run(&req).unwrap();
    st.set_phase(crate::obs::Phase::Ready);
    let app = router(st.clone());
    (dir, st, app)
}

async fn explain(app: &Router, user: &str, s: &str, o: &str) -> R {
    let body = json!({
        "triple": {
            "s": {"type": "uri", "value": format!("http://ex.org/{s}")},
            "p": {"type": "uri", "value": "http://www.w3.org/1999/02/22-rdf-syntax-ns#type"},
            "o": {"type": "uri", "value": format!("http://ex.org/{o}")},
        },
        "maxTrees": 5,
    });
    call(
        app,
        "POST",
        "/$/reason/ex/explain",
        &[
            ("authorization", &b(user)),
            ("content-type", "application/json"),
        ],
        &body.to_string(),
    )
    .await
}

#[tokio::test]
async fn a_hidden_ontology_answers_as_not_entailed() {
    let (_dir, _st, app) = server();
    let full = explain(&app, "efull", "alice", "Employee").await;
    assert_eq!(full.status, StatusCode::OK, "{}", full.text());
    assert_eq!(full.json()["trees"][0]["rule"]["name"], "rdfs9");
    let both = explain(&app, "eboth", "alice", "Employee").await;
    assert_eq!(both.status, StatusCode::OK, "{}", both.text());

    let hidden = explain(&app, "ehr", "alice", "Employee").await;
    let absent = explain(&app, "efull", "bob", "Employee").await;
    for r in [&hidden, &absent] {
        assert_eq!(r.status, StatusCode::NOT_FOUND, "{}", r.text());
        assert_eq!(r.json()["code"], "no-visible-derivation");
    }
    let strip = |r: &R| {
        let mut j = r.json();
        j.as_object_mut().unwrap().remove("requestId");
        j
    };
    assert_eq!(strip(&hidden), strip(&absent));
}
