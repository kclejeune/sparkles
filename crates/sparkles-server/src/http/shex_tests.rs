//! `POST /{ds}/shex` over HTTP: routing, permissions, rate-limit class, metrics,
//! parameters, and the `validation-work` budget.

use super::super::*;
use axum::body::Body;
use axum::extract::Request;
use sparkles::store::StoreOptions;
use tower::ServiceExt;

struct Resp {
    status: StatusCode,
    headers: HeaderMap,
    body: Vec<u8>,
}

impl Resp {
    fn json(&self) -> J {
        serde_json::from_slice(&self.body)
            .unwrap_or_else(|e| panic!("{e}: {}", String::from_utf8_lossy(&self.body)))
    }
    fn text(&self) -> String {
        String::from_utf8_lossy(&self.body).into_owned()
    }
}

async fn send(app: &Router, req: Request<Body>) -> Resp {
    let res = app.clone().oneshot(req).await.unwrap();
    let status = res.status();
    let headers = res.headers().clone();
    let body = axum::body::to_bytes(res.into_body(), usize::MAX)
        .await
        .unwrap()
        .to_vec();
    Resp {
        status,
        headers,
        body,
    }
}

fn server() -> (tempfile::TempDir, Router) {
    let dir = tempfile::tempdir().unwrap();
    let st = AppState::new(dir.path(), StoreOptions::default(), Duration::from_secs(30)).unwrap();
    let st = Arc::new(st);
    let ds = st.attach("ds", DbType::Mem, None).unwrap();
    ds.store
        .load(&[Source::from_bytes(
            b"<http://ex.org/a> <http://ex.org/p> 1 . <http://ex.org/g> { <http://ex.org/a> <http://ex.org/p> 2 }"
                .to_vec(),
            RdfFormat::TriG,
            None,
        )])
        .unwrap();
    let app = router(st);
    (dir, app)
}

fn post(uri: &str, body: &str) -> Request<Body> {
    Request::post(uri)
        .header(header::CONTENT_TYPE, "text/shex")
        .body(Body::from(body.to_string()))
        .unwrap()
}

const SCHEMA: &str = "<http://ex.org/S> { <http://ex.org/p> . }";

#[test]
fn route_and_class() {
    let methods = crate::auth::ROUTES
        .iter()
        .find(|(r, _)| *r == "/{ds}/shex")
        .map(|(_, m)| *m);
    assert_eq!(methods, Some(&["POST"][..]));
    let uri: Uri = "/ds/shex".parse().unwrap();
    assert_eq!(
        crate::ratelimit::classify(Some("/{ds}/shex"), &Method::POST, &uri, &HeaderMap::new()),
        Some(crate::ratelimit::Class::Query)
    );
    assert_eq!(Op::Shex.as_str(), "shex");
    assert!(Op::ALL.contains(&Op::Shex));
}

#[test]
fn validation_work_is_a_507_budget() {
    let e = Error::BudgetExceeded(sparkles::Budget {
        kind: BudgetKind::ValidationWork,
        limit: 100_000,
        requested: 100_001,
    });
    let ApiError(status, body) = ApiError::from(e);
    assert_eq!(status, StatusCode::INSUFFICIENT_STORAGE);
    assert_eq!(body["budget"], "validation-work");
    assert_eq!(body["limit"], 100_000);
    assert_eq!(body["requested"], 100_001);
}

#[tokio::test]
async fn dataset_info_lists_the_endpoint() {
    let (_dir, app) = server();
    let r = send(
        &app,
        Request::get("/$/datasets/ds").body(Body::empty()).unwrap(),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK);
    assert_eq!(r.json()["endpoints"]["shex"], "/ds/shex");
}

