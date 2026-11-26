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

    // Default w3id .htaccess text output: strips shared 'myorg' mount and leaves
    // content negotiation to Sparkles.
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
    assert!(text.contains("ids/myorg/.htaccess"), "{text}");
    assert!(text.contains("RewriteRule ^mv/1\\.0/?$ "), "{text}");
    assert!(!text.contains("RewriteRule ^myorg/"), "{text}");
    assert!(!text.contains("RewriteCond %{HTTP_ACCEPT}"), "{text}");
    assert!(
        text.contains(
            "https://rdf.example.org/$/ld/doc?iri=https\\%3A\\%2F\\%2Fw3id.org\\%2Fmyorg\\%2Fmv\\%2F1.0 [R=303,L,NE]"
        ),
        "{text}"
    );
    assert!(
        text.contains(
            "RewriteRule ^mv/(.+)$ https://rdf.example.org/$/ld/doc?iri=https\\%3A\\%2F\\%2Fw3id.org\\%2Fmyorg\\%2Fmv\\%2F$1 [B,R=303,L,NE]"
        ),
        "{text}"
    );
    assert!(
        text.contains("RewriteRule ^mvh/?$ https://rdf.example.org/$/ld/ns/mvh [R=303,L,NE]"),
        "{text}"
    );

    // PURL recipe JSON output uses PURL's supported `partial` type (not `303 partial`).
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
    let purl_content = j["content"].as_str().unwrap();
    assert!(
        purl_content.contains("partial https://w3id.org/myorg/mv/ -> "),
        "{purl_content}"
    );
    assert!(!purl_content.contains("303 partial"), "{purl_content}");

    // Invalid base URLs (injection / mod_rewrite metacharacters / non-http) are rejected with 400.
    for bad_base in [
        "javascript:alert(1)",
        "https://rdf.example.org/foo%0AEvilHeader",
        "https://user:pw@rdf.example.org",
        "https://rdf.example.org/$1",
        "https://rdf.example.org/%25%7BHTTP_HOST%7D",
        "https://rdf.example.org/%7Bfoo%7D",
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

    // 6. Prefix existence oracle check: private prefix `acl` and nonexistent prefix `nope`
    //    must answer identically for unacceptable Accept, bad format, bad at, bad describe
    //    params, and invalid local path characters.
    for (suffix, accept, expected) in [
        ("", Some("image/png"), StatusCode::NOT_ACCEPTABLE),
        (
            "?format=bogus",
            Some("text/turtle"),
            StatusCode::BAD_REQUEST,
        ),
        (
            "?at=not-a-selector",
            Some("text/turtle"),
            StatusCode::BAD_REQUEST,
        ),
        (
            "?describe-max-triples=0",
            Some("text/turtle"),
            StatusCode::BAD_REQUEST,
        ),
        ("/bad%20local", Some("text/turtle"), StatusCode::BAD_REQUEST),
    ] {
        let priv_r = send(
            &app,
            get_req(&format!("/$/ld/ns/acl{suffix}"), accept, None),
        )
        .await;
        let miss_r = send(
            &app,
            get_req(&format!("/$/ld/ns/nope{suffix}"), accept, None),
        )
        .await;
        assert_eq!(priv_r.status, expected, "acl{suffix}: {}", priv_r.text());
        assert_eq!(
            priv_r.status, miss_r.status,
            "oracle leak on {suffix}: acl={} nope={}",
            priv_r.status, miss_r.status
        );
    }

    // 7. A caller with a graph grant whose triple protections hide all triples of a
    //    registered versionIri must receive 404 (never version-conflict / snapshot-conflict
    //    or dataset head commit leakage).
    let hidden_ver_trig = r#"
@prefix owl: <http://www.w3.org/2002/07/owl#> .
@prefix acl: <http://example.org/acl/> .
<http://example.org/g/public> {
  <http://example.org/acl/v1> acl:secretNote "HIDDEN VERSION META" .
}
"#;
    ds.store
        .load(&[Source::from_bytes(
            hidden_ver_trig.as_bytes().to_vec(),
            RdfFormat::TriG,
            None,
        )])
        .unwrap();
    let add_ver = send(
        &app,
        post_json(
            "/$/resources/aclvocab/versions",
            json!({
                "version": "1.0",
                "versionIri": "http://example.org/acl/v1",
                "snapshot": "commit:2"
            }),
            Some(&admin_auth),
        ),
    )
    .await;
    assert_eq!(add_ver.status, StatusCode::CREATED, "{}", add_ver.text());

    // pub_reader cannot read any triple of acl/v1 -> 404 even with conflicting ?version= or ?at=.
    for uri in [
        format!(
            "/$/ld/doc?iri={}&version=2.0",
            enc("http://example.org/acl/v1")
        ),
        format!(
            "/$/ld/doc?iri={}&at=commit:999",
            enc("http://example.org/acl/v1")
        ),
        format!("/$/ld/doc?iri={}&at=commit:999", enc(pub_iri)),
    ] {
        let r = send(&app, get_req(&uri, Some("text/turtle"), Some(&pub_auth))).await;
        assert_eq!(r.status, StatusCode::NOT_FOUND, "{uri}: {}", r.text());
        assert!(
            !r.text().contains("head is"),
            "leaked dataset head in {uri}: {}",
            r.text()
        );
    }
    // admin CAN read acl/v1 -> gets 400 version-conflict and 400 snapshot-conflict.
    let adm_vc = send(
        &app,
        get_req(
            &format!(
                "/$/ld/doc?iri={}&version=2.0",
                enc("http://example.org/acl/v1")
            ),
            Some("text/turtle"),
            Some(&admin_auth),
        ),
    )
    .await;
    assert_eq!(adm_vc.status, StatusCode::BAD_REQUEST, "{}", adm_vc.text());
    assert_eq!(adm_vc.json()["code"], "version-conflict");

    let adm_sc = send(
        &app,
        get_req(
            &format!(
                "/$/ld/doc?iri={}&at=commit:999",
                enc("http://example.org/acl/v1")
            ),
            Some("text/turtle"),
            Some(&admin_auth),
        ),
    )
    .await;
    assert_eq!(adm_sc.status, StatusCode::BAD_REQUEST, "{}", adm_sc.text());
    assert_eq!(adm_sc.json()["code"], "snapshot-conflict");
}

