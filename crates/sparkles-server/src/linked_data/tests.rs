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
        ("/$/ld/search", "/$/ld/search?q=anatomy"),
        ("/$/ld/resources/{id}/doc", "/$/ld/resources/anatomy/doc"),
        ("/$/ontology/{ds}/doc", "/$/ontology/anatomy/doc"),
        ("/ols4/api/search", "/ols4/api/search?q=anatomy"),
        (
            "/ols4/api/ontologies/{id}/terms/{iri}",
            "/ols4/api/ontologies/anatomy/terms/urn%253Aexample",
        ),
        (
            "/ols4/api/ontologies/{id}/terms/{iri}/{relation}",
            "/ols4/api/ontologies/anatomy/terms/urn%253Aexample/parents",
        ),
        ("/$/ld/ns/{prefix}", "/$/ld/ns/ex"),
        ("/$/ld/ns/{prefix}/", "/$/ld/ns/ex/"),
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
    let ds = st.attach("recds", DbType::Mem, None).unwrap();
    let trig = r#"
@prefix owl: <http://www.w3.org/2002/07/owl#> .
@prefix rdfs: <http://www.w3.org/2000/01/rdf-schema#> .
<http://example.org/rec/g> {
  <https://w3id.org/myorg/mv/> a owl:Ontology ;
    rdfs:label "My Slash Root" .
  <https://w3id.org/myorg/mv/Person> a owl:Class ;
    rdfs:label "My Slash Person" .
  <https://w3id.org/myorg/mvh> a owl:Ontology ;
    owl:versionIRI <https://w3id.org/myorg/mvh/1.0#> ;
    rdfs:label "My Hash Vocab v1" .
  <https://w3id.org/myorg/mvh#Concept> a owl:Class ;
    rdfs:label "My Hash Concept" .
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
                "id": "myvocab",
                "kind": "vocabulary",
                "title": "My Vocab",
                "namespaces": [
                    {"prefix": "mv", "iri": "https://w3id.org/myorg/mv/"},
                    {"prefix": "mvh", "iri": "https://w3id.org/myorg/mvh#"}
                ],
                "graphs": [{"dataset": "recds", "graph": "http://example.org/rec/g"}],
                "versions": [
                    {
                        "version": "1.0",
                        "versionIri": "https://w3id.org/myorg/mv/1.0",
                        "snapshot": "snapshot:v1"
                    },
                    {
                        "version": "1.0-hash",
                        "versionIri": "https://w3id.org/myorg/mvh/1.0#"
                    }
                ]
            }),
            None,
        ),
    )
    .await;
    assert_eq!(reg.status, StatusCode::CREATED, "{}", reg.text());

    // Default w3id .htaccess text output: strips shared 'myorg' mount, matches raw
    // %{THE_REQUEST} paths and leaves content negotiation to Sparkles.
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
    assert!(text.contains("AllowEncodedSlashes NoDecode"), "{text}");
    assert!(
        text.contains(r"RewriteCond %{THE_REQUEST} ^\S+\s+/myorg/mv/1\.0(?:\s|$)"),
        "{text}"
    );
    assert!(!text.contains("RewriteCond %{HTTP_ACCEPT}"), "{text}");
    assert!(
        text.contains(
            "RewriteRule ^ https://rdf.example.org/$/ld/doc?iri=https\\%3A\\%2F\\%2Fw3id.org\\%2Fmyorg\\%2Fmv\\%2F1.0 [QSD,R=303,L,NE]"
        ),
        "{text}"
    );
    assert!(
        text.contains(
            "RewriteRule ^__sparkles_ld__/(.+)$ https://rdf.example.org/$/ld/doc?iri=https\\%3A\\%2F\\%2Fw3id.org\\%2Fmyorg\\%2Fmv\\%2F$1 [B,QSD,R=303,L,NE]"
        ),
        "{text}"
    );
    assert!(
        text.contains("RewriteRule ^ https://rdf.example.org/$/ld/ns/mvh [QSD,R=303,L,NE]"),
        "{text}"
    );

    // PURL recipe JSON output uses PURL's supported `partial` type, strips `#` from
    // hash-terminated version IRIs, and its slash partial target works for both the
    // empty-suffix root (`/$/ld/ns/mv/`) and subterms (`/$/ld/ns/mv/Person`).
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
    assert_eq!(j["versions"].as_array().unwrap().len(), 2);
    let purl_content = j["content"].as_str().unwrap();
    assert!(
        purl_content
            .contains("partial https://w3id.org/myorg/mv/ -> https://rdf.example.org/$/ld/ns/mv/"),
        "{purl_content}"
    );
    assert!(
        purl_content.contains("303 https://w3id.org/myorg/mvh/1.0 -> "),
        "hash-terminated versionIri PURL source must omit fragment:\n{purl_content}"
    );
    assert!(
        !purl_content.contains("303 https://w3id.org/myorg/mvh/1.0#"),
        "{purl_content}"
    );
    assert!(!purl_content.contains("303 partial"), "{purl_content}");

    // Follow exported PURL targets directly against Sparkles's router:
    // 1. Slash partial with empty suffix (`/$/ld/ns/mv/`) resolves identically to `/$/ld/ns/mv`.
    let slash_root = send(&app, get_req("/$/ld/ns/mv/", Some("text/turtle"), None)).await;
    assert_eq!(
        slash_root.status,
        StatusCode::SEE_OTHER,
        "PURL empty-suffix slash root failed: {}",
        slash_root.text()
    );
    let slash_root_doc = send(
        &app,
        get_req(
            slash_root.header("location").unwrap(),
            Some("text/turtle"),
            None,
        ),
    )
    .await;
    assert_eq!(
        slash_root_doc.status,
        StatusCode::OK,
        "{}",
        slash_root_doc.text()
    );
    assert!(
        slash_root_doc.text().contains("My Slash Root"),
        "{}",
        slash_root_doc.text()
    );

    // 2. Slash partial with term suffix (`/$/ld/ns/mv/Person`).
    let slash_term = send(
        &app,
        get_req("/$/ld/ns/mv/Person", Some("text/turtle"), None),
    )
    .await;
    assert_eq!(
        slash_term.status,
        StatusCode::SEE_OTHER,
        "{}",
        slash_term.text()
    );
    let slash_term_doc = send(
        &app,
        get_req(
            slash_term.header("location").unwrap(),
            Some("text/turtle"),
            None,
        ),
    )
    .await;
    assert_eq!(
        slash_term_doc.status,
        StatusCode::OK,
        "{}",
        slash_term_doc.text()
    );
    assert!(
        slash_term_doc.text().contains("My Slash Person"),
        "{}",
        slash_term_doc.text()
    );

    // 3. Hash version IRI PURL target answers 200 OK directly.
    let hver_target = j["versions"][1]["targetUrl"]
        .as_str()
        .unwrap()
        .strip_prefix("https://rdf.example.org")
        .unwrap();
    let hver_resp = send(&app, get_req(hver_target, Some("text/turtle"), None)).await;
    assert_eq!(hver_resp.status, StatusCode::OK, "{}", hver_resp.text());
    assert!(
        hver_resp.text().contains("My Hash Vocab v1"),
        "{}",
        hver_resp.text()
    );

    // Unsupported recipe inputs (non-http(s) scheme or query-bearing IRI) are rejected explicitly.
    for (id, ns_iri) in [
        ("urnvocab", "urn:example:vocab:"),
        ("qvocab", "https://example.org/q?x=1"),
    ] {
        let cr = send(
            &app,
            post_json(
                "/$/resources",
                json!({
                    "id": id,
                    "kind": "vocabulary",
                    "title": id,
                    "namespaces": [{"prefix": id, "iri": ns_iri}]
                }),
                None,
            ),
        )
        .await;
        assert_eq!(cr.status, StatusCode::CREATED, "{}", cr.text());
        let bad = send(
            &app,
            get_req(
                &format!("/$/resources/{id}/recipes?base=https://rdf.example.org"),
                None,
                None,
            ),
        )
        .await;
        assert_eq!(bad.status, StatusCode::BAD_REQUEST, "{id}: {}", bad.text());
        assert_eq!(bad.json()["code"], "bad-recipe");
    }

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
    std::fs::write(&auth_path, &auth_toml).unwrap();
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

    // 8. Unrelated readable owl:Ontology / skos:ConceptScheme and unlinked vocabulary
    //    terms in the same allowed graph must NOT authorize a private hash-version lookup
    //    or leak catalog metadata / history headers / 400 conflict responses.
    let hash_ver_trig = r#"
@prefix owl: <http://www.w3.org/2002/07/owl#> .
@prefix skos: <http://www.w3.org/2004/02/skos/core#> .
@prefix rdfs: <http://www.w3.org/2000/01/rdf-schema#> .
@prefix acl: <http://example.org/acl/> .
<http://example.org/g/public> {
  <http://unrelated.example/ont> a owl:Ontology ;
    rdfs:label "Unrelated Public Ontology" .
  <http://unrelated.example/scheme> a skos:ConceptScheme ;
    rdfs:label "Unrelated Public Scheme" .
  <http://example.org/aclh#PublicTerm> rdfs:label "Unlinked Hash Term" .
  <http://example.org/aclh/v1#> acl:secretNote "HIDDEN HASH VERSION META" .
}
"#;
    ds.store
        .load(&[Source::from_bytes(
            hash_ver_trig.as_bytes().to_vec(),
            RdfFormat::TriG,
            None,
        )])
        .unwrap();
    let reg_hv = send(
        &app,
        post_json(
            "/$/resources",
            json!({
                "id": "aclhvocab",
                "kind": "vocabulary",
                "title": "Classified Hash Vocabulary",
                "namespaces": [{"prefix": "aclh", "iri": "http://example.org/aclh#"}],
                "graphs": [{"dataset": "acl_ds", "graph": "http://example.org/g/public"}],
                "versions": [{
                    "version": "1.0",
                    "versionIri": "http://example.org/aclh/v1#",
                    "snapshot": "commit:3"
                }]
            }),
            Some(&admin_auth),
        ),
    )
    .await;
    assert_eq!(reg_hv.status, StatusCode::CREATED, "{}", reg_hv.text());

    for uri in [
        format!("/$/ld?iri={}", enc("http://example.org/aclh/v1")),
        format!("/$/ld?iri={}", enc("http://example.org/aclh/v1#")),
        format!(
            "/$/ld/doc?iri={}&version=2.0",
            enc("http://example.org/aclh/v1")
        ),
        format!(
            "/$/ld/doc?iri={}&at=commit:999",
            enc("http://example.org/aclh/v1#")
        ),
    ] {
        let r = send(&app, get_req(&uri, Some("text/turtle"), Some(&pub_auth))).await;
        assert_eq!(
            r.status,
            StatusCode::NOT_FOUND,
            "unrelated graph ontology authorized private hash version on {uri}: {}",
            r.text()
        );
        assert!(r.header("sparkles-at").is_none());
        assert!(!r.text().contains("Classified Hash Vocabulary"));
        assert!(!r.text().contains("Unrelated Public Ontology"));
    }

    // admin CAN read the hidden hash version and receives neither unrelated ontology.
    let adm_hv = send(
        &app,
        get_req(
            &format!("/$/ld?iri={}", enc("http://example.org/aclh/v1")),
            Some("text/turtle"),
            Some(&admin_auth),
        ),
    )
    .await;
    assert_eq!(adm_hv.status, StatusCode::OK, "{}", adm_hv.text());
    assert!(
        adm_hv.text().contains("HIDDEN HASH VERSION META"),
        "{}",
        adm_hv.text()
    );
    assert!(
        !adm_hv.text().contains("Unrelated Public Ontology"),
        "unrelated ontology leaked into hash version DESCRIBE:\n{}",
        adm_hv.text()
    );
    let adm_hv_conflict = send(
        &app,
        get_req(
            &format!(
                "/$/ld/doc?iri={}&version=2.0",
                enc("http://example.org/aclh/v1")
            ),
            Some("text/turtle"),
            Some(&admin_auth),
        ),
    )
    .await;
    assert_eq!(adm_hv_conflict.status, StatusCode::BAD_REQUEST);
    assert_eq!(adm_hv_conflict.json()["code"], "version-conflict");
}

