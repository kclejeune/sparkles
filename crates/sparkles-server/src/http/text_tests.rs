//! `GET /{ds}/text`: ranked full-text hits with HTML snippets.

use super::*;
use axum::body::Body;
use axum::extract::Request;
use sparkles::store::StoreOptions;
use tower::ServiceExt;

async fn get_json(app: &Router, uri: &str) -> (StatusCode, J) {
    let res = app
        .clone()
        .oneshot(Request::get(uri).body(Body::empty()).unwrap())
        .await
        .unwrap();
    let status = res.status();
    let body = axum::body::to_bytes(res.into_body(), usize::MAX)
        .await
        .unwrap();
    (status, serde_json::from_slice(&body).unwrap())
}

#[tokio::test]
async fn text_search_returns_hits_with_snippets() {
    let dir = tempfile::tempdir().unwrap();
    let st = Arc::new(
        AppState::new(dir.path(), StoreOptions::default(), Duration::from_secs(30)).unwrap(),
    );
    st.attach("c", DbType::Mem, None).unwrap();
    let ds = st.get("c").unwrap();
    ds.store
        .load(&[sparkles::io::Source::from_bytes(
            br#"<urn:a> <http://www.w3.org/2000/01/rdf-schema#label> "Fox & <Hound>"@en .
<urn:b> <http://www.w3.org/2000/01/rdf-schema#comment> "a brown fox" .
"#
            .to_vec(),
            sparkles::io::RdfFormat::NTriples,
            None,
        )])
        .unwrap();
    ds.store.enable_text(Default::default()).unwrap();
    let app = router(st.clone());

    let (status, j) = get_json(&app, "/c/text?q=fox").await;
    assert_eq!(status, StatusCode::OK, "{j}");
    let hits = j["hits"].as_array().unwrap();
    assert_eq!(hits.len(), 2, "{j}");
    let a = hits.iter().find(|h| h["s"]["value"] == "urn:a").unwrap();
    assert_eq!(a["snippet"], "<mark>Fox</mark> &amp; &lt;Hound&gt;");
    assert_eq!(a["literal"]["value"], "Fox & <Hound>");
    assert_eq!(a["literal"]["xml:lang"], "en");
    assert_eq!(
        a["p"]["value"],
        "http://www.w3.org/2000/01/rdf-schema#label"
    );
    assert!(a["score"].as_f64().unwrap() > 0.0);
    assert_eq!(j["limited"], false);

    // restricted to a predicate, without snippets, limited
    let (_, j) = get_json(
        &app,
        "/c/text?q=fox&predicate=http%3A%2F%2Fwww.w3.org%2F2000%2F01%2Frdf-schema%23comment&highlight=false&limit=1",
    )
    .await;
    let hits = j["hits"].as_array().unwrap();
    assert_eq!(hits.len(), 1, "{j}");
    assert_eq!(hits[0]["s"]["value"], "urn:b");
    assert!(hits[0].get("snippet").is_none());
    assert_eq!(j["limited"], true);

    for (uri, needle) in [
        ("/c/text", "q: required"),
        ("/c/text?q=fox&limit=0", "limit"),
        ("/c/text?q=fox&predicate=not%20an%20iri", "predicate"),
        ("/c/text?q=label:fox", "field names"),
    ] {
        let (status, j) = get_json(&app, uri).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{uri}: {j}");
        assert!(j["error"].as_str().unwrap().contains(needle), "{uri}: {j}");
    }
}
