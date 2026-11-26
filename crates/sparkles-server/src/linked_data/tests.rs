//! Unit and HTTP router integration tests for the C32 Phase 1 linked-data front end.

use super::*;
use crate::state::{AppState, DbType};
use axum::Router;
use axum::body::Body;
use axum::http::{HeaderMap, Request, StatusCode, header};
use oxrdfio::{RdfFormat, RdfParser};
use serde_json::{Value as J, json};
use sparkles::io::Source;
use sparkles::store::StoreOptions;
use std::sync::Arc;
use std::time::Duration;
use tower::ServiceExt;

struct Resp {
    status: StatusCode,
    headers: HeaderMap,
    body: Vec<u8>,
}

impl Resp {
    fn text(&self) -> String {
        String::from_utf8_lossy(&self.body).into_owned()
    }
    fn json(&self) -> J {
        serde_json::from_slice(&self.body).unwrap_or_else(|e| panic!("{e}: {}", self.text()))
    }
    fn header(&self, name: &str) -> Option<&str> {
        self.headers.get(name).and_then(|v| v.to_str().ok())
    }
}

async fn send(app: &Router, req: Request<Body>) -> Resp {
    let r = app.clone().oneshot(req).await.unwrap();
    let status = r.status();
    let headers = r.headers().clone();
    let body = axum::body::to_bytes(r.into_body(), usize::MAX)
        .await
        .unwrap()
        .to_vec();
    Resp {
        status,
        headers,
        body,
    }
}

fn get_req(uri: &str, accept: Option<&str>, auth: Option<&str>) -> Request<Body> {
    let mut b = Request::get(uri);
    if let Some(a) = accept {
        b = b.header(header::ACCEPT, a);
    }
    if let Some(au) = auth {
        b = b.header(header::AUTHORIZATION, au);
    }
    b.body(Body::empty()).unwrap()
}

fn post_json(uri: &str, body: J, auth: Option<&str>) -> Request<Body> {
    let mut b = Request::post(uri).header(header::CONTENT_TYPE, "application/json");
    if let Some(au) = auth {
        b = b.header(header::AUTHORIZATION, au);
    }
    b.body(Body::from(body.to_string())).unwrap()
}

fn enc(s: &str) -> String {
    url::form_urlencoded::byte_serialize(s.as_bytes()).collect()
}

#[test]
fn content_negotiation_follows_rfc_9110() {
    assert_eq!(negotiate_format(None), Some(Format::Turtle));
    assert_eq!(negotiate_format(Some("*/*")), Some(Format::Turtle));
    assert_eq!(
        negotiate_format(Some(
            "text/html,application/xhtml+xml,application/xml;q=0.9,*/*;q=0.8"
        )),
        Some(Format::Html)
    );
    assert_eq!(
        negotiate_format(Some("text/html;q=0.9, */*;q=0.9")),
        Some(Format::Html)
    );
    assert_eq!(negotiate_format(Some("text/turtle")), Some(Format::Turtle));
    assert_eq!(
        negotiate_format(Some("application/ld+json, text/turtle;q=0.8")),
        Some(Format::JsonLd)
    );
    assert_eq!(
        negotiate_format(Some("application/rdf+xml")),
        Some(Format::RdfXml)
    );
    assert_eq!(
        negotiate_format(Some("application/n-triples")),
        Some(Format::NTriples)
    );
    // Explicit q=0 excludes text/turtle even when */* is present.
    assert_eq!(
        negotiate_format(Some("text/turtle;q=0, */*;q=0.5")),
        Some(Format::JsonLd)
    );
    assert_eq!(negotiate_format(Some("image/png")), None);
}

#[test]
fn linked_data_routes_classify_as_queries_for_rate_limiting() {
    for (route, uri) in [
        ("/$/ld", "/$/ld?iri=http://ex/a"),
        ("/$/ld/doc", "/$/ld/doc?iri=http://ex/a"),
        ("/$/ld/ns/{prefix}", "/$/ld/ns/ex"),
        ("/$/ld/ns/{prefix}/{*local}", "/$/ld/ns/ex/Person"),
    ] {
        let c = crate::ratelimit::classify(
            Some(route),
            &axum::http::Method::GET,
            &uri.parse().unwrap(),
            &HeaderMap::new(),
        );
        assert!(matches!(c, Some(crate::ratelimit::Class::Query)), "{route}");
    }
}