#[tokio::test]
async fn parameters_are_checked() {
    let (_dir, app) = server();
    for (q, needle) in [
        ("graph=not%20an%20iri", "invalid graph IRI"),
        ("results=some", "invalid results"),
        ("format=ttl", "unknown report format"),
        ("semact-trace=yes", "invalid semact-trace"),
        ("map=x&node=y", "either map or node"),
        ("shape=S", "shape needs node"),
    ] {
        let r = send(&app, post(&format!("/ds/shex?{q}"), SCHEMA)).await;
        assert_eq!(r.status, StatusCode::BAD_REQUEST, "{q}: {}", r.text());
        assert!(
            r.json()["error"].as_str().unwrap().contains(needle),
            "{q}: {}",
            r.text()
        );
    }
    let r = send(
        &app,
        post(
            "/ds/shex?graph=http://ex.org/none&node=%3Chttp://ex.org/a%3E&shape=%3Chttp://ex.org/S%3E",
            SCHEMA,
        ),
    )
    .await;
    assert_eq!(r.status, StatusCode::NOT_FOUND, "{}", r.text());
    assert_eq!(r.json()["error"], "no such graph: <http://ex.org/none>");
    let r = send(&app, post("/nope/shex?node=x", SCHEMA)).await;
    assert_eq!(r.status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn requests_count_as_shex() {
    let (_dir, app) = server();
    let r = send(
        &app,
        post(
            "/ds/shex?graph=http://ex.org/g&results=nonconformant&format=smap&node=%3Chttp://ex.org/a%3E&shape=%3Chttp://ex.org/S%3E",
            SCHEMA,
        ),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    assert_eq!(r.text(), "");
    let m = send(
        &app,
        Request::get("/$/metrics").body(Body::empty()).unwrap(),
    )
    .await
    .text();
    assert!(
        m.contains(r#"sparkles_requests_total{dataset="ds",operation="shex""#),
        "{m}"
    );
    assert!(m.contains(r#"budget="validation-work""#), "{m}");
}

// ------------------------------------------------------------- validation ----

const PEOPLE: &str = r#"
@prefix ex: <http://ex.org/> .
@prefix foaf: <http://xmlns.com/foaf/0.1/> .
ex:alice a foaf:Person ; foaf:name "Alice" ; foaf:knows ex:bob .
ex:bob a foaf:Person ; foaf:name "Bob" .
ex:carol a foaf:Person ; foaf:knows ex:alice .
"#;

const PERSON: &str = "PREFIX ex: <http://ex.org/>
PREFIX foaf: <http://xmlns.com/foaf/0.1/>
PREFIX xsd: <http://www.w3.org/2001/XMLSchema#>
start = @ex:Person
ex:Person { foaf:name xsd:string ; foaf:knows @ex:Person * }";

/// A server whose dataset `ds` holds `data` (Turtle), its state changed by `setup`.
fn server_with(data: &str, setup: impl FnOnce(&mut AppState)) -> (tempfile::TempDir, Router) {
    let dir = tempfile::tempdir().unwrap();
    let mut st =
        AppState::new(dir.path(), StoreOptions::default(), Duration::from_secs(30)).unwrap();
    setup(&mut st);
    let st = Arc::new(st);
    let ds = st.attach("ds", DbType::Mem, None).unwrap();
    ds.store
        .load(&[Source::from_bytes(
            data.as_bytes().to_vec(),
            RdfFormat::Turtle,
            None,
        )])
        .unwrap();
    (dir, router(st))
}

fn enc(s: &str) -> String {
    percent_encoding::utf8_percent_encode(s, percent_encoding::NON_ALPHANUMERIC).to_string()
}

fn post_as(uri: &str, ct: &str, body: &str) -> Request<Body> {
    Request::post(uri)
        .header(header::CONTENT_TYPE, ct)
        .body(Body::from(body.to_string()))
        .unwrap()
}

fn error_of(r: &Resp) -> String {
    r.json()["error"].as_str().unwrap_or_default().to_string()
}

#[tokio::test]
async fn validates_a_query_map() {
    let (_dir, app) = server_with(PEOPLE, |_| {});
    let map = enc("{FOCUS a foaf:Person}@ex:Person");
    let r = send(&app, post(&format!("/ds/shex?map={map}"), PERSON)).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    assert!(r.headers.contains_key("sparkles-commit"));
    assert_eq!(r.headers[header::CONTENT_TYPE], "application/json");
    let j = r.json();
    // carol has no name; alice and bob conform
    assert_eq!(j["conforms"], false);
    assert_eq!(j["counts"]["conformant"], 2);
    assert_eq!(j["counts"]["nonconformant"], 1);
    let results = j["results"].as_array().unwrap();
    assert_eq!(results.len(), 3);
    let carol = results
        .iter()
        .find(|x| x["node"]["value"] == "http://ex.org/carol")
        .unwrap();
    assert_eq!(carol["status"], "nonconformant");
    assert_eq!(carol["shape"]["value"], "http://ex.org/Person");
    assert!(carol["reason"].as_str().is_some());
    assert!(!carol["appinfo"]["failures"].as_array().unwrap().is_empty());
    assert!(j.get("stats").is_none());

    // only the nonconformant results, with the counts of all; the typing's counters
    let r = send(
        &app,
        post(
            &format!("/ds/shex?map={map}&results=nonconformant&stats=true"),
            PERSON,
        ),
    )
    .await;
    let j = r.json();
    assert_eq!(j["results"].as_array().unwrap().len(), 1);
    assert_eq!(j["counts"]["conformant"], 2);
    assert!(j["stats"]["pairs"].as_u64().unwrap() >= 3, "{j}");
    assert!(j["stats"]["evaluations"].as_u64().is_some());
    assert!(j["stats"]["waves"].is_array());
}

#[tokio::test]
async fn report_formats() {
    let (_dir, app) = server_with(PEOPLE, |_| {});
    let node = enc("<http://ex.org/carol>");
    let q = format!("/ds/shex?node={node}");
    let r = send(&app, post(&format!("{q}&format=smap"), PERSON)).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    assert_eq!(r.text(), "<http://ex.org/carol>@!START\n");
    assert!(
        r.headers[header::CONTENT_TYPE]
            .to_str()
            .unwrap()
            .starts_with("text/plain")
    );
    let r = send(&app, post(&format!("{q}&format=text"), PERSON)).await;
    assert!(
        r.text().starts_with(
            "<http://ex.org/carol> @ START :: Focus = <http://ex.org/carol>, Status = nonconformant, Reason = "
        ),
        "{}",
        r.text()
    );
    let r = send(&app, post(&format!("{q}&format=shapemap"), PERSON)).await;
    let j = r.json();
    assert_eq!(j[0]["node"], "<http://ex.org/carol>");
    assert_eq!(j[0]["shape"], "START");
    assert_eq!(j[0]["status"], "nonconformant");
    // the Accept header picks the text report
    let mut req = post(&q, PERSON);
    req.headers_mut()
        .insert(header::ACCEPT, "text/plain".parse().unwrap());
    let r = send(&app, req).await;
    assert!(r.text().contains("Status = nonconformant"), "{}", r.text());
    // a node and a shape
    let shape = enc("<http://ex.org/Person>");
    let alice = enc("<http://ex.org/alice>");
    let r = send(
        &app,
        post(
            &format!("/ds/shex?node={alice}&shape={shape}&format=text"),
            PERSON,
        ),
    )
    .await;
    assert_eq!(r.text(), "OK\n");
}

#[tokio::test]
async fn shexj_bodies_and_envelopes() {
    let (_dir, app) = server_with(PEOPLE, |_| {});
    let shexj = r#"{"@context": "http://www.w3.org/ns/shex.jsonld", "type": "Schema",
        "shapes": [{"id": "http://ex.org/Named", "type": "Shape", "expression":
          {"type": "TripleConstraint", "predicate": "http://xmlns.com/foaf/0.1/name"}}]}"#;
    let map = enc("<http://ex.org/bob>@<http://ex.org/Named>");
    for ct in ["application/shex+json", "application/json"] {
        let r = send(
            &app,
            post_as(&format!("/ds/shex?map={map}&format=smap"), ct, shexj),
        )
        .await;
        assert_eq!(r.status, StatusCode::OK, "{ct}: {}", r.text());
        assert_eq!(r.text(), "<http://ex.org/bob>@<http://ex.org/Named>\n");
    }
    // the envelope: the schema imports a body given with it, and an EXTERNAL shape is
    // defined by the externs
    let mut env = json!({
        "schema": "PREFIX ex: <http://ex.org/>\nIMPORT <http://ex.org/common>\nex:Knower { foaf:knows @ex:Named }",
        "imports": {"http://ex.org/common": "PREFIX ex: <http://ex.org/> PREFIX foaf: <http://xmlns.com/foaf/0.1/>
            ex:Named { foaf:name . }"},
        "map": [{"node": "http://ex.org/alice", "shape": "http://ex.org/Knower"},
                {"node": "http://ex.org/carol", "shape": "http://ex.org/Knower"}],
    });
    let r = send(
        &app,
        post_as("/ds/shex?format=smap", "application/json", &env.to_string()),
    )
    .await;
    // foaf: is not declared in the importing schema
    assert_eq!(r.status, StatusCode::BAD_REQUEST, "{}", r.text());
    assert_eq!(r.json()["line"], 3);
    env["schema"] = json!(
        "PREFIX ex: <http://ex.org/> PREFIX foaf: <http://xmlns.com/foaf/0.1/>
         IMPORT <http://ex.org/common>
         ex:Knower { foaf:knows @ex:Named ; ex:x @ex:Ext ? }
         ex:Ext EXTERNAL"
    );
    env["externs"] = json!("<http://ex.org/Ext> { }");
    let r = send(
        &app,
        post_as("/ds/shex?format=smap", "application/json", &env.to_string()),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    // alice knows bob and carol knows alice, both named
    assert_eq!(
        r.text(),
        "<http://ex.org/alice>@<http://ex.org/Knower>\n<http://ex.org/carol>@<http://ex.org/Knower>\n"
    );
    // without the externs, the EXTERNAL shape has no definition
    env.as_object_mut().unwrap().remove("externs");
    let r = send(
        &app,
        post_as("/ds/shex", "application/json", &env.to_string()),
    )
    .await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST, "{}", r.text());
    assert!(error_of(&r).contains("Ext"), "{}", r.text());
}

#[tokio::test]
async fn request_errors() {
    let (_dir, app) = server_with(PEOPLE, |st| st.limits.max_query_body_bytes = Some(4096));
    let node = enc("<http://ex.org/alice>");
    // a syntax error, with its position
    let r = send(
        &app,
        post(
            &format!("/ds/shex?node={node}"),
            "PREFIX ex: <http://ex.org/>\nex:S { ex:p @@ }",
        ),
    )
    .await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST, "{}", r.text());
    let j = r.json();
    assert_eq!(j["line"], 2, "{j}");
    assert!(j["column"].as_u64().is_some());
    assert!(
        error_of(&r).starts_with("schema syntax error at line 2"),
        "{j}"
    );
    // a shape map syntax error
    let r = send(
        &app,
        post(&format!("/ds/shex?map={}", enc("<x>@@")), PERSON),
    )
    .await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST, "{}", r.text());
    assert!(
        error_of(&r).starts_with("shape map syntax error"),
        "{}",
        r.text()
    );
    // a label the schema does not define; START without a start shape
    let shape = enc("<http://ex.org/Nope>");
    let r = send(
        &app,
        post(&format!("/ds/shex?node={node}&shape={shape}"), PERSON),
    )
    .await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST, "{}", r.text());
    assert!(error_of(&r).contains("http://ex.org/Nope"), "{}", r.text());
    let r = send(&app, post(&format!("/ds/shex?node={node}"), SCHEMA)).await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST, "{}", r.text());
    // a structure error
    let r = send(
        &app,
        post(
            &format!("/ds/shex?node={node}"),
            "start = @<http://ex.org/Missing>",
        ),
    )
    .await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST, "{}", r.text());
    // no shape map
    let r = send(&app, post("/ds/shex", PERSON)).await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST);
    assert!(error_of(&r).contains("no shape map"), "{}", r.text());
    // envelopes: unknown keys, a map twice
    let r = send(
        &app,
        post_as(
            "/ds/shex",
            "application/json",
            r#"{"schema": "", "mapp": "x"}"#,
        ),
    )
    .await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST);
    assert!(error_of(&r).contains("mapp"), "{}", r.text());
    let r = send(
        &app,
        post_as(
            &format!("/ds/shex?node={node}"),
            "application/json",
            &json!({"schema": PERSON, "map": "<x>@START"}).to_string(),
        ),
    )
    .await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST);
    assert!(error_of(&r).contains("not both"), "{}", r.text());
    // a body over --max-query-body-mb
    let big = format!("{PERSON}\n#{}", "x".repeat(5000));
    let r = send(&app, post(&format!("/ds/shex?node={node}"), &big)).await;
    assert_eq!(r.status, StatusCode::PAYLOAD_TOO_LARGE);
}