#[tokio::test]
async fn linked_data_enforces_query_memory_pool_grant_across_blocking() {
    let dir = tempfile::tempdir().unwrap();
    // Small query pool (32 KiB) with NO per-query memory ceiling: only the pool grant
    // can refuse oversized allocations, proving the admitted grant survives `blocking(...)`.
    let mut st = AppState::with_memory(
        dir.path(),
        StoreOptions::default(),
        Duration::from_secs(30),
        Some(32 * 1024),
    )
    .unwrap();
    st.limits.query_memory_bytes = None;
    st.set_phase(crate::obs::Phase::Ready);
    let st = Arc::new(st);

    let ds = st.attach("poolds", DbType::Mem, None).unwrap();
    let big_lit = "x".repeat(48 * 1024);
    let trig = format!(
        "<http://example.org/pool/g> {{\n  <http://example.org/pool/Big> <http://www.w3.org/2000/01/rdf-schema#comment> \"{big_lit}\" .\n}}\n"
    );
    ds.store
        .load(&[Source::from_bytes(trig.into_bytes(), RdfFormat::TriG, None)])
        .unwrap();

    let app = crate::http::router(st.clone());
    let reg = send(
        &app,
        post_json(
            "/$/resources",
            json!({
                "id": "poolv",
                "kind": "vocabulary",
                "title": "Pool Vocab",
                "namespaces": [{"prefix": "pool", "iri": "http://example.org/pool/"}],
                "graphs": [{"dataset": "poolds", "graph": "http://example.org/pool/g"}]
            }),
            None,
        ),
    )
    .await;
    assert_eq!(reg.status, StatusCode::CREATED, "{}", reg.text());

    let r = send(
        &app,
        get_req(
            &format!("/$/ld/doc?iri={}", enc("http://example.org/pool/Big")),
            Some("text/turtle"),
            None,
        ),
    )
    .await;
    assert_eq!(
        r.status,
        StatusCode::INSUFFICIENT_STORAGE,
        "expected 507 from memory pool grant, got {}: {}",
        r.status,
        r.text()
    );
}