#[cfg(feature = "auth")]
#[tokio::test]
async fn hidden_target_cold_replay_budget_failure_returns_404_not_507() {
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
dataset = "cold_ds"
predicates = ["http://example.org/cold/secretNote"]

[[users]]
name = "admin"
password = "{admin}"
server = ["server-admin"]

[[users]]
name = "pub_reader"
password = "{pub_reader}"
[[users.grants]]
dataset = "cold_ds"
level = "read"
graphs = ["http://example.org/cold/g"]
"#,
        admin = h("admin-pw"),
        pub_reader = h("pub-pw"),
    );
    let auth_path = dir.path().join("auth.toml");
    std::fs::write(&auth_path, &auth_toml).unwrap();
    let auth = crate::auth::load(Some(&auth_path), dir.path(), "127.0.0.1", false).unwrap();

    let basic = |u: &str, pw: &str| {
        format!(
            "Basic {}",
            base64::engine::general_purpose::STANDARD.encode(format!("{u}:{pw}"))
        )
    };
    let admin_auth = basic("admin", "admin-pw");
    let pub_auth = basic("pub_reader", "pub-pw");

    // Populate a persistent dataset with enough quads in commit 1 that cold replay exceeds
    // a small query memory ceiling, then advance to commit 2 so snapshot:cold is historical.
    {
        let mut st =
            AppState::new(dir.path(), StoreOptions::default(), Duration::from_secs(30)).unwrap();
        st.auth = auth.clone();
        let st = Arc::new(st);
        st.create("cold_ds", DbType::Persistent).unwrap();
        let app = crate::http::router(st.clone());

        let big_pub = "p".repeat(8 * 1024);
        let big_secret = "s".repeat(8 * 1024);
        let mut ttl = format!(
            "@prefix rdfs: <http://www.w3.org/2000/01/rdf-schema#> .\n\
             @prefix cold: <http://example.org/cold/> .\n\
             cold:PublicTerm rdfs:label \"Public Term\" ;\n\
               rdfs:comment \"{big_pub}\" .\n\
             cold:v1 cold:secretNote \"{big_secret}\" .\n"
        );
        for i in 0..400 {
            ttl.push_str(&format!(
                "cold:Pad{i} rdfs:comment \"padding-entry-{i:04}-payload-for-wal-replay\" .\n"
            ));
        }
        let put1 = send(
            &app,
            Request::put("/cold_ds/data?graph=http%3A%2F%2Fexample.org%2Fcold%2Fg")
                .header(header::CONTENT_TYPE, "text/turtle")
                .header(header::AUTHORIZATION, &admin_auth)
                .body(Body::from(ttl))
                .unwrap(),
        )
        .await;
        assert!(put1.status.is_success(), "{}", put1.text());

        let snap = send(
            &app,
            post_json(
                "/$/snapshots/cold_ds",
                json!({"name": "cold"}),
                Some(&admin_auth),
            ),
        )
        .await;
        assert_eq!(snap.status, StatusCode::CREATED, "{}", snap.text());

        // Commit 2 keeps a small live PublicTerm (for pub_reader) and a hidden v1 triple
        // (for admin only) at head, while snapshot:cold holds the larger historical state
        // and requires WAL replay when cold.
        let put2 = send(
            &app,
            Request::put("/cold_ds/data?graph=http%3A%2F%2Fexample.org%2Fcold%2Fg")
                .header(header::CONTENT_TYPE, "text/turtle")
                .header(header::AUTHORIZATION, &admin_auth)
                .body(Body::from(
                    "<http://example.org/cold/PublicTerm> <http://www.w3.org/2000/01/rdf-schema#label> \"Public Term\" .\n\
                     <http://example.org/cold/v1> <http://example.org/cold/secretNote> \"HIDDEN LIVE\" .\n",
                ))
                .unwrap(),
        )
        .await;
        assert!(put2.status.is_success(), "{}", put2.text());

        let reg = send(
            &app,
            post_json(
                "/$/resources",
                json!({
                    "id": "coldv",
                    "kind": "vocabulary",
                    "title": "Cold Vocab",
                    "namespaces": [{"prefix": "cold", "iri": "http://example.org/cold/"}],
                    "graphs": [{"dataset": "cold_ds", "graph": "http://example.org/cold/g"}],
                    "versions": [{
                        "version": "1.0",
                        "versionIri": "http://example.org/cold/v1",
                        "snapshot": "snapshot:cold"
                    }]
                }),
                Some(&admin_auth),
            ),
        )
        .await;
        assert_eq!(reg.status, StatusCode::CREATED, "{}", reg.text());
    }

    let hidden_uris = [
        format!("/$/ld?iri={}", enc("http://example.org/cold/v1")),
        format!("/$/ld/doc?iri={}", enc("http://example.org/cold/v1")),
        format!(
            "/$/ld/doc?iri={}&version=2.0",
            enc("http://example.org/cold/v1")
        ),
        format!(
            "/$/ld/doc?iri={}&at=commit:999",
            enc("http://example.org/cold/v1")
        ),
    ];

    // 1. Cold replay exceeds the 24 KiB cap inside `snapshot_for_limited`.
    {
        let mut st =
            AppState::new(dir.path(), StoreOptions::default(), Duration::from_secs(30)).unwrap();
        st.auth = auth.clone();
        st.limits.query_memory_bytes = Some(24 * 1024);
        let st = Arc::new(st);
        let cold_ds = crate::http::dataset(&st, "cold_ds")
            .ok()
            .expect("cold_ds dataset");
        assert!(
            matches!(
                cold_ds.store.snapshot_at_limited_bytes(
                    &sparkles::history::At::Snapshot("cold".to_string()),
                    &sparkles::history::HistoryOptions::default(),
                    24 * 1024,
                ),
                Err(sparkles::Error::BudgetExceeded(b))
                    if b.kind == sparkles::BudgetKind::Memory
            ),
            "cold replay must exceed 24 KiB"
        );
        let app = crate::http::router(st.clone());

        for uri in &hidden_uris {
            let r = send(&app, get_req(uri, Some("text/turtle"), Some(&pub_auth))).await;
            assert_eq!(
                r.status,
                StatusCode::NOT_FOUND,
                "cold-replay-exceeds-cap disclosed hidden catalog entry on {uri}: {}",
                r.text()
            );
            assert_eq!(r.json()["code"], "linked-data-not-found");
            assert!(
                !r.text().contains("requested"),
                "cold-replay-exceeds-cap: {}",
                r.text()
            );
        }

        let auth_cold = send(
            &app,
            get_req(
                &format!(
                    "/$/ld/doc?iri={}&at=snapshot:cold",
                    enc("http://example.org/cold/PublicTerm")
                ),
                Some("text/turtle"),
                Some(&pub_auth),
            ),
        )
        .await;
        assert_eq!(
            auth_cold.status,
            StatusCode::INSUFFICIENT_STORAGE,
            "cold-replay-exceeds-cap: {}",
            auth_cold.text()
        );
    }

    // 2. Use an in-memory retained snapshot whose retained delta (~60+ KiB) exceeds the
    //    live 1-triple probe's memory footprint so `snapshot_for_limited` succeeds under
    //    `mem_cap = mem_retained + 1024`, the post-replay ACL-filtered `sparql::query()`
    //    fails under the 1 KiB remainder, and `live_is_readable` still succeeds under the
    //    full `mem_cap` after `snap_charge` is released.
    {
        let mem_dir = tempfile::tempdir().unwrap();
        let mem_auth_path = mem_dir.path().join("auth.toml");
        std::fs::write(&mem_auth_path, &auth_toml).unwrap();
        let mem_auth =
            crate::auth::load(Some(&mem_auth_path), mem_dir.path(), "127.0.0.1", false).unwrap();

        let mut st = AppState::new(
            mem_dir.path(),
            StoreOptions::default(),
            Duration::from_secs(30),
        )
        .unwrap();
        st.auth = mem_auth.clone();
        let st = Arc::new(st);
        st.create("cold_ds", DbType::Mem).unwrap();
        let setup_app = crate::http::router(st.clone());

        let big_pub = "p".repeat(8 * 1024);
        let big_secret = "s".repeat(8 * 1024);
        let mut ttl = format!(
            "@prefix rdfs: <http://www.w3.org/2000/01/rdf-schema#> .\n\
             @prefix cold: <http://example.org/cold/> .\n\
             cold:PublicTerm rdfs:label \"Public Term\" ;\n\
               rdfs:comment \"{big_pub}\" .\n\
             cold:v1 cold:secretNote \"{big_secret}\" .\n"
        );
        for i in 0..400 {
            ttl.push_str(&format!(
                "cold:Pad{i} rdfs:comment \"padding-entry-{i:04}-payload-for-wal-replay\" .\n"
            ));
        }
        let put1 = send(
            &setup_app,
            Request::put("/cold_ds/data?graph=http%3A%2F%2Fexample.org%2Fcold%2Fg")
                .header(header::CONTENT_TYPE, "text/turtle")
                .header(header::AUTHORIZATION, &admin_auth)
                .body(Body::from(ttl))
                .unwrap(),
        )
        .await;
        assert!(put1.status.is_success(), "{}", put1.text());

        let snap = send(
            &setup_app,
            post_json(
                "/$/snapshots/cold_ds",
                json!({"name": "cold"}),
                Some(&admin_auth),
            ),
        )
        .await;
        assert_eq!(snap.status, StatusCode::CREATED, "{}", snap.text());

        let put2 = send(
            &setup_app,
            Request::put("/cold_ds/data?graph=http%3A%2F%2Fexample.org%2Fcold%2Fg")
                .header(header::CONTENT_TYPE, "text/turtle")
                .header(header::AUTHORIZATION, &admin_auth)
                .body(Body::from(
                    "<http://example.org/cold/PublicTerm> <http://www.w3.org/2000/01/rdf-schema#label> \"Public Term\" .\n\
                     <http://example.org/cold/v1> <http://example.org/cold/secretNote> \"HIDDEN LIVE\" .\n",
                ))
                .unwrap(),
        )
        .await;
        assert!(put2.status.is_success(), "{}", put2.text());

        let reg = send(
            &setup_app,
            post_json(
                "/$/resources",
                json!({
                    "id": "coldv",
                    "kind": "vocabulary",
                    "title": "Cold Vocab",
                    "namespaces": [{"prefix": "cold", "iri": "http://example.org/cold/"}],
                    "graphs": [{"dataset": "cold_ds", "graph": "http://example.org/cold/g"}],
                    "versions": [{
                        "version": "1.0",
                        "versionIri": "http://example.org/cold/v1",
                        "snapshot": "snapshot:cold"
                    }]
                }),
                Some(&admin_auth),
            ),
        )
        .await;
        assert_eq!(reg.status, StatusCode::CREATED, "{}", reg.text());

        let mem_ds = crate::http::dataset(&st, "cold_ds")
            .ok()
            .expect("cold_ds dataset");
        let (_, _, mem_retained) = mem_ds
            .store
            .snapshot_at_limited_bytes(
                &sparkles::history::At::Snapshot("cold".to_string()),
                &sparkles::history::HistoryOptions::default(),
                64 * 1024 * 1024,
            )
            .unwrap();
        assert!(
            mem_retained > 24 * 1024,
            "expected in-memory retained snapshot > 24 KiB, got {mem_retained}"
        );
        // Bounded lookup needs 1 KiB (`READ_METADATA_BYTES`) headroom above `mem_retained`,
        // leaving 1 KiB for `sparql::query()` while full `mem_cap` (> 25 KiB) easily runs
        // the live 1-triple readability probe.
        let mem_cap = mem_retained + 1024;

        let (mem_snap, _, check_retained) = mem_ds
            .store
            .snapshot_at_limited_bytes(
                &sparkles::history::At::Snapshot("cold".to_string()),
                &sparkles::history::HistoryOptions::default(),
                mem_cap,
            )
            .expect("in-memory retained snapshot lookup must succeed under mem_cap");
        assert_eq!(check_retained, mem_retained);

        let mut hdrs = HeaderMap::new();
        hdrs.insert(header::AUTHORIZATION, pub_auth.parse().unwrap());
        let pub_principal = st
            .auth
            .as_ref()
            .unwrap()
            .authenticate(&hdrs, None, None, None, None, None)
            .await
            .unwrap()
            .principal;
        let mut pub_opts = CallerContext::for_caller(&st, &mem_ds, &pub_principal).read_options();
        pub_opts.default_graph_uris = vec!["http://example.org/cold/g".to_string()];
        pub_opts.default_graph_extra.clear();
        pub_opts.max_memory_bytes = Some(mem_cap - mem_retained);
        assert!(
            matches!(
                sparkles::sparql::query(
                    mem_snap.clone(),
                    "DESCRIBE <http://example.org/cold/PublicTerm>",
                    &pub_opts,
                ),
                Err(sparkles::Error::BudgetExceeded(b))
                    if b.kind == sparkles::BudgetKind::Memory
            ),
            "post-replay query for PublicTerm must fail under remaining budget {}",
            mem_cap - mem_retained
        );
        assert!(
            matches!(
                sparkles::sparql::query(
                    mem_snap,
                    "DESCRIBE <http://example.org/cold/v1>",
                    &pub_opts,
                ),
                Err(sparkles::Error::BudgetExceeded(b))
                    if b.kind == sparkles::BudgetKind::Memory
            ),
            "post-replay ACL-filtered query for hidden v1 must fail under remaining budget {}",
            mem_cap - mem_retained
        );
        let mut live_opts = pub_opts.clone();
        live_opts.max_memory_bytes = Some(mem_cap);
        live_opts.describe.max_triples = Some(1);
        let live_res = sparkles::sparql::query(
            mem_ds.store.snapshot(),
            "DESCRIBE <http://example.org/cold/PublicTerm>",
            &live_opts,
        )
        .expect("live 1-triple probe for PublicTerm must succeed under full mem_cap");
        assert!(!live_res.triples().is_empty());

        // Drop the temporary router and apply `mem_cap` directly on this exact `AppState`.
        drop(setup_app);
        drop(mem_ds);
        let mut st = Arc::try_unwrap(st).ok().expect("unique AppState");
        st.limits.query_memory_bytes = Some(mem_cap);
        let app = crate::http::router(Arc::new(st));

        for uri in &hidden_uris {
            let r = send(&app, get_req(uri, Some("text/turtle"), Some(&pub_auth))).await;
            assert_eq!(
                r.status,
                StatusCode::NOT_FOUND,
                "post-replay-query-budget-exhausted disclosed hidden catalog entry on {uri}: {}",
                r.text()
            );
            assert_eq!(r.json()["code"], "linked-data-not-found");
            assert!(
                !r.text().contains("requested"),
                "post-replay-query-budget-exhausted: {}",
                r.text()
            );
        }

        let auth_mem = send(
            &app,
            get_req(
                &format!(
                    "/$/ld/doc?iri={}&at=snapshot:cold",
                    enc("http://example.org/cold/PublicTerm")
                ),
                Some("text/turtle"),
                Some(&pub_auth),
            ),
        )
        .await;
        assert_eq!(
            auth_mem.status,
            StatusCode::INSUFFICIENT_STORAGE,
            "post-replay-query-budget-exhausted: {}",
            auth_mem.text()
        );
    }

    // 3. Cover the defensive zero-remaining-budget (`charge.engine_budget()`) arm
    //    separately in both `probe_one_dataset` (`redirect_slash: true`) and
    //    `resolve_and_describe` (`redirect_slash: false`), verifying charge release
    //    before `live_is_readable` and clean pool accounting.
    {
        let mut st = AppState::with_memory(
            dir.path(),
            StoreOptions::default(),
            Duration::from_secs(30),
            Some(256 * 1024),
        )
        .unwrap();
        st.auth = auth.clone();
        st.limits.query_memory_bytes = Some(128 * 1024);
        let st = Arc::new(st);
        let warm_ds = crate::http::dataset(&st, "cold_ds")
            .ok()
            .expect("cold_ds dataset");
        warm_ds
            .store
            .snapshot_at_limited_bytes(
                &sparkles::history::At::Snapshot("cold".to_string()),
                &sparkles::history::HistoryOptions::default(),
                64 * 1024 * 1024,
            )
            .unwrap();

        let mut hdrs = HeaderMap::new();
        hdrs.insert(header::AUTHORIZATION, pub_auth.parse().unwrap());
        let pub_principal = st
            .auth
            .as_ref()
            .unwrap()
            .authenticate(&hdrs, None, None, None, None, None)
            .await
            .unwrap()
            .principal;
        let catalog = st.resources.read().unwrap();
        let pool = st.catalog.pool().expect("query pool");
        let hooks = super::TestHooks {
            expire_deadline_before_dataset: None,
            exhaust_budget_after_settle: true,
        };

        for redirect_slash in [true, false] {
            let grant = Arc::new(pool.empty_grant());
            let err = super::resolve_and_describe_with_hooks(
                &st,
                &pub_principal,
                &catalog,
                LookupTarget::Iri("http://example.org/cold/v1".to_string()),
                None,
                None,
                &crate::http::Params::default(),
                Some(grant),
                redirect_slash,
                hooks,
            )
            .err()
            .expect("hidden v1 must fail");
            let resp = axum::response::IntoResponse::into_response(err);
            assert_eq!(
                resp.status(),
                StatusCode::NOT_FOUND,
                "defensive zero-budget arm disclosed hidden v1 (redirect_slash={redirect_slash})"
            );
            assert_eq!(pool.granted(), 0, "hidden v1 leaked pool charge");

            let grant = Arc::new(pool.empty_grant());
            let err = super::resolve_and_describe_with_hooks(
                &st,
                &pub_principal,
                &catalog,
                LookupTarget::Iri("http://example.org/cold/PublicTerm".to_string()),
                None,
                Some(sparkles::history::At::Snapshot("cold".to_string())),
                &crate::http::Params::default(),
                Some(grant),
                redirect_slash,
                hooks,
            )
            .err()
            .expect("PublicTerm must fail with 507");
            let resp = axum::response::IntoResponse::into_response(err);
            assert_eq!(
                resp.status(),
                StatusCode::INSUFFICIENT_STORAGE,
                "defensive zero-budget arm must release snap_charge before live_is_readable (redirect_slash={redirect_slash})"
            );
            assert_eq!(pool.granted(), 0, "PublicTerm leaked pool charge");
        }
    }

    // 4. Expired deadline on historical lookup must also stay 404 (never 408) for a hidden target.
    let mut st_to =
        AppState::new(dir.path(), StoreOptions::default(), Duration::from_nanos(1)).unwrap();
    st_to.auth = auth;
    let app_to = crate::http::router(Arc::new(st_to));
    for uri in &hidden_uris {
        let r = send(&app_to, get_req(uri, Some("text/turtle"), Some(&pub_auth))).await;
        assert_eq!(
            r.status,
            StatusCode::NOT_FOUND,
            "expired deadline disclosed hidden catalog entry on {uri}: {}",
            r.text()
        );
        assert_eq!(r.json()["code"], "linked-data-not-found");
    }
}

