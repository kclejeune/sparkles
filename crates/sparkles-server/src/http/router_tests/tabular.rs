//! CSV and TSV tables in `POST /{ds}/upload` (spec C05 §7).

use super::{Server, select_rows, send, server};
use crate::state::{AppState, DbType};
use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use sparkles::store::StoreOptions;
use std::sync::Arc;
use std::time::Duration;

const PEOPLE: &str = "id,name\n7,Ann\n8,Bob\n";

fn plain(uri: &str, ct: &str, body: &str) -> Request<Body> {
    Request::post(uri)
        .header(header::CONTENT_TYPE, ct)
        .body(Body::from(body.to_string()))
        .unwrap()
}

/// A multipart body: `(field name, file name, content)`.
fn multipart(uri: &str, parts: &[(&str, Option<&str>, &str)]) -> Request<Body> {
    let b = "sparkles-test-boundary";
    let mut body = String::new();
    for (name, file, content) in parts {
        body.push_str(&format!(
            "--{b}\r\nContent-Disposition: form-data; name=\"{name}\""
        ));
        if let Some(f) = file {
            body.push_str(&format!("; filename=\"{f}\""));
        }
        body.push_str(&format!("\r\n\r\n{content}\r\n"));
    }
    body.push_str(&format!("--{b}--\r\n"));
    Request::post(uri)
        .header(
            header::CONTENT_TYPE,
            format!("multipart/form-data; boundary={b}"),
        )
        .body(Body::from(body))
        .unwrap()
}

#[tokio::test]
async fn csv_bodies_use_the_default_mapping() {
    let s = server();
    let r = send(
        &s.app,
        plain(
            "/ds/upload?base=http%3A%2F%2Fexample.org%2Fp%2F&key=id&graph=urn:people",
            "text/csv",
            PEOPLE,
        ),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let j = r.json();
    assert_eq!(j["count"], 4);
    assert_eq!(j["tables"][0]["file"], "body.csv");
    assert_eq!(j["tables"][0]["rows"], 2);
    assert_eq!(
        select_rows(
            &s.app,
            "SELECT * { GRAPH <urn:people> { <http://example.org/p/7> <http://example.org/p/name> \"Ann\" } }"
        )
        .await,
        1
    );
    // TSV, and no base: the server has no file URL to name things with
    let r = send(
        &s.app,
        plain(
            "/ds/upload?base=http://e/",
            "text/tab-separated-values",
            "a\tb\n1\t\"2\n",
        ),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    assert_eq!(
        select_rows(&s.app, "SELECT * { ?s <http://e/b> \"\\\"2\" }").await,
        1
    );
    let r = send(&s.app, plain("/ds/upload", "text/csv", PEOPLE)).await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST, "{}", r.text());
    assert!(r.text().contains("needs a base IRI"), "{}", r.text());
}

#[tokio::test]
async fn multipart_uploads_take_mappings_and_templates() {
    let s = server();
    let mapping = r#"{"@context": "http://www.w3.org/ns/csvw",
        "tableSchema": {"aboutUrl": "http://example.org/person/{id}", "columns": [
            {"name": "id", "datatype": "integer", "propertyUrl": "http://example.org/id"},
            {"name": "name", "propertyUrl": "http://schema.org/name", "lang": "en"}]}}"#;
    let r = send(
        &s.app,
        multipart(
            "/ds/upload",
            &[
                ("mapping", None, mapping),
                ("file", Some("people.csv"), PEOPLE),
                ("file", Some("more.ttl"), "<http://example.org/person/7> <http://example.org/knows> <http://example.org/person/8> ."),
            ],
        ),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    assert_eq!(r.json()["count"], 5);
    assert_eq!(r.json()["tables"][0]["file"], "people.csv");
    assert_eq!(
        select_rows(
            &s.app,
            "SELECT * { <http://example.org/person/8> <http://schema.org/name> \"Bob\"@en ; <http://example.org/id> 8 }"
        )
        .await,
        1
    );
    let template = "CONSTRUCT { ?s <http://example.org/label> ?name } WHERE { BIND(IRI(CONCAT('http://example.org/t/', ?id)) AS ?s) }";
    let r = send(
        &s.app,
        multipart(
            "/ds/upload",
            &[
                ("template", None, template),
                ("file", Some("people.csv"), PEOPLE),
            ],
        ),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    assert_eq!(
        select_rows(&s.app, "SELECT * { ?s <http://example.org/label> ?n }").await,
        2
    );
    // refused: a key with a mapping, a mapping without a table, a template with SERVICE
    let r = send(
        &s.app,
        multipart(
            "/ds/upload?key=id",
            &[
                ("mapping", None, mapping),
                ("file", Some("people.csv"), PEOPLE),
            ],
        ),
    )
    .await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST, "{}", r.text());
    let r = send(
        &s.app,
        multipart(
            "/ds/upload",
            &[
                ("mapping", None, mapping),
                ("file", Some("a.ttl"), "<urn:a> <urn:b> <urn:c> ."),
            ],
        ),
    )
    .await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST, "{}", r.text());
    let r = send(
        &s.app,
        multipart(
            "/ds/upload",
            &[
                (
                    "template",
                    None,
                    "CONSTRUCT { ?s ?p ?o } WHERE { SERVICE <http://e/q> { ?s ?p ?o } }",
                ),
                ("file", Some("people.csv"), PEOPLE),
            ],
        ),
    )
    .await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST, "{}", r.text());
}

#[tokio::test]
async fn bad_tables_commit_nothing() {
    let s = server();
    let before = s.state.get("ds").unwrap().store.head_commit().seq;
    let mapping = r#"{"@context": "http://www.w3.org/ns/csvw",
        "tableSchema": {"aboutUrl": "http://e/{id}", "columns": [
            {"name": "id", "datatype": "integer", "propertyUrl": "http://e/id"},
            {"name": "name", "propertyUrl": "http://e/name"}]}}"#;
    let r = send(
        &s.app,
        multipart(
            "/ds/upload",
            &[
                ("mapping", None, mapping),
                ("file", Some("good.ttl"), "<urn:a> <urn:b> <urn:c> ."),
                ("file", Some("people.csv"), "id,name\n7,Ann\nx,Bob\n"),
            ],
        ),
    )
    .await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST, "{}", r.text());
    assert!(
        r.json()["error"]
            .as_str()
            .unwrap()
            .contains("people.csv: row 3, column 1 (id): \"x\" is not a valid integer"),
        "{}",
        r.text()
    );
    assert_eq!(s.state.get("ds").unwrap().store.head_commit().seq, before);
}

#[tokio::test]
async fn converted_tables_count_against_the_decompressed_limit() {
    let dir = tempfile::tempdir().unwrap();
    let mut st =
        AppState::new(dir.path(), StoreOptions::default(), Duration::from_secs(30)).unwrap();
    st.limits.max_decompressed_bytes = Some(200);
    let state = Arc::new(st);
    state.attach("ds", DbType::Mem, None).unwrap();
    let s = Server {
        app: super::super::router(state.clone()),
        state,
        _dir: dir,
    };
    let mut big = String::from("id,name\n");
    for i in 0..100 {
        big.push_str(&format!("{i},name {i}\n"));
    }
    let r = send(&s.app, plain("/ds/upload?base=http://e/", "text/csv", &big)).await;
    assert_eq!(r.status, StatusCode::PAYLOAD_TOO_LARGE, "{}", r.text());
    assert_eq!(s.state.get("ds").unwrap().store.snapshot().len(), 0);
}