#[tokio::test]
async fn html_rendering_enforces_max_result_bytes_during_construction() {
    let dir = tempfile::tempdir().unwrap();
    let mut st =
        AppState::new(dir.path(), StoreOptions::default(), Duration::from_secs(30)).unwrap();
    st.limits.max_result_bytes = Some(1024);
    let st = Arc::new(st);

    let ds = st.attach("htmlds", DbType::Mem, None).unwrap();
    let big = "a".repeat(32 * 1024);
    let trig = format!(
        "<http://example.org/h/g> {{\n  <http://example.org/h/Big> <http://www.w3.org/2000/01/rdf-schema#comment> \"{big}\" .\n}}\n"
    );
    ds.store
        .load(&[Source::from_bytes(trig.into_bytes(), RdfFormat::TriG, None)])
        .unwrap();

    let app = crate::http::router(st.clone());
    send(
        &app,
        post_json(
            "/$/resources",
            json!({
                "id": "hv",
                "kind": "vocabulary",
                "title": "HTML Vocab",
                "namespaces": [{"prefix": "h", "iri": "http://example.org/h/"}],
                "graphs": [{"dataset": "htmlds", "graph": "http://example.org/h/g"}]
            }),
            None,
        ),
    )
    .await;

    let r = send(
        &app,
        get_req(
            &format!("/$/ld/doc?iri={}", enc("http://example.org/h/Big")),
            Some("text/html"),
            None,
        ),
    )
    .await;
    assert_eq!(r.status, StatusCode::INSUFFICIENT_STORAGE, "{}", r.text());
}

#[tokio::test]
async fn accept_negotiation_combines_headers_checks_params_and_emits_only_offered_types() {
    // No synthetic application/xhtml+xml offer.
    assert_eq!(negotiate_format(Some("application/xhtml+xml")), None);
    // Explicit text/html;q=0 is not bypassed by application/*.
    assert_eq!(
        negotiate_format(Some("text/html;q=0, application/*;q=0.5")),
        Some(Format::JsonLd)
    );
    // Incompatible media parameter (charset) is rejected.
    assert_eq!(
        negotiate_format(Some("text/html; charset=iso-8859-1")),
        None
    );
    assert_eq!(
        negotiate_format(Some("text/html; charset=\"utf-8\"")),
        Some(Format::Html)
    );
    // Parameterized range is more specific than bare media type per RFC 9110 §12.5.1.
    assert_eq!(
        negotiate_format(Some(
            "text/html; charset=utf-8; q=0.5, text/html; q=0.9, text/turtle; q=0.7"
        )),
        Some(Format::Turtle)
    );

    // Multiple Accept header fields are combined per RFC 9110 §5.3.
    let dir = tempfile::tempdir().unwrap();
    let st = Arc::new(
        AppState::new(dir.path(), StoreOptions::default(), Duration::from_secs(30)).unwrap(),
    );
    let ds = st.attach("accds", DbType::Mem, None).unwrap();
    ds.store
        .load(&[Source::from_bytes(
            b"<http://example.org/acc/g> { <http://example.org/acc/T> <http://www.w3.org/2000/01/rdf-schema#label> \"T\" . }"
                .to_vec(),
            RdfFormat::TriG,
            None,
        )])
        .unwrap();
    let app = crate::http::router(st.clone());
    send(
        &app,
        post_json(
            "/$/resources",
            json!({
                "id": "accv",
                "kind": "vocabulary",
                "title": "Accept Vocab",
                "namespaces": [{"prefix": "acc", "iri": "http://example.org/acc/"}],
                "graphs": [{"dataset": "accds", "graph": "http://example.org/acc/g"}]
            }),
            None,
        ),
    )
    .await;

    let req = Request::get(format!("/$/ld/doc?iri={}", enc("http://example.org/acc/T")))
        .header(header::ACCEPT, "image/png")
        .header(header::ACCEPT, "text/turtle")
        .body(Body::empty())
        .unwrap();
    let resp = send(&app, req).await;
    assert_eq!(resp.status, StatusCode::OK, "{}", resp.text());
    assert_eq!(
        resp.header("content-type"),
        Some("text/turtle; charset=utf-8")
    );
}