#[tokio::test]
async fn slash_303_hash_200_and_content_negotiation_with_vary_accept() {
    let dir = tempfile::tempdir().unwrap();
    let st = Arc::new(
        AppState::new(dir.path(), StoreOptions::default(), Duration::from_secs(30)).unwrap(),
    );
    let ds = st.attach("vocabs", DbType::Mem, None).unwrap();
    let trig = r#"
@prefix rdf: <http://www.w3.org/1999/02/22-rdf-syntax-ns#> .
@prefix rdfs: <http://www.w3.org/2000/01/rdf-schema#> .
@prefix owl: <http://www.w3.org/2002/07/owl#> .
@prefix skos: <http://www.w3.org/2004/02/skos/core#> .

<http://example.org/graphs/slash> {
  <http://example.org/slash/> a owl:Ontology ;
    rdfs:label "Slash Vocabulary"@en .
  <http://example.org/slash/Person> a owl:Class ;
    rdfs:label "Person"@en ;
    rdfs:comment "A human individual."@en ;
    rdfs:seeAlso <https://schema.org/Person> .
}

<http://example.org/graphs/hash> {
  <http://example.org/hash> a owl:Ontology ;
    rdfs:label "Hash Vocabulary"@en .
  <http://example.org/hash#Concept> a skos:Concept ;
    skos:prefLabel "Core Concept"@en .
}
"#;
    ds.store
        .load(&[Source::from_bytes(
            trig.as_bytes().to_vec(),
            RdfFormat::TriG,
            None,
        )])
        .unwrap();

    let app = crate::http::router(st.clone());
    // Register slash and hash vocabularies in the resource catalog.
    let r = send(
        &app,
        post_json(
            "/$/resources",
            json!({
                "id": "slashv",
                "kind": "vocabulary",
                "title": "Slash Vocabulary",
                "license": "CC-BY-4.0",
                "namespaces": [{"prefix": "slash", "iri": "http://example.org/slash/"}],
                "graphs": [{"dataset": "vocabs", "graph": "http://example.org/graphs/slash"}],
                "versions": [{"version": "1.0"}]
            }),
            None,
        ),
    )
    .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());

    let r = send(
        &app,
        post_json(
            "/$/resources",
            json!({
                "id": "hashv",
                "kind": "vocabulary",
                "title": "Hash Vocabulary",
                "namespaces": [{"prefix": "hash", "iri": "http://example.org/hash#"}],
                "graphs": [{"dataset": "vocabs", "graph": "http://example.org/graphs/hash"}],
                "versions": [{"version": "1.0"}]
            }),
            None,
        ),
    )
    .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());

    // 1. Slash IRI requested by a browser -> 303 See Other with Vary: Accept.
    let iri = "http://example.org/slash/Person";
    let r = send(
        &app,
        get_req(
            &format!("/$/ld?iri={}", enc(iri)),
            Some("text/html,application/xhtml+xml,application/xml;q=0.9,*/*;q=0.8"),
            None,
        ),
    )
    .await;
    assert_eq!(r.status, StatusCode::SEE_OTHER, "{}", r.text());
    assert_eq!(r.header("vary"), Some("Accept"));
    let loc = r.header("location").expect("303 location").to_string();
    assert!(loc.starts_with("/$/ld/doc?"), "{loc}");
    assert!(loc.contains("format=html"), "{loc}");

    // Follow 303 -> 200 OK HTML with Vary: Accept, Link alternates, and content.
    let doc = send(&app, get_req(&loc, Some("text/html"), None)).await;
    assert_eq!(doc.status, StatusCode::OK, "{}", doc.text());
    assert_eq!(doc.header("content-type"), Some("text/html; charset=utf-8"));
    assert_eq!(doc.header("vary"), Some("Accept"));
    assert!(doc.header("link").unwrap_or("").contains("text/turtle"));
    let body = doc.text();
    assert!(body.contains("<h1>Person</h1>"), "{body}");
    assert!(body.contains("A human individual."), "{body}");
    assert!(body.contains("CC-BY-4.0"), "{body}");

    // 2. Slash IRI via prefix route /$/ld/ns/slash/Person -> 303 See Other for RDF agents.
    for (accept, want_fmt, want_ct, rdf_fmt) in [
        (
            "text/turtle",
            "format=turtle",
            "text/turtle; charset=utf-8",
            RdfFormat::Turtle,
        ),
        (
            "application/ld+json",
            "format=jsonld",
            "application/ld+json",
            RdfFormat::JsonLd {
                profile: oxrdfio::JsonLdProfileSet::empty(),
            },
        ),
        (
            "application/rdf+xml",
            "format=rdfxml",
            "application/rdf+xml",
            RdfFormat::RdfXml,
        ),
        (
            "application/n-triples",
            "format=ntriples",
            "application/n-triples",
            RdfFormat::NTriples,
        ),
    ] {
        let r = send(&app, get_req("/$/ld/ns/slash/Person", Some(accept), None)).await;
        assert_eq!(r.status, StatusCode::SEE_OTHER, "{accept}: {}", r.text());
        assert_eq!(r.header("vary"), Some("Accept"));
        let loc = r.header("location").unwrap().to_string();
        assert!(loc.contains(want_fmt), "{accept} -> {loc}");

        let doc = send(&app, get_req(&loc, Some(accept), None)).await;
        assert_eq!(doc.status, StatusCode::OK, "{accept}: {}", doc.text());
        assert_eq!(doc.header("content-type"), Some(want_ct));
        assert_eq!(doc.header("vary"), Some("Accept"));
        let parsed: Vec<_> = RdfParser::from_format(rdf_fmt)
            .for_slice(&doc.body)
            .collect::<Result<Vec<_>, _>>()
            .unwrap_or_else(|e| panic!("failed to parse {accept}: {e}\n{}", doc.text()));
        assert!(parsed.len() >= 4, "{accept}: {parsed:?}");
    }

    // 3. Hash IRI answers 200 OK directly (no 303 redirect) for both browser and agent.
    let hash_iri = "http://example.org/hash#Concept";
    let html_r = send(
        &app,
        get_req(
            &format!("/$/ld?iri={}", enc(hash_iri)),
            Some("text/html"),
            None,
        ),
    )
    .await;
    assert_eq!(html_r.status, StatusCode::OK, "{}", html_r.text());
    assert_eq!(html_r.header("vary"), Some("Accept"));
    assert_eq!(
        html_r.header("content-type"),
        Some("text/html; charset=utf-8")
    );
    assert!(html_r.text().contains("Core Concept"));

    let ttl_r = send(
        &app,
        get_req(
            &format!("/$/ld?iri={}", enc(hash_iri)),
            Some("text/turtle"),
            None,
        ),
    )
    .await;
    assert_eq!(ttl_r.status, StatusCode::OK, "{}", ttl_r.text());
    assert_eq!(ttl_r.header("vary"), Some("Accept"));
    assert_eq!(
        ttl_r.header("content-type"),
        Some("text/turtle; charset=utf-8")
    );
    assert!(ttl_r.text().contains("Core Concept"));

    // 4. Bare hash namespace document (/$/ld/ns/hash) answers 200 OK directly with its terms.
    let ns_r = send(&app, get_req("/$/ld/ns/hash", Some("text/turtle"), None)).await;
    assert_eq!(ns_r.status, StatusCode::OK, "{}", ns_r.text());
    assert_eq!(ns_r.header("vary"), Some("Accept"));
    let ns_body = ns_r.text();
    assert!(ns_body.contains("Hash Vocabulary"), "{ns_body}");
    assert!(ns_body.contains("Core Concept"), "{ns_body}");

    // 5. Unacceptable Accept -> 406 Not Acceptable with Vary: Accept.
    let unacc = send(
        &app,
        get_req(
            &format!("/$/ld?iri={}", enc(hash_iri)),
            Some("image/png"),
            None,
        ),
    )
    .await;
    assert_eq!(unacc.status, StatusCode::NOT_ACCEPTABLE);
    assert_eq!(unacc.header("vary"), Some("Accept"));

    // 6. Bounded DESCRIBE truncation header and HTML notice.
    let trunc = send(
        &app,
        get_req(
            &format!("/$/ld/doc?iri={}&describe-max-triples=1", enc(iri)),
            Some("application/n-triples"),
            None,
        ),
    )
    .await;
    assert_eq!(trunc.status, StatusCode::OK, "{}", trunc.text());
    assert_eq!(trunc.header("sparkles-describe-truncated"), Some("true"));
    assert_eq!(
        trunc
            .text()
            .lines()
            .filter(|l| !l.trim().is_empty())
            .count(),
        1
    );
    let trunc_html = send(
        &app,
        get_req(
            &format!("/$/ld/doc?iri={}&describe-max-triples=1", enc(iri)),
            Some("text/html"),
            None,
        ),
    )
    .await;
    assert_eq!(trunc_html.status, StatusCode::OK);
    assert!(trunc_html.text().contains("truncated"));
}

