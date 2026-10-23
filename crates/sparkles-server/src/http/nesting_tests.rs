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
async fn describe_iris_get_a_400() {
    let (_dir, app) = server();
    let ct = "application/sparql-query";
    let describe = |n: usize| {
        format!(
            "DESCRIBE {} WHERE {{ ?s ?p ?o }}",
            "<http://ex.org/a> ".repeat(n)
        )
    };
    for path in ["/c/sparql", "/c/explain"] {
        refused(&send(&app, post(path, ct, describe(100_000))).await, 1024);
    }
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

/// A query of one basic graph pattern of `n` triple patterns, each of which matches.
fn bgp(n: usize) -> String {
    format!(
        "PREFIX : <http://ex.org/> SELECT * WHERE {{ {}}}",
        ":a :p :b . ".repeat(n)
    )
}

/// A `DELETE WHERE` of `n` triple patterns that match nothing.
fn delete_where(n: usize) -> String {
    let mut u = String::from("PREFIX : <http://ex.org/> DELETE WHERE { ");
    for i in 0..n {
        u.push_str(&format!(":x :p :o{i} . "));
    }
    u.push('}');
    u
}

fn plan_refused(r: &Resp) {
    let limit = sparkles::sparql::depth::MAX_PLAN_DEPTH;
    assert_eq!(r.status, StatusCode::BAD_REQUEST, "{}", r.text());
    assert!(
        r.text()
            .contains(&format!("query plan nested deeper than {limit} levels")),
        "{}",
        r.text()
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_basic_graph_pattern_at_the_plan_limit_runs_and_explains() {
    use sparkles::sparql::depth::MAX_PLAN_DEPTH;
    let (_dir, app) = server();
    // each triple pattern is a join and a sort, and the projection and the group are plan
    // levels too
    let n = (MAX_PLAN_DEPTH - 3) / 2;
    let ct = "application/sparql-query";
    let r = send(&app, post("/c/sparql", ct, bgp(n))).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let r = send(&app, post("/c/explain", ct, bgp(n))).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    assert!(r.body.len() > 100 * n, "{}", r.body.len());
    // the explanation reads plans of up to 256 levels, and refuses a deeper one cleanly; the
    // route comes with the MCP tools
    #[cfg(feature = "mcp")]
    {
        let explain = Request::post("/c/sparql/explain")
            .header(header::CONTENT_TYPE, "application/json")
            .header(header::ACCEPT, "application/json")
            .body(Body::from(json!({ "query": bgp(n) }).to_string()))
            .unwrap();
        let r = send(&app, explain).await;
        assert_eq!(r.status, StatusCode::BAD_REQUEST, "{}", r.text());
        assert!(r.text().contains("the plan nests deeper than 256 levels"));
    }
    let u = delete_where((MAX_PLAN_DEPTH - 2) / 2);
    let r = send(&app, post("/c/update", "application/sparql-update", u)).await;
    assert!(r.status.is_success(), "{}", r.text());
}

#[tokio::test(flavor = "multi_thread")]
async fn nested_ranges_get_a_400() {
    let (_dir, app) = server();
    let q = "SELECT * WHERE { ?s ((<http://ex.org/p>{32}){32}){4} ?o }";
    for path in ["/c/sparql", "/c/explain"] {
        let r = send(&app, post(path, "application/sparql-query", q.to_string())).await;
        plan_refused(&r);
    }
    // the JSON explanation route comes with the MCP tools
    #[cfg(feature = "mcp")]
    {
        let explain = Request::post("/c/sparql/explain")
            .header(header::CONTENT_TYPE, "application/json")
            .header(header::ACCEPT, "application/json")
            .body(Body::from(json!({ "query": q }).to_string()))
            .unwrap();
        plan_refused(&send(&app, explain).await);
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_basic_graph_pattern_past_the_plan_limit_gets_a_400() {
    use sparkles::sparql::depth::MAX_PLAN_DEPTH;
    let (_dir, app) = server();
    let ct = "application/sparql-query";
    for n in [(MAX_PLAN_DEPTH - 3) / 2 + 1, MAX_PLAN_DEPTH + 1, 100_000] {
        for path in ["/c/sparql", "/c/explain"] {
            plan_refused(&send(&app, post(path, ct, bgp(n))).await);
        }
    }
    for n in [(MAX_PLAN_DEPTH - 2) / 2 + 1, MAX_PLAN_DEPTH + 1, 100_000] {
        let u = delete_where(n);
        plan_refused(&send(&app, post("/c/update", "application/sparql-update", u)).await);
    }
}

/// The JSON of a failed plan nests twice as deep as the plan, and writing it and dropping
/// it recurse. A debug build needs more stack for that than a 2 MiB thread has.
#[test]
fn a_failed_plan_at_the_limit_is_written_on_a_small_stack() {
    use sparkles::sparql::depth::MAX_PLAN_DEPTH;
    std::thread::Builder::new()
        .stack_size(2 << 20)
        .spawn(|| {
            // built without recursion (`json!` would copy the plan by recursion): each plan
            // level is an object and an array
            let mut plan = json!({ "operator": "scan" });
            for _ in 0..MAX_PLAN_DEPTH {
                let mut node = serde_json::Map::new();
                node.insert("operator".into(), "join".into());
                node.insert("children".into(), serde_json::Value::Array(vec![plan]));
                plan = serde_json::Value::Object(node);
            }
            let mut body = serde_json::Map::new();
            body.insert("error".into(), "timed out".into());
            body.insert("plan".into(), plan);
            let body = serde_json::Value::Object(body);
            let depth = json_depth(&body);
            assert!(depth > 2 * MAX_PLAN_DEPTH, "{depth}");
            let mut resp = ApiError(StatusCode::REQUEST_TIMEOUT, body).into_response();
            assert_eq!(resp.status(), StatusCode::REQUEST_TIMEOUT);
            // the middleware that adds the request id writes the body again
            add_request_id(&mut resp, Some("r1".into()));
            let length: usize = resp.headers()[header::CONTENT_LENGTH]
                .to_str()
                .unwrap()
                .parse()
                .unwrap();
            assert!(length > 20 * MAX_PLAN_DEPTH, "{length}");
            drop(resp);
        })
        .unwrap()
        .join()
        .unwrap();
}
