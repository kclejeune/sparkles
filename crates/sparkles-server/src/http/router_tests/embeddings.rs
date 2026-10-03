//! Vector indexes that compute their vectors (F08), against a mock embeddings endpoint:
//! the configuration rules of the API, the worker the server starts, text searches and
//! `reembed`.

use super::*;
use sparkles::vector::embed::mock::MockProvider;
use sparkles::vector::embed::{EmbeddingConfig, Environment, SecretSource};

const DIM: usize = 8;

fn put(name: &str, body: &J) -> Request<Body> {
    Request::put(format!("/$/vector/ds/{name}"))
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(body.to_string()))
        .unwrap()
}

/// Poll the index's status until its embedding is idle and caught up.
async fn caught_up(app: &Router, name: &str) -> J {
    let mut last = J::Null;
    for _ in 0..500 {
        last = get_json(app, &format!("/$/vector/ds/{name}")).await;
        let e = &last["embedding"];
        if e["state"] == "idle" && e["appliedSeq"] == e["headSeq"] {
            return last;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    panic!("the embedding did not catch up: {last}");
}

async fn sparql(app: &Router, q: &str) -> Resp {
    get_uri(
        app,
        &format!(
            "/ds/sparql?query={}",
            percent_encoding::utf8_percent_encode(q, percent_encoding::NON_ALPHANUMERIC)
        ),
    )
    .await
}

#[tokio::test]
async fn embedding_indexes_through_the_api() {
    let mock = MockProvider::start(DIM);
    let dir = tempfile::tempdir().unwrap();
    let key = dir.path().join("key");
    std::fs::write(&key, "sk-router\n").unwrap();
    sparkles::vector::embed::set_environment(Environment {
        outbound: sparkles::outbound::OutboundPolicy {
            allow_private: true,
            ..Default::default()
        },
        secrets: [("mock".to_string(), SecretSource::File(key))].into(),
        ..Default::default()
    });
    let s = server();
    let mut body = serde_json::json!({
        "predicate": "http://example.org/emb",
        "dimension": DIM,
        "embedding": {
            "url": mock.url(),
            "model": "mock-model",
            "predicates": ["http://xmlns.com/foaf/0.1/name"],
            "apiKey": {"env": "HOME"}
        }
    });
    // keys from the server's environment or files are for the command line
    let r = send(&s.app, put("names", &body)).await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST, "{}", r.text());
    assert!(r.text().contains("apiKey"), "{}", r.text());
    body["embedding"]["apiKey"] = serde_json::json!({"secret": "nope"});
    let r = send(&s.app, put("names", &body)).await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST, "{}", r.text());
    assert!(r.text().contains("no secret named"), "{}", r.text());
    body["embedding"]["apiKey"] = serde_json::json!({"secret": "mock"});
    let r = send(&s.app, put("names", &body)).await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());

    // the worker the server started embeds the names already there
    let st = caught_up(&s.app, "names").await;
    assert_eq!(st["embedding"]["embedded"], 2, "{st}");
    assert_eq!(st["embedding"]["model"], "mock-model");
    assert_eq!(
        mock.state().auth.last().unwrap().as_deref(),
        Some("Bearer sk-router")
    );
    // the configuration keeps the secret's name, never the key
    assert!(!st.to_string().contains("sk-router"));

    // a write is embedded after it commits
    let r = send(
        &s.app,
        Request::post("/ds/update")
            .header(header::CONTENT_TYPE, "application/sparql-update")
            .body(Body::from(
                "INSERT DATA { <http://example.org/carol> <http://xmlns.com/foaf/0.1/name> \"Carol\" }",
            ))
            .unwrap(),
    )
    .await;
    assert!(r.status.is_success(), "{}", r.text());
    let st = caught_up(&s.app, "names").await;
    assert_eq!(st["embedding"]["embedded"], 3, "{st}");

    // searching with text
    let r = sparql(
        &s.app,
        "SELECT ?s WHERE { (?s ?score) <urn:x-sparkles:vectorSearch> (<http://example.org/emb> \"Carol\" 1) }",
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    assert!(
        r.text().contains("http://example.org/carol"),
        "{}",
        r.text()
    );
    let r = sparql(
        &s.app,
        "SELECT ?s WHERE { (?s ?score) <urn:x-sparkles:vectorSearch> (<http://example.org/none> \"Carol\" 1) }",
    )
    .await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST, "{}", r.text());

    // re-embedding sends every name again
    let before = mock.state().inputs.len();
    let r = send(
        &s.app,
        Request::post("/$/vector/ds/names/reembed")
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(r.status, StatusCode::ACCEPTED, "{}", r.text());
    caught_up(&s.app, "names").await;
    assert_eq!(mock.state().inputs.len(), before + 3);
    let r = send(
        &s.app,
        Request::post("/$/vector/ds/nope/reembed")
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(r.status, StatusCode::NOT_FOUND);
}

#[test]
fn the_api_checks_endpoints_against_the_outbound_policy() {
    let mut e = EmbeddingConfig::new("http://127.0.0.1:9/v1/embeddings", "m")
        .from_predicates(&["http://x/p"]);
    let strict = Environment::default();
    let m = crate::vector::check_embedding(&strict, &e).unwrap_err();
    assert!(m.starts_with("url:") && m.contains("loopback"), "{m}");
    e.url = "https://api.example.org/v1/embeddings".into();
    assert!(crate::vector::check_embedding(&strict, &e).is_ok());
    e.api_key = Some(sparkles::vector::embed::ApiKey::File("/etc/passwd".into()));
    assert!(crate::vector::check_embedding(&strict, &e).is_err());
}