#[tokio::test]
async fn result_bytes_budget() {
    let (_dir, app) = server_with(PEOPLE, |st| st.limits.max_result_bytes = Some(60));
    let map = enc("{FOCUS a foaf:Person}@ex:Person");
    let r = send(&app, post(&format!("/ds/shex?map={map}"), PERSON)).await;
    assert_eq!(r.status, StatusCode::INSUFFICIENT_STORAGE, "{}", r.text());
    assert_eq!(r.json()["budget"], "result-bytes");
}

#[tokio::test]
async fn typing_pairs_fit_the_memory_budget() {
    // 64 bytes per pair: one pair; alice's typing needs bob's too
    let (_dir, app) = server_with(PEOPLE, |st| st.limits.query_memory_bytes = Some(64));
    let node = enc("<http://ex.org/alice>");
    let r = send(&app, post(&format!("/ds/shex?node={node}"), PERSON)).await;
    assert_eq!(r.status, StatusCode::INSUFFICIENT_STORAGE, "{}", r.text());
    assert_eq!(r.json()["budget"], "validation-work");
}

#[tokio::test]
async fn file_imports_need_the_load_directory() {
    let files = tempfile::tempdir().unwrap();
    std::fs::write(
        files.path().join("common.shex"),
        "<http://ex.org/Named> { <http://xmlns.com/foaf/0.1/name> . }",
    )
    .unwrap();
    let url = sparkles_shex::resolve::file_url(&files.path().join("common"));
    let schema = format!(
        "IMPORT <{url}> <http://ex.org/K> {{ <http://xmlns.com/foaf/0.1/knows> @<http://ex.org/Named> }}"
    );
    let map = enc("<http://ex.org/carol>@<http://ex.org/K>");
    let (_dir, closed) = server_with(PEOPLE, |_| {});
    let r = send(&closed, post(&format!("/ds/shex?map={map}"), &schema)).await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST, "{}", r.text());
    assert!(error_of(&r).contains("import not allowed"), "{}", r.text());
    let dir = files.path().to_path_buf();
    let (_dir, open) = server_with(PEOPLE, move |st| {
        st.file_loads = sparkles::sparql::FileLoads::under(&dir).unwrap()
    });
    let r = send(
        &open,
        post(&format!("/ds/shex?map={map}&format=smap"), &schema),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    assert_eq!(r.text(), "<http://ex.org/carol>@<http://ex.org/K>\n");
    // http(s) imports follow the outbound policy: loopback is refused by default
    let schema = "IMPORT <http://127.0.0.1:9/common> <http://ex.org/S> { }";
    let map = enc("<http://ex.org/a>@<http://ex.org/S>");
    let r = send(&open, post(&format!("/ds/shex?map={map}"), schema)).await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST, "{}", r.text());
    assert!(error_of(&r).contains("import not allowed"), "{}", r.text());
}