#[tokio::test]
async fn multi_dataset_historical_deadline_expiry_propagates_timeout_only_after_readability() {
    let dir = tempfile::tempdir().unwrap();
    let st = Arc::new(
        AppState::new(dir.path(), StoreOptions::default(), Duration::from_secs(30)).unwrap(),
    );
    st.create("mto_ds1", DbType::Persistent).unwrap();
    st.create("mto_ds2", DbType::Persistent).unwrap();
    let app = crate::http::router(st.clone());

    let put1 = send(
        &app,
        Request::put("/mto_ds1/data?graph=http%3A%2F%2Fexample.org%2Fmto%2Fg1")
            .header(header::CONTENT_TYPE, "text/turtle")
            .body(Body::from(
                "<http://example.org/mto/Shared> <http://www.w3.org/2000/01/rdf-schema#label> \"From DS1\" .\n",
            ))
            .unwrap(),
    )
    .await;
    assert!(put1.status.is_success(), "{}", put1.text());
    let put2 = send(
        &app,
        Request::put("/mto_ds2/data?graph=http%3A%2F%2Fexample.org%2Fmto%2Fg2")
            .header(header::CONTENT_TYPE, "text/turtle")
            .body(Body::from(
                "<http://example.org/mto/Shared> <http://www.w3.org/2000/01/rdf-schema#comment> \"From DS2\" .\n\
                 <http://example.org/mto/OnlyInDs2> <http://www.w3.org/2000/01/rdf-schema#label> \"Only DS2\" .\n",
            ))
            .unwrap(),
    )
    .await;
    assert!(put2.status.is_success(), "{}", put2.text());

    for ds_name in ["mto_ds1", "mto_ds2"] {
        let snap = send(
            &app,
            post_json(
                &format!("/$/snapshots/{ds_name}"),
                json!({"name": "v1"}),
                None,
            ),
        )
        .await;
        assert_eq!(snap.status, StatusCode::CREATED, "{}", snap.text());
        // Advance head so snapshot:v1 is a historical state on both datasets.
        let post = send(
            &app,
            Request::post(format!(
                "/{ds_name}/data?graph=http%3A%2F%2Fexample.org%2Fmto%2F{}",
                if ds_name == "mto_ds1" { "g1" } else { "g2" }
            ))
            .header(header::CONTENT_TYPE, "text/turtle")
            .body(Body::from(
                "<http://example.org/mto/Extra> <http://www.w3.org/2000/01/rdf-schema#label> \"head\" .\n",
            ))
            .unwrap(),
        )
        .await;
        assert!(post.status.is_success(), "{}", post.text());
    }

    let reg = send(
        &app,
        post_json(
            "/$/resources",
            json!({
                "id": "mto",
                "kind": "vocabulary",
                "title": "Multi-dataset timeout vocab",
                "namespaces": [{"prefix": "mto", "iri": "http://example.org/mto/"}],
                "graphs": [
                    {"dataset": "mto_ds1", "graph": "http://example.org/mto/g1"},
                    {"dataset": "mto_ds2", "graph": "http://example.org/mto/g2"}
                ]
            }),
            None,
        ),
    )
    .await;
    assert_eq!(reg.status, StatusCode::CREATED, "{}", reg.text());

    let catalog = st.resources.read().unwrap();
    let principal = crate::auth::Principal::local();
    let snap_at = sparkles::history::At::Snapshot("v1".to_string());

    // Sanity check: without expiry, `Shared` merges triples from both datasets.
    let merged = match super::resolve_and_describe_with_hooks(
        &st,
        &principal,
        &catalog,
        LookupTarget::Iri("http://example.org/mto/Shared".to_string()),
        None,
        Some(snap_at.clone()),
        &crate::http::Params::default(),
        None,
        false,
        super::TestHooks::default(),
    ) {
        Ok(v) => v,
        Err(e) => {
            let resp = axum::response::IntoResponse::into_response(e);
            panic!(
                "unexpired multi-dataset lookup should succeed, got {}",
                resp.status()
            );
        }
    };
    let LookupOutcome::Document(desc) = merged else {
        panic!("expected Document outcome");
    };
    assert_eq!(desc.triples.as_slice().len(), 2);

    let expire_before_second = super::TestHooks {
        expire_deadline_before_dataset: Some(1),
        exhaust_budget_after_settle: false,
    };

    // With an earlier readable triple from dataset 0, expiry before dataset 1 MUST
    // propagate 408 Request Timeout rather than returning an incomplete 200 OK.
    let err_after_read = super::resolve_and_describe_with_hooks(
        &st,
        &principal,
        &catalog,
        LookupTarget::Iri("http://example.org/mto/Shared".to_string()),
        None,
        Some(snap_at.clone()),
        &crate::http::Params::default(),
        None,
        false,
        expire_before_second,
    )
    .err()
    .expect("expired deadline after earlier readable dataset must not return incomplete 200");
    let resp_after = axum::response::IntoResponse::into_response(err_after_read);
    assert_eq!(resp_after.status(), StatusCode::REQUEST_TIMEOUT);

    // Without an earlier readable triple from dataset 0, historical expiry before dataset 1
    // MUST preserve the generic 404 response.
    let err_before_read = super::resolve_and_describe_with_hooks(
        &st,
        &principal,
        &catalog,
        LookupTarget::Iri("http://example.org/mto/OnlyInDs2".to_string()),
        None,
        Some(snap_at),
        &crate::http::Params::default(),
        None,
        false,
        expire_before_second,
    )
    .err()
    .expect("expired historical deadline without readability must fail");
    let resp_before = axum::response::IntoResponse::into_response(err_before_read);
    assert_eq!(resp_before.status(), StatusCode::NOT_FOUND);

    // Non-historical (live) lookup still propagates 408 Request Timeout when expiring
    // before dataset 1 even when dataset 0 had no triples.
    let err_live = super::resolve_and_describe_with_hooks(
        &st,
        &principal,
        &catalog,
        LookupTarget::Iri("http://example.org/mto/OnlyInDs2".to_string()),
        None,
        None,
        &crate::http::Params::default(),
        None,
        false,
        expire_before_second,
    )
    .err()
    .expect("expired live deadline must return 408");
    let resp_live = axum::response::IntoResponse::into_response(err_live);
    assert_eq!(resp_live.status(), StatusCode::REQUEST_TIMEOUT);
}