#[tokio::test]
async fn hash_terminated_version_iri_resolves_with_and_without_fragment() {
    let dir = tempfile::tempdir().unwrap();
    let st = Arc::new(
        AppState::new(dir.path(), StoreOptions::default(), Duration::from_secs(30)).unwrap(),
    );
    st.create("hverds", DbType::Persistent).unwrap();
    let app = crate::http::router(st.clone());

    let data = r#"
@prefix owl: <http://www.w3.org/2002/07/owl#> .
@prefix rdfs: <http://www.w3.org/2000/01/rdf-schema#> .
<http://example.org/hver> a owl:Ontology ;
  owl:versionIRI <http://example.org/hver/1.0#> ;
  rdfs:label "Hash Versioned Ontology v1" .
<http://example.org/hver/1.0#Item> a owl:Class ;
  rdfs:label "Hash Version Item" .
"#;
    let put = send(
        &app,
        Request::put("/hverds/data?graph=http%3A%2F%2Fexample.org%2Fhver%2Fg")
            .header(header::CONTENT_TYPE, "text/turtle")
            .body(Body::from(data))
            .unwrap(),
    )
    .await;
    assert!(put.status.is_success(), "{}", put.text());

    let snap = send(
        &app,
        post_json("/$/snapshots/hverds", json!({"name": "hv1"}), None),
    )
    .await;
    assert_eq!(snap.status, StatusCode::CREATED, "{}", snap.text());

    let reg = send(
        &app,
        post_json(
            "/$/resources",
            json!({
                "id": "hverv",
                "kind": "vocabulary",
                "title": "Hash Versioned",
                "namespaces": [{"prefix": "hver", "iri": "http://example.org/hver#"}],
                "graphs": [{"dataset": "hverds", "graph": "http://example.org/hver/g"}],
                "versions": [{
                    "version": "1.0",
                    "versionIri": "http://example.org/hver/1.0#",
                    "snapshot": "snapshot:hv1"
                }]
            }),
            None,
        ),
    )
    .await;
    assert_eq!(reg.status, StatusCode::CREATED, "{}", reg.text());

    // Both fragmentless document form and trailing-# spelling answer 200 OK with the
    // ontology assertion and hash-document terms.
    for iri in [
        "http://example.org/hver/1.0",
        "http://example.org/hver/1.0#",
    ] {
        let r = send(
            &app,
            get_req(
                &format!("/$/ld?iri={}", enc(iri)),
                Some("text/turtle"),
                None,
            ),
        )
        .await;
        assert_eq!(r.status, StatusCode::OK, "{iri}: {}", r.text());
        assert_eq!(r.header("sparkles-at"), Some("snapshot:hv1"));
        // Direct versionIri request must not emit a self-referential rel="original" link.
        assert!(
            !r.header("link").unwrap_or("").contains("rel=\"original\""),
            "unexpected self-original link: {:?}",
            r.header("link")
        );
        let body = r.text();
        assert!(body.contains("Hash Versioned Ontology v1"), "{iri}: {body}");
        assert!(body.contains("Hash Version Item"), "{iri}: {body}");
    }
}

