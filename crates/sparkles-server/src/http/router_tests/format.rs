//! `POST /$/format` through the router: both body forms, the error shapes, the body
//! limit, `--format-endpoint`, CORS.

use super::*;
use crate::state::FormatEndpoint;
use serde_json::json;

fn fmt_server(conf: impl FnOnce(&mut AppState)) -> (tempfile::TempDir, Router) {
    let dir = tempfile::tempdir().unwrap();
    let mut st =
        AppState::new(dir.path(), StoreOptions::default(), Duration::from_secs(30)).unwrap();
    conf(&mut st);
    (dir, router(Arc::new(st)))
}

async fn post_as(app: &Router, uri: &str, content_type: &str, body: &str) -> Resp {
    let req = Request::post(uri)
        .header(header::CONTENT_TYPE, content_type)
        .body(Body::from(body.to_string()))
        .unwrap();
    send(app, req).await
}

async fn post_json(app: &Router, uri: &str, body: J) -> Resp {
    post_as(app, uri, "application/json", &body.to_string()).await
}

#[tokio::test]
async fn formats_json_bodies() {
    let (_d, app) = fmt_server(|_| {});
    // the cursor counts UTF-16 code units: after the emoji (two units) is 11
    let text = "SELECT ('😀' AS ?x) {}\n";
    let r = post_json(
        &app,
        "/$/format",
        json!({ "text": text, "language": "sparql", "cursorOffset": 11 }),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    assert_eq!(r.content_type, "application/json");
    assert_eq!(
        r.json(),
        json!({
            "text": text,
            "changed": false,
            "language": "sparql",
            "cursorOffset": 11,
            "warnings": [],
        })
    );
    // no language: sniffed; an undeclared prefix is a warning with its position
    let r = post_json(&app, "/$/format", json!({ "text": "ASK { ?s ex:p ?o }" })).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let j = r.json();
    assert_eq!(j["language"], "sparql");
    assert_eq!(j["cursorOffset"], J::Null);
    assert_eq!(j["warnings"][0]["code"], "undeclared-prefix");
    assert_eq!(j["warnings"][0]["line"], 1);
    assert_eq!(j["warnings"][0]["column"], 10);
    // options from the body and the query string
    let r = post_json(
        &app,
        "/$/format?operatorPosition=trailing&prefixGroup=rdf,rdfs&prefixGroup=%22%22,ex",
        json!({ "text": "ASK {}", "options": { "lineWidth": 80, "typeShorthand": false } }),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
}

#[tokio::test]
async fn syntax_errors_have_the_query_error_shape() {
    let (_d, app) = fmt_server(|_| {});
    let r = post_json(
        &app,
        "/$/format",
        json!({ "text": "select * {", "language": "sparql" }),
    )
    .await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST);
    let j = r.json();
    assert_eq!(j["code"], "syntax");
    assert_eq!(j["language"], "sparql");
    assert_eq!(j["line"], 1);
    assert_eq!(j["column"], 11);
    assert!(
        j["error"]
            .as_str()
            .unwrap()
            .starts_with("SPARQL syntax error at line 1, column 11: expected"),
        "{j}"
    );
    assert!(j["detail"].is_string(), "{j}");
    assert!(j["requestId"].is_string(), "{j}");
}

#[tokio::test]
async fn bad_requests() {
    let (_d, app) = fmt_server(|_| {});
    let bad = |r: Resp| {
        assert_eq!(r.status, StatusCode::BAD_REQUEST, "{}", r.text());
        let j = r.json();
        assert_eq!(j["code"], "bad-request", "{j}");
        assert!(j["requestId"].is_string(), "{j}");
        j
    };
    let j = bad(post_json(
        &app,
        "/$/format",
        json!({ "text": "ASK {}", "options": { "lineWidth": 1000 } }),
    )
    .await);
    assert_eq!(j["option"], "lineWidth");
    assert_eq!(
        j["error"],
        "lineWidth: expected an integer from 40 to 400, got 1000"
    );
    let j = bad(post_json(
        &app,
        "/$/format",
        json!({ "text": "ASK {}", "options": { "line-width": 80 } }),
    )
    .await);
    assert_eq!(j["option"], "line-width");
    let j = bad(post_json(
        &app,
        "/$/format",
        json!({ "text": "ASK {}", "options": { "quoteStyle": "single" } }),
    )
    .await);
    assert_eq!(j["option"], "quoteStyle");
    let j = bad(post_json(&app, "/$/format?indentWidth=x", json!({ "text": "ASK {}" })).await);
    assert_eq!(j["option"], "indentWidth");
    let j = bad(post_json(
        &app,
        "/$/format?prefixGroup=rdf,rdf",
        json!({ "text": "ASK {}" }),
    )
    .await);
    assert_eq!(j["option"], "prefixGroups");
    bad(post_json(&app, "/$/format?sort=yes", json!({ "text": "ASK {}" })).await);
    bad(post_json(
        &app,
        "/$/format",
        json!({ "text": "ASK {}", "cursorOffset": 7 }),
    )
    .await);
    bad(post_json(
        &app,
        "/$/format",
        json!({ "text": "ASK {}", "language": "cobol" }),
    )
    .await);
    bad(post_json(&app, "/$/format", json!({ "language": "sparql" })).await);
    bad(post_as(&app, "/$/format", "application/json", "{").await);
    // the query string's options are checked even where the body's override them
    let r = post_json(
        &app,
        "/$/format?lineWidth=10",
        json!({ "text": "ASK {}", "options": { "lineWidth": 80 } }),
    )
    .await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn unsupported_languages() {
    let (_d, app) = fmt_server(|_| {});
    let r = post_json(
        &app,
        "/$/format",
        json!({ "text": "<a> <b> <c> .", "language": "turtle" }),
    )
    .await;
    assert_eq!(r.status, StatusCode::UNSUPPORTED_MEDIA_TYPE);
    assert_eq!(r.json()["error"], "turtle formatting is not available yet");
    let r = post_json(
        &app,
        "/$/format",
        json!({ "text": "x", "language": "rdfxml" }),
    )
    .await;
    assert_eq!(r.status, StatusCode::UNSUPPORTED_MEDIA_TYPE);
    assert_eq!(r.json()["error"], sparkles_fmt::RDF_XML_MESSAGE);
    let r = post_as(&app, "/$/format", "application/rdf+xml", "<rdf:RDF/>").await;
    assert_eq!(r.status, StatusCode::UNSUPPORTED_MEDIA_TYPE);
    assert_eq!(r.json()["error"], sparkles_fmt::RDF_XML_MESSAGE);
    let r = post_as(&app, "/$/format", "text/turtle", "<a> <b> <c> .").await;
    assert_eq!(r.status, StatusCode::UNSUPPORTED_MEDIA_TYPE);
    let r = post_as(&app, "/$/format", "image/png", "x").await;
    assert_eq!(r.status, StatusCode::UNSUPPORTED_MEDIA_TYPE);
}

#[tokio::test]
async fn raw_bodies() {
    let (_d, app) = fmt_server(|_| {});
    let req = Request::post("/$/format")
        .header(header::CONTENT_TYPE, "application/sparql-update")
        .body(Body::from("CLEAR ALL"))
        .unwrap();
    let res = app.clone().oneshot(req).await.unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    assert_eq!(
        res.headers()[header::CONTENT_TYPE],
        "application/sparql-update"
    );
    assert_eq!(res.headers()["sparkles-format-changed"], "false");
    let body = axum::body::to_bytes(res.into_body(), usize::MAX)
        .await
        .unwrap();
    assert_eq!(&body[..], b"CLEAR ALL");
    // text/plain needs the language
    let r = post_as(&app, "/$/format", "text/plain", "ASK {}").await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST);
    let r = post_as(&app, "/$/format?language=sparql", "text/plain", "ASK {}").await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    assert_eq!(r.text(), "ASK {}\n");
    // errors stay JSON
    let r = post_as(&app, "/$/format", "application/sparql-query", "ASK {").await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST);
    assert_eq!(r.json()["code"], "syntax");
}