#[tokio::test]
async fn version_iri_resolves_to_pinned_snapshot() {
    let dir = tempfile::tempdir().unwrap();
    let st = Arc::new(
        AppState::new(dir.path(), StoreOptions::default(), Duration::from_secs(30)).unwrap(),
    );
    st.create("verds", DbType::Persistent).unwrap();
    let app = crate::http::router(st.clone());

    // Load v1 into persistent dataset and pin snapshot:v1.
    let v1_data = r#"
@prefix owl: <http://www.w3.org/2002/07/owl#> .
@prefix rdfs: <http://www.w3.org/2000/01/rdf-schema#> .
<http://example.org/ver/> a owl:Ontology ;
  owl:versionIRI <http://example.org/ver/1.0> ;
  rdfs:label "Versioned Vocab v1" .
<http://example.org/ver/Term> a owl:Class ;
  rdfs:label "Term at v1" .
"#;
    let put_v1 = send(
        &app,
        Request::put("/verds/data?graph=http%3A%2F%2Fexample.org%2Fver%2Fg")
            .header(header::CONTENT_TYPE, "text/turtle")
            .body(Body::from(v1_data))
            .unwrap(),
    )
    .await;
    assert!(put_v1.status.is_success(), "{}", put_v1.text());

    let snap_v1 = send(
        &app,
        post_json("/$/snapshots/verds", json!({"name": "v1"}), None),
    )
    .await;
    assert_eq!(snap_v1.status, StatusCode::CREATED, "{}", snap_v1.text());

    // Replace graph with v2 data and pin snapshot:v2.
    let v2_data = r#"
@prefix owl: <http://www.w3.org/2002/07/owl#> .
@prefix rdfs: <http://www.w3.org/2000/01/rdf-schema#> .
<http://example.org/ver/> a owl:Ontology ;
  owl:versionIRI <http://example.org/ver/2.0> ;
  rdfs:label "Versioned Vocab v2" .
<http://example.org/ver/Term> a owl:Class ;
  rdfs:label "Term at v2" .
"#;
    let put_v2 = send(
        &app,
        Request::put("/verds/data?graph=http%3A%2F%2Fexample.org%2Fver%2Fg")
            .header(header::CONTENT_TYPE, "text/turtle")
            .body(Body::from(v2_data))
            .unwrap(),
    )
    .await;
    assert!(put_v2.status.is_success(), "{}", put_v2.text());

    let snap_v2 = send(
        &app,
        post_json("/$/snapshots/verds", json!({"name": "v2"}), None),
    )
    .await;
    assert_eq!(snap_v2.status, StatusCode::CREATED, "{}", snap_v2.text());

    // Register vocabulary with versionIri + pinned snapshot on both versions.
    let reg = send(
        &app,
        post_json(
            "/$/resources",
            json!({
                "id": "vervocab",
                "kind": "vocabulary",
                "title": "Versioned Vocab",
                "namespaces": [{"prefix": "ver", "iri": "http://example.org/ver/"}],
                "graphs": [{"dataset": "verds", "graph": "http://example.org/ver/g"}],
                "versions": [
                    {
                        "version": "1.0",
                        "versionIri": "http://example.org/ver/1.0",
                        "snapshot": "snapshot:v1"
                    },
                    {
                        "version": "2.0",
                        "versionIri": "http://example.org/ver/2.0",
                        "snapshot": "snapshot:v2"
                    }
                ]
            }),
            None,
        ),
    )
    .await;
    assert_eq!(reg.status, StatusCode::CREATED, "{}", reg.text());

    // Duplicate versionIri on another resource is refused with 409 version-iri-owned.
    let dup = send(
        &app,
        post_json(
            "/$/resources",
            json!({
                "id": "other",
                "kind": "source",
                "title": "Other",
                "versions": [{"version": "1.0", "versionIri": "http://example.org/ver/1.0"}]
            }),
            None,
        ),
    )
    .await;
    assert_eq!(dup.status, StatusCode::CONFLICT, "{}", dup.text());
    assert_eq!(dup.json()["code"], "version-iri-owned");

    // DCAT RDF export includes owl:versionIRI.
    let dcat = send(
        &app,
        get_req(
            "/$/resources/vervocab?format=ntriples",
            Some("application/n-triples"),
            None,
        ),
    )
    .await;
    assert_eq!(dcat.status, StatusCode::OK);
    assert!(
        dcat.text()
            .contains("<http://www.w3.org/2002/07/owl#versionIRI> <http://example.org/ver/1.0>"),
        "{}",
        dcat.text()
    );

    // Resolving version 1.0 IRI yields 303 -> 200 with v1 data and history/Memento headers.
    let v1_redir = send(
        &app,
        get_req(
            &format!("/$/ld?iri={}", enc("http://example.org/ver/1.0")),
            Some("text/turtle"),
            None,
        ),
    )
    .await;
    assert_eq!(
        v1_redir.status,
        StatusCode::SEE_OTHER,
        "{}",
        v1_redir.text()
    );
    let v1_loc = v1_redir.header("location").unwrap().to_string();
    let v1_doc = send(&app, get_req(&v1_loc, Some("text/turtle"), None)).await;
    assert_eq!(v1_doc.status, StatusCode::OK, "{}", v1_doc.text());
    assert_eq!(v1_doc.header("sparkles-at"), Some("snapshot:v1"));
    assert!(v1_doc.header("memento-datetime").is_some());
    assert!(
        v1_doc.text().contains("Versioned Vocab v1"),
        "{}",
        v1_doc.text()
    );
    assert!(
        !v1_doc.text().contains("Versioned Vocab v2"),
        "{}",
        v1_doc.text()
    );

    // Resolving a term with ?version=1.0 pins to snapshot:v1, while live returns v2.
    let term_v1 = send(
        &app,
        get_req(
            &format!(
                "/$/ld/doc?iri={}&version=1.0",
                enc("http://example.org/ver/Term")
            ),
            Some("text/turtle"),
            None,
        ),
    )
    .await;
    assert_eq!(term_v1.status, StatusCode::OK, "{}", term_v1.text());
    assert_eq!(term_v1.header("sparkles-at"), Some("snapshot:v1"));
    assert!(term_v1.text().contains("Term at v1"), "{}", term_v1.text());
    assert!(!term_v1.text().contains("Term at v2"), "{}", term_v1.text());

    let term_live = send(
        &app,
        get_req(
            &format!("/$/ld/doc?iri={}", enc("http://example.org/ver/Term")),
            Some("text/turtle"),
            None,
        ),
    )
    .await;
    assert_eq!(term_live.status, StatusCode::OK, "{}", term_live.text());
    assert!(
        term_live.text().contains("Term at v2"),
        "{}",
        term_live.text()
    );
}