#[tokio::test]
async fn multi_dataset_global_cap_enforces_strictest_limit_in_both_orders_and_history_headers() {
    let dir = tempfile::tempdir().unwrap();
    let st = Arc::new(
        AppState::new(dir.path(), StoreOptions::default(), Duration::from_secs(30)).unwrap(),
    );
    st.create("mds_hi", DbType::Persistent).unwrap();
    st.create("mds_lo", DbType::Persistent).unwrap();
    let app = crate::http::router(st.clone());

    let hi_ttl = r#"
@prefix rdfs: <http://www.w3.org/2000/01/rdf-schema#> .
<http://example.org/multi/Term>
  rdfs:label "L1", "L2", "L3", "L4", "L5" .
"#;
    let lo_ttl = r#"
@prefix rdfs: <http://www.w3.org/2000/01/rdf-schema#> .
<http://example.org/multi/Term>
  rdfs:comment "C1" .
"#;
    for (ds, ttl) in [("mds_hi", hi_ttl), ("mds_lo", lo_ttl)] {
        let put = send(
            &app,
            Request::put(format!(
                "/{ds}/data?graph=http%3A%2F%2Fexample.org%2Fmulti%2Fg"
            ))
            .header(header::CONTENT_TYPE, "text/turtle")
            .body(Body::from(ttl))
            .unwrap(),
        )
        .await;
        assert!(put.status.is_success(), "{}", put.text());
    }
    // Configure mds_hi maxTriples=10 and mds_lo maxTriples=2.
    for (ds, max_t) in [("mds_hi", 10), ("mds_lo", 2)] {
        let put = send(
            &app,
            Request::put(format!("/$/describe/{ds}"))
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(json!({"maxTriples": max_t}).to_string()))
                .unwrap(),
        )
        .await;
        assert_eq!(put.status, StatusCode::OK, "{}", put.text());
    }

    // Order A: high-limit dataset first, low-limit dataset second.
    let reg = send(
        &app,
        post_json(
            "/$/resources",
            json!({
                "id": "multiv",
                "kind": "vocabulary",
                "title": "Multi Vocab",
                "namespaces": [{"prefix": "mul", "iri": "http://example.org/multi/"}],
                "graphs": [
                    {"dataset": "mds_hi", "graph": "http://example.org/multi/g"},
                    {"dataset": "mds_lo", "graph": "http://example.org/multi/g"}
                ]
            }),
            None,
        ),
    )
    .await;
    assert_eq!(reg.status, StatusCode::CREATED, "{}", reg.text());

    let r1 = send(
        &app,
        get_req(
            &format!("/$/ld/doc?iri={}", enc("http://example.org/multi/Term")),
            Some("application/n-triples"),
            None,
        ),
    )
    .await;
    assert_eq!(r1.status, StatusCode::OK, "{}", r1.text());
    assert_eq!(r1.header("sparkles-describe-truncated"), Some("true"));
    let count1 = r1.text().lines().filter(|l| !l.trim().is_empty()).count();
    assert_eq!(count1, 2, "{}", r1.text());

    // Order B: swap dataset ceilings so the first visited dataset has maxTriples=2 and the
    // second has maxTriples=10.
    for (ds, max_t) in [("mds_hi", 2), ("mds_lo", 10)] {
        let put = send(
            &app,
            Request::put(format!("/$/describe/{ds}"))
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(json!({"maxTriples": max_t}).to_string()))
                .unwrap(),
        )
        .await;
        assert_eq!(put.status, StatusCode::OK, "{}", put.text());
    }

    let r2 = send(
        &app,
        get_req(
            &format!("/$/ld/doc?iri={}", enc("http://example.org/multi/Term")),
            Some("application/n-triples"),
            None,
        ),
    )
    .await;
    assert_eq!(r2.status, StatusCode::OK, "{}", r2.text());
    assert_eq!(r2.header("sparkles-describe-truncated"), Some("true"));
    let count2 = r2.text().lines().filter(|l| !l.trim().is_empty()).count();
    assert_eq!(count2, 2, "{}", r2.text());

    // Multi-dataset historical merge where both datasets contribute must omit single-dataset
    // Sparkles-At / Sparkles-Head headers.
    for ds in ["mds_hi", "mds_lo"] {
        let put = send(
            &app,
            Request::put(format!("/$/describe/{ds}"))
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(json!({"maxTriples": 20}).to_string()))
                .unwrap(),
        )
        .await;
        assert_eq!(put.status, StatusCode::OK, "{}", put.text());
    }
    let hist = send(
        &app,
        get_req(
            &format!(
                "/$/ld/doc?iri={}&at=commit:1",
                enc("http://example.org/multi/Term")
            ),
            Some("application/n-triples"),
            None,
        ),
    )
    .await;
    assert_eq!(hist.status, StatusCode::OK, "{}", hist.text());
    assert!(
        hist.text().contains("L1") && hist.text().contains("C1"),
        "{}",
        hist.text()
    );
    assert!(
        hist.header("sparkles-at").is_none(),
        "merged multi-dataset response must not emit one dataset's Sparkles-At"
    );
}

