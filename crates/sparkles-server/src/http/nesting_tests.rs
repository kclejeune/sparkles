//! Deeply nested queries, updates and uploads get a 400, never a stack overflow (which
//! aborts the server). The test runtime's threads have tokio's default 2 MiB stack, a
//! quarter of what the server gives its own.

use super::*;
use axum::body::Body;
use axum::extract::Request;
use sparkles::store::StoreOptions;
use tower::ServiceExt;

struct Resp {
    status: StatusCode,
    body: Vec<u8>,
}

impl Resp {
    fn text(&self) -> String {
        String::from_utf8_lossy(&self.body).into_owned()
    }
}

async fn send(app: &Router, req: Request<Body>) -> Resp {
    let res = app.clone().oneshot(req).await.unwrap();
    let status = res.status();
    let body = axum::body::to_bytes(res.into_body(), usize::MAX)
        .await
        .unwrap()
        .to_vec();
    Resp { status, body }
}

fn server() -> (tempfile::TempDir, Router) {
    let dir = tempfile::tempdir().unwrap();
    let st = AppState::new(dir.path(), StoreOptions::default(), Duration::from_secs(30)).unwrap();
    let st = Arc::new(st);
    let ds = st.attach("c", DbType::Mem, None).unwrap();
    ds.store
        .load(&[sparkles::io::Source::from_bytes(
            b"<http://ex.org/a> <http://ex.org/p> <http://ex.org/b> .".to_vec(),
            sparkles::io::RdfFormat::Turtle,
            None,
        )])
        .unwrap();
    (dir, router(st))
}

fn post(uri: &str, ct: &str, body: String) -> Request<Body> {
    Request::post(uri)
        .header(header::CONTENT_TYPE, ct)
        .body(Body::from(body))
        .unwrap()
}

/// A query whose groups nest `n` levels.
fn groups(n: usize) -> String {
    format!(
        "SELECT * WHERE {} ?s ?p ?o {}",
        "{".repeat(n),
        "}".repeat(n)
    )
}

/// An update whose WHERE groups nest `n` levels.
fn where_groups(n: usize) -> String {
    format!(
        "DELETE {{ ?s ?p ?o }} WHERE {}?s ?p <http://ex.org/x>{}",
        "{ ".repeat(n),
        " }".repeat(n)
    )
}

/// A query whose `||` chain nests `k` levels.
fn or_chain(k: usize) -> String {
    format!(
        "SELECT * WHERE {{ ?s ?p ?o FILTER(?o = ?o{}) }}",
        " || ?o = ?o".repeat(k)
    )
}

fn refused(r: &Resp, limit: usize) {
    assert_eq!(r.status, StatusCode::BAD_REQUEST, "{}", r.text());
    assert!(
        r.text()
            .contains(&format!("nested deeper than {limit} levels")),
        "{}",
        r.text()
    );
}

const DEPTHS: [usize; 3] = [spargebra::nesting::MAX_NESTING + 1, 10_000, 100_000];

#[tokio::test(flavor = "multi_thread")]
async fn nested_queries_get_a_400() {
    let (_dir, app) = server();
    for path in ["/c/sparql", "/c/explain"] {
        let ct = "application/sparql-query";
        let r = send(&app, post(path, ct, groups(100))).await;
        assert_eq!(r.status, StatusCode::OK, "{path}: {}", r.text());
        let r = send(&app, post(path, ct, or_chain(500))).await;
        assert_eq!(r.status, StatusCode::OK, "{path}: {}", r.text());
        for n in DEPTHS {
            refused(&send(&app, post(path, ct, groups(n))).await, 256);
        }
        refused(&send(&app, post(path, ct, or_chain(100_000))).await, 1024);
    }
    // the form encoding too
    let form = format!(
        "query={}",
        percent_encoding::utf8_percent_encode(&groups(10_000), percent_encoding::NON_ALPHANUMERIC)
    );
    let r = send(
        &app,
        post("/c/sparql", "application/x-www-form-urlencoded", form),
    )
    .await;
    refused(&r, 256);
}