/// The imports of one validation share `--outbound-request-max-mb`: past it the request
/// answers 507 naming the budget, as SPARQL `LOAD` does.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn imports_share_the_outbound_request_budget() {
    use std::io::{Read, Write};
    let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = l.local_addr().unwrap().port();
    std::thread::spawn(move || {
        for c in l.incoming() {
            let Ok(mut c) = c else { continue };
            let _ = c.set_read_timeout(Some(Duration::from_millis(200)));
            let mut req = [0u8; 4096];
            let n = c.read(&mut req).unwrap_or(0);
            let path = String::from_utf8_lossy(&req[..n])
                .split_whitespace()
                .nth(1)
                .unwrap_or("/")
                .trim_start_matches('/')
                .to_string();
            // ~700 KB of comments, then one shape named after the path
            let mut body: String = (0..12_000).map(|i| format!("# {i:0>56}\n")).collect();
            body.push_str(&format!("<http://ex.org/{path}> {{ }}\n"));
            let _ = write!(
                c,
                "HTTP/1.1 200 OK\r\ncontent-type: text/shex\r\nconnection: close\r\n\r\n{body}"
            );
        }
    });
    let (_dir, app) = server_with(PEOPLE, |st| {
        st.outbound.allow_private = true;
        st.outbound.max_request_bytes = 1 << 20;
    });
    let schema = |imports: &[&str]| {
        let mut s: String = imports
            .iter()
            .map(|i| format!("IMPORT <http://127.0.0.1:{port}/{i}>\n"))
            .collect();
        s.push_str("<http://ex.org/S> { }");
        s
    };
    let map = enc("<http://ex.org/alice>@<http://ex.org/S>");
    // one import fits
    let r = send(
        &app,
        post(&format!("/ds/shex?map={map}&format=smap"), &schema(&["A"])),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    // two do not
    let r = send(
        &app,
        post(&format!("/ds/shex?map={map}"), &schema(&["A", "B"])),
    )
    .await;
    assert_eq!(r.status, StatusCode::INSUFFICIENT_STORAGE, "{}", r.text());
    let j = r.json();
    assert_eq!(j["budget"], "outbound-bytes");
    assert_eq!(j["limit"], 1 << 20);
}