#[tokio::test]
async fn cross_vocabulary_html_links_do_not_propagate_version_param() {
    let dir = tempfile::tempdir().unwrap();
    let st = Arc::new(
        AppState::new(dir.path(), StoreOptions::default(), Duration::from_secs(30)).unwrap(),
    );
    st.create("xds", DbType::Persistent).unwrap();
    let app = crate::http::router(st.clone());

    let trig = r#"
@prefix rdfs: <http://www.w3.org/2000/01/rdf-schema#> .
<http://example.org/x/g1> {
  <http://example.org/v1/A>
    rdfs:label "Term A" ;
    rdfs:seeAlso <http://example.org/v1/B>, <http://example.org/v2/Other> .
}
<http://example.org/x/g2> {
  <http://example.org/v2/Other> rdfs:label "Other" .
}
"#;
    let put = send(
        &app,
        Request::put("/xds/data")
            .header(header::CONTENT_TYPE, "application/trig")
            .body(Body::from(trig))
            .unwrap(),
    )
    .await;
    assert!(put.status.is_success(), "{}", put.text());

    send(
        &app,
        post_json("/$/snapshots/xds", json!({"name": "s1"}), None),
    )
    .await;
    // Create a second commit so snapshot:s1 is historical (testing rel="original" stripping version).
    send(
        &app,
        Request::post("/xds/data?graph=http%3A%2F%2Fexample.org%2Fx%2Fg2")
            .header(header::CONTENT_TYPE, "text/turtle")
            .body(Body::from(
                "<http://example.org/v2/Other> <http://www.w3.org/2000/01/rdf-schema#comment> \"v2\" .",
            ))
            .unwrap(),
    )
    .await;

    send(
        &app,
        post_json(
            "/$/resources",
            json!({
                "id": "vocab1",
                "kind": "vocabulary",
                "title": "Vocab 1",
                "namespaces": [{"prefix": "v1", "iri": "http://example.org/v1/"}],
                "graphs": [{"dataset": "xds", "graph": "http://example.org/x/g1"}],
                "versions": [{"version": "1.0", "snapshot": "snapshot:s1"}]
            }),
            None,
        ),
    )
    .await;
    send(
        &app,
        post_json(
            "/$/resources",
            json!({
                "id": "vocab2",
                "kind": "vocabulary",
                "title": "Vocab 2",
                "namespaces": [{"prefix": "v2", "iri": "http://example.org/v2/"}],
                "graphs": [{"dataset": "xds", "graph": "http://example.org/x/g2"}]
            }),
            None,
        ),
    )
    .await;

    let resp = send(
        &app,
        get_req(
            &format!(
                "/$/ld/doc?iri={}&version=1.0",
                enc("http://example.org/v1/A")
            ),
            Some("text/html"),
            None,
        ),
    )
    .await;
    assert_eq!(resp.status, StatusCode::OK, "{}", resp.text());
    // rel="original" strips version=1.0 so it points to the live resource, not the Memento.
    let link = resp.header("link").unwrap_or("");
    assert!(
        link.contains(&format!(
            "</$/ld/doc?iri={}>; rel=\"original\"",
            enc("http://example.org/v1/A")
        )),
        "expected live rel=\"original\" without version=1.0: {link}"
    );

    let html = resp.text();
    let same_link = format!(
        "/$/ld?iri={}&amp;version=1.0",
        enc("http://example.org/v1/B")
    );
    let cross_link = format!("/$/ld?iri={}\"", enc("http://example.org/v2/Other"));
    assert!(
        html.contains(&same_link),
        "missing same-vocab version link:\n{html}"
    );
    assert!(
        html.contains(&cross_link),
        "cross-vocab link must omit version=1.0:\n{html}"
    );
}

