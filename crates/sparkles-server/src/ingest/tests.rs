//! `POST /$/ingest/{ds}` and its task routes against mock providers (spec C18 Phase 4:
//! A9's second half, A18, A54 to A56, confirmation, the review modes and cancellation).

use super::fixtures;
use crate::models::mock::{self, MockModel, Received};
use crate::models::{Models, ModelsConfig};
use crate::state::{AppState, DbType};
use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use serde_json::{Value, json};
use sparkles::io::{RdfFormat, Source};
use std::sync::Arc;
use std::time::Duration;
use tower::ServiceExt;

const ORG: &str = r#"@prefix ex:     <http://example.org/> .
@prefix schema: <http://schema.org/> .
@prefix org:    <http://www.w3.org/ns/org#> .
@prefix rdfs:   <http://www.w3.org/2000/01/rdf-schema#> .
<https://example.org/hr> {
  ex:ana a schema:Person ; rdfs:label "Ana Lima"@en .
  ex:kai a schema:Person ; rdfs:label "Kai Ito"@en ; org:memberOf ex:payments .
  ex:payments a org:OrganizationalUnit ; rdfs:label "Payments team"@en ; org:unitOf ex:acme .
  ex:acme a org:Organization ; rdfs:label "Acme Corp"@en .
}
"#;

const MEMBER_OF: &str = "http://www.w3.org/ns/org#memberOf";

/// A server with `org`, and the providers `cheap` and `top` at `urls` when given.
fn app(urls: Option<[&str; 2]>) -> (Arc<AppState>, Router) {
    let mut st = AppState::standalone(Default::default(), Duration::from_secs(30));
    if let Some(urls) = urls {
        let mut providers = serde_json::Map::new();
        for (name, url) in ["cheap", "top"].into_iter().zip(urls) {
            providers.insert(
                name.into(),
                json!({ "kind": "openai", "endpoint": url, "requestTimeoutSecs": 5,
                        "allowedModels": ["m"], "structuredOutput": "json-schema",
                        "pricing": { "inputPerMTok": 1.0, "outputPerMTok": 2.0 } }),
            );
        }
        let cfg = json!({ "providers": providers, "roles": {
            "extract": [{ "provider": "cheap", "model": "m" }, { "provider": "top", "model": "m" }]
        }});
        st.models = Some(Arc::new(Models::new(
            ModelsConfig::parse(&cfg.to_string()).unwrap(),
            Default::default(),
            sparkles::outbound::OutboundPolicy {
                allow_private: true,
                ..Default::default()
            },
        )));
    }
    let st = Arc::new(st);
    let ds = st.attach("org", DbType::Mem, None).unwrap();
    ds.store
        .load(&[Source::from_bytes(
            ORG.as_bytes().to_vec(),
            RdfFormat::TriG,
            None,
        )])
        .unwrap();
    let router = crate::http::router(st.clone());
    (st, router)
}

async fn send(app: &Router, req: Request<Body>) -> (StatusCode, Value) {
    let res = app.clone().oneshot(req).await.unwrap();
    let s = res.status();
    let b = axum::body::to_bytes(res.into_body(), usize::MAX)
        .await
        .unwrap();
    let v = serde_json::from_slice(&b)
        .unwrap_or_else(|_| Value::String(String::from_utf8_lossy(&b).into_owned()));
    (s, v)
}

fn req(method: &str, uri: &str, body: Value) -> Request<Body> {
    Request::builder()
        .method(method)
        .uri(uri)
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(body.to_string()))
        .unwrap()
}

/// A multipart part: field, file name, content type and bytes.
type Part<'a> = (&'a str, Option<&'a str>, Option<&'a str>, &'a [u8]);