// ------------------------------------------------- ShExR and SPARQL selectors ----

/// The data and schema of the acceptance examples.
const B1_DATA: &str = r#"
@prefix ex: <http://ex.org/> .
@prefix foaf: <http://xmlns.com/foaf/0.1/> .
ex:alice a ex:Person ; foaf:name "Alice" ; foaf:age 30 ; foaf:knows ex:bob .
ex:bob   a ex:Person ; foaf:name "Bob" ; foaf:knows ex:alice .
ex:carol a ex:Person ; foaf:age 200 .
ex:acme  a ex:Org ; foaf:name "ACME" ; ex:city "Paris" ; ex:mayor ex:bob .
"#;

const B1_SCHEMA: &str = "PREFIX ex: <http://ex.org/> PREFIX foaf: <http://xmlns.com/foaf/0.1/> \
PREFIX xsd: <http://www.w3.org/2001/XMLSchema#>
start = @ex:Person
ex:Person EXTRA a { a [ex:Person] ; foaf:name xsd:string ; foaf:age xsd:integer MAXINCLUSIVE 150 ? ; foaf:knows @ex:Person * }
ex:Org CLOSED { a [ex:Org] ; foaf:name . ; ex:city [\"Paris\" \"Kyoto\"] }";

const B1_MAP: &str = "{FOCUS a ex:Person}@ex:Person,ex:acme@ex:Org";

/// A report without its timing.
fn report(r: &Resp) -> J {
    let mut j = r.json();
    j.as_object_mut().unwrap().remove("millis");
    j
}

/// A report without the reasons (which use the schema's prefixes).
fn without_reasons(mut j: J) -> J {
    for x in j["results"].as_array_mut().unwrap() {
        x.as_object_mut().unwrap().remove("reason");
    }
    j
}

/// (node, status) of a report's results, in order.
fn verdicts(j: &J) -> Vec<(String, String)> {
    j["results"]
        .as_array()
        .unwrap()
        .iter()
        .map(|x| {
            (
                x["node"]["value"]
                    .as_str()
                    .unwrap()
                    .trim_start_matches("http://ex.org/")
                    .to_string(),
                x["status"].as_str().unwrap().to_string(),
            )
        })
        .collect()
}

#[tokio::test]
async fn shexr_bodies() {
    let (_dir, app) = server_with(B1_DATA, |_| {});
    let q = format!("/ds/shex?map={}", enc(B1_MAP));
    let b1 = send(&app, post(&q, B1_SCHEMA)).await;
    assert_eq!(b1.status, StatusCode::OK, "{}", b1.text());
    let expected = report(&b1);
    assert_eq!(expected["counts"]["conformant"], 2);
    assert_eq!(expected["counts"]["nonconformant"], 2);
    let schema = sparkles_shex::Schema::parse_shexc(B1_SCHEMA, None).unwrap();
    let turtle = schema.to_shexr_turtle();
    let nt = sparkles_shex::shexr::to_text(&schema, RdfFormat::NTriples);
    let xml = sparkles_shex::shexr::to_text(&schema, RdfFormat::RdfXml);
    // the Turtle's prefixes are the schema's, for the shape map; N-Triples has none
    let full = format!(
        "/ds/shex?map={}",
        enc("{FOCUS a <http://ex.org/Person>}@<http://ex.org/Person>,\
             <http://ex.org/acme>@<http://ex.org/Org>")
    );
    for ct in [
        "text/turtle",
        "text/turtle; charset=utf-8",
        "application/trig",
    ] {
        let r = send(&app, post_as(&q, ct, &turtle)).await;
        assert_eq!(r.status, StatusCode::OK, "{ct}: {}", r.text());
        assert_eq!(report(&r), expected, "{ct}");
    }
    for (ct, body) in [
        ("application/n-triples", &nt),
        ("application/n-quads", &nt),
        ("application/rdf+xml", &xml),
    ] {
        let r = send(&app, post_as(&full, ct, body)).await;
        assert_eq!(r.status, StatusCode::OK, "{ct}: {}", r.text());
        assert_eq!(
            without_reasons(report(&r)),
            without_reasons(expected.clone()),
            "{ct}"
        );
    }
    let r = send(&app, post_as(&q, "application/n-triples", &nt)).await;
    assert!(error_of(&r).contains("undefined prefix"), "{}", r.text());
    // schema-format names the syntax of a body the media type does not
    for (f, body) in [("shexr", turtle.as_str()), ("shexc", B1_SCHEMA)] {
        let r = send(
            &app,
            post_as(&format!("{q}&schema-format={f}"), "text/plain", body),
        )
        .await;
        assert_eq!(r.status, StatusCode::OK, "{f}: {}", r.text());
        assert_eq!(report(&r), expected, "{f}");
    }
    let r = send(
        &app,
        post_as(
            &format!("{full}&schema-format=shexr"),
            "application/n-triples",
            &nt,
        ),
    )
    .await;
    assert_eq!(
        without_reasons(report(&r)),
        without_reasons(expected.clone())
    );
    // the envelope's schemaFormat
    let env = json!({"schema": turtle, "schemaFormat": "shexr", "map": B1_MAP});
    let r = send(
        &app,
        post_as("/ds/shex", "application/json", &env.to_string()),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    assert_eq!(report(&r), expected);

    // errors: an RDF syntax error has its place; a ShExR error has none
    let r = send(
        &app,
        post_as(
            &q,
            "text/turtle",
            "PREFIX sx: <http://www.w3.org/ns/shex#>\n[] a sx:Schema ;;",
        ),
    )
    .await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST, "{}", r.text());
    assert_eq!(r.json()["line"], 2, "{}", r.text());
    let r = send(
        &app,
        post_as(
            &q,
            "text/turtle",
            "<http://ex.org/a> <http://ex.org/b> <http://ex.org/c> .",
        ),
    )
    .await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST, "{}", r.text());
    let j = r.json();
    assert!(j.get("line").is_none() && j.get("column").is_none(), "{j}");
    assert_eq!(
        j["error"],
        "schema error: ShExR: no node has type sx:Schema"
    );
    let r = send(
        &app,
        post_as(&format!("{q}&schema-format=rdf"), "text/turtle", &turtle),
    )
    .await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST);
    assert!(
        error_of(&r).contains("unknown schema-format"),
        "{}",
        r.text()
    );
}

