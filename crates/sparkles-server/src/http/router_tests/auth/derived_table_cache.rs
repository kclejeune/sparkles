//! Process-wide cache activity is visible only to metrics principals.

use super::*;

#[tokio::test]
async fn derived_table_cache_stats_require_metrics_even_after_hidden_activity() {
    let s = auth_server();
    let reader_stats = |uri: String| {
        let app = s.app.clone();
        async move { get_as(&app, &uri, Some(&b("bob"))).await }
    };
    for uri in ["/$/stats", "/$/stats/wiki"] {
        let r = reader_stats(uri.into()).await;
        assert_eq!(r.status, StatusCode::OK, "{}", r.text());
        assert!(r.json().get("derivedTableCache").is_none());
    }
    s.state.get("secret").unwrap().store.load(&[Source::from_bytes(
        b"<urn:child> <http://www.w3.org/2000/01/rdf-schema#subClassOf> <urn:parent> .\n<urn:hidden> a <urn:child> .".to_vec(),
        oxrdfio::RdfFormat::Turtle,
        None,
    )]).unwrap();
    let query: String =
        form_urlencoded::byte_serialize(b"ASK { <urn:hidden> a <urn:parent> }").collect();
    let uri = format!("/secret/sparql?reasoning=demand&reasoningProfile=rdfs&query={query}");
    for _ in 0..2 {
        let r = call(
            &s.app,
            "GET",
            &uri,
            &[
                ("authorization", &b("alice")),
                ("accept", "application/sparql-results+json"),
            ],
            "",
        )
        .await;
        assert_eq!(r.status, StatusCode::OK, "{}", r.text());
        assert_eq!(r.json()["boolean"], true);
    }
    let operator = get_as(&s.app, "/$/stats", Some(&bearer(&t_prom()))).await;
    assert_eq!(operator.status, StatusCode::OK, "{}", operator.text());
    let body = operator.json();
    let cache = &body["derivedTableCache"];
    assert_eq!(cache["scope"], "process");
    assert!(cache["capacityBytes"].is_u64());
    assert!(cache["estimatedBytes"].as_u64().unwrap() > 0);
    assert!(cache["tables"].as_u64().unwrap() > 0);
    let one = get_as(&s.app, "/$/stats/wiki", Some(&b("alice"))).await;
    assert_eq!(one.status, StatusCode::OK, "{}", one.text());
    assert_eq!(one.json()["derivedTableCache"]["scope"], "process");
    for uri in ["/$/stats", "/$/stats/wiki"] {
        let r = reader_stats(uri.into()).await;
        assert_eq!(r.status, StatusCode::OK, "{}", r.text());
        let body = r.json();
        assert!(body.get("derivedTableCache").is_none());
        assert!(body["datasets"].get("/secret").is_none());
    }
    let anonymous = get_as(&s.app, "/$/stats", None).await;
    assert_eq!(anonymous.status, StatusCode::OK);
    assert!(anonymous.json().get("derivedTableCache").is_none());
}