#[tokio::test(flavor = "multi_thread")]
async fn nested_updates_get_a_400() {
    let (_dir, app) = server();
    let ct = "application/sparql-update";
    let data = |n: usize| {
        format!(
            "INSERT DATA {{ <http://ex.org/a> <http://ex.org/p> {}<http://ex.org/o>{} }}",
            "<<( <http://ex.org/s> <http://ex.org/p> ".repeat(n - 1),
            " )>>".repeat(n - 1)
        )
    };
    let r = send(&app, post("/c/update", ct, data(100))).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let r = send(&app, post("/c/update", ct, where_groups(100))).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    for n in DEPTHS {
        refused(&send(&app, post("/c/update", ct, data(n))).await, 256);
        refused(
            &send(&app, post("/c/update", ct, where_groups(n))).await,
            256,
        );
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn nested_uploads_get_a_400() {
    let (_dir, app) = server();
    let turtle = |n: usize| {
        format!(
            "<http://ex.org/a> <http://ex.org/p> {}<http://ex.org/o>{} .",
            "<<( <http://ex.org/s> <http://ex.org/p> ".repeat(n),
            " )>>".repeat(n)
        )
    };
    let json_ld = |n: usize| {
        format!(
            "{}{{\"@id\": \"http://ex.org/o\"}}{}",
            "{\"http://ex.org/p\": ".repeat(n - 1),
            "}".repeat(n - 1)
        )
    };
    let r = send(&app, post("/c/data", "text/turtle", turtle(100))).await;
    assert!(r.status.is_success(), "{}", r.text());
    let r = send(&app, post("/c/data", "application/ld+json", json_ld(100))).await;
    assert!(r.status.is_success(), "{}", r.text());
    for n in DEPTHS {
        refused(
            &send(&app, post("/c/data", "text/turtle", turtle(n))).await,
            256,
        );
        let r = send(&app, post("/c/data", "application/ld+json", json_ld(n))).await;
        refused(&r, sparkles::nesting::MAX_JSON_LD);
    }
}

#[cfg(feature = "shacl")]
#[tokio::test(flavor = "multi_thread")]
async fn nested_shacl_sparql_constraints_get_a_400() {
    let (_dir, app) = server();
    let shapes = |n: usize| {
        format!(
            r#"@prefix sh: <http://www.w3.org/ns/shacl#> . @prefix ex: <http://ex.org/> .
ex:S a sh:NodeShape ; sh:targetNode ex:a ;
  sh:sparql [ sh:select """SELECT $this WHERE {} $this ?p ?o {} FILTER(false) }}""" ] ."#,
            "{ ".repeat(n),
            " }".repeat(n - 1)
        )
    };
    let r = send(&app, post("/c/shacl", "text/turtle", shapes(100))).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    for n in DEPTHS {
        refused(
            &send(&app, post("/c/shacl", "text/turtle", shapes(n))).await,
            256,
        );
    }
}

#[cfg(feature = "shex")]
#[tokio::test(flavor = "multi_thread")]
async fn nested_shex_selectors_get_a_400() {
    let (_dir, app) = server();
    let schema = "PREFIX ex: <http://ex.org/> ex:S { ex:p IRI }".to_string();
    let map = |n: usize| {
        let q = format!(
            "SELECT ?x WHERE {} ?x ?p ?o {}",
            "{ ".repeat(n),
            " }".repeat(n)
        );
        let m = format!("SPARQL \"\"\"{q}\"\"\"@<http://ex.org/S>");
        percent_encoding::utf8_percent_encode(&m, percent_encoding::NON_ALPHANUMERIC).to_string()
    };
    let r = send(
        &app,
        post(
            &format!("/c/shex?map={}", map(100)),
            "text/shex",
            schema.clone(),
        ),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    // (a URI holds at most 64 KiB)
    for n in [spargebra::nesting::MAX_NESTING + 1, 3_000] {
        let r = send(
            &app,
            post(
                &format!("/c/shex?map={}", map(n)),
                "text/shex",
                schema.clone(),
            ),
        )
        .await;
        refused(&r, 256);
    }
}