#[tokio::test]
async fn body_limit_and_endpoint_switch() {
    let (_d, app) = fmt_server(|st| st.format.max_bytes = Some(64));
    let text = format!("ASK {{}} # {}", "x".repeat(100));
    let r = post_json(&app, "/$/format", json!({ "text": text })).await;
    assert_eq!(r.status, StatusCode::PAYLOAD_TOO_LARGE);
    assert!(
        r.json()["error"]
            .as_str()
            .unwrap()
            .contains("--format-max-mb")
    );
    let r = post_json(&app, "/$/format", json!({ "text": "ASK {}" })).await;
    assert_eq!(r.status, StatusCode::OK);

    let (_d, app) = fmt_server(|st| st.format.endpoint = FormatEndpoint::Off);
    let r = post_json(&app, "/$/format", json!({ "text": "ASK {}" })).await;
    assert_eq!(r.status, StatusCode::NOT_FOUND);
    // without auth there is no anonymous caller: `authenticated` admits everyone
    let (_d, app) = fmt_server(|st| st.format.endpoint = FormatEndpoint::Authenticated);
    let r = post_json(&app, "/$/format", json!({ "text": "ASK {}" })).await;
    assert_eq!(r.status, StatusCode::OK);
}

#[tokio::test]
async fn the_cursor_counts_utf16_units_both_ways() {
    let (_d, app) = fmt_server(|_| {});
    // astral characters (two UTF-16 units each) before the cursor, in a document that
    // changes (formatting drops the byte order mark, one unit)
    let text = "\u{feff}SELECT ('😀𝔸' AS ?x) {}";
    let before = "\u{feff}SELECT ('😀𝔸";
    let units = before.encode_utf16().count();
    let r = post_json(
        &app,
        "/$/format",
        json!({ "text": text, "cursorOffset": units }),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let j = r.json();
    assert_eq!(j["changed"], true);
    // still right after the same characters in the output
    let out = j["text"].as_str().unwrap();
    let end = out.find("𝔸").unwrap() + "𝔸".len();
    assert_eq!(j["cursorOffset"], out[..end].encode_utf16().count(), "{j}");
    // past the end, by one unit
    let r = post_json(
        &app,
        "/$/format",
        json!({ "text": text, "cursorOffset": text.encode_utf16().count() + 1 }),
    )
    .await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn default_body_limit_and_deadline() {
    // the default limit is 16 MiB
    let (_d, app) = fmt_server(|_| {});
    let big = format!("ASK {{}} # {}", "x".repeat(17 << 20));
    let r = post_as(&app, "/$/format", "application/sparql-query", &big).await;
    assert_eq!(r.status, StatusCode::PAYLOAD_TOO_LARGE);
    assert_eq!(
        r.json()["error"],
        "request body exceeds 16.0 MiB (--format-max-mb)"
    );
    // a deadline that has passed
    let (_d, app) = fmt_server(|st| st.format.timeout = Duration::ZERO);
    let r = post_json(&app, "/$/format", json!({ "text": "ASK {}" })).await;
    assert_eq!(r.status, StatusCode::REQUEST_TIMEOUT, "{}", r.text());
    assert!(r.json()["requestId"].is_string());
}

#[tokio::test]
async fn long_syntax_errors_are_cut() {
    let (_d, app) = fmt_server(|_| {});
    let r = post_json(
        &app,
        "/$/format",
        json!({ "text": "SELECT * {\n  FILTER(?o > )\n}", "language": "sparql" }),
    )
    .await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST);
    let j = r.json();
    let error = j["error"].as_str().unwrap();
    assert!(error.starts_with("SPARQL syntax error at line "), "{j}");
    assert!(error.ends_with(", …") && !error.contains('\n'), "{j}");
    let detail = j["detail"].as_str().unwrap();
    assert!(
        detail.starts_with("expected one of ") && !error.contains(detail),
        "{j}"
    );
}

#[tokio::test]
async fn cors_exposes_the_changed_header() {
    let (_d, app) = fmt_server(|st| st.cors_origins = vec!["https://app.example".into()]);
    let req = Request::post("/$/format")
        .header(header::CONTENT_TYPE, "application/sparql-query")
        .header(header::ORIGIN, "https://app.example")
        .body(Body::from("ASK {}"))
        .unwrap();
    let res = app.clone().oneshot(req).await.unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let exposed = res.headers()[header::ACCESS_CONTROL_EXPOSE_HEADERS]
        .to_str()
        .unwrap()
        .to_ascii_lowercase();
    assert!(exposed.contains("sparkles-format-changed"), "{exposed}");
}

#[cfg(feature = "auth")]
#[tokio::test]
async fn authenticated_mode_refuses_anonymous_callers() {
    use super::auth::{Fixture, b, call_from, config_text, default_peer};
    for (mode, anonymous) in [
        (FormatEndpoint::On, StatusCode::OK),
        (FormatEndpoint::Authenticated, StatusCode::UNAUTHORIZED),
        (FormatEndpoint::Off, StatusCode::NOT_FOUND),
    ] {
        let dir = tempfile::tempdir().unwrap();
        let config = dir.path().join("auth.toml");
        std::fs::write(&config, config_text(&Fixture::default())).unwrap();
        let mut st =
            AppState::new(dir.path(), StoreOptions::default(), Duration::from_secs(30)).unwrap();
        st.auth = Some(Arc::new(
            crate::auth::Auth::open(&config, dir.path()).unwrap().0,
        ));
        st.format.endpoint = mode;
        let st = Arc::new(st);
        st.set_phase(crate::obs::Phase::Ready);
        let app = router(st);
        let ct = ("content-type", "application/json");
        let body = r#"{"text": "ASK {}"}"#;
        let r = call_from(&app, default_peer(), "POST", "/$/format", &[ct], body).await;
        assert_eq!(r.status, anonymous, "{mode:?}: {}", r.text());
        let alice = b("alice");
        let signed_in = [ct, ("authorization", alice.as_str())];
        let r = call_from(&app, default_peer(), "POST", "/$/format", &signed_in, body).await;
        let expected = match mode {
            FormatEndpoint::Off => StatusCode::NOT_FOUND,
            _ => StatusCode::OK,
        };
        assert_eq!(r.status, expected, "{mode:?}: {}", r.text());
    }
}