#[tokio::test]
async fn html_escapes_xss_payloads_and_blocks_unsafe_href_schemes() {
    let dir = tempfile::tempdir().unwrap();
    let st = Arc::new(
        AppState::new(dir.path(), StoreOptions::default(), Duration::from_secs(30)).unwrap(),
    );
    let ds = st.attach("xssds", DbType::Mem, None).unwrap();
    let trig = r#"
@prefix rdfs: <http://www.w3.org/2000/01/rdf-schema#> .
<http://example.org/xss/g> {
  <http://example.org/xss/Term>
    rdfs:label "<script>alert('xss')</script> & \"quoted\"" ;
    rdfs:comment "<img src=x onerror=alert(1)>"@en ;
    rdfs:seeAlso <javascript:alert(1)> ;
    rdfs:isDefinedBy <https://example.org/safe> .
}
"#;
    ds.store
        .load(&[Source::from_bytes(
            trig.as_bytes().to_vec(),
            RdfFormat::TriG,
            None,
        )])
        .unwrap();

    let app = crate::http::router(st.clone());
    let reg = send(
        &app,
        post_json(
            "/$/resources",
            json!({
                "id": "xssv",
                "kind": "vocabulary",
                "title": "<b>Evil & \"Title\"</b>",
                "namespaces": [{"prefix": "xss", "iri": "http://example.org/xss/"}],
                "graphs": [{"dataset": "xssds", "graph": "http://example.org/xss/g"}]
            }),
            None,
        ),
    )
    .await;
    assert_eq!(reg.status, StatusCode::CREATED, "{}", reg.text());

    let resp = send(
        &app,
        get_req(
            &format!("/$/ld/doc?iri={}", enc("http://example.org/xss/Term")),
            Some("text/html"),
            None,
        ),
    )
    .await;
    assert_eq!(resp.status, StatusCode::OK, "{}", resp.text());
    let csp = resp.header("content-security-policy").unwrap_or("");
    assert!(csp.contains("default-src 'none'"), "{csp}");
    assert!(csp.contains("frame-ancestors 'none'"), "{csp}");

    let body = resp.text();
    assert!(!body.contains("<script>"), "raw script tag leaked:\n{body}");
    assert!(!body.contains("<img src=x"), "raw img tag leaked:\n{body}");
    assert!(!body.contains("<b>Evil"), "raw title tag leaked:\n{body}");
    assert!(
        !body.to_ascii_lowercase().contains("href=\"javascript:"),
        "unsafe javascript href leaked:\n{body}"
    );
    assert!(
        body.contains("&lt;script&gt;alert(&#39;xss&#39;)&lt;/script&gt; &amp; &quot;quoted&quot;"),
        "{body}"
    );
    assert!(
        body.contains("&lt;img src=x onerror=alert(1)&gt;"),
        "{body}"
    );
    assert!(
        body.contains("&lt;b&gt;Evil &amp; &quot;Title&quot;&lt;/b&gt;"),
        "{body}"
    );
    assert!(body.contains("href=\"https://example.org/safe\""), "{body}");
}