#[tokio::test]
async fn multi_dataset_historical_deferred_error_propagates_in_both_dataset_orders() {
    // 1. Cold persistent replay failure (`snapshot_for_limited`) in both dataset orders:
    //    `[a_fail, b_small]` (failing dataset sorts first) and `[a_small, z_fail]`
    //    (contributing dataset sorts first).
    let cold_cap: u64 = 512 * 1024;
    for (failing_ds_name, small_ds_name) in [("a_fail", "b_small"), ("z_fail", "a_small")] {
        let dir = tempfile::tempdir().unwrap();
        {
            let st = Arc::new(
                AppState::new(dir.path(), StoreOptions::default(), Duration::from_secs(30))
                    .unwrap(),
            );
            st.create(failing_ds_name, DbType::Persistent).unwrap();
            st.create(small_ds_name, DbType::Persistent).unwrap();
            let app = crate::http::router(st.clone());

            let big_hist = "h".repeat(8 * 1024);
            let mut fail_ttl = format!(
                "@prefix rdfs: <http://www.w3.org/2000/01/rdf-schema#> .\n\
                 @prefix mdef: <http://example.org/mdef/> .\n\
                 mdef:Shared rdfs:label \"From failing historical\" ;\n\
                   rdfs:comment \"{big_hist}\" .\n\
                 mdef:OnlyInFailing rdfs:label \"Only in failing historical\" .\n"
            );
            for i in 0..600 {
                fail_ttl.push_str(&format!(
                    "mdef:Pad{i} rdfs:comment \"padding-entry-{i:04}-payload-for-wal-replay\" .\n"
                ));
            }
            let put_fail = send(
                &app,
                Request::put(format!(
                    "/{failing_ds_name}/data?graph=http%3A%2F%2Fexample.org%2Fmdef%2Fg_fail"
                ))
                .header(header::CONTENT_TYPE, "text/turtle")
                .body(Body::from(fail_ttl))
                .unwrap(),
            )
            .await;
            assert!(put_fail.status.is_success(), "{}", put_fail.text());

            let put_small = send(
                &app,
                Request::put(format!(
                    "/{small_ds_name}/data?graph=http%3A%2F%2Fexample.org%2Fmdef%2Fg_small"
                ))
                .header(header::CONTENT_TYPE, "text/turtle")
                .body(Body::from(
                    "<http://example.org/mdef/Shared> <http://www.w3.org/2000/01/rdf-schema#comment> \"From small historical\" .\n",
                ))
                .unwrap(),
            )
            .await;
            assert!(put_small.status.is_success(), "{}", put_small.text());

            for ds_name in [failing_ds_name, small_ds_name] {
                let snap = send(
                    &app,
                    post_json(
                        &format!("/$/snapshots/{ds_name}"),
                        json!({"name": "v1"}),
                        None,
                    ),
                )
                .await;
                assert_eq!(snap.status, StatusCode::CREATED, "{}", snap.text());
            }

            // Clear `Shared` and `OnlyInFailing` from `failing_ds_name`'s live snapshot so
            // its live readability probe returns empty, while advancing `small_ds_name`'s head
            // so `snapshot:v1` is historical on both datasets.
            let clear_fail_live = send(
                &app,
                Request::put(format!(
                    "/{failing_ds_name}/data?graph=http%3A%2F%2Fexample.org%2Fmdef%2Fg_fail"
                ))
                .header(header::CONTENT_TYPE, "text/turtle")
                .body(Body::from(
                    "<http://example.org/mdef/OtherLive> <http://www.w3.org/2000/01/rdf-schema#label> \"live head\" .\n",
                ))
                .unwrap(),
            )
            .await;
            assert!(
                clear_fail_live.status.is_success(),
                "{}",
                clear_fail_live.text()
            );

            let advance_small = send(
                &app,
                Request::post(format!(
                    "/{small_ds_name}/data?graph=http%3A%2F%2Fexample.org%2Fmdef%2Fg_small"
                ))
                .header(header::CONTENT_TYPE, "text/turtle")
                .body(Body::from(
                    "<http://example.org/mdef/OtherSmall> <http://www.w3.org/2000/01/rdf-schema#label> \"live head\" .\n",
                ))
                .unwrap(),
            )
            .await;
            assert!(
                advance_small.status.is_success(),
                "{}",
                advance_small.text()
            );

            let reg = send(
                &app,
                post_json(
                    "/$/resources",
                    json!({
                        "id": "mdef",
                        "kind": "vocabulary",
                        "title": "Multi-dataset deferred error vocab",
                        "namespaces": [{"prefix": "mdef", "iri": "http://example.org/mdef/"}],
                        "graphs": [
                            {"dataset": failing_ds_name, "graph": "http://example.org/mdef/g_fail"},
                            {"dataset": small_ds_name, "graph": "http://example.org/mdef/g_small"}
                        ]
                    }),
                    None,
                ),
            )
            .await;
            assert_eq!(reg.status, StatusCode::CREATED, "{}", reg.text());

            // Uncapped sanity check: the same request merges triples from both datasets.
            let uncapped = send(
                &app,
                get_req(
                    &format!(
                        "/$/ld/doc?iri={}&at=snapshot:v1",
                        enc("http://example.org/mdef/Shared")
                    ),
                    Some("text/turtle"),
                    None,
                ),
            )
            .await;
            assert_eq!(uncapped.status, StatusCode::OK, "{}", uncapped.text());
            let body = uncapped.text();
            assert!(
                body.contains("From failing historical") && body.contains("From small historical"),
                "uncapped lookup for ({failing_ds_name}, {small_ds_name}) must merge both datasets: {body}"
            );
        }

        // Reopen cold with a 512 KiB query memory ceiling: large enough for `small_ds`'s
        // cold replay (~340 KiB peak, ~130 KiB retained), query, and response, but well
        // below `failing_ds`'s >1.3 MiB cold replay footprint.
        let mut st =
            AppState::new(dir.path(), StoreOptions::default(), Duration::from_secs(30)).unwrap();
        st.limits.query_memory_bytes = Some(cold_cap);
        let st = Arc::new(st);

        let fail_ds = crate::http::dataset(&st, failing_ds_name)
            .ok()
            .expect("failing dataset");
        let small_ds = crate::http::dataset(&st, small_ds_name)
            .ok()
            .expect("small dataset");
        assert!(
            matches!(
                fail_ds.store.snapshot_at_limited_bytes(
                    &sparkles::history::At::Snapshot("v1".to_string()),
                    &sparkles::history::HistoryOptions::default(),
                    cold_cap,
                ),
                Err(sparkles::Error::BudgetExceeded(b))
                    if b.kind == sparkles::BudgetKind::Memory
            ),
            "cold replay on {failing_ds_name} must exceed {cold_cap} bytes"
        );
        let (small_snap, _, small_retained) = small_ds
            .store
            .snapshot_at_limited_bytes(
                &sparkles::history::At::Snapshot("v1".to_string()),
                &sparkles::history::HistoryOptions::default(),
                cold_cap,
            )
            .expect("small dataset historical snapshot must fit within cold_cap");
        let small_opts = sparkles::sparql::QueryOptions {
            default_graph_uris: vec!["http://example.org/mdef/g_small".to_string()],
            max_memory_bytes: Some(cold_cap - small_retained),
            ..Default::default()
        };
        let small_res = sparkles::sparql::query(
            small_snap,
            "DESCRIBE <http://example.org/mdef/Shared>",
            &small_opts,
        )
        .expect("small dataset query must succeed under remaining cold_cap");
        assert!(!small_res.triples().is_empty());
        let live_opts = sparkles::sparql::QueryOptions {
            default_graph_uris: vec!["http://example.org/mdef/g_fail".to_string()],
            ..Default::default()
        };
        let live_fail = sparkles::sparql::query(
            fail_ds.store.snapshot(),
            "DESCRIBE <http://example.org/mdef/Shared>",
            &live_opts,
        )
        .unwrap();
        assert!(
            live_fail.triples().is_empty(),
            "failing dataset live probe for Shared must be empty"
        );
        drop(fail_ds);
        drop(small_ds);

        let app = crate::http::router(st);

        // `Shared` is readable in `small_ds_name`, so both dataset orders MUST return 507
        // rather than emitting `small_ds_name`'s partial subset as an unmarked 200 OK.
        let shared = send(
            &app,
            get_req(
                &format!(
                    "/$/ld/doc?iri={}&at=snapshot:v1",
                    enc("http://example.org/mdef/Shared")
                ),
                Some("text/turtle"),
                None,
            ),
        )
        .await;
        assert_eq!(
            shared.status,
            StatusCode::INSUFFICIENT_STORAGE,
            "cold replay order ({failing_ds_name}, {small_ds_name}) must return 507, got {}",
            shared.text()
        );

        // `OnlyInFailing` has no readable triple on any dataset (neither live nor small_ds),
        // so both dataset orders MUST preserve generic 404 linked-data-not-found.
        let hidden = send(
            &app,
            get_req(
                &format!(
                    "/$/ld/doc?iri={}&at=snapshot:v1",
                    enc("http://example.org/mdef/OnlyInFailing")
                ),
                Some("text/turtle"),
                None,
            ),
        )
        .await;
        assert_eq!(
            hidden.status,
            StatusCode::NOT_FOUND,
            "cold replay order ({failing_ds_name}, {small_ds_name}) must keep 404 when unreadable, got {}",
            hidden.text()
        );
        assert_eq!(hidden.json()["code"], "linked-data-not-found");
    }

    // 2. Post-replay `sparql::query` memory failure in both dataset orders using in-memory
    //    retained snapshots (`snapshot_for_limited` succeeds, `sparql::query` fails under
    //    the 1 KiB remainder on the failing dataset while the small dataset succeeds).
    for (failing_ds_name, small_ds_name) in
        [("a_mem_fail", "b_mem_small"), ("z_mem_fail", "a_mem_small")]
    {
        let dir = tempfile::tempdir().unwrap();
        let st = Arc::new(
            AppState::new(dir.path(), StoreOptions::default(), Duration::from_secs(30)).unwrap(),
        );
        st.create(failing_ds_name, DbType::Mem).unwrap();
        st.create(small_ds_name, DbType::Mem).unwrap();
        let setup_app = crate::http::router(st.clone());

        let big_shared = "x".repeat(8 * 1024);
        let big_only = "y".repeat(8 * 1024);
        let mut fail_ttl = format!(
            "@prefix rdfs: <http://www.w3.org/2000/01/rdf-schema#> .\n\
             @prefix mdefm: <http://example.org/mdefm/> .\n\
             mdefm:Shared rdfs:label \"From mem failing\" ;\n\
               rdfs:comment \"{big_shared}\" .\n\
             mdefm:OnlyInFailing rdfs:comment \"{big_only}\" .\n"
        );
        for i in 0..400 {
            fail_ttl.push_str(&format!(
                "mdefm:Pad{i} rdfs:comment \"padding-entry-{i:04}-payload-for-retained-delta\" .\n"
            ));
        }
        let put_fail = send(
            &setup_app,
            Request::put(format!(
                "/{failing_ds_name}/data?graph=http%3A%2F%2Fexample.org%2Fmdefm%2Fg_fail"
            ))
            .header(header::CONTENT_TYPE, "text/turtle")
            .body(Body::from(fail_ttl))
            .unwrap(),
        )
        .await;
        assert!(put_fail.status.is_success(), "{}", put_fail.text());

        let put_small = send(
            &setup_app,
            Request::put(format!(
                "/{small_ds_name}/data?graph=http%3A%2F%2Fexample.org%2Fmdefm%2Fg_small"
            ))
            .header(header::CONTENT_TYPE, "text/turtle")
            .body(Body::from(
                "<http://example.org/mdefm/Shared> <http://www.w3.org/2000/01/rdf-schema#comment> \"From mem small\" .\n",
            ))
            .unwrap(),
        )
        .await;
        assert!(put_small.status.is_success(), "{}", put_small.text());

        for ds_name in [failing_ds_name, small_ds_name] {
            let snap = send(
                &setup_app,
                post_json(
                    &format!("/$/snapshots/{ds_name}"),
                    json!({"name": "v1"}),
                    None,
                ),
            )
            .await;
            assert_eq!(snap.status, StatusCode::CREATED, "{}", snap.text());
        }

        let clear_fail_live = send(
            &setup_app,
            Request::put(format!(
                "/{failing_ds_name}/data?graph=http%3A%2F%2Fexample.org%2Fmdefm%2Fg_fail"
            ))
            .header(header::CONTENT_TYPE, "text/turtle")
            .body(Body::from(
                "<http://example.org/mdefm/OtherLive> <http://www.w3.org/2000/01/rdf-schema#label> \"live head\" .\n",
            ))
            .unwrap(),
        )
        .await;
        assert!(
            clear_fail_live.status.is_success(),
            "{}",
            clear_fail_live.text()
        );

        let advance_small = send(
            &setup_app,
            Request::post(format!(
                "/{small_ds_name}/data?graph=http%3A%2F%2Fexample.org%2Fmdefm%2Fg_small"
            ))
            .header(header::CONTENT_TYPE, "text/turtle")
            .body(Body::from(
                "<http://example.org/mdefm/OtherSmall> <http://www.w3.org/2000/01/rdf-schema#label> \"live head\" .\n",
            ))
            .unwrap(),
        )
        .await;
        assert!(
            advance_small.status.is_success(),
            "{}",
            advance_small.text()
        );

        let reg = send(
            &setup_app,
            post_json(
                "/$/resources",
                json!({
                    "id": "mdefm",
                    "kind": "vocabulary",
                    "title": "Multi-dataset post-replay deferred error vocab",
                    "namespaces": [{"prefix": "mdefm", "iri": "http://example.org/mdefm/"}],
                    "graphs": [
                        {"dataset": failing_ds_name, "graph": "http://example.org/mdefm/g_fail"},
                        {"dataset": small_ds_name, "graph": "http://example.org/mdefm/g_small"}
                    ]
                }),
                None,
            ),
        )
        .await;
        assert_eq!(reg.status, StatusCode::CREATED, "{}", reg.text());

        let uncapped = send(
            &setup_app,
            get_req(
                &format!(
                    "/$/ld/doc?iri={}&at=snapshot:v1",
                    enc("http://example.org/mdefm/Shared")
                ),
                Some("text/turtle"),
                None,
            ),
        )
        .await;
        assert_eq!(uncapped.status, StatusCode::OK, "{}", uncapped.text());
        let body = uncapped.text();
        assert!(
            body.contains("From mem failing") && body.contains("From mem small"),
            "uncapped mem lookup for ({failing_ds_name}, {small_ds_name}) must merge both datasets: {body}"
        );

        let fail_ds = crate::http::dataset(&st, failing_ds_name)
            .ok()
            .expect("failing mem dataset");
        let small_ds = crate::http::dataset(&st, small_ds_name)
            .ok()
            .expect("small mem dataset");
        let (_, _, fail_retained) = fail_ds
            .store
            .snapshot_at_limited_bytes(
                &sparkles::history::At::Snapshot("v1".to_string()),
                &sparkles::history::HistoryOptions::default(),
                64 * 1024 * 1024,
            )
            .unwrap();
        let mem_cap = fail_retained + 1024;
        let (fail_snap, _, check_retained) = fail_ds
            .store
            .snapshot_at_limited_bytes(
                &sparkles::history::At::Snapshot("v1".to_string()),
                &sparkles::history::HistoryOptions::default(),
                mem_cap,
            )
            .expect("failing mem dataset snapshot lookup must succeed under mem_cap");
        assert_eq!(check_retained, fail_retained);
        let q_opts = sparkles::sparql::QueryOptions {
            default_graph_uris: vec!["http://example.org/mdefm/g_fail".to_string()],
            max_memory_bytes: Some(mem_cap - fail_retained),
            ..Default::default()
        };
        assert!(
            matches!(
                sparkles::sparql::query(
                    fail_snap,
                    "DESCRIBE <http://example.org/mdefm/Shared>",
                    &q_opts,
                ),
                Err(sparkles::Error::BudgetExceeded(b))
                    if b.kind == sparkles::BudgetKind::Memory
            ),
            "post-replay query on {failing_ds_name} must fail under remainder {}",
            mem_cap - fail_retained
        );
        let (small_snap, _, small_retained) = small_ds
            .store
            .snapshot_at_limited_bytes(
                &sparkles::history::At::Snapshot("v1".to_string()),
                &sparkles::history::HistoryOptions::default(),
                mem_cap,
            )
            .expect("small mem dataset snapshot must fit under mem_cap");
        let small_opts = sparkles::sparql::QueryOptions {
            default_graph_uris: vec!["http://example.org/mdefm/g_small".to_string()],
            max_memory_bytes: Some(mem_cap - small_retained),
            ..Default::default()
        };
        let small_res = sparkles::sparql::query(
            small_snap,
            "DESCRIBE <http://example.org/mdefm/Shared>",
            &small_opts,
        )
        .expect("small mem dataset query must succeed under mem_cap");
        assert!(!small_res.triples().is_empty());

        drop(setup_app);
        drop(fail_ds);
        drop(small_ds);
        let mut st = Arc::try_unwrap(st).ok().expect("unique AppState");
        st.limits.query_memory_bytes = Some(mem_cap);
        let app = crate::http::router(Arc::new(st));

        let shared = send(
            &app,
            get_req(
                &format!(
                    "/$/ld/doc?iri={}&at=snapshot:v1",
                    enc("http://example.org/mdefm/Shared")
                ),
                Some("text/turtle"),
                None,
            ),
        )
        .await;
        assert_eq!(
            shared.status,
            StatusCode::INSUFFICIENT_STORAGE,
            "post-replay order ({failing_ds_name}, {small_ds_name}) must return 507, got {}",
            shared.text()
        );

        let hidden = send(
            &app,
            get_req(
                &format!(
                    "/$/ld/doc?iri={}&at=snapshot:v1",
                    enc("http://example.org/mdefm/OnlyInFailing")
                ),
                Some("text/turtle"),
                None,
            ),
        )
        .await;
        assert_eq!(
            hidden.status,
            StatusCode::NOT_FOUND,
            "post-replay order ({failing_ds_name}, {small_ds_name}) must keep 404 when unreadable, got {}",
            hidden.text()
        );
        assert_eq!(hidden.json()["code"], "linked-data-not-found");
    }
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
    // 4 KiB comfortably fits the fixed HTML page scaffold (~2 KiB) so Small succeeds
    // while Big fails while streaming its literal.
    st.limits.max_result_bytes = Some(4096);
    let st = Arc::new(st);

    let ds = st.attach("htmlds", DbType::Mem, None).unwrap();
    let big = "a".repeat(32 * 1024);
    let trig = format!(
        "<http://example.org/h/g> {{\n  \
         <http://example.org/h/Small> <http://www.w3.org/2000/01/rdf-schema#label> \"Small\" .\n  \
         <http://example.org/h/Big> <http://www.w3.org/2000/01/rdf-schema#comment> \"{big}\" .\n\
         }}\n"
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

    let small_r = send(
        &app,
        get_req(
            &format!("/$/ld/doc?iri={}", enc("http://example.org/h/Small")),
            Some("text/html"),
            None,
        ),
    )
    .await;
    assert_eq!(small_r.status, StatusCode::OK, "{}", small_r.text());

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
async fn html_escaping_expansion_and_response_body_charge_shared_memory_account() {
    let dir = tempfile::tempdir().unwrap();
    // 1.5 MiB query memory limit and 1.5 MiB query memory pool, with default 1 GiB
    // max_result_bytes: a 120 KiB `rdfs:label` of `&` characters fits comfortably
    // through DESCRIBE (~1.2 MiB peak) and as Turtle (~120.5 KiB, 1x), while HTML
    // rendering escapes `&` -> `&amp;` (5x) in `<title>`, `<h1>` and the table (~1.8 MiB
    // total), exceeding the shared memory account and answering 507.
    let mut st = AppState::with_memory(
        dir.path(),
        StoreOptions::default(),
        Duration::from_secs(30),
        Some(1536 * 1024),
    )
    .unwrap();
    st.limits.query_memory_bytes = Some(1536 * 1024);
    st.set_phase(crate::obs::Phase::Ready);
    let st = Arc::new(st);

    let ds = st.attach("escds", DbType::Mem, None).unwrap();
    let amps = "&".repeat(120 * 1024);
    let trig = format!(
        "<http://example.org/esc/g> {{\n  \
         <http://example.org/esc/Expand> <http://www.w3.org/2000/01/rdf-schema#label> \"{amps}\" .\n\
         }}\n"
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
                "id": "escv",
                "kind": "vocabulary",
                "title": "Escape Vocab",
                "namespaces": [{"prefix": "esc", "iri": "http://example.org/esc/"}],
                "graphs": [{"dataset": "escds", "graph": "http://example.org/esc/g"}]
            }),
            None,
        ),
    )
    .await;
    assert_eq!(reg.status, StatusCode::CREATED, "{}", reg.text());

    // Turtle output (~120.5 KiB) fits inside 1.5 MiB and keeps its pool grant charged
    // through `Bytes::from_owner` even after the HTTP Body is polled and dropped, until
    // the yielded frame `Bytes` itself drops.
    use http_body_util::BodyExt as _;
    let pool = st.catalog.pool().expect("pool configured");
    let raw_ttl = app
        .clone()
        .oneshot(get_req(
            &format!("/$/ld/doc?iri={}", enc("http://example.org/esc/Expand")),
            Some("text/turtle"),
            None,
        ))
        .await
        .unwrap();
    let ttl_status = raw_ttl.status();
    if ttl_status != StatusCode::OK {
        let err_body = axum::body::to_bytes(raw_ttl.into_body(), usize::MAX)
            .await
            .unwrap();
        panic!(
            "expected 200 OK for Turtle within 1.5 MiB budget, got {ttl_status}: {}",
            String::from_utf8_lossy(&err_body)
        );
    }
    assert!(
        pool.granted() > 0,
        "response body must keep its pool grant charged until consumed/dropped"
    );
    let mut ttl_body = raw_ttl.into_body();
    let frame = ttl_body
        .frame()
        .await
        .expect("expected data frame")
        .expect("data frame error");
    let ttl_bytes = frame.into_data().expect("expected data frame bytes");
    drop(ttl_body);
    assert!(ttl_bytes.len() >= 120 * 1024);
    assert!(
        pool.granted() > 0,
        "pool grant must remain charged after body drops while response Bytes frame is still live"
    );
    drop(ttl_bytes);
    assert_eq!(
        pool.granted(),
        0,
        "pool grant must be released once response Bytes frame drops"
    );

    // HTML output (~1.8 MiB escaped + live QueryResult > 1.5 MiB) exceeds the shared
    // memory account and answers 507 even though max_result_bytes is 1 GiB.
    let html_r = send(
        &app,
        get_req(
            &format!("/$/ld/doc?iri={}", enc("http://example.org/esc/Expand")),
            Some("text/html"),
            None,
        ),
    )
    .await;
    assert_eq!(
        html_r.status,
        StatusCode::INSUFFICIENT_STORAGE,
        "escaped HTML expansion should exceed shared memory budget: {}",
        html_r.text()
    );
    assert_eq!(pool.granted(), 0);
}