#[test]
fn schema_media_types() {
    use super::enabled::schema_hint;
    use sparkles_shex::SchemaFormat as F;
    let turtle = Some(F::ShExR(RdfFormat::Turtle));
    assert_eq!(schema_hint("text/shex", None), Some(F::ShExC));
    for ct in [
        "application/shex+json",
        "application/json",
        "application/ld+json",
    ] {
        assert_eq!(schema_hint(ct, None), Some(F::ShExJ), "{ct}");
    }
    assert_eq!(schema_hint("text/turtle", None), turtle);
    assert_eq!(
        schema_hint("application/n-triples", None),
        Some(F::ShExR(RdfFormat::NTriples))
    );
    assert_eq!(
        schema_hint("application/rdf+xml", None),
        Some(F::ShExR(RdfFormat::RdfXml))
    );
    for ct in ["text/plain", "", "application/x-www-form-urlencoded"] {
        assert_eq!(schema_hint(ct, None), None, "{ct}");
    }
    assert_eq!(schema_hint("text/plain", turtle), turtle);
    assert_eq!(
        schema_hint("application/n-triples", turtle),
        Some(F::ShExR(RdfFormat::NTriples))
    );
    assert_eq!(schema_hint("text/turtle", Some(F::ShExC)), Some(F::ShExC));
}