#[tokio::test]
async fn purl_and_w3id_recipe_export_covers_namespaces_and_versions() {
    let dir = tempfile::tempdir().unwrap();
    let st = Arc::new(
        AppState::new(dir.path(), StoreOptions::default(), Duration::from_secs(30)).unwrap(),
    );
    let app = crate::http::router(st.clone());

    let reg = send(
        &app,
        post_json(
            "/$/resources",
            json!({
                "id": "myvocab",
                "kind": "vocabulary",
                "title": "My Vocab",
                "namespaces": [
                    {"prefix": "mv", "iri": "https://w3id.org/myorg/mv/"},
                    {"prefix": "mvh", "iri": "https://w3id.org/myorg/mvh#"}
                ],
                "versions": [
                    {
                        "version": "1.0",
                        "versionIri": "https://w3id.org/myorg/mv/1.0",
                        "snapshot": "snapshot:v1"
                    }
                ]
            }),
            None,
        ),
    )
    .await;
    assert_eq!(reg.status, StatusCode::CREATED, "{}", reg.text());

    // Default w3id .htaccess text output.
    let w3id = send(
        &app,
        get_req(
            "/$/resources/myvocab/recipes?base=https://rdf.example.org",
            Some("text/plain"),
            None,
        ),
    )
    .await;
    assert_eq!(w3id.status, StatusCode::OK, "{}", w3id.text());
    assert_eq!(
        w3id.header("content-type"),
        Some("text/plain; charset=utf-8")
    );
    assert_eq!(w3id.header("vary"), Some("Accept"));
    let text = w3id.text();
    assert!(text.contains("RewriteEngine on"), "{text}");
    assert!(text.contains("R=303,L"), "{text}");
    assert!(
        text.contains("https://rdf.example.org/$/ld/doc?iri="),
        "{text}"
    );
    assert!(text.contains("myorg/mv/1\\.0"), "{text}");
    assert!(
        text.contains("https://rdf.example.org/$/ld/ns/mv"),
        "{text}"
    );
    assert!(
        text.contains("https://rdf.example.org/$/ld/ns/mvh"),
        "{text}"
    );

    // PURL recipe JSON output.
    let purl_json = send(
        &app,
        get_req(
            "/$/resources/myvocab/recipes?kind=purl&base=https://rdf.example.org&format=json",
            Some("application/json"),
            None,
        ),
    )
    .await;
    assert_eq!(purl_json.status, StatusCode::OK, "{}", purl_json.text());
    let j = purl_json.json();
    assert_eq!(j["kind"], "purl");
    assert_eq!(j["namespaces"].as_array().unwrap().len(), 2);
    assert_eq!(j["versions"].as_array().unwrap().len(), 1);
    assert!(j["content"].as_str().unwrap().contains("303 partial"));

    // Invalid base URLs (injection / non-http) are rejected with 400.
    for bad_base in [
        "javascript:alert(1)",
        "https://rdf.example.org/foo%0AEvilHeader",
        "https://user:pw@rdf.example.org",
    ] {
        let bad = send(
            &app,
            get_req(
                &format!("/$/resources/myvocab/recipes?base={bad_base}"),
                None,
                None,
            ),
        )
        .await;
        assert_eq!(bad.status, StatusCode::BAD_REQUEST, "{bad_base}");
    }

    // Non-vocabulary resource returns 409 not-a-vocabulary.
    send(
        &app,
        post_json(
            "/$/resources",
            json!({"id": "src", "kind": "source", "title": "Source"}),
            None,
        ),
    )
    .await;
    let non_vocab = send(&app, get_req("/$/resources/src/recipes", None, None)).await;
    assert_eq!(non_vocab.status, StatusCode::CONFLICT);
    assert_eq!(non_vocab.json()["code"], "not-a-vocabulary");
}