#[tokio::test]
async fn historical_snapshot_charge_persists_through_rendering_and_writers_charge_capacity() {
    use std::io::Write as _;

    // 1. Capacity-before-growth accounting in both `HtmlWriter` and `ChargedLimitedWriter`:
    //    an append that fits the logical remainder (`10 <= 400 - 256`) but crosses current
    //    capacity (`256 -> 512 > 400`) must fail BEFORE reallocating.
    {
        let mut charge = super::MemoryCharge::new(None, Some(400));
        let mut hw = super::html::HtmlWriter::new(Some(1024), &mut charge);
        hw.push_str(&"a".repeat(256)).unwrap();
        assert_eq!(hw.capacity(), 256);
        assert_eq!(hw.len(), 256);
        let err = hw.push_str("0123456789").unwrap_err();
        assert!(
            matches!(
                err,
                sparkles::Error::BudgetExceeded(sparkles::Budget {
                    kind: sparkles::BudgetKind::Memory,
                    limit: 400,
                    requested: 512,
                })
            ),
            "unexpected HtmlWriter error: {err:?}"
        );
        assert_eq!(hw.capacity(), 256, "HtmlWriter reallocated past limit");
        assert_eq!(hw.len(), 256);

        // `max_result_bytes` still bounds logical payload length independently of capacity:
        // growing 0 -> 256 -> 512 keeps logical length 300 <= 300 even though capacity is 512.
        let mut big_charge = super::MemoryCharge::new(None, Some(1024));
        let mut hw2 = super::html::HtmlWriter::new(Some(300), &mut big_charge);
        hw2.push_str(&"b".repeat(256)).unwrap();
        hw2.push_str(&"b".repeat(44)).unwrap();
        assert_eq!(hw2.capacity(), 512);
        let err2 = hw2.push_str("x").unwrap_err();
        assert!(
            matches!(
                err2,
                sparkles::Error::BudgetExceeded(sparkles::Budget {
                    kind: sparkles::BudgetKind::ResultBytes,
                    limit: 300,
                    requested: 301,
                })
            ),
            "unexpected HtmlWriter ResultBytes error: {err2:?}"
        );
    }
    {
        let mut charge = super::MemoryCharge::new(None, Some(400));
        let mut rw = super::ChargedLimitedWriter::new(Some(1024), &mut charge);
        rw.write_all(&[b'a'; 256]).unwrap();
        assert_eq!(rw.capacity(), 256);
        assert_eq!(rw.len(), 256);
        assert!(rw.write_all(b"0123456789").is_err());
        let err = rw.take_budget_err().expect("expected budget error");
        assert!(
            matches!(
                err,
                sparkles::Error::BudgetExceeded(sparkles::Budget {
                    kind: sparkles::BudgetKind::Memory,
                    limit: 400,
                    requested: 512,
                })
            ),
            "unexpected ChargedLimitedWriter error: {err:?}"
        );
        assert_eq!(
            rw.capacity(),
            256,
            "ChargedLimitedWriter reallocated past limit"
        );
        assert_eq!(rw.len(), 256);

        let mut big_charge = super::MemoryCharge::new(None, Some(1024));
        let mut rw2 = super::ChargedLimitedWriter::new(Some(300), &mut big_charge);
        rw2.write_all(&[b'b'; 256]).unwrap();
        rw2.write_all(&[b'b'; 44]).unwrap();
        assert_eq!(rw2.capacity(), 512);
        assert!(rw2.write_all(b"x").is_err());
        let err2 = rw2.take_budget_err().expect("expected ResultBytes error");
        assert!(
            matches!(
                err2,
                sparkles::Error::BudgetExceeded(sparkles::Budget {
                    kind: sparkles::BudgetKind::ResultBytes,
                    limit: 300,
                    requested: 301,
                })
            ),
            "unexpected ChargedLimitedWriter ResultBytes error: {err2:?}"
        );
    }

    // 2. Historical snapshot charge remains held across `QueryResult` through rendering:
    //    use an HTML-escapable `rdfs:label` of `&` characters so the rendered HTML buffer
    //    capacity (~2 MiB) exceeds `query_peak`, letting us set a ceiling where `snapshot +
    //    query` fits AND `result + rendered_buffer` alone would fit, while `snapshot +
    //    query_result + rendered_buffer_capacity` together exceed the ceiling.
    let dir = tempfile::tempdir().unwrap();
    let payload = "&".repeat(120 * 1024);
    let (retained, query_peak, res_bytes, html_cap) = {
        let st = Arc::new(
            AppState::new(dir.path(), StoreOptions::default(), Duration::from_secs(30)).unwrap(),
        );
        st.create("snapds", DbType::Persistent).unwrap();
        let app = crate::http::router(st.clone());

        let mut ttl = format!(
            "@prefix rdfs: <http://www.w3.org/2000/01/rdf-schema#> .\n\
             @prefix sh: <http://example.org/sh/> .\n\
             sh:Big rdfs:label \"{payload}\" .\n"
        );
        for i in 0..800 {
            ttl.push_str(&format!(
                "sh:Pad{i} rdfs:comment \"historical-delta-padding-entry-{i:04}-for-retained-snapshot\" .\n"
            ));
        }
        let put1 = send(
            &app,
            Request::put("/snapds/data?graph=http%3A%2F%2Fexample.org%2Fsh%2Fg")
                .header(header::CONTENT_TYPE, "text/turtle")
                .body(Body::from(ttl))
                .unwrap(),
        )
        .await;
        assert!(put1.status.is_success(), "{}", put1.text());

        let snap = send(
            &app,
            post_json("/$/snapshots/snapds", json!({"name": "h1"}), None),
        )
        .await;
        assert_eq!(snap.status, StatusCode::CREATED, "{}", snap.text());

        // Advance head so snapshot:h1 is a historical state with a non-trivial delta charge.
        let post2 = send(
            &app,
            Request::post("/snapds/data?graph=http%3A%2F%2Fexample.org%2Fsh%2Fg")
                .header(header::CONTENT_TYPE, "text/turtle")
                .body(Body::from(
                    "<http://example.org/sh/Other> <http://www.w3.org/2000/01/rdf-schema#label> \"head\" .",
                ))
                .unwrap(),
        )
        .await;
        assert!(post2.status.is_success(), "{}", post2.text());

        let reg = send(
            &app,
            post_json(
                "/$/resources",
                json!({
                    "id": "shv",
                    "kind": "vocabulary",
                    "title": "Snap Hist Vocab",
                    "namespaces": [{"prefix": "sh", "iri": "http://example.org/sh/"}],
                    "graphs": [{"dataset": "snapds", "graph": "http://example.org/sh/g"}]
                }),
                None,
            ),
        )
        .await;
        assert_eq!(reg.status, StatusCode::CREATED, "{}", reg.text());

        let ds = crate::http::dataset(&st, "snapds")
            .ok()
            .expect("snapds dataset");
        let (snap_arc, _, retained) = ds
            .store
            .snapshot_at_limited_bytes(
                &sparkles::history::At::Snapshot("h1".to_string()),
                &sparkles::history::HistoryOptions::default(),
                64 * 1024 * 1024,
            )
            .unwrap();
        let opts = sparkles::sparql::QueryOptions {
            default_graph_uris: vec!["http://example.org/sh/g".to_string()],
            ..Default::default()
        };
        let qres = sparkles::sparql::query(snap_arc, "DESCRIBE <http://example.org/sh/Big>", &opts)
            .unwrap();
        let res_bytes: u64 = qres.triples().iter().map(super::triple_bytes).sum();
        let query_peak = qres.mem_peak_bytes;

        let ok_resp = send(
            &app,
            get_req(
                &format!(
                    "/$/ld/doc?iri={}&at=snapshot:h1",
                    enc("http://example.org/sh/Big")
                ),
                Some("text/html"),
                None,
            ),
        )
        .await;
        assert_eq!(ok_resp.status, StatusCode::OK, "{}", ok_resp.text());
        let html_len = ok_resp.body.len();
        let mut cap = 256usize;
        while cap < html_len {
            cap *= 2;
        }
        (retained, query_peak, res_bytes, cap as u64)
    };

    // Choose a tight ceiling strictly above BOTH `retained + query_peak` (replay + DESCRIBE
    // fit) AND `res_bytes + html_cap` (releasing `snap_charge` early would bypass the limit),
    // and strictly below `retained + res_bytes + html_cap` (keeping `snap_charge` live across
    // `QueryResult` during HTML rendering must fail with 507).
    let lower = (retained + query_peak).max(res_bytes + html_cap);
    let upper = retained + res_bytes + html_cap;
    assert!(
        lower < upper,
        "fixture invariant violated: lower ({lower}) >= upper ({upper}) (retained={retained}, query_peak={query_peak}, res_bytes={res_bytes}, html_cap={html_cap})"
    );
    let tight_limit = lower + (upper - lower) / 2;

    let mut st = AppState::with_memory(
        dir.path(),
        StoreOptions::default(),
        Duration::from_secs(30),
        Some(16 * 1024 * 1024),
    )
    .unwrap();
    st.limits.query_memory_bytes = Some(tight_limit);
    st.set_phase(crate::obs::Phase::Ready);
    let st = Arc::new(st);
    let app = crate::http::router(st.clone());

    let fail_r = send(
        &app,
        get_req(
            &format!(
                "/$/ld/doc?iri={}&at=snapshot:h1",
                enc("http://example.org/sh/Big")
            ),
            Some("text/html"),
            None,
        ),
    )
    .await;
    assert_eq!(
        fail_r.status,
        StatusCode::INSUFFICIENT_STORAGE,
        "historical snapshot + QueryResult + rendered buffer must exceed tight_limit={tight_limit}: {}",
        fail_r.text()
    );
    assert_eq!(st.catalog.pool().unwrap().granted(), 0);
    drop(app);
    drop(st);

    // Raising the ceiling to cover all three simultaneously succeeds cleanly.
    let mut st_ok = AppState::with_memory(
        dir.path(),
        StoreOptions::default(),
        Duration::from_secs(30),
        Some(16 * 1024 * 1024),
    )
    .unwrap();
    st_ok.limits.query_memory_bytes = Some(upper + 64 * 1024);
    st_ok.set_phase(crate::obs::Phase::Ready);
    let st_ok = Arc::new(st_ok);
    let app_ok = crate::http::router(st_ok.clone());
    let ok_r = send(
        &app_ok,
        get_req(
            &format!(
                "/$/ld/doc?iri={}&at=snapshot:h1",
                enc("http://example.org/sh/Big")
            ),
            Some("text/html"),
            None,
        ),
    )
    .await;
    assert_eq!(ok_r.status, StatusCode::OK, "{}", ok_r.text());
    assert_eq!(st_ok.catalog.pool().unwrap().granted(), 0);
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
    // Media parameters following `q` still participate in matching (RFC 9110 §12.5.1).
    assert_eq!(
        negotiate_format(Some(
            "text/html;q=0.8;charset=iso-8859-1, text/turtle;q=0.7"
        )),
        Some(Format::Turtle)
    );
    assert_eq!(
        negotiate_format(Some("text/html;q=0.8;charset=utf-8, text/turtle;q=0.7")),
        Some(Format::Html)
    );
    assert_eq!(
        negotiate_format(Some(
            "text/html;q=0.5;charset=utf-8, text/html;q=0.9, text/turtle;q=0.7"
        )),
        Some(Format::Turtle)
    );
    // Duplicate weight parameter is malformed and ignored.
    assert_eq!(negotiate_format(Some("text/html;q=0.8;q=0.9")), None);

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

    // Descendant under a slash versionIri also pins history via its IRI, so even when
    // requested with matching explicit `?version=` or `?at=`, `rel="original"` is omitted.
    let slash_v1 = r#"
@prefix rdfs: <http://www.w3.org/2000/01/rdf-schema#> .
<http://example.org/sver/1.0/Item> rdfs:label "Slash Version Descendant v1" .
"#;
    send(
        &app,
        Request::put("/hverds/data?graph=http%3A%2F%2Fexample.org%2Fsver%2Fg")
            .header(header::CONTENT_TYPE, "text/turtle")
            .body(Body::from(slash_v1))
            .unwrap(),
    )
    .await;
    send(
        &app,
        post_json("/$/snapshots/hverds", json!({"name": "sv1"}), None),
    )
    .await;
    // Advance head so snapshot:sv1 is historical.
    send(
        &app,
        Request::post("/hverds/data?graph=http%3A%2F%2Fexample.org%2Fsver%2Fg")
            .header(header::CONTENT_TYPE, "text/turtle")
            .body(Body::from(
                "<http://example.org/sver/1.0/Item> <http://www.w3.org/2000/01/rdf-schema#comment> \"v2\" .",
            ))
            .unwrap(),
    )
    .await;
    send(
        &app,
        post_json(
            "/$/resources",
            json!({
                "id": "sverv",
                "kind": "vocabulary",
                "title": "Slash Versioned",
                "namespaces": [{"prefix": "sver", "iri": "http://example.org/sver/"}],
                "graphs": [{"dataset": "hverds", "graph": "http://example.org/sver/g"}],
                "versions": [{
                    "version": "1.0",
                    "versionIri": "http://example.org/sver/1.0/",
                    "snapshot": "snapshot:sv1"
                }]
            }),
            None,
        ),
    )
    .await;
    for suffix in [
        "&version=1.0",
        "&at=snapshot:sv1",
        "&version=1.0&at=snapshot:sv1",
    ] {
        let r = send(
            &app,
            get_req(
                &format!(
                    "/$/ld/doc?iri={}{suffix}",
                    enc("http://example.org/sver/1.0/Item")
                ),
                Some("text/turtle"),
                None,
            ),
        )
        .await;
        assert_eq!(r.status, StatusCode::OK, "{suffix}: {}", r.text());
        assert_eq!(r.header("sparkles-at"), Some("snapshot:sv1"));
        assert!(r.header("memento-datetime").is_some());
        assert!(
            !r.header("link").unwrap_or("").contains("rel=\"original\""),
            "slash versionIri descendant with {suffix} must not emit rel=\"original\": {:?}",
            r.header("link")
        );
    }

    // Hash-document 200/201 term-limit boundary check: 200 terms are not truncated,
    // adding a 201st term sets `Sparkles-Describe-Truncated: true`.
    let mut two_hundred = String::from("@prefix rdfs: <http://www.w3.org/2000/01/rdf-schema#> .\n");
    for i in 0..200 {
        two_hundred.push_str(&format!(
            "<http://example.org/lim#T{i:03}> rdfs:label \"T{i:03}\" .\n"
        ));
    }
    let put_lim = send(
        &app,
        Request::put("/hverds/data?graph=http%3A%2F%2Fexample.org%2Flim%2Fg")
            .header(header::CONTENT_TYPE, "text/turtle")
            .body(Body::from(two_hundred))
            .unwrap(),
    )
    .await;
    assert!(put_lim.status.is_success(), "{}", put_lim.text());
    send(
        &app,
        post_json(
            "/$/resources",
            json!({
                "id": "limv",
                "kind": "vocabulary",
                "title": "Limit Vocab",
                "namespaces": [{"prefix": "lim", "iri": "http://example.org/lim#"}],
                "graphs": [{"dataset": "hverds", "graph": "http://example.org/lim/g"}]
            }),
            None,
        ),
    )
    .await;
    let r200 = send(&app, get_req("/$/ld/ns/lim", Some("text/turtle"), None)).await;
    assert_eq!(r200.status, StatusCode::OK, "{}", r200.text());
    assert!(
        r200.header("sparkles-describe-truncated").is_none(),
        "200 terms must not be marked truncated"
    );
    let post_201 = send(
        &app,
        Request::post("/hverds/data?graph=http%3A%2F%2Fexample.org%2Flim%2Fg")
            .header(header::CONTENT_TYPE, "text/turtle")
            .body(Body::from(
                "<http://example.org/lim#T200> <http://www.w3.org/2000/01/rdf-schema#label> \"T200\" .",
            ))
            .unwrap(),
    )
    .await;
    assert!(post_201.status.is_success(), "{}", post_201.text());
    let r201 = send(&app, get_req("/$/ld/ns/lim", Some("text/turtle"), None)).await;
    assert_eq!(r201.status, StatusCode::OK, "{}", r201.text());
    assert_eq!(
        r201.header("sparkles-describe-truncated"),
        Some("true"),
        "201 terms must trigger hash-document truncation"
    );
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
    rdfs:seeAlso <http://example.org/v1/B>,
                 <http://example.org/v2/Other>,
                 <http://example.org/v1/nested/SubTerm> .
}
<http://example.org/x/g2> {
  <http://example.org/v2/Other> rdfs:label "Other" .
}
<http://example.org/x/g3> {
  <http://example.org/v1/nested/SubTerm> rdfs:label "Nested SubTerm" .
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
    // Nested vocabulary whose namespace sits inside vocab1's `/v1/` prefix.
    send(
        &app,
        post_json(
            "/$/resources",
            json!({
                "id": "vocab_nested",
                "kind": "vocabulary",
                "title": "Nested Vocab",
                "namespaces": [{"prefix": "v1n", "iri": "http://example.org/v1/nested/"}],
                "graphs": [{"dataset": "xds", "graph": "http://example.org/x/g3"}]
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
    let nested_link = format!(
        "/$/ld?iri={}\"",
        enc("http://example.org/v1/nested/SubTerm")
    );
    assert!(
        html.contains(&same_link),
        "missing same-vocab version link:\n{html}"
    );
    assert!(
        html.contains(&cross_link),
        "cross-vocab link must omit version=1.0:\n{html}"
    );
    assert!(
        html.contains(&nested_link),
        "nested-namespace link owned by another vocabulary must omit version=1.0:\n{html}"
    );
}

/// Evaluate generated Apache 2.4 `.htaccess` `RewriteCond %{THE_REQUEST}` / `RewriteRule`
/// directives against a raw HTTP/1.1 request line (`GET <raw_target> HTTP/1.1`), including
/// internal `[DPI,QSD,E=...]` handoffs, `[B]` backreference escaping, and `[R=400,L]`
/// rejections. Returns `(status_code, optional_location)`.
fn eval_htaccess_request(htaccess: &str, raw_target: &str) -> Option<(u16, Option<String>)> {
    let the_request = format!("GET {raw_target} HTTP/1.1");
    let mut uri = raw_target
        .split('?')
        .next()
        .unwrap_or(raw_target)
        .trim_start_matches('/')
        .to_string();
    let mut env: std::collections::HashMap<String, String> = std::collections::HashMap::new();
    let mut pending_conds: Vec<(String, String)> = Vec::new();

    for line in htaccess.lines().map(str::trim) {
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if let Some(rest) = line.strip_prefix("RewriteCond ") {
            let mut parts = rest.splitn(2, ' ');
            let test = parts.next().unwrap().trim().to_string();
            let pat = parts.next().unwrap().trim().to_string();
            pending_conds.push((test, pat));
            continue;
        }
        let Some(rest) = line.strip_prefix("RewriteRule ") else {
            pending_conds.clear();
            continue;
        };
        let parts: Vec<&str> = rest.split_whitespace().collect();
        assert_eq!(parts.len(), 3, "malformed RewriteRule: {line}");
        let (rule_pat, subst, flags) = (parts[0], parts[1], parts[2].trim_matches(['[', ']']));

        let rule_re = regex::Regex::new(rule_pat).unwrap();
        let Some(rule_caps) = rule_re.captures(&uri) else {
            pending_conds.clear();
            continue;
        };

        let mut cond_match = true;
        let mut last_cond_caps: Vec<Option<String>> = Vec::new();
        for (test, pat) in pending_conds.drain(..) {
            let val = if test == "%{THE_REQUEST}" {
                the_request.as_str()
            } else if let Some(var) = test
                .strip_prefix("%{ENV:")
                .and_then(|s| s.strip_suffix('}'))
            {
                env.get(var).map(String::as_str).unwrap_or("")
            } else {
                panic!("unexpected RewriteCond test string: {test}");
            };
            if let Some(eq) = pat.strip_prefix('=') {
                if val != eq {
                    cond_match = false;
                    break;
                }
            } else {
                let re = regex::Regex::new(&pat).unwrap();
                if let Some(c) = re.captures(val) {
                    last_cond_caps = (0..c.len())
                        .map(|i| c.get(i).map(|m| m.as_str().to_string()))
                        .collect();
                } else {
                    cond_match = false;
                    break;
                }
            }
        }
        if !cond_match {
            continue;
        }

        let has_b = flags.split(',').any(|f| f == "B");
        let has_ne = flags.split(',').any(|f| f == "NE");
        let status_code = flags
            .split(',')
            .find_map(|f| f.strip_prefix("R=").and_then(|n| n.parse::<u16>().ok()));

        let expand = |template: &str, escape_rule_caps: bool| -> String {
            let mut out = String::new();
            let mut chars = template.chars().peekable();
            while let Some(c) = chars.next() {
                if c == '\\' {
                    if let Some(next) = chars.next() {
                        out.push(next);
                    }
                } else if c == '%' && chars.peek() == Some(&'{') {
                    let _ = chars.next();
                    let mut expr = String::new();
                    for ch in chars.by_ref() {
                        if ch == '}' {
                            break;
                        }
                        expr.push(ch);
                    }
                    if let Some(var) = expr.strip_prefix("ENV:")
                        && let Some(v) = env.get(var)
                    {
                        out.push_str(v);
                    }
                } else if c == '%' && chars.peek().is_some_and(char::is_ascii_digit) {
                    let idx = chars.next().unwrap().to_digit(10).unwrap() as usize;
                    if let Some(Some(cap)) = last_cond_caps.get(idx) {
                        out.push_str(cap);
                    }
                } else if c == '$' && chars.peek().is_some_and(char::is_ascii_digit) {
                    let idx = chars.next().unwrap().to_digit(10).unwrap() as usize;
                    if let Some(m) = rule_caps.get(idx) {
                        let cap = m.as_str();
                        if escape_rule_caps {
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
            out
        };

        let mut next_env_updates = Vec::new();
        for f in flags.split(',') {
            if let Some(rest) = f.strip_prefix("E=")
                && let Some((k, v)) = rest.split_once(':')
            {
                next_env_updates.push((k.to_string(), expand(v, false)));
            }
        }
        let mut out = expand(subst, has_b);
        for (k, v) in next_env_updates {
            env.insert(k, v);
        }

        if let Some(code) = status_code {
            if subst == "-" {
                return Some((code, None));
            }
            if !has_ne {
                out = out.replace('%', "%25");
            }
            return Some((code, Some(out)));
        }
        uri = out;
    }
    None
}

#[tokio::test]
async fn apache_recipe_round_trips_encoded_version_iri_and_dynamic_encoded_terms() {
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
  <https://w3id.org/myorg> a owl:Ontology ;
    rdfs:label "Mount Root Version" .
  <https://w3id.org/myorg/mv/> a owl:Ontology ;
    owl:versionIRI <https://w3id.org/myorg/mv/rev%3B1> ;
    rdfs:label "Encoded Version rev%3B1" .
  <https://w3id.org/myorg/mv/rev;1> a owl:Class ;
    rdfs:label "Unencoded Distinct rev;1" .
  <https://w3id.org/myorg/mv/code%3Bpart> a owl:Class ;
    rdfs:label "Dynamic Encoded code%3Bpart" .
  <https://w3id.org/myorg/mv/code;part> a owl:Class ;
    rdfs:label "Dynamic Unencoded code;part" .
  <https://w3id.org/myorg/mv/a&b> a owl:Class ;
    rdfs:label "Ampersand Term" .
  <https://w3id.org/myorg/mv/a+b> a owl:Class ;
    rdfs:label "Plus Term" .
  <https://w3id.org/myorg/mv/Term> a owl:Class ;
    rdfs:label "Bare Term Without Query" .
  <https://w3id.org/myorg/mv/Term?edition=1> a owl:Class ;
    rdfs:label "Query Edition Term" .
  <https://w3id.org/myorg/mv/a%3Bb?c=1&d=a+b> a owl:Class ;
    rdfs:label "Encoded Path And Query Term" .
  <https://w3id.org/myorg/mv/?edition=1> a owl:Class ;
    rdfs:label "Slash Root With Query" .
  <https://w3id.org/myorg/mv//Term> a owl:Class ;
    rdfs:label "Repeated Slash Suffix Term" .
  <https://w3id.org/myorg/mv/a//b> a owl:Class ;
    rdfs:label "Internal Repeated Slash Term" .
  <https://w3id.org/myorg/vocab//> a owl:Ontology ;
    rdfs:label "Double Slash Namespace Root" .
  <https://w3id.org/myorg/vocab//Term> a owl:Class ;
    rdfs:label "Double Slash Namespace Term" .
  <https://w3id.org/myorg/vocab//Term?edition=1> a owl:Class ;
    rdfs:label "Double Slash Namespace Query Term" .
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
                "namespaces": [
                    {"prefix": "mv", "iri": "https://w3id.org/myorg/mv/"},
                    {"prefix": "dbl", "iri": "https://w3id.org/myorg/vocab//"}
                ],
                "graphs": [{"dataset": "apds", "graph": "http://example.org/ap/g"}],
                "versions": [
                    {
                        "version": "1.0",
                        "versionIri": "https://w3id.org/myorg/mv/rev%3B1",
                        "snapshot": "snapshot:v1"
                    },
                    {
                        "version": "0.9",
                        "versionIri": "https://w3id.org/myorg",
                        "snapshot": "snapshot:v1"
                    }
                ]
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
    assert!(
        text.contains("RewriteOptions AllowNoSlash"),
        "expected RewriteOptions AllowNoSlash in generated .htaccess:\n{text}"
    );

    let cases = [
        ("/myorg", "Mount Root Version", Some("snapshot:v1")),
        (
            "/myorg/mv/rev%3B1",
            "Encoded Version rev%3B1",
            Some("snapshot:v1"),
        ),
        ("/myorg/mv/rev;1", "Unencoded Distinct rev;1", None),
        ("/myorg/mv/code%3Bpart", "Dynamic Encoded code%3Bpart", None),
        ("/myorg/mv/code;part", "Dynamic Unencoded code;part", None),
        ("/myorg/mv/a&b", "Ampersand Term", None),
        ("/myorg/mv/a+b", "Plus Term", None),
        ("/myorg/mv/Term", "Bare Term Without Query", None),
        ("/myorg/mv/Term?edition=1", "Query Edition Term", None),
        (
            "/myorg/mv/a%3Bb?c=1&d=a+b",
            "Encoded Path And Query Term",
            None,
        ),
        ("/myorg/mv/?edition=1", "Slash Root With Query", None),
        ("/myorg/mv//Term", "Repeated Slash Suffix Term", None),
        ("/myorg/mv/a//b", "Internal Repeated Slash Term", None),
        ("/myorg/vocab//", "Double Slash Namespace Root", None),
        ("/myorg/vocab//Term", "Double Slash Namespace Term", None),
        (
            "/myorg/vocab//Term?edition=1",
            "Double Slash Namespace Query Term",
            None,
        ),
    ];

    for (raw_path, want_label, want_at) in cases {
        let (code, loc) = eval_htaccess_request(&text, raw_path)
            .unwrap_or_else(|| panic!("no rule matched {raw_path} in:\n{text}"));
        assert_eq!(code, 303, "expected 303 for {raw_path}");
        let loc = loc.unwrap_or_else(|| panic!("missing Location for {raw_path}"));
        let rel = loc
            .strip_prefix("https://rdf.example.org")
            .unwrap_or_else(|| panic!("bad redirect {loc}"));
        let resp = send(&app, get_req(rel, Some("text/turtle"), None)).await;
        assert_eq!(
            resp.status,
            StatusCode::OK,
            "{raw_path} -> {rel} failed: {}",
            resp.text()
        );
        assert!(
            resp.text().contains(want_label),
            "{raw_path} -> {rel} missing {want_label:?}:\n{}",
            resp.text()
        );
        assert_eq!(resp.header("sparkles-at"), want_at, "{raw_path}");
    }

    // Trailing-slash `/myorg/` must NOT match the slashless mount-root IRI `/myorg`.
    assert_eq!(
        eval_htaccess_request(&text, "/myorg/"),
        None,
        "slashed mount root must not match slashless version IRI"
    );
    // Single-slash `/myorg/vocab/Term` must NOT redirect into `/myorg/vocab//Term`.
    assert_eq!(
        eval_htaccess_request(&text, "/myorg/vocab/Term"),
        None,
        "single-slash path must not match double-slash namespace"
    );
    // Multi-`?` query must be explicitly rejected (`400`) rather than altered or dropped.
    assert_eq!(
        eval_htaccess_request(&text, "/myorg/mv/Term?a=1?b=2"),
        Some((400, None)),
        "multi-? query must be rejected with 400"
    );

    // If Apache 2.4 (`apache2` or `httpd`) is installed on this host, verify the exact
    // generated `.htaccess` against real HTTP/1.1 requests to Apache as well.
    let apache_bin = ["/usr/sbin/apache2", "/usr/sbin/httpd"]
        .into_iter()
        .find(|p| std::path::Path::new(p).exists());
    if let Some(bin) = apache_bin
        && std::path::Path::new("/usr/lib/apache2/modules/mod_rewrite.so").exists()
    {
        use std::io::{Read, Write};
        use std::net::{TcpListener, TcpStream};
        use std::process::{Command, Stdio};

        let apache_dir = tempfile::tempdir().unwrap();
        let docroot = apache_dir.path().join("docroot");
        let mount_dir = docroot.join("myorg");
        let logs_dir = apache_dir.path().join("logs");
        std::fs::create_dir_all(&mount_dir).unwrap();
        std::fs::create_dir_all(&logs_dir).unwrap();
        std::fs::write(mount_dir.join(".htaccess"), &text).unwrap();

        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        drop(listener);

        let conf = format!(
            "ServerRoot \"{}\"\n\
             ServerName localhost\n\
             PidFile \"{}/httpd.pid\"\n\
             Listen 127.0.0.1:{port}\n\
             LoadModule mpm_event_module /usr/lib/apache2/modules/mod_mpm_event.so\n\
             LoadModule authz_core_module /usr/lib/apache2/modules/mod_authz_core.so\n\
             LoadModule rewrite_module /usr/lib/apache2/modules/mod_rewrite.so\n\
             LoadModule dir_module /usr/lib/apache2/modules/mod_dir.so\n\
             ErrorLog \"{}/error.log\"\n\
             DocumentRoot \"{}\"\n\
             <Directory \"{}\">\n\
                 AllowOverride All\n\
                 Require all granted\n\
             </Directory>\n",
            apache_dir.path().display(),
            apache_dir.path().display(),
            logs_dir.display(),
            docroot.display(),
            docroot.display(),
        );
        let conf_path = apache_dir.path().join("httpd.conf");
        std::fs::write(&conf_path, conf).unwrap();

        struct ApacheGuard(std::process::Child);
        impl Drop for ApacheGuard {
            fn drop(&mut self) {
                let _ = self.0.kill();
                let _ = self.0.wait();
            }
        }

        // `-X` runs Apache in single-process mode with no forked worker children so
        // `ApacheGuard` reliably tears down every process and inherited FD on exit.
        let _apache = ApacheGuard(
            Command::new(bin)
                .args(["-X", "-f", conf_path.to_str().unwrap()])
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
                .expect("spawn apache2"),
        );

        // Wait up to 3s for Apache to listen.
        let mut ready = false;
        for _ in 0..30 {
            if TcpStream::connect(("127.0.0.1", port)).is_ok() {
                ready = true;
                break;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        assert!(ready, "apache2 did not start on port {port}");

        let raw_http = |raw_path: &str| -> String {
            let mut stream = TcpStream::connect(("127.0.0.1", port)).unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(5)))
                .unwrap();
            write!(
                stream,
                "GET {raw_path} HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nConnection: close\r\n\r\n"
            )
            .unwrap();
            let mut raw_resp = String::new();
            stream.read_to_string(&mut raw_resp).unwrap();
            raw_resp
        };

        for (raw_path, want_label, want_at) in cases {
            let raw_resp = raw_http(raw_path);
            assert!(
                raw_resp.starts_with("HTTP/1.1 303"),
                "apache2 {raw_path}: {raw_resp}"
            );
            let loc = raw_resp
                .lines()
                .find_map(|l| {
                    let (k, v) = l.split_once(':')?;
                    k.eq_ignore_ascii_case("location").then(|| v.trim())
                })
                .unwrap_or_else(|| panic!("no Location header for {raw_path}: {raw_resp}"));
            let rel = loc
                .strip_prefix("https://rdf.example.org")
                .unwrap_or_else(|| panic!("unexpected apache2 Location {loc}"));
            let resp = send(&app, get_req(rel, Some("text/turtle"), None)).await;
            assert_eq!(
                resp.status,
                StatusCode::OK,
                "apache2 {raw_path} -> {rel} failed: {}",
                resp.text()
            );
            assert!(
                resp.text().contains(want_label),
                "apache2 {raw_path} -> {rel} missing {want_label:?}:\n{}",
                resp.text()
            );
            assert_eq!(resp.header("sparkles-at"), want_at, "apache2 {raw_path}");
        }

        // Verify real Apache refuses trailing-slash `/myorg/` (distinct from slashless
        // `/myorg`), refuses single-slash `/myorg/vocab/Term` (404, no redirect), and
        // explicitly rejects multi-`?` queries (400).
        let mount_slash = raw_http("/myorg/");
        assert!(
            mount_slash.starts_with("HTTP/1.1 404"),
            "apache2 /myorg/ should be 404 (distinct from /myorg), got: {mount_slash}"
        );
        let miss = raw_http("/myorg/vocab/Term");
        assert!(
            miss.starts_with("HTTP/1.1 404"),
            "apache2 /myorg/vocab/Term should be 404, got: {miss}"
        );
        let bad_q = raw_http("/myorg/mv/Term?a=1?b=2");
        assert!(
            bad_q.starts_with("HTTP/1.1 400"),
            "apache2 /myorg/mv/Term?a=1?b=2 should be 400, got: {bad_q}"
        );

        drop(_apache);
    }

    // Root-path slash versionIri (`https://example.org/`) matches `/` and `/<term>` cleanly.
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
    assert_eq!(
        eval_htaccess_request(&root_text, "/"),
        Some((
            303,
            Some("https://rdf.example.org/$/ld/doc?iri=https%3A%2F%2Fexample.org%2F".to_string())
        ))
    );
    assert_eq!(
        eval_htaccess_request(&root_text, "/code%3Bpart"),
        Some((
            303,
            Some(
                "https://rdf.example.org/$/ld/doc?iri=https%3A%2F%2Fexample.org%2Fcode%253Bpart"
                    .to_string()
            )
        ))
    );
    assert_eq!(
        eval_htaccess_request(&root_text, "/Term?edition=1"),
        Some((
            303,
            Some(
                "https://rdf.example.org/$/ld/doc?iri=https%3A%2F%2Fexample.org%2FTerm%3Fedition%3D1"
                    .to_string()
            )
        ))
    );
}

#[cfg(feature = "auth")]
#[tokio::test]
async fn html_does_not_mark_dataset_version_with_unreadable_graphs_as_hosted() {
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

[[users]]
name = "admin"
password = "{admin}"
server = ["server-admin"]

[[users]]
name = "pub_reader"
password = "{pub_reader}"
[[users.grants]]
dataset = "ds_acl"
level = "read"
graphs = ["http://example.org/ds/public"]
"#,
        admin = h("admin-pw"),
        pub_reader = h("pub-pw"),
    );
    let auth_path = dir.path().join("auth.toml");
    std::fs::write(&auth_path, &auth_toml).unwrap();
    let auth = crate::auth::load(Some(&auth_path), dir.path(), "127.0.0.1", false).unwrap();

    let mut st =
        AppState::new(dir.path(), StoreOptions::default(), Duration::from_secs(30)).unwrap();
    st.auth = auth;
    let st = Arc::new(st);
    let ds = st.attach("ds_acl", DbType::Mem, None).unwrap();
    let trig = r#"
@prefix rdfs: <http://www.w3.org/2000/01/rdf-schema#> .
<http://example.org/ds/public> {
  <http://example.org/pubv/Term>
    rdfs:label "Public Term" ;
    rdfs:seeAlso <http://example.org/ds/v1-pub>,
                 <http://example.org/ds/v2-priv>,
                 <http://example.org/ds/unregistered> .
  <http://example.org/ds/v1-pub> rdfs:label "Readable Dataset Version" .
}
<http://example.org/ds/private> {
  <http://example.org/ds/v2-priv> rdfs:label "Private Dataset Version" .
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
    let app = crate::http::router(st.clone());

    let reg_v = send(
        &app,
        post_json(
            "/$/resources",
            json!({
                "id": "pubv",
                "kind": "vocabulary",
                "title": "Public Vocab",
                "namespaces": [{"prefix": "pubv", "iri": "http://example.org/pubv/"}],
                "graphs": [{"dataset": "ds_acl", "graph": "http://example.org/ds/public"}]
            }),
            Some(&admin_auth),
        ),
    )
    .await;
    assert_eq!(reg_v.status, StatusCode::CREATED, "{}", reg_v.text());

    let reg_d = send(
        &app,
        post_json(
            "/$/resources",
            json!({
                "id": "my_ds_res",
                "kind": "dataset",
                "title": "Mixed Dataset",
                "dataset": "ds_acl",
                "versions": [
                    {
                        "version": "1.0",
                        "versionIri": "http://example.org/ds/v1-pub",
                        "graphs": [{"graph": "http://example.org/ds/public"}]
                    },
                    {
                        "version": "2.0",
                        "versionIri": "http://example.org/ds/v2-priv",
                        "graphs": [{"graph": "http://example.org/ds/private"}]
                    }
                ]
            }),
            Some(&admin_auth),
        ),
    )
    .await;
    assert_eq!(reg_d.status, StatusCode::CREATED, "{}", reg_d.text());

    // pub_reader has dataset-level read on `ds_acl` restricted to `public`:
    // HTML must mark `v1-pub` as hosted (`/$/ld?iri=...`) and render `v2-priv` as an
    // external link indistinguishable from `unregistered`.
    let pub_html = send(
        &app,
        get_req(
            &format!("/$/ld/doc?iri={}", enc("http://example.org/pubv/Term")),
            Some("text/html"),
            Some(&pub_auth),
        ),
    )
    .await;
    assert_eq!(pub_html.status, StatusCode::OK, "{}", pub_html.text());
    let body = pub_html.text();
    let local_pub = format!("href=\"/$/ld?iri={}\"", enc("http://example.org/ds/v1-pub"));
    let local_priv = format!(
        "href=\"/$/ld?iri={}\"",
        enc("http://example.org/ds/v2-priv")
    );
    let ext_priv = "href=\"http://example.org/ds/v2-priv\" rel=\"noopener noreferrer\"";
    let ext_unreg = "href=\"http://example.org/ds/unregistered\" rel=\"noopener noreferrer\"";
    assert!(
        body.contains(&local_pub),
        "missing hosted v1-pub link:\n{body}"
    );
    assert!(
        !body.contains(&local_priv),
        "unreadable dataset version v2-priv leaked as hosted link:\n{body}"
    );
    assert!(
        body.contains(ext_priv),
        "unreadable dataset version v2-priv must render as plain external link:\n{body}"
    );
    assert!(body.contains(ext_unreg), "{body}");

    // Direct lookup of v2-priv by pub_reader returns 404.
    let priv_lookup = send(
        &app,
        get_req(
            &format!("/$/ld/doc?iri={}", enc("http://example.org/ds/v2-priv")),
            Some("text/turtle"),
            Some(&pub_auth),
        ),
    )
    .await;
    assert_eq!(priv_lookup.status, StatusCode::NOT_FOUND);

    // admin can read both graphs and sees both versions linked locally.
    let adm_html = send(
        &app,
        get_req(
            &format!("/$/ld/doc?iri={}", enc("http://example.org/pubv/Term")),
            Some("text/html"),
            Some(&admin_auth),
        ),
    )
    .await;
    assert_eq!(adm_html.status, StatusCode::OK, "{}", adm_html.text());
    let adm_body = adm_html.text();
    assert!(adm_body.contains(&local_pub), "{adm_body}");
    assert!(adm_body.contains(&local_priv), "{adm_body}");
}

mod publishing {
    use super::*;

    const ROOT: &str = "https://example.org/vocabulary/";
    const CHILD: &str = "https://example.org/vocabulary/A_001";
    const PARENT: &str = "https://example.org/vocabulary/A_002";
    const PUBLIC: &str = "https://example.org/graph/public";
    const PRIVATE: &str = "https://example.org/graph/private";

    fn seed(st: &AppState) {
        let ds = st.attach("vocabulary", DbType::Mem, None).unwrap();
        let data = r#"
@prefix v: <https://example.org/vocabulary/> .
@prefix rdf: <http://www.w3.org/1999/02/22-rdf-syntax-ns#> .
@prefix rdfs: <http://www.w3.org/2000/01/rdf-schema#> .
@prefix owl: <http://www.w3.org/2002/07/owl#> .
@prefix skos: <http://www.w3.org/2004/02/skos/core#> .
@prefix dct: <http://purl.org/dc/terms/> .
<https://example.org/graph/public> {
  v: a owl:Ontology ; rdfs:label "Cardiac ontology" ; dct:creator "Ada" .
  v:A_001 a owl:Class ; rdfs:label "Cardiac muscle"@en, "Muscle cardiaque"@fr ;
    skos:altLabel "Heart muscle" ;
    skos:definition "Hidden myocardium" ;
    rdfs:comment "Contractile tissue" ;
    rdfs:subClassOf v:A_002 ;
    skos:scopeNote "Used in anatomy" ;
    skos:example "<script>unsafe</script>" .
  v:A_002 a owl:Class ; rdfs:label "Muscle tissue" ;
    rdfs:subClassOf v:A_001 .
  v:Masked skos:definition "Hidden myocardial entity" .
}
<https://example.org/graph/private> {
  v:Private a owl:Class ; rdfs:subClassOf v:A_001 ; rdfs:label "Private cardiac information" .
}
v:Default a owl:Class ; rdfs:label "Cardiac default graph" .
"#;
        ds.store
            .load(&[Source::from_bytes(
                data.as_bytes().to_vec(),
                RdfFormat::TriG,
                None,
            )])
            .unwrap();
    }

    async fn register(app: &Router, auth: Option<&str>) {
        for (id, kind, title, graphs, dataset) in [
            (
                "anatomy",
                "vocabulary",
                "Vocabulary & anatomy",
                vec![PUBLIC],
                None,
            ),
            (
                "secret",
                "vocabulary",
                "Unpublished resource",
                vec![PRIVATE],
                None,
            ),
            (
                "all-data",
                "dataset",
                "All data",
                vec![],
                Some("vocabulary"),
            ),
        ] {
            let mut json = json!({ "id": id, "kind": kind, "title": title });
            if let Some(dataset) = dataset {
                json["dataset"] = json!(dataset);
            } else {
                json["graphs"] = json!(
                    graphs
                        .iter()
                        .map(|graph| json!({"dataset": "vocabulary", "graph": graph}))
                        .collect::<Vec<_>>()
                );
                let namespace = if id == "anatomy" {
                    ROOT
                } else {
                    "https://example.org/private/"
                };
                json["namespaces"] = json!([{"prefix": id, "iri": namespace}]);
            }
            let response = send(app, post_json("/$/resources", json, auth)).await;
            assert_eq!(response.status, StatusCode::CREATED, "{}", response.text());
        }
    }

    async fn get(app: &Router, uri: &str, auth: Option<&str>) -> Resp {
        let mut request = get_req(uri, None, auth);
        // Use one correlation ID so the hidden/missing body comparison checks
        // the complete error envelope.
        request.headers_mut().insert(
            crate::obs::X_REQUEST_ID.clone(),
            axum::http::HeaderValue::from_static("linked-data-test"),
        );
        send(app, request).await
    }

    fn state(dir: &tempfile::TempDir) -> AppState {
        AppState::new(dir.path(), StoreOptions::default(), Duration::from_secs(20)).unwrap()
    }

    fn term_path() -> String {
        format!("/ols4/api/ontologies/anatomy/terms/{}", enc(&enc(CHILD)))
    }

    #[tokio::test]
    async fn documents_cover_terms_hierarchy_examples_and_static_export() {
        let dir = tempfile::tempdir().unwrap();
        let st = Arc::new(state(&dir));
        seed(&st);
        let app = crate::http::router(st);
        register(&app, None).await;

        let live = get(&app, "/$/ld/resources/anatomy/doc", None).await;
        assert_eq!(live.status, StatusCode::OK, "{}", live.text());
        assert_eq!(
            live.header("content-type"),
            Some("text/html; charset=utf-8")
        );
        assert_eq!(live.header("cache-control"), Some("private, no-store"));
        assert_eq!(live.header("content-security-policy"), Some(html::HTML_CSP));
        let text = live.text();
        for value in [
            "Vocabulary &amp; anatomy",
            "Annotation coverage",
            "Cardiac ontology",
            "Ontology metadata",
            "Ada",
            "Cardiac muscle",
            "Definitions",
            "Contractile tissue",
            "Hierarchy",
            "Usage",
            "Used in anatomy",
            "Examples",
            "&lt;script&gt;unsafe&lt;/script&gt;",
        ] {
            assert!(text.contains(value), "missing {value}: {text}");
        }
        assert!(!text.contains("Private cardiac information"));
        assert!(!text.contains("Cardiac default graph"));
        assert!(!text.contains("<script>"));
        let first_anchor = text
            .split("href=\"#")
            .nth(1)
            .unwrap()
            .split('"')
            .next()
            .unwrap();
        assert!(text.contains(&format!("id=\"{first_anchor}\"")));

        let export = get(&app, "/$/ld/resources/anatomy/doc?download=true", None).await;
        assert_eq!(export.status, StatusCode::OK, "{}", export.text());
        assert_eq!(
            export.header("content-disposition"),
            Some("attachment; filename=\"ontology.html\"")
        );
        assert_eq!(export.body, live.body);
        let focused = get(
            &app,
            &format!("/$/ld/resources/anatomy/doc?iri={}", enc(CHILD)),
            None,
        )
        .await;
        assert_eq!(focused.status, StatusCode::OK, "{}", focused.text());
        assert!(focused.text().contains("Children"));
        assert!(focused.text().contains("href=\"?iri="));
        let focused_static = get(
            &app,
            &format!(
                "/$/ld/resources/anatomy/doc?iri={}&download=true",
                enc(CHILD)
            ),
            None,
        )
        .await;
        assert_eq!(
            focused_static.status,
            StatusCode::OK,
            "{}",
            focused_static.text()
        );
        assert!(!focused_static.text().contains("href=\"?iri="));

        let dataset = get(&app, "/$/ontology/vocabulary/doc", None).await;
        assert_eq!(dataset.status, StatusCode::OK, "{}", dataset.text());
        assert!(dataset.text().contains("Cardiac default graph"));
        assert!(dataset.text().contains("Private cardiac information"));

        let hosted = send(
            &app,
            get_req(
                &format!("/$/ld/doc?iri={}", enc(CHILD)),
                Some("text/html"),
                None,
            ),
        )
        .await;
        assert_eq!(hosted.status, StatusCode::OK, "{}", hosted.text());
        assert_eq!(hosted.header("vary"), Some("Accept"));
        assert!(hosted.text().contains("<h2>Definitions</h2>"));
        assert!(hosted.text().contains("<h2>Hierarchy</h2>"));
    }

    #[tokio::test]
    async fn search_modes_facets_pagination_and_ols_wire_shapes() {
        let dir = tempfile::tempdir().unwrap();
        let st = Arc::new(state(&dir));
        seed(&st);
        let app = crate::http::router(st);
        register(&app, None).await;

        let search = get(&app, "/$/ld/search?q=cardiac&mode=lexical&limit=1", None).await;
        assert_eq!(search.status, StatusCode::OK, "{}", search.text());
        assert_eq!(search.header("cache-control"), Some("private, no-store"));
        let results = search.json();
        assert_eq!(results["total"], 7);
        assert_eq!(results["hits"].as_array().unwrap().len(), 1);
        assert_eq!(
            results["facets"],
            json!({"all-data": 4, "anatomy": 2, "secret": 1})
        );
        let uri = results["hits"][0]["documentation"].as_str().unwrap();
        assert_eq!(get(&app, uri, None).await.status, StatusCode::OK);

        for (q, mode, expected) in [
            ("contractile tissue", "lexical", 1),
            ("heart mus", "prefix", 1),
            ("cardac muscle", "vector", 1),
            ("cardac muscle", "lexical", 0),
        ] {
            let response = get(
                &app,
                &format!("/$/ld/search?q={}&mode={mode}&ontology=anatomy", enc(q)),
                None,
            )
            .await;
            assert_eq!(response.status, StatusCode::OK, "{}", response.text());
            assert_eq!(response.json()["total"], expected, "{}", response.text());
        }

        let find = get(&app, "/ols4/api/select?q=heart+muscle&exact=true&queryFields=synonym&ontology=anatomy&lang=fr", None).await;
        assert_eq!(find.status, StatusCode::OK, "{}", find.text());
        let find = find.json();
        assert_eq!(find["response"]["numFound"], 1);
        assert_eq!(find["response"]["docs"][0]["label"], "Muscle cardiaque");
        assert_eq!(find["response"]["docs"][0]["obo_id"], "A:001");
        assert_eq!(
            find["facet_counts"]["facet_fields"]["ontologyId"],
            json!(["anatomy", "1"])
        );

        let suggest = get(&app, "/ols4/api/suggest?q=heart+mus&ontology=anatomy", None).await;
        assert_eq!(suggest.status, StatusCode::OK, "{}", suggest.text());
        assert_eq!(
            suggest.json()["response"]["docs"],
            json!([{"autosuggest": "Heart muscle"}])
        );

        let ontologies = get(&app, "/ols4/api/ontologies?page=1&size=1", None).await;
        assert_eq!(ontologies.status, StatusCode::OK, "{}", ontologies.text());
        assert_eq!(
            ontologies.json()["_embedded"]["ontologies"][0]["ontologyId"],
            "anatomy"
        );
        assert_eq!(ontologies.json()["page"]["totalElements"], 3);

        let terms = get(&app, "/ols4/api/ontologies/anatomy/terms?size=1", None).await;
        assert_eq!(terms.status, StatusCode::OK, "{}", terms.text());
        let terms = terms.json();
        assert_eq!(terms["page"]["totalElements"], 2);
        let next = terms["_links"]["next"]["href"].as_str().unwrap();
        assert_eq!(
            get(&app, next, None).await.json()["_embedded"]["terms"][0]["iri"],
            PARENT
        );
        let term_uri = term_path();
        let term = get(&app, &term_uri, None).await;
        assert_eq!(term.status, StatusCode::OK, "{}", term.text());
        assert_eq!(term.json()["iri"], CHILD);
        for relation in ["parents", "children", "ancestors", "descendants"] {
            let result = get(&app, &format!("{term_uri}/{relation}"), None).await;
            assert_eq!(result.status, StatusCode::OK, "{}", result.text());
            assert_eq!(result.json()["page"]["totalElements"], 1);
            assert_eq!(result.json()["_embedded"]["terms"][0]["iri"], PARENT);
        }
        let invalid = get(&app, "/$/ld/search?q=cardiac&offset=10000", None).await;
        assert_eq!(invalid.status, StatusCode::BAD_REQUEST);
    }

    #[cfg(feature = "auth")]
    #[tokio::test]
    async fn publishing_checks_dataset_graph_and_triple_grants_before_indexing() {
        use base64::Engine as _;
        let dir = tempfile::tempdir().unwrap();
        let password = crate::auth::hash_password_with("secret", 8, 1, 1).unwrap();
        let auth_file = dir.path().join("auth.toml");
        std::fs::write(
            &auth_file,
            format!(
                r#"
version = 1
[anonymous]
datasets = {{}}
[[protections]]
name = "definition"
dataset = "vocabulary"
predicates = ["http://www.w3.org/2004/02/skos/core#definition"]
[[users]]
name = "operator"
password = "{password}"
server = ["server-admin"]
[[users]]
name = "reader"
password = "{password}"
[[users.grants]]
dataset = "vocabulary"
level = "read"
graphs = ["{PUBLIC}"]
[[users]]
name = "gsp"
password = "{password}"
[[users.grants]]
dataset = "vocabulary"
level = "read"
graphs = ["{PUBLIC}"]
endpoints = ["gsp-r"]
"#
            ),
        )
        .unwrap();
        let mut st = state(&dir);
        st.auth = crate::auth::load(Some(&auth_file), dir.path(), "127.0.0.1", false).unwrap();
        seed(&st);
        let st = Arc::new(st);
        let app = crate::http::router(st);
        let basic = |name| {
            format!(
                "Basic {}",
                base64::engine::general_purpose::STANDARD.encode(format!("{name}:secret"))
            )
        };
        let admin = basic("operator");
        let reader = basic("reader");
        let gsp = basic("gsp");
        register(&app, Some(&admin)).await;

        // Prime each surface as an administrator before sending the same request as
        // a reader. Neither a private graph nor protected triples may affect the
        // restricted body, counts or navigation.
        for uri in [
            "/$/ontology/vocabulary/doc?download=true",
            "/$/ld/resources/anatomy/doc",
            "/ols4/api/search?q=hidden+myocardium&queryFields=description",
            "/$/ld/search?q=cardiac&mode=lexical",
            "/ols4/api/ontologies",
        ] {
            let privileged = get(&app, uri, Some(&admin)).await;
            assert_eq!(privileged.status, StatusCode::OK, "{}", privileged.text());
            let restricted = get(&app, uri, Some(&reader)).await;
            assert_eq!(restricted.status, StatusCode::OK, "{}", restricted.text());
            assert_eq!(
                restricted.header("cache-control"),
                Some("private, no-store")
            );
            assert_ne!(privileged.body, restricted.body, "{uri}");
            for secret in [
                "Hidden myocardium",
                "Hidden myocardial entity",
                "Private cardiac information",
                "Unpublished resource",
                "/secret",
                "Cardiac default graph",
            ] {
                assert!(
                    !restricted.text().contains(secret),
                    "{uri}: {}",
                    restricted.text()
                );
            }
        }

        for (hidden, missing) in [
            (
                "/$/ld/resources/secret/doc".to_string(),
                "/$/ld/resources/missing/doc".to_string(),
            ),
            (
                "/ols4/api/ontologies/secret".to_string(),
                "/ols4/api/ontologies/missing".to_string(),
            ),
            (
                format!(
                    "/$/ld/resources/anatomy/doc?iri={}",
                    enc(&format!("{ROOT}Masked"))
                ),
                format!(
                    "/$/ld/resources/anatomy/doc?iri={}",
                    enc(&format!("{ROOT}Nonexistent"))
                ),
            ),
            (
                format!(
                    "/ols4/api/ontologies/all-data/terms/{}",
                    enc(&enc(&format!("{ROOT}Private")))
                ),
                format!(
                    "/ols4/api/ontologies/all-data/terms/{}",
                    enc(&enc(&format!("{ROOT}Nonexistent")))
                ),
            ),
        ] {
            let hidden = get(&app, &hidden, Some(&reader)).await;
            let missing = get(&app, &missing, Some(&reader)).await;
            assert_eq!(hidden.status, StatusCode::NOT_FOUND, "{}", hidden.text());
            assert_eq!(hidden.body, missing.body);
        }
        for auth in [None, Some(gsp.as_str())] {
            let list = get(&app, "/ols4/api/ontologies", auth).await;
            assert_eq!(list.status, StatusCode::OK, "{}", list.text());
            assert_eq!(list.json()["_embedded"]["ontologies"], json!([]));
            let doc = get(&app, "/$/ontology/vocabulary/doc", auth).await;
            assert_eq!(doc.status, StatusCode::NOT_FOUND, "{}", doc.text());
        }
        let response = get(&app, "/$/ld/search?q=cardiac&mode=lexical", Some(&reader)).await;
        assert_eq!(
            response.json()["facets"],
            json!({"all-data": 2, "anatomy": 2})
        );
        let response = get(&app, "/ols4/api/suggest?q=private+cardiac", Some(&reader)).await;
        assert_eq!(response.json()["response"]["numFound"], 0);
        let all_terms = format!(
            "/ols4/api/ontologies/all-data/terms/{}/children",
            enc(&enc(CHILD))
        );
        let children = get(&app, &all_terms, Some(&reader)).await;
        assert_eq!(children.json()["page"]["totalElements"], 1);
        assert_eq!(children.json()["_embedded"]["terms"][0]["iri"], PARENT);

        let term = get(&app, &term_path(), Some(&reader)).await;
        assert_eq!(term.status, StatusCode::OK, "{}", term.text());
        assert_eq!(term.json()["description"], json!(["Contractile tissue"]));
        let search = get(
            &app,
            "/$/ld/search?q=private+cardiac&mode=lexical&ontology=secret",
            Some(&reader),
        )
        .await;
        assert_eq!(search.json(), json!({"total": 0, "hits": [], "facets": {}}));
    }

    #[tokio::test]
    async fn publications_fail_closed_over_the_result_limit() {
        let dir = tempfile::tempdir().unwrap();
        let mut st = state(&dir);
        st.limits.max_result_bytes = Some(256);
        seed(&st);
        let app = crate::http::router(Arc::new(st));
        register(&app, None).await;
        for uri in [
            "/$/ontology/vocabulary/doc?download=true",
            "/ols4/api/ontologies",
            "/$/ld/search?q=cardiac",
        ] {
            let result = get(&app, uri, None).await;
            assert_eq!(
                result.status,
                StatusCode::INSUFFICIENT_STORAGE,
                "{uri}: {}",
                result.text()
            );
            assert_eq!(result.header("cache-control"), Some("private, no-store"));
            assert!(result.header("content-disposition").is_none());
        }
    }
}
