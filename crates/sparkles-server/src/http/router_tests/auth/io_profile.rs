//! `profile=io` reports process-wide counters, so it needs the metrics permission.

use super::*;

#[tokio::test]
async fn profile_io_needs_the_metrics_permission() {
    let s = build(Fixture {
        extra: format!(
            r#"
[[tokens]]
name = "profiler"
hash = "{}"
datasets = {{ wiki = "read" }}
server = ["metrics"]
"#,
            token_hash(&tok('D'))
        ),
        ..Default::default()
    });
    let uri = format!("/wiki{ASK}&format=sparkles&profile=io");
    let sparkles = |auth: &str| {
        [
            ("authorization", auth.to_string()),
            ("accept", "application/x-sparkles+json".to_string()),
        ]
    };
    let ask = |auth: String, uri: String| {
        let app = s.app.clone();
        async move {
            let h = sparkles(&auth);
            let h: Vec<(&str, &str)> = h.iter().map(|(k, v)| (*k, v.as_str())).collect();
            call(&app, "GET", &uri, &h, "").await
        }
    };
    // bob reads wiki but holds no server permission
    let bob = ask(b("bob"), uri.clone()).await;
    assert_eq!(bob.status, StatusCode::FORBIDDEN, "{}", bob.text());
    assert!(bob.json()["error"].as_str().unwrap().contains("metrics"));
    // without the parameter bob's query runs
    let plain = ask(b("bob"), format!("/wiki{ASK}&format=sparkles")).await;
    assert_eq!(plain.status, StatusCode::OK, "{}", plain.text());
    // a holder of the permission gets the profile
    let ok = ask(bearer(&tok('D')), uri).await;
    assert_eq!(ok.status, StatusCode::OK, "{}", ok.text());
    assert!(ok.json()["meta"]["io"].is_object(), "{}", ok.text());
    // another value is a bad request, whoever asks
    for who in [b("bob"), bearer(&tok('D'))] {
        let r = ask(who, format!("/wiki{ASK}&format=sparkles&profile=bogus")).await;
        assert_eq!(r.status, StatusCode::BAD_REQUEST, "{}", r.text());
        assert!(r.json()["error"].as_str().unwrap().contains("io"));
    }
}