#[cfg(feature = "auth")]
#[tokio::test]
async fn strict_read_permissions_prevent_leaking_private_terms_graphs_and_triples() {
    use base64::Engine as _;

    let dir = tempfile::tempdir().unwrap();
    let h = |pw: &str| crate::auth::hash_password_with(pw, 8, 1, 1).unwrap();
    let auth_toml = format!(
        r#"
version = 1

[server]
public_url = "http://localhost:3030"

[anonymous]
datasets = {{}}

[[protections]]
name = "secret_notes"
dataset = "acl_ds"
predicates = ["http://example.org/acl/secretNote"]

[[users]]
name = "admin"
password = "{admin}"
server = ["server-admin"]

[[users]]
name = "pub_reader"
password = "{pub_reader}"
[[users.grants]]
dataset = "acl_ds"
level = "read"
graphs = ["http://example.org/g/public"]

[[users]]
name = "outsider"
password = "{outsider}"
datasets = {{ other_ds = "read" }}
"#,
        admin = h("admin-pw"),
        pub_reader = h("pub-pw"),
        outsider = h("out-pw"),
    );
    let auth_path = dir.path().join("auth.toml");
    std::fs::write(&auth_path, auth_toml).unwrap();
    let auth = crate::auth::load(Some(&auth_path), dir.path(), "127.0.0.1", false).unwrap();

    let mut st =
        AppState::new(dir.path(), StoreOptions::default(), Duration::from_secs(30)).unwrap();
    st.auth = auth;
    let st = Arc::new(st);

    let ds = st.attach("acl_ds", DbType::Mem, None).unwrap();
    st.attach("other_ds", DbType::Mem, None).unwrap();
    let trig = r#"
@prefix rdfs: <http://www.w3.org/2000/01/rdf-schema#> .
@prefix acl: <http://example.org/acl/> .

<http://example.org/g/public> {
  acl:PublicTerm rdfs:label "Public Term" ;
    acl:secretNote "TOP SECRET NOTE" .
}