/// A multipart body of `parts`.
fn multipart(uri: &str, parts: &[Part]) -> Request<Body> {
    let b = "sparkles-ingest-boundary";
    let mut body: Vec<u8> = Vec::new();
    for (name, file, ct, content) in parts {
        body.extend_from_slice(
            format!("--{b}\r\nContent-Disposition: form-data; name=\"{name}\"").as_bytes(),
        );
        if let Some(f) = file {
            body.extend_from_slice(format!("; filename=\"{f}\"").as_bytes());
        }
        if let Some(c) = ct {
            body.extend_from_slice(format!("\r\nContent-Type: {c}").as_bytes());
        }
        body.extend_from_slice(b"\r\n\r\n");
        body.extend_from_slice(content);
        body.extend_from_slice(b"\r\n");
    }
    body.extend_from_slice(format!("--{b}--\r\n").as_bytes());
    Request::post(uri)
        .header(
            header::CONTENT_TYPE,
            format!("multipart/form-data; boundary={b}"),
        )
        .body(Body::from(body))
        .unwrap()
}

/// Let ingestion use the providers, which may receive documents.
async fn enable(app: &Router) {
    let (s, v) = send(
        app,
        req(
            "PUT",
            "/$/assistant/org",
            json!({ "enabled": true, "ingest": true, "send": "documents" }),
        ),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{v}");
}

/// Start a task and wait until it ends or waits.
async fn ingest(app: &Router, r: Request<Body>) -> Value {
    let (s, v) = send(app, r).await;
    assert_eq!(s, StatusCode::ACCEPTED, "{v}");
    let id = v["id"].as_str().unwrap().to_string();
    wait(app, &id).await
}

async fn wait(app: &Router, id: &str) -> Value {
    let (s, v) = send(
        app,
        req("GET", &format!("/$/ingest/org/{id}?wait=30"), Value::Null),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    v
}

fn pdf_upload(bytes: &[u8], extra: &[(&str, &str)]) -> Request<Body> {
    let mut parts: Vec<Part> = extra
        .iter()
        .map(|(k, v)| (*k, None, None, v.as_bytes()))
        .collect();
    parts.push(("file", Some("report.pdf"), Some("application/pdf"), bytes));
    multipart("/$/ingest/org", &parts)
}

/// The first enumeration value of the request's schema at `path` containing `needle`.
fn enum_value(r: &Received, path: &[&str], needle: &str) -> String {
    let mut s = &r.body["response_format"]["json_schema"]["schema"];
    for p in path {
        s = &s[*p];
    }
    s["enum"]
        .as_array()
        .unwrap_or_else(|| panic!("no enum at {path:?}: {}", r.body))
        .iter()
        .filter_map(Value::as_str)
        .find(|v| v.contains(needle))
        .unwrap_or_else(|| panic!("no {needle} in {s}"))
        .to_string()
}

const FACT_P: [&str; 6] = ["properties", "facts", "items", "properties", "p", ""];
const MENTION_TYPE: [&str; 6] = ["properties", "mentions", "items", "properties", "type", ""];

/// The extraction of the report and the notes: who is a member of which team, with
/// the quote that says so.
fn extraction(r: &Received) -> String {
    // every message: a retry's last message is the correction, not the text
    let prompt: String = r.body["messages"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|m| m["content"].as_str())
        .collect::<Vec<_>>()
        .join("\n");
    let p = enum_value(r, &FACT_P[..5], "memberOf");
    let person = enum_value(r, &MENTION_TYPE[..5], "Person");
    let unit = enum_value(r, &MENTION_TYPE[..5], "OrganizationalUnit");
    let mut mentions = Vec::new();
    let mut facts = Vec::new();
    let text = prompt.split("Text:\n<<<\n").nth(1).unwrap_or("");
    for (i, (who, team, quote)) in [
        (
            "Ana Lima",
            "payments team",
            "Ana Lima moved to the payments team in October.",
        ),
        (
            "Kai Berg",
            "platform team",
            "Kai Berg leads the platform team.",
        ),
        (
            "Ana Lima",
            "payments team",
            "Ana Lima moved to the payments team this week.",
        ),
    ]
    .into_iter()
    .enumerate()
    {
        if !text.contains(quote) {
            continue;
        }
        mentions.push(json!({ "key": format!("p{i}"), "text": who, "type": person }));
        mentions.push(json!({ "key": format!("t{i}"), "text": team, "type": unit }));
        facts.push(
            json!({ "s": format!("p{i}"), "p": p, "o": format!("t{i}"), "literal": "",
            "datatype": "", "lang": "", "quote": quote, "confidence": 0.9 }),
        );
    }
    json!({ "mentions": mentions, "facts": facts }).to_string()
}

fn proposals_mock() -> MockModel {
    MockModel::start(|r, _| (200, mock::openai(&extraction(r))))
}

async fn review(app: &Router, branch: &str) -> Value {
    let (s, v) = send(
        app,
        req(
            "GET",
            &format!("/$/memory/org/review/{branch}"),
            Value::Null,
        ),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    v
}

async fn ask(app: &Router, q: &str) -> bool {
    let r = Request::post("/org/sparql")
        .header(header::CONTENT_TYPE, "application/sparql-query")
        .header(header::ACCEPT, "application/sparql-results+json")
        .body(Body::from(q.to_string()))
        .unwrap();
    let (s, v) = send(app, r).await;
    assert_eq!(s, StatusCode::OK, "{v}");
    v["boolean"].as_bool().unwrap()
}

const NOTES: &str =
    "# Stand-up\n\nAna Lima moved to the payments team this week.\n\nThe review is on Friday.\n";

#[tokio::test]
async fn markdown_without_a_provider_registers_on_a_branch() {
    let (_st, app) = app(None);
    let v = ingest(
        &app,
        req(
            "POST",
            "/$/ingest/org",
            json!({ "text": NOTES, "format": "text/markdown", "title": "Stand-up" }),
        ),
    )
    .await;
    assert_eq!(v["status"], "done", "{v}");
    let r = &v["result"];
    assert_eq!(r["outcome"], "registered", "{v}");
    assert_eq!(r["format"], "markdown");
    assert_eq!(r["branch"], "ingest.stand-up-1");
    assert!(r["notes"][0].as_str().unwrap().contains("no fact"), "{v}");
    let rv = review(&app, "ingest.stand-up-1").await;
    assert_eq!(rv["sources"][0]["text"], NOTES, "{rv}");
    // the same text again writes nothing
    let v = ingest(
        &app,
        req(
            "POST",
            "/$/ingest/org",
            json!({ "text": NOTES, "format": "text/markdown", "title": "Stand-up",
                    "branch": "ingest.stand-up-1" }),
        ),
    )
    .await;
    assert_eq!(v["result"]["outcome"], "already-registered", "{v}");
    // the task list and a task that is not there
    let (s, l) = send(&app, req("GET", "/$/ingest/org", Value::Null)).await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(l["tasks"].as_array().unwrap().len(), 2, "{l}");
    assert_eq!(l["capabilities"]["pdf"], cfg!(feature = "pdf"));
    let (s, _) = send(
        &app,
        req("GET", "/$/ingest/org/0123456789abcdef", Value::Null),
    )
    .await;
    assert_eq!(s, StatusCode::NOT_FOUND);
    // refusals before any task
    let (s, e) = send(&app, req("POST", "/$/ingest/org", json!({}))).await;
    assert_eq!(s, StatusCode::BAD_REQUEST, "{e}");
    let (s, e) = send(
        &app,
        req(
            "POST",
            "/$/ingest/org",
            json!({ "text": "x", "mode": "later" }),
        ),
    )
    .await;
    assert_eq!(s, StatusCode::BAD_REQUEST, "{e}");
    let (s, _) = send(&app, req("POST", "/$/ingest/nope", json!({ "text": "x" }))).await;
    assert_eq!(s, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn html_upload_keeps_the_main_content() {
    let (_st, app) = app(None);
    let html = "<html><head><title>Payments</title></head><body><nav>Home</nav><main><h1>Payments</h1><p>Ana Lima moved to the payments team.</p></main></body></html>";
    let v = ingest(
        &app,
        multipart(
            "/$/ingest/org",
            &[(
                "file",
                Some("team.html"),
                Some("text/html"),
                html.as_bytes(),
            )],
        ),
    )
    .await;
    assert_eq!(v["status"], "done", "{v}");
    assert_eq!(v["result"]["format"], "html");
    let b = v["result"]["branch"].as_str().unwrap().to_string();
    assert_eq!(b, "ingest.payments-1");
    let rv = review(&app, &b).await;
    assert_eq!(
        rv["sources"][0]["text"],
        "# Payments\n\nAna Lima moved to the payments team.\n"
    );
    assert_eq!(rv["sources"][0]["title"], "Payments");
    // a format that is not an input
    let v = ingest(
        &app,
        req(
            "POST",
            "/$/ingest/org",
            json!({ "text": "<a> <b> <c> .", "format": "text/turtle" }),
        ),
    )
    .await;
    assert_eq!(v["status"], "failed");
    assert_eq!(v["error"]["code"], "unsupported-format", "{v}");
}

/// A54: a born-digital PDF of three pages, with a fact on page 2.
#[cfg(feature = "pdf")]
#[tokio::test]
async fn a54_pdf_pages_and_spans() {
    let m = proposals_mock();
    let (_st, app) = app(Some([&m.url(), &m.url()]));
    enable(&app).await;
    let v = ingest(&app, pdf_upload(&fixtures::report(), &[])).await;
    assert_eq!(v["status"], "done", "{v}");
    let r = &v["result"];
    assert_eq!(r["outcome"], "proposed", "{v}");
    assert_eq!(r["pages"].as_array().unwrap().len(), 3, "{v}");
    assert_eq!(r["proposed"], 2, "{v}");
    assert_eq!(
        r["factsByPage"],
        json!([{ "page": 1, "facts": 1 }, { "page": 2, "facts": 1 }])
    );
    assert_eq!(v["usage"]["chunks"][0]["provider"], "cheap", "{v}");
    assert!(v["estimate"]["tokens"].as_u64().unwrap() > 0, "{v}");
    assert!(
        v["estimate"]["estimatedCost"].as_f64().unwrap() > 0.0,
        "{v}"
    );
    let b = r["branch"].as_str().unwrap();
    let rv = review(&app, b).await;
    let src = &rv["sources"][0];
    let text = src["text"].as_str().unwrap();
    for n in 1..=3 {
        assert!(text.contains(&format!("<!-- Page {n} -->")), "{text}");
    }
    let pages = src["pages"].as_array().unwrap();
    assert_eq!(pages.len(), 3, "{src}");
    assert_eq!(pages[1]["page"], 2);
    let starts: Vec<u64> = pages.iter().map(|p| p["start"].as_u64().unwrap()).collect();
    // the fact of page 2 passes the span check and lies on page 2
    let kai = rv["facts"]
        .as_array()
        .unwrap()
        .iter()
        .find(|f| {
            f["quote"]
                .as_str()
                .is_some_and(|q| q.starts_with("Kai Berg"))
        })
        .unwrap_or_else(|| panic!("{rv}"));
    assert_eq!(kai["signals"]["span"], "pass", "{kai}");
    let start = kai["span"]["start"].as_u64().unwrap();
    assert!(
        start >= starts[1] && start < starts[2],
        "{start} {starts:?}"
    );
    // nothing reaches main before the merge
    assert!(
        !ask(
            &app,
            &format!("ASK {{ GRAPH ?g {{ <http://example.org/ana> <{MEMBER_OF}> ?t }} }}")
        )
        .await
    );
}

/// A55: without OCR, scanned pages are refused with their pages and reasons, and
/// `allowPartial` registers the other pages.
#[cfg(feature = "pdf")]
#[tokio::test]
async fn a55_needs_ocr() {
    let (st, app) = app(None);
    let v = ingest(&app, pdf_upload(&fixtures::scanned(), &[])).await;
    assert_eq!(v["status"], "failed", "{v}");
    assert_eq!(v["error"]["code"], "needs-ocr", "{v}");
    let pages: Vec<u64> = v["error"]["pages"]
        .as_array()
        .unwrap()
        .iter()
        .map(|p| p["page"].as_u64().unwrap())
        .collect();
    assert_eq!(pages, [1, 2]);
    // no source, no branch
    let ds = st.get("org").unwrap();
    assert!(
        ds.store
            .branches()
            .unwrap()
            .iter()
            .all(|b| !b.name.starts_with("ingest."))
    );
    let v = ingest(&app, pdf_upload(&fixtures::mixed(), &[])).await;
    assert_eq!(v["error"]["code"], "needs-ocr", "{v}");
    assert_eq!(
        v["error"]["pages"],
        json!([{ "page": 3, "reasons": ["scanned"] }]),
        "{v}"
    );
    let v = ingest(
        &app,
        pdf_upload(&fixtures::mixed(), &[("allowPartial", "true")]),
    )
    .await;
    assert_eq!(v["status"], "done", "{v}");
    assert_eq!(v["result"]["omittedPages"], json!([3]), "{v}");
    let rv = review(&app, v["result"]["branch"].as_str().unwrap()).await;
    let src = &rv["sources"][0];
    assert_eq!(src["omittedPages"], json!([3]), "{src}");
    let pages: Vec<u64> = src["pages"]
        .as_array()
        .unwrap()
        .iter()
        .map(|p| p["page"].as_u64().unwrap())
        .collect();
    assert_eq!(pages, [1, 2, 4]);
}

/// A56: the same PDF gives the same text, digest and rendition.
#[cfg(feature = "pdf")]
#[tokio::test]
async fn a56_conversion_is_deterministic() {
    let o = super::convert::Options {
        allow_partial: false,
        max_input: super::convert::MAX_INPUT_BYTES,
        max_text: super::convert::MAX_TEXT_BYTES,
        deadline: std::time::Instant::now() + Duration::from_secs(30),
        pdf: Arc::default(),
    };
    let pdf = fixtures::report();
    let a = super::convert::convert(&pdf, super::convert::Format::Pdf, &o).unwrap();
    let b = super::convert::convert(&pdf, super::convert::Format::Pdf, &o).unwrap();
    assert_eq!(a, b);
    assert!(a.text.starts_with("<!-- Page 1 -->"), "{}", a.text);
    let (_st, app) = app(None);
    let one = ingest(&app, pdf_upload(&pdf, &[])).await;
    let b1 = one["result"]["branch"].as_str().unwrap().to_string();
    let two = ingest(&app, pdf_upload(&pdf, &[("branch", &b1)])).await;
    assert_eq!(two["result"]["outcome"], "already-registered", "{two}");
    assert_eq!(two["result"]["digest"], one["result"]["digest"]);
    assert_eq!(two["result"]["rendition"], one["result"]["rendition"]);
}

/// With OCR configured, pages that need it go to OCR, never to a refusal: models that
/// are not there fail with `ocr-failed`, and nothing is downloaded.
#[cfg(feature = "pdf-ocr")]
#[test]
fn ocr_without_models_fails() {
    let dir = tempfile::tempdir().unwrap();
    let rt = super::PdfRuntime::new(
        1,
        Some(super::OcrConfig {
            models: dir.path().join("none"),
            pdfium: None,
        }),
    );
    let o = super::convert::Options {
        allow_partial: false,
        max_input: super::convert::MAX_INPUT_BYTES,
        max_text: super::convert::MAX_TEXT_BYTES,
        deadline: std::time::Instant::now() + Duration::from_secs(30),
        pdf: Arc::new(rt),
    };
    let e =
        super::convert::convert(&fixtures::scanned(), super::convert::Format::Pdf, &o).unwrap_err();
    assert_eq!(e.code, "ocr-failed", "{e:?}");
    assert!(std::fs::read_dir(dir.path()).unwrap().next().is_none());
}

/// A56: a build without the `pdf` feature refuses a PDF.
#[cfg(not(feature = "pdf"))]
#[tokio::test]
async fn a56_without_pdf_feature() {
    let (_st, app) = app(None);
    let v = ingest(&app, pdf_upload(&fixtures::report(), &[])).await;
    assert_eq!(v["error"]["code"], "unsupported-format", "{v}");
}

/// A9, second half: the structured output's predicates are the profile's, so
/// `ex:leads` cannot be proposed; an answer that uses it fails validation, is retried,
/// and the chunk moves to the next pair.
#[tokio::test]
async fn a9_profile_enum_and_escalation() {
    let bad = MockModel::start(|r, _| {
        let mut v: Value = serde_json::from_str(&extraction(r)).unwrap();
        for f in v["facts"].as_array_mut().unwrap() {
            f["p"] = "ex:leads".into();
        }
        (200, mock::openai(&v.to_string()))
    });
    let good = proposals_mock();
    let (_st, app) = app(Some([&bad.url(), &good.url()]));
    enable(&app).await;
    let v = ingest(
        &app,
        req(
            "POST",
            "/$/ingest/org",
            json!({ "text": NOTES, "format": "text/markdown", "title": "Notes" }),
        ),
    )
    .await;
    assert_eq!(v["status"], "done", "{v}");
    let schema = &bad.requests()[0].body["response_format"]["json_schema"]["schema"];
    let preds = schema["properties"]["facts"]["items"]["properties"]["p"]["enum"]
        .as_array()
        .unwrap();
    assert!(!preds.is_empty());
    assert!(
        preds.iter().all(|p| !p.as_str().unwrap().contains("leads")),
        "{preds:?}"
    );
    assert_eq!(bad.requests().len(), 2, "one retry, then the next pair");
    let esc = &v["usage"]["escalations"][0];
    assert_eq!(esc["signal"], "invalid-output", "{v}");
    assert_eq!(esc["to"]["provider"], "top");
    assert_eq!(v["usage"]["chunks"][0]["provider"], "top");
    assert_eq!(v["result"]["proposed"], 1, "{v}");
    // Ana is linked to the existing IRI
    assert_eq!(v["result"]["entities"]["linked"], 1, "{v}");
    let rv = review(&app, v["result"]["branch"].as_str().unwrap()).await;
    assert!(!rv.to_string().contains("leads"));
}

/// The estimate above the dataset's threshold waits for a confirmation; a cancel ends
/// a waiting task.
#[tokio::test]
async fn confirmation_and_cancel() {
    let m = proposals_mock();
    let (_st, app) = app(Some([&m.url(), &m.url()]));
    enable(&app).await;
    let (s, v) = send(
        &app,
        req(
            "PUT",
            "/$/ingest/org/settings",
            json!({ "confirmTokens": 10 }),
        ),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    let body = json!({ "text": NOTES, "format": "text/markdown", "title": "Notes" });
    let v = ingest(&app, req("POST", "/$/ingest/org", body.clone())).await;
    assert_eq!(v["status"], "awaiting-confirmation", "{v}");
    assert_eq!(v["estimate"]["needsConfirmation"], true);
    assert!(m.requests().is_empty(), "no call before the confirmation");
    let id = v["id"].as_str().unwrap();
    let (s, c) = send(
        &app,
        req("POST", &format!("/$/ingest/org/{id}/confirm"), Value::Null),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{c}");
    let v = wait(&app, id).await;
    assert_eq!(v["status"], "done", "{v}");
    assert_eq!(m.requests().len(), 1);
    // a second confirmation is refused
    let (s, _) = send(
        &app,
        req("POST", &format!("/$/ingest/org/{id}/confirm"), Value::Null),
    )
    .await;
    assert_eq!(s, StatusCode::CONFLICT);
    // cancel while waiting
    let mut b2 = body.clone();
    b2["title"] = "Other".into();
    b2["text"] = format!("{NOTES}\nMore.\n").into();
    let v = ingest(&app, req("POST", "/$/ingest/org", b2)).await;
    assert_eq!(v["status"], "awaiting-confirmation", "{v}");
    let id = v["id"].as_str().unwrap();
    let (s, _) = send(
        &app,
        req("DELETE", &format!("/$/ingest/org/{id}"), Value::Null),
    )
    .await;
    assert_eq!(s, StatusCode::ACCEPTED);
    let v = wait(&app, id).await;
    assert_eq!(v["status"], "cancelled", "{v}");
    // a finished task is forgotten
    let (s, _) = send(
        &app,
        req("DELETE", &format!("/$/ingest/org/{id}"), Value::Null),
    )
    .await;
    assert_eq!(s, StatusCode::NO_CONTENT);
    let (s, _) = send(
        &app,
        req("GET", &format!("/$/ingest/org/{id}"), Value::Null),
    )
    .await;
    assert_eq!(s, StatusCode::NOT_FOUND);
    // confirm in advance
    let mut b3 = body.clone();
    b3["confirm"] = true.into();
    b3["text"] = format!("{NOTES}\nAgain.\n").into();
    let v = ingest(&app, req("POST", "/$/ingest/org", b3)).await;
    assert_eq!(v["status"], "done", "{v}");
}

/// `preview` writes nothing until the approval, which writes to `main` unless `main`
/// moved; `auto` merges what passes every check.
#[tokio::test]
async fn preview_approve_and_auto() {
    let m = proposals_mock();
    let (_st, app) = app(Some([&m.url(), &m.url()]));
    enable(&app).await;
    let q = format!("ASK {{ GRAPH ?g {{ <http://example.org/ana> <{MEMBER_OF}> ?t }} }}");
    let v = ingest(
        &app,
        req(
            "POST",
            "/$/ingest/org",
            json!({ "text": NOTES, "format": "text/markdown", "title": "Notes", "mode": "preview" }),
        ),
    )
    .await;
    assert_eq!(v["status"], "awaiting-approval", "{v}");
    assert_eq!(v["result"]["outcome"], "preview");
    assert!(v["result"]["branch"].is_null());
    assert_eq!(v["result"]["proposed"], 1, "{v}");
    assert!(!ask(&app, &q).await);
    let id = v["id"].as_str().unwrap();
    let (s, a) = send(
        &app,
        req("POST", &format!("/$/ingest/org/{id}/approve"), Value::Null),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{a}");
    assert_eq!(a["status"], "done");
    assert!(ask(&app, &q).await);
    // a preview whose base moved is refused
    let other = "# Other\n\nKai Berg leads the platform team.\n";
    let v = ingest(
        &app,
        req(
            "POST",
            "/$/ingest/org",
            json!({ "text": other, "format": "text/markdown", "mode": "preview" }),
        ),
    )
    .await;
    assert_eq!(v["status"], "awaiting-approval", "{v}");
    let id = v["id"].as_str().unwrap();
    let r = Request::post("/org/update")
        .header(header::CONTENT_TYPE, "application/sparql-update")
        .body(Body::from("INSERT DATA { <http://e/x> <http://e/y> 1 }"))
        .unwrap();
    let (s, _) = send(&app, r).await;
    assert!(s.is_success());
    let (s, e) = send(
        &app,
        req("POST", &format!("/$/ingest/org/{id}/approve"), Value::Null),
    )
    .await;
    assert_eq!(s, StatusCode::CONFLICT, "{e}");
    assert_eq!(e["code"], "conflict");
    // auto: every fact passes, so the proposals are merged into main
    let v = ingest(
        &app,
        req(
            "POST",
            "/$/ingest/org",
            json!({ "text": other, "format": "text/markdown", "title": "Platform", "mode": "auto" }),
        ),
    )
    .await;
    assert_eq!(v["status"], "done", "{v}");
    assert_eq!(v["result"]["outcome"], "merged", "{v}");
    assert!(
        ask(
            &app,
            &format!("ASK {{ GRAPH ?g {{ ?k <{MEMBER_OF}> ?t . ?k ?l \"Kai Berg\"@en }} }}")
        )
        .await
            || ask(
                &app,
                &format!("ASK {{ GRAPH ?g {{ ?k <{MEMBER_OF}> ?t . ?k ?l \"Kai Berg\" }} }}")
            )
            .await
    );
}

/// A18: a table of 10,000 rows takes one model call for its mapping, and the dry run of
/// the upload with that mapping converts every row.
#[tokio::test]
async fn a18_csv_mapping_draft() {
    let m = MockModel::start(|r, _| {
        let s = &r.body["response_format"]["json_schema"]["schema"]["properties"];
        let pick = |e: &Value, needle: &str| {
            e["enum"]
                .as_array()
                .unwrap()
                .iter()
                .filter_map(Value::as_str)
                .find(|v| v.contains(needle))
                .unwrap()
                .to_string()
        };
        let class = pick(&s["class"], "Person");
        let member = pick(
            &s["columns"]["items"]["properties"]["predicate"],
            "memberOf",
        );
        let a = json!({
            "subjectColumn": "id", "class": class,
            "columns": [
                { "column": "id", "predicate": "", "kind": "literal", "datatype": "", "lang": "" },
                { "column": "team", "predicate": member, "kind": "iri", "datatype": "", "lang": "" }
            ]
        });
        (200, mock::openai(&a.to_string()))
    });
    let (_st, app) = app(Some([&m.url(), &m.url()]));
    enable(&app).await;
    let mut csv = String::from("id,team\n");
    for i in 0..10_000 {
        csv.push_str(&format!("{i},team{}\n", i % 7));
    }
    let v = ingest(
        &app,
        multipart(
            "/$/ingest/org",
            &[
                ("base", None, None, b"http://example.org/people/"),
                ("file", Some("people.csv"), Some("text/csv"), csv.as_bytes()),
            ],
        ),
    )
    .await;
    assert_eq!(v["status"], "done", "{v}");
    let r = &v["result"];
    assert_eq!(r["outcome"], "mapping-draft", "{v}");
    assert_eq!(r["drafted"], "model");
    assert_eq!(r["rows"], 10_000);
    assert_eq!(r["triples"], 20_000, "a type and a team per row");
    assert_eq!(r["preview"]["rows"], 100);
    assert!(m.requests().len() <= 2);
    // the sample, not the table, went to the model
    let prompt = mock::prompt_of(&m.requests()[0]);
    assert!(
        prompt.contains("19,team5") && !prompt.contains("21,team0"),
        "{prompt}"
    );
    let mapping = r["mapping"].to_string();
    let up = Request::post("/org/upload?dryRun=true")
        .header(
            header::CONTENT_TYPE,
            "multipart/form-data; boundary=sparkles-ingest-boundary",
        )
        .body(Body::from(
            [
                "--sparkles-ingest-boundary\r\nContent-Disposition: form-data; name=\"mapping\"\r\n\r\n",
                &mapping,
                "\r\n--sparkles-ingest-boundary\r\nContent-Disposition: form-data; name=\"file\"; filename=\"people.csv\"\r\n\r\n",
                &csv,
                "\r\n--sparkles-ingest-boundary--\r\n",
            ]
            .concat(),
        ))
        .unwrap();
    let (s, d) = send(&app, up).await;
    assert_eq!(s, StatusCode::OK, "{d}");
    assert_eq!(d["dryRun"], true, "{d}");
    assert_eq!(d["commit"]["inserted"], 20_000, "{d}");
}