/// Simulate Apache 2.4 `mod_rewrite` expansion of a `RewriteRule` substitution string
/// (`\%` -> `%`, `\$` -> `$`, `$1` replaced with `B`-escaped capture when `B` is set).
fn simulate_apache_rewrite(subst: &str, flags: &str, capture_1: Option<&str>) -> String {
    let has_b = flags.split(',').any(|f| f == "B");
    let has_ne = flags.split(',').any(|f| f == "NE");
    let mut out = String::new();
    let mut chars = subst.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\\' {
            if let Some(next) = chars.next() {
                out.push(next);
            }
        } else if c == '%' && chars.peek().is_some_and(char::is_ascii_digit) {
            // Unescaped %N expands as an empty RewriteCond backreference in Apache!
            let _ = chars.next();
        } else if c == '$' && chars.peek() == Some(&'1') {
            let _ = chars.next();
            if let Some(cap) = capture_1 {
                if has_b {
                    let escaped: String =
                        url::form_urlencoded::byte_serialize(cap.as_bytes()).collect();
                    out.push_str(&escaped);
                } else {
                    out.push_str(cap);
                }
            }
        } else {
            out.push(c);
        }
    }
    if !has_ne {
        // Without NE, Apache re-encodes '%' as '%25'.
        out = out.replace('%', "%25");
    }
    out
}