<http://example.org/g/private> {
  acl:PrivateTerm rdfs:label "Private Term" .
}
"#;
    ds.store
        .load(&[Source::from_bytes(
            trig.as_bytes().to_vec(),
            RdfFormat::TriG,
            None,
        )])
        .unwrap();

    let basic = |u: &str, pw: &str| {
        format!(
            "Basic {}",
            base64::engine::general_purpose::STANDARD.encode(format!("{u}:{pw}"))
        )
    };
    let admin_auth = basic("admin", "admin-pw");
    let pub_auth = basic("pub_reader", "pub-pw");
    let out_auth = basic("outsider", "out-pw");

    let app = crate::http::router(st.clone());
    let reg = send(
        &app,
        post_json(
            "/$/resources",
            json!({
                "id": "aclvocab",
                "kind": "vocabulary",
                "title": "Classified Vocabulary",
                "namespaces": [{"prefix": "acl", "iri": "http://example.org/acl/"}],
                "graphs": [
                    {"dataset": "acl_ds", "graph": "http://example.org/g/public"},
                    {"dataset": "acl_ds", "graph": "http://example.org/g/private"}
                ]
            }),
            Some(&admin_auth),
        ),
    )
    .await;
    assert_eq!(reg.status, StatusCode::CREATED, "{}", reg.text());

    let pub_iri = "http://example.org/acl/PublicTerm";
    let priv_iri = "http://example.org/acl/PrivateTerm";

    // 1. Anonymous and outsider get 404 (never 303, never leaking vocabulary title/existence).
    for caller in [None, Some(out_auth.as_str())] {
        for uri in [
            format!("/$/ld?iri={}", enc(pub_iri)),
            format!("/$/ld/doc?iri={}", enc(pub_iri)),
            "/$/ld/ns/acl/PublicTerm".to_string(),
            format!("/$/ld?iri={}", enc(priv_iri)),
            "/$/ld/ns/acl/PrivateTerm".to_string(),
        ] {
            let r = send(&app, get_req(&uri, Some("text/html"), caller)).await;
            assert_eq!(r.status, StatusCode::NOT_FOUND, "{uri}: {}", r.text());
            assert!(r.header("location").is_none());
            assert!(!r.text().contains("Classified Vocabulary"));
            assert!(!r.text().contains("Public Term"));
            assert!(!r.text().contains("Private Term"));
        }
    }

    // 2. pub_reader can read PublicTerm (303 -> 200), but triple protection strips secretNote
    //    from BOTH HTML and Turtle!
    let r = send(
        &app,
        get_req(
            &format!("/$/ld?iri={}", enc(pub_iri)),
            Some("text/html"),
            Some(&pub_auth),
        ),
    )
    .await;
    assert_eq!(r.status, StatusCode::SEE_OTHER, "{}", r.text());
    let loc = r.header("location").unwrap().to_string();
    let html_doc = send(&app, get_req(&loc, Some("text/html"), Some(&pub_auth))).await;
    assert_eq!(html_doc.status, StatusCode::OK, "{}", html_doc.text());
    assert!(html_doc.text().contains("Public Term"));
    assert!(
        !html_doc.text().contains("TOP SECRET NOTE"),
        "triple protection leaked in HTML:\n{}",
        html_doc.text()
    );

    let ttl_doc = send(
        &app,
        get_req(
            &format!("/$/ld/doc?iri={}", enc(pub_iri)),
            Some("text/turtle"),
            Some(&pub_auth),
        ),
    )
    .await;
    assert_eq!(ttl_doc.status, StatusCode::OK, "{}", ttl_doc.text());
    assert!(ttl_doc.text().contains("Public Term"));
    assert!(
        !ttl_doc.text().contains("TOP SECRET NOTE"),
        "triple protection leaked in Turtle:\n{}",
        ttl_doc.text()
    );

    // 3. pub_reader CANNOT read PrivateTerm (in g/private) even though it shares namespace acl:
    //    gets 404 Not Found with NO 303 redirect!
    for uri in [
        format!("/$/ld?iri={}", enc(priv_iri)),
        format!("/$/ld/doc?iri={}", enc(priv_iri)),
        "/$/ld/ns/acl/PrivateTerm".to_string(),
    ] {
        let r = send(&app, get_req(&uri, Some("text/turtle"), Some(&pub_auth))).await;
        assert_eq!(r.status, StatusCode::NOT_FOUND, "{uri}: {}", r.text());
        assert!(r.header("location").is_none());
        assert!(!r.text().contains("Private Term"));
    }

    // 4. admin sees both PrivateTerm and the protected secretNote.
    let admin_priv = send(
        &app,
        get_req(
            &format!("/$/ld/doc?iri={}", enc(priv_iri)),
            Some("text/turtle"),
            Some(&admin_auth),
        ),
    )
    .await;
    assert_eq!(admin_priv.status, StatusCode::OK, "{}", admin_priv.text());
    assert!(admin_priv.text().contains("Private Term"));

    let admin_pub = send(
        &app,
        get_req(
            &format!("/$/ld/doc?iri={}", enc(pub_iri)),
            Some("text/turtle"),
            Some(&admin_auth),
        ),
    )
    .await;
    assert_eq!(admin_pub.status, StatusCode::OK);
    assert!(admin_pub.text().contains("TOP SECRET NOTE"));

    // 5. Recipe export requires server-admin.
    let non_admin_recipe = send(
        &app,
        get_req("/$/resources/aclvocab/recipes", None, Some(&pub_auth)),
    )
    .await;
    assert_eq!(non_admin_recipe.status, StatusCode::FORBIDDEN);
}
