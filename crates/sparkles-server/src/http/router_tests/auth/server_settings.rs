//! Server-wide model settings and runtime secrets with access control (spec C19 A11,
//! A12): a server administrator adds a provider and its key and the next ask uses them,
//! a dataset administrator cannot, and no answer carries the key.

use super::*;
use crate::models::mock::{self, MockModel};

const KEY: &str = "sk-test-a11-0123456789";

fn json_call(auth: &str) -> [(&str, &str); 2] {
    [
        ("authorization", auth),
        ("content-type", "application/json"),
    ]
}

/// A mock that drafts one query for every request.
fn drafter() -> MockModel {
    MockModel::start(|_, _| {
        let draft = serde_json::json!({
            "query": "SELECT ?s WHERE { ?s ?p ?o }",
            "explanation": "It lists the subjects.",
            "assumptions": [],
            "clarify": { "question": "", "choices": [] },
            "graph": { "subject": "", "predicate": "", "object": "" }
        });
        (200, mock::openai(&draft.to_string()))
    })
}

#[tokio::test(flavor = "multi_thread")]
async fn a11_a12_server_admins_change_models_and_keys() {
    let s = auth_server();
    let m = drafter();
    let alice = b("alice");
    let carol = b("carol");
    let patch = serde_json::json!({
        "providers": { "gw": {
            "kind": "openai", "endpoint": format!("{}/v1", m.url()),
            "apiKey": { "secret": "gw-key" }, "allowedModels": ["m"]
        }},
        "roles": { "draft": [{ "provider": "gw", "model": "m" }] }
    })
    .to_string();
    let secret = format!(r#"{{"value": "{KEY}"}}"#);
    // a dataset admin gets 403 on both routes, and reads nothing
    for (method, uri, body) in [
        ("PATCH", "/$/server/settings/models", patch.as_str()),
        ("GET", "/$/server/settings/models", ""),
        ("PUT", "/$/server/secrets/gw-key", secret.as_str()),
        ("DELETE", "/$/server/secrets/gw-key", ""),
        ("GET", "/$/server/secrets", ""),
    ] {
        let r = call(&s.app, method, uri, &json_call(&carol), body).await;
        assert_eq!(
            r.status,
            StatusCode::FORBIDDEN,
            "{method} {uri}: {}",
            r.text()
        );
    }
    assert!(s.state.models().is_none());
    // the server admin adds the provider, its role list and its key
    let r = call(
        &s.app,
        "PATCH",
        "/$/server/settings/models",
        &json_call(&alice),
        &patch,
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let r = call(
        &s.app,
        "PUT",
        "/$/server/secrets/gw-key",
        &json_call(&alice),
        &secret,
    )
    .await;
    assert_eq!(r.status, StatusCode::NO_CONTENT, "{}", r.text());
    // carol turns on the assistant of her dataset; the next ask uses the provider
    let r = call(
        &s.app,
        "PUT",
        "/$/assistant/wiki",
        &json_call(&carol),
        r#"{"enabled": true}"#,
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    #[cfg(feature = "mcp")]
    {
        let r = call(
            &s.app,
            "POST",
            "/wiki/ask",
            &[
                ("authorization", &carol),
                ("content-type", "application/json"),
                ("accept", "application/json"),
            ],
            r#"{"question": "What is there?"}"#,
        )
        .await;
        assert_eq!(r.status, StatusCode::OK, "{}", r.text());
        let reqs = m.requests();
        assert!(!reqs.is_empty());
        assert_eq!(
            reqs[0].header("authorization"),
            Some(format!("Bearer {KEY}").as_str())
        );
        assert!(!r.text().contains(KEY));
    }
    // A12: no answer carries the key
    for uri in [
        "/$/server/secrets",
        "/$/server/settings/models",
        "/$/models",
        "/$/settings",
    ] {
        let r = get_as(&s.app, uri, Some(&alice)).await;
        assert_eq!(r.status, StatusCode::OK, "{uri}: {}", r.text());
        assert!(!r.text().contains(KEY), "{uri}: {}", r.text());
    }
    let list = get_as(&s.app, "/$/server/secrets", Some(&alice))
        .await
        .json();
    assert_eq!(list["secrets"][0]["name"], "gw-key");
    assert_eq!(list["secrets"][0]["source"], "runtime");
    assert_eq!(list["secrets"][0]["providers"], serde_json::json!(["gw"]));
    // a dataset admin still may not name endpoints in the dataset's settings
    let r = call(
        &s.app,
        "PATCH",
        "/$/settings/wiki/assistant",
        &json_call(&carol),
        r#"{"roles": {"draft": [{"provider": "gw", "model": "m", "endpoint": "http://x"}]}}"#,
    )
    .await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST, "{}", r.text());
}