#[tokio::test]
async fn apache_recipe_round_trips_version_iri_and_punctuated_local_term() {
    let dir = tempfile::tempdir().unwrap();
    let st = Arc::new(
        AppState::new(dir.path(), StoreOptions::default(), Duration::from_secs(30)).unwrap(),
    );
    st.create("apds", DbType::Persistent).unwrap();
    let app = crate::http::router(st.clone());

    let trig = r#"
@prefix owl: <http://www.w3.org/2002/07/owl#> .
@prefix rdfs: <http://www.w3.org/2000/01/rdf-schema#> .
<http://example.org/ap/g> {
  <https://w3id.org/myorg/mv/> a owl:Ontology ;
    owl:versionIRI <https://w3id.org/myorg/mv/1.0> ;
    rdfs:label "Apache Vocab v1" .
  <https://w3id.org/myorg/mv/a&b?c=1> a owl:Class ;
    rdfs:label "Punctuated Term" .
}
"#;
    let put = send(
        &app,
        Request::put("/apds/data")
            .header(header::CONTENT_TYPE, "application/trig")
            .body(Body::from(trig))
            .unwrap(),
    )
    .await;
    assert!(put.status.is_success(), "{}", put.text());
    send(
        &app,
        post_json("/$/snapshots/apds", json!({"name": "v1"}), None),
    )
    .await;
    send(
        &app,
        post_json(
            "/$/resources",
            json!({
                "id": "apvocab",
                "kind": "vocabulary",
                "title": "Apache Vocab",
                "namespaces": [{"prefix": "mv", "iri": "https://w3id.org/myorg/mv/"}],
                "graphs": [{"dataset": "apds", "graph": "http://example.org/ap/g"}],
                "versions": [{
                    "version": "1.0",
                    "versionIri": "https://w3id.org/myorg/mv/1.0",
                    "snapshot": "snapshot:v1"
                }]
            }),
            None,
        ),
    )
    .await;

    let recipe = send(
        &app,
        get_req(
            "/$/resources/apvocab/recipes?kind=w3id&base=https://rdf.example.org",
            Some("text/plain"),
            None,
        ),
    )
    .await;
    assert_eq!(recipe.status, StatusCode::OK, "{}", recipe.text());
    let text = recipe.text();

    let mut ver_target = None;
    let mut term_target = None;
    for line in text.lines() {
        let Some(rest) = line.strip_prefix("RewriteRule ") else {
            continue;
        };
        let parts: Vec<&str> = rest.split_whitespace().collect();
        assert_eq!(parts.len(), 3, "malformed RewriteRule: {line}");
        let (pat, subst, flags) = (parts[0], parts[1], parts[2].trim_matches(['[', ']']));
        if pat == "^mv/1\\.0/?$" {
            ver_target = Some(simulate_apache_rewrite(subst, flags, None));
        } else if pat == "^mv/(.+)$" {
            // Apache decodes incoming `/myorg/mv/a%26b%3Fc=1` to `mv/a&b?c=1` before capture.
            term_target = Some(simulate_apache_rewrite(subst, flags, Some("a&b?c=1")));
        }
    }

    let ver_url = ver_target.expect("version RewriteRule missing");
    let ver_rel = ver_url
        .strip_prefix("https://rdf.example.org")
        .expect("base prefix");
    let ver_resp = send(&app, get_req(ver_rel, Some("text/turtle"), None)).await;
    assert_eq!(
        ver_resp.status,
        StatusCode::OK,
        "version redirect {ver_rel} failed: {}",
        ver_resp.text()
    );
    assert!(
        ver_resp.text().contains("Apache Vocab v1"),
        "{}",
        ver_resp.text()
    );

    let term_url = term_target.expect("term RewriteRule missing");
    let term_rel = term_url
        .strip_prefix("https://rdf.example.org")
        .expect("base prefix");
    let term_resp = send(&app, get_req(term_rel, Some("text/turtle"), None)).await;
    assert_eq!(
        term_resp.status,
        StatusCode::OK,
        "punctuated term redirect {term_rel} failed: {}",
        term_resp.text()
    );
    assert!(
        term_resp.text().contains("Punctuated Term"),
        "{}",
        term_resp.text()
    );

    // Root-path slash versionIri (`https://example.org/`) emits `^$` and `^(.+)$`, never `^/(.+)$`.
    send(
        &app,
        post_json(
            "/$/resources",
            json!({
                "id": "rootv",
                "kind": "vocabulary",
                "title": "Root Vocab",
                "namespaces": [{"prefix": "rv", "iri": "https://example.org/"}],
                "versions": [{"version": "1.0", "versionIri": "https://example.org/"}]
            }),
            None,
        ),
    )
    .await;
    let root_rec = send(
        &app,
        get_req(
            "/$/resources/rootv/recipes?kind=htaccess&base=https://rdf.example.org",
            Some("text/plain"),
            None,
        ),
    )
    .await;
    assert_eq!(root_rec.status, StatusCode::OK, "{}", root_rec.text());
    let root_text = root_rec.text();
    assert!(root_text.contains("RewriteRule ^$ "), "{root_text}");
    assert!(root_text.contains("RewriteRule ^(.+)$ "), "{root_text}");
    assert!(!root_text.contains("RewriteRule ^/"), "{root_text}");
}