#[tokio::test]
async fn sparql_selectors() {
    let (_dir, app) = server_with(B1_DATA, |_| {});
    let b1 = send(
        &app,
        post(&format!("/ds/shex?map={}", enc(B1_MAP)), B1_SCHEMA),
    )
    .await;
    let expected = report(&b1);
    let v = |n: &str, s: &str| (n.to_string(), s.to_string());
    assert_eq!(
        verdicts(&expected),
        [
            v("alice", "conformant"),
            v("bob", "conformant"),
            v("carol", "nonconformant"),
            v("acme", "nonconformant")
        ]
    );
    // the query has no prefixes but its own
    let map = r#"SPARQL """SELECT ?focus { ?focus a <http://ex.org/Person> }"""@ex:Person,ex:acme@ex:Org"#;
    let r = send(&app, post(&format!("/ds/shex?map={}", enc(map)), B1_SCHEMA)).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    assert_eq!(report(&r), expected);
    // ?focus, else the first projected variable
    let map = "SPARQL 'PREFIX ex: <http://ex.org/> SELECT ?x ?y { ?x ex:mayor ?y }'@ex:Org";
    let r = send(
        &app,
        post(&format!("/ds/shex?map={}&format=smap", enc(map)), B1_SCHEMA),
    )
    .await;
    assert_eq!(r.text(), "<http://ex.org/acme>@!<http://ex.org/Org>\n");
    // counted after the expansion: the nonconformant ones, and the counts of all
    let map = "SPARQL 'SELECT ?s { ?s a ?c } ORDER BY ?s'@START";
    let r = send(
        &app,
        post(
            &format!("/ds/shex?map={}&results=nonconformant", enc(map)),
            B1_SCHEMA,
        ),
    )
    .await;
    let j = r.json();
    assert_eq!(j["counts"]["conformant"], 2, "{j}");
    assert_eq!(j["counts"]["nonconformant"], 2, "{j}");
    assert_eq!(
        verdicts(&j),
        [v("acme", "nonconformant"), v("carol", "nonconformant")]
    );
    // a JSON shape map in the envelope
    let env = json!({"schema": B1_SCHEMA, "map": [
        {"node": "SPARQL \"\"\"SELECT ?focus { ?focus a <http://ex.org/Person> }\"\"\"",
         "shape": "http://ex.org/Person"},
        {"node": "http://ex.org/acme", "shape": "http://ex.org/Org"}]});
    let r = send(
        &app,
        post_as("/ds/shex", "application/json", &env.to_string()),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    assert_eq!(report(&r), expected);

    // checked when the map is parsed: not SELECT, SERVICE, a prefix of the map
    for (map, needle) in [
        (
            "ex:alice@ex:Person,\nSPARQL 'ASK {}'@ex:Person",
            "SELECT query",
        ),
        (
            "SPARQL 'SELECT ?x { SERVICE <http://ex.org/sparql> { ?x ?p ?o } }'@ex:Person",
            "SERVICE is not allowed",
        ),
        (
            "SPARQL 'SELECT ?x { ?x a ex:Person }'@ex:Person",
            "invalid SPARQL selector",
        ),
    ] {
        let r = send(&app, post(&format!("/ds/shex?map={}", enc(map)), B1_SCHEMA)).await;
        assert_eq!(r.status, StatusCode::BAD_REQUEST, "{map}: {}", r.text());
        let j = r.json();
        assert!(error_of(&r).contains(needle), "{map}: {j}");
        assert!(
            j["line"].as_u64().is_some() && j["column"].as_u64().is_some(),
            "{j}"
        );
    }
    let map = "ex:alice@ex:Person,\nSPARQL 'ASK {}'@ex:Person";
    let r = send(&app, post(&format!("/ds/shex?map={}", enc(map)), B1_SCHEMA)).await;
    let j = r.json();
    assert_eq!((&j["line"], &j["column"]), (&json!(2), &json!(1)), "{j}");
}

#[tokio::test]
async fn sparql_selectors_have_the_query_budgets() {
    // --max-query-rows
    let (_dir, app) = server_with(B1_DATA, |st| st.limits.max_rows = 2);
    let map = "SPARQL 'SELECT ?focus { ?focus a <http://ex.org/Person> }'@ex:Person";
    let r = send(&app, post(&format!("/ds/shex?map={}", enc(map)), B1_SCHEMA)).await;
    assert_eq!(r.status, StatusCode::INSUFFICIENT_STORAGE, "{}", r.text());
    assert!(r.json()["budget"].is_string(), "{}", r.text());
    // the report's size counts the selected nodes
    let (_dir, app) = server_with(B1_DATA, |st| st.limits.max_result_bytes = Some(60));
    let r = send(&app, post(&format!("/ds/shex?map={}", enc(map)), B1_SCHEMA)).await;
    assert_eq!(r.status, StatusCode::INSUFFICIENT_STORAGE, "{}", r.text());
    assert_eq!(r.json()["budget"], "result-bytes");
    // the validation's timeout covers the selector: a long chain's closure
    let chain: String = (0..2000)
        .map(|i| {
            format!(
                "<http://ex.org/n{i}> <http://ex.org/next> <http://ex.org/n{}> .\n",
                i + 1
            )
        })
        .collect();
    let (_dir, app) = server_with(&chain, |_| {});
    let map = "SPARQL 'SELECT ?focus { ?focus <http://ex.org/next>* ?o }'@<http://ex.org/S>";
    let r = send(
        &app,
        post(
            &format!("/ds/shex?map={}&timeout=0.001", enc(map)),
            "<http://ex.org/S> { }",
        ),
    )
    .await;
    assert_eq!(r.status, StatusCode::REQUEST_TIMEOUT, "{}", r.text());
}
